// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! `memmesh` — single binary, multiple subcommands.
//!
//!   migrate   Apply pending schema migrations
//!   save      Insert / upsert a memory item
//!   get       Fetch a memory item by id
//!   search    Filter + text-match memory items
//!   mcp       Start the MCP stdio server (wire to Claude Code / Cursor / etc)
//!
//! v1 ships SQLite-backed local mode only. Postgres / sync / vector search
//! land in subsequent releases.

mod installer;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use installer::{Action, SkillBundle, Tool};
use memory_core::config::DatabaseBackend;
use memory_core::{MemoryItem, MemoryScope};
use memory_license::License;
use memory_storage::{postgres::PostgresStore, sqlite::SqliteStore, MemoryFilter, Storage};
use std::sync::Arc;

/// Default DB path under $XDG_DATA_HOME / ~/.local/share / fallback CWD.
fn default_db_path() -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let dir = std::path::Path::new(&home).join(".memmesh");
        std::fs::create_dir_all(&dir).ok();
        return dir.join("memory.db").to_string_lossy().to_string();
    }
    "./memory.db".to_string()
}

#[derive(Parser)]
#[command(
    name = "memmesh",
    version,
    about = "ThinkFleet memory engine — local-first, MCP-native"
)]
struct Cli {
    /// Path to the SQLite database. Created if missing.
    #[arg(long, env = "THINKFLEET_MEMORY_DB", default_value_t = default_db_path())]
    db: String,

    /// Log level (trace / debug / info / warn / error). Honors RUST_LOG too.
    #[arg(long, default_value = "info")]
    log: String,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Apply pending schema migrations. Safe to run repeatedly.
    Migrate,

    /// Save a memory item. Auto-generates an id if not provided.
    Save {
        /// Memory id. Auto-generated (UUIDv7) if omitted.
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        platform: String,
        #[arg(long)]
        project: Option<String>,
        #[arg(long, help = "Maps to chatbotId in the underlying schema")]
        agent: Option<String>,
        #[arg(long, help = "Maps to chatIdentityId")]
        user: Option<String>,
        #[arg(long, help = "Maps to sessionKey")]
        session: Option<String>,
        /// Memory type (fact / preference / rule / etc).
        #[arg(long, default_value = "fact")]
        r#type: String,
        /// Memory content (the actual remembered text).
        #[arg(long)]
        content: String,
        /// Scope: platform / project / location / agent / user / session.
        #[arg(long, default_value = "project")]
        scope: String,
        #[arg(long, default_value_t = 5.0)]
        importance: f32,
        #[arg(long, default_value_t = 1.0)]
        confidence: f32,
    },

    /// Fetch a memory item by id. Bumps lastAccessedAt.
    Get { id: String },

    /// Delete a memory item. Soft-delete by default (status = rejected so
    /// sync can propagate). Use --hard for a physical SQL DELETE.
    Delete {
        id: String,
        /// Physical SQL DELETE — loses sync traceability. Use sparingly.
        #[arg(long)]
        hard: bool,
    },

    /// Observe a piece of raw text and let the engine decide what to save.
    ///
    /// Designed for use from AI-tool hooks (e.g. Claude Code's
    /// UserPromptSubmit). Heuristic extraction runs over the input; any
    /// candidates that fire a rule are saved automatically. Defaults pull
    /// `userId` from $USER and `projectId` from the current git repo name.
    Observe {
        /// Text to observe. If omitted, reads from stdin.
        #[arg(long)]
        content: Option<String>,
        /// Role of the speaker: user / assistant / system.
        #[arg(long, default_value = "user")]
        role: String,
        #[arg(long)]
        platform: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        session: Option<String>,
        /// RFC3339 timestamp of when this happened in the world (event time),
        /// as opposed to now (ingest time). Behavior mining buckets patterns by
        /// this, so set it when observing back-dated text.
        #[arg(long, value_name = "RFC3339")]
        occurred_at: Option<DateTime<Utc>>,
        /// Print structured JSON of what was saved (machine-readable mode
        /// for hooks). Without it, prints a human-readable summary.
        #[arg(long)]
        json: bool,
    },

    /// Search memory items.
    Search {
        /// Substring to match against content (optional).
        #[arg(long)]
        query: Option<String>,
        #[arg(long)]
        platform: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        r#type: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: u32,
        #[arg(long, default_value_t = 0)]
        offset: u32,
        /// Output format. `json` (default) for machine consumers,
        /// `claude-context` for a compact text block suitable for piping
        /// into an AI tool's context via a SessionStart hook.
        #[arg(long, default_value = "json")]
        format: String,
    },

    /// Find near-duplicate memories within a scope and non-destructively
    /// collapse the redundant ones into a survivor (via supersede, so history
    /// and provenance are kept). Two memories are "duplicates" when their
    /// embeddings' cosine similarity is >= `--threshold`. Safe + idempotent:
    /// re-running skips anything already collapsed, and `--dry-run` shows what
    /// would happen without writing. Requires semantic embeddings for cosine
    /// matching; without them it falls back to exact normalized-text equality.
    Consolidate {
        #[arg(long)]
        platform: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        r#type: Option<String>,
        /// Cosine similarity at/above which two memories are considered
        /// duplicates. Higher = stricter. Default 0.95.
        #[arg(long, default_value_t = 0.95)]
        threshold: f32,
        /// Report what would be collapsed without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },

    /// Start the MCP stdio server. Wire this into an AI tool's MCP config
    /// (e.g. Claude Code, Cursor) so it can read/write memory.
    Mcp,

    /// Start the local HTTP REST API. Consumed by the desktop GUI for
    /// memory browsing / management / install orchestration. Binds to
    /// loopback only — no auth in v1.
    Serve {
        /// Address to bind. Default 127.0.0.1:7878.
        #[arg(long, default_value = "127.0.0.1:7878")]
        http: String,
    },

    /// Launch the local web console: starts the REST API, serves the
    /// memory management UI, and opens it in your browser. One command to
    /// see engine status, browse memories, tail logs, and tweak config.
    Console {
        /// Address to bind. Default 127.0.0.1:7878.
        #[arg(long, default_value = "127.0.0.1:7878")]
        http: String,
        /// Don't auto-open a browser; just print the URL.
        #[arg(long)]
        no_open: bool,
    },

    /// Start a REMOTE MCP server over Streamable HTTP for web chats
    /// (ChatGPT + Claude.ai custom connectors), which can't use a local
    /// stdio server. Unlike `mcp` (stdio) and `console` (loopback, no auth),
    /// this is meant to be exposed publicly via a tunnel, so it REQUIRES a
    /// bearer token. Pass --token or set MEMMESH_MCP_TOKEN; if neither is
    /// set, a random token is generated and printed.
    ServeMcp {
        /// Address to bind. Default 127.0.0.1:7899 (put a tunnel in front).
        #[arg(long, default_value = "127.0.0.1:7899")]
        http: String,
        /// Bearer token clients must send. Auto-generated if omitted.
        #[arg(long, env = "MEMMESH_MCP_TOKEN")]
        token: Option<String>,
    },

    /// Encrypted secrets vault. Store credentials the AI can *reference* but
    /// never read — values are entered here (never in chat), sealed with the
    /// OS-keychain-backed master key, and used via execute-through-vault so
    /// plaintext never reaches the model.
    Secret {
        #[command(subcommand)]
        action: SecretAction,
    },

    /// Manage the agent teaching skill (markdown that tells the AI when
    /// and how to use the memory tools).
    Skill {
        #[command(subcommand)]
        action: SkillAction,
    },

    /// Bind a working-directory path to a SaaS (platform, project). Used in
    /// SaaS-connected mode so memories written from this cwd push to the
    /// right central project. In local-only mode this is a no-op signal but
    /// the binding is still recorded for forward compatibility.
    Bind {
        /// Path to bind. Defaults to the current working directory.
        #[arg(long)]
        cwd: Option<String>,
        /// SaaS platform id. Defaults to the signed-in platform (from
        /// `~/.memmesh/config.toml`).
        #[arg(long)]
        platform: Option<String>,
        /// SaaS project id. Required.
        #[arg(long)]
        project: String,
    },

    /// Inspect / manage project bindings.
    Bindings {
        #[command(subcommand)]
        action: BindingsAction,
    },

    /// Read or write the engine config (`~/.memmesh/config.toml`).
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },

    /// Inspect or set the per-platform monthly LLM cost budget. The
    /// budget is the gate every paid-API call goes through — set a cap
    /// here and the engine refuses to exceed it for the rest of the
    /// month. Default state is no cap (unlimited spend).
    Budget {
        #[command(subcommand)]
        action: BudgetAction,
    },

    /// Run one sync cycle against the SaaS and exit. Useful for one-shot
    /// pushes from CI / scripts, and for debugging without leaving a
    /// long-lived `serve` running.
    Sync {
        /// Skip the token validation probe. The cycle still attempts the
        /// HTTP calls; this just suppresses the upfront /v1/users/me ping.
        #[arg(long)]
        skip_token_check: bool,
    },

    /// Run retrieval-quality evaluations. Each fixture is a YAML file
    /// with a synthetic corpus + queries + expected hits. The runner
    /// seeds a fresh in-memory store, runs each query, and emits a
    /// scorecard (P@K, R@K, MRR).
    ///
    /// Used as a regression net before/after changes to extraction,
    /// scoring, or ranking. CI-friendly: `--json` for machine consumers,
    /// `--fixture <path>` for a single fixture.
    Eval {
        /// Path to a fixture YAML file. If omitted, runs the bundled
        /// fixture suite baked into the binary at build time.
        #[arg(long)]
        fixture: Option<String>,
        /// Emit the full scorecard (including per-query rows) as JSON
        /// to stdout. Without this flag, a human-readable summary
        /// table is printed.
        #[arg(long)]
        json: bool,
    },

    /// Install memmesh into every AI tool we can detect.
    ///
    /// For each tool: writes the MCP server config block AND drops the
    /// agent teaching skill into the tool's skills directory. Existing
    /// configs are merged — other MCP servers stay untouched.
    Install {
        /// Only install for a specific tool. Repeatable: `--tool cursor
        /// --tool claude-code`. If omitted, installs to every detected
        /// tool. Known ids: claude-code, cursor, windsurf, codex, cline,
        /// continue, goose, zed.
        #[arg(long)]
        tool: Vec<String>,

        /// Show what would change without writing anything.
        #[arg(long)]
        dry_run: bool,

        /// Install even when the tool isn't detected (creates the config
        /// dir + file from scratch).
        #[arg(long)]
        force: bool,

        /// Skip the MCP config — only install the skill.
        #[arg(long)]
        skill_only: bool,

        /// Skip the skill — only install the MCP config.
        #[arg(long)]
        mcp_only: bool,

        /// Skip the Claude Code `UserPromptSubmit` auto-observe hook.
        /// Default: hooks are installed (this is the main UX value).
        #[arg(long)]
        no_hooks: bool,

        /// Override the binary path written into the MCP configs. By
        /// default uses the running binary's absolute path.
        #[arg(long)]
        binary: Option<String>,
    },

    /// Display the current license status: plan tier, memory cap,
    /// expiry, feature flags. Reads from `MEMORY_LICENSE_TOKEN` (env
    /// var) or `MEMORY_LICENSE_PATH` (file), falling back to the free
    /// tier when no token is loaded.
    License {
        /// Machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },

    /// Redeem a purchase code at api.memmesh.ai for a signed
    /// license JWT bound to this machine, then persist it locally so
    /// future engine starts pick it up automatically.
    ///
    /// Get a code at https://api.memmesh.ai/upgrade. The code
    /// is single-use and machine-bound; if you reinstall on a new
    /// machine, get a fresh code from your dashboard.
    Activate {
        /// Activation code from your purchase email (e.g. TF-A1B2-C3D4-E5F6).
        code: String,

        /// Override the activation endpoint. Useful for staging and
        /// local mock servers during development. Defaults to
        /// `https://api.memmesh.ai/api/v1/license/activate`,
        /// or the value of THINKFLEET_ACTIVATION_ENDPOINT env var if
        /// set.
        #[arg(long, env = "THINKFLEET_ACTIVATION_ENDPOINT")]
        endpoint: Option<String>,
    },
}

#[derive(Subcommand)]
enum BindingsAction {
    /// List every binding on this machine. JSON output for scripting.
    List,
    /// Show the binding that resolves the given cwd (or the current cwd).
    /// Exact match only — doesn't walk the dir tree (that's a runtime-only
    /// behavior, intentionally hidden from CLI to keep it predictable).
    Show {
        #[arg(long)]
        cwd: Option<String>,
    },
    /// Remove a binding. Idempotent.
    Remove {
        #[arg(long)]
        cwd: Option<String>,
    },
}

#[derive(Subcommand)]
enum BudgetAction {
    /// Show the current period's spend + remaining headroom for a
    /// platform. Defaults to the signed-in platform from config.
    Show {
        #[arg(long)]
        platform: Option<String>,
    },
    /// Set the monthly cap in USD. Existing cumulative spend is
    /// preserved — only the ceiling moves.
    SetCap {
        #[arg(long)]
        platform: Option<String>,
        /// Cap in US dollars (e.g. `--usd 5.00`).
        #[arg(long)]
        usd: f64,
    },
    /// Remove the cap. Engine will allow any spend.
    ClearCap {
        #[arg(long)]
        platform: Option<String>,
    },
    /// Record a spend manually. For testing / backfill of an external
    /// charge that wasn't routed through the gate.
    Record {
        #[arg(long)]
        platform: Option<String>,
        #[arg(long)]
        usd: f64,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Print the resolved config (TOML + env merged) to stdout.
    Show,
    /// Set the `[sync]` section: SaaS URL, bearer token, platform id.
    /// Called by the desktop bridge after login. Writes the config file
    /// atomically; existing values are replaced.
    SetSync {
        #[arg(long)]
        url: String,
        #[arg(long)]
        token: String,
        #[arg(long)]
        platform_id: String,
    },
    /// Remove the `[sync]` section — drops the engine back into local-only
    /// mode. Used at logout.
    ClearSync,
}

#[derive(Subcommand)]
enum SecretAction {
    /// Store (or replace) a secret. Prompts for the value on a hidden line —
    /// it is never passed as an argument or echoed.
    Set {
        /// Reference name, e.g. `aws-prod`. Used as `{{memmesh:aws-prod}}`.
        name: String,
        #[arg(long, help = "Category, e.g. aws / openai / db / password")]
        kind: Option<String>,
        #[arg(long, help = "Non-secret note shown in listings")]
        desc: Option<String>,
    },
    /// List stored secrets (names, kinds, descriptions) — never values.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Remove a secret.
    Rm { name: String },
    /// Execute-through-vault: run a command with `{{memmesh:NAME}}` references
    /// resolved in-process. Output is returned with secret values scrubbed.
    Run {
        /// The command to run (quote it). e.g. "aws s3 ls --profile {{memmesh:aws-prod}}"
        command: String,
    },
}

#[derive(Subcommand)]
enum SkillAction {
    /// Print the skill markdown to stdout.
    Print,

    /// Install the skill into a tool's skills directory.
    Install {
        /// Tool to install for: `claude-code` (~/.claude/skills/), `cursor`
        /// (~/.cursor/skills/), or `--dir <path>` for a custom location.
        #[arg(long, default_value = "claude-code")]
        tool: String,

        /// Custom install directory. Overrides `--tool`.
        #[arg(long)]
        dir: Option<String>,
    },
}

/// Skill markdown baked into the binary at build time. Single source of
/// truth — the file on disk and the bytes in the binary cannot drift.
const SKILL_MD: &str = include_str!("../../../skills/memmesh/SKILL.md");

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Hold the log-file guard for the whole process so buffered log lines
    // flush on exit. Dropping it early would truncate the tail of the log.
    let _log_guard = init_tracing(&cli.log)?;

    // Resolve license once per CLI invocation. `tflk_...` license keys
    // trigger an HTTP exchange against the SaaS to receive a JWT; raw
    // JWTs verify locally without network.
    let license = License::load_from_env(Utc::now()).await;

    // Pick the storage backend from config, then dispatch into the generic
    // `run`. Both arms monomorphize `run` — the whole command surface works
    // identically on SQLite or Postgres.
    let cfg = memory_core::config::Config::load_or_default();
    match cfg.database.backend {
        DatabaseBackend::Sqlite => {
            let db_url = format!("sqlite://{}?mode=rwc", cli.db);
            let store = SqliteStore::connect(&db_url)
                .await
                .with_context(|| format!("opening sqlite store at {}", cli.db))?;
            store.migrate().await.context("running migrations")?;
            run(Arc::new(store), cli, license).await
        }
        DatabaseBackend::Postgres => {
            let url = cfg.database.url.clone().ok_or_else(|| {
                anyhow!(
                    "[database] backend is \"postgres\" but no url is set. Add a url \
                     to ~/.memmesh/config.toml (or set THINKFLEET_DATABASE_URL), or \
                     switch back to sqlite in the console."
                )
            })?;
            let store = PostgresStore::connect(&url)
                .await
                .with_context(|| "connecting to postgres")?;
            store.migrate().await.context("running postgres migrations")?;
            tracing::info!("storage backend: postgres");
            run(Arc::new(store), cli, license).await
        }
    }
}

/// Generic command dispatch — runs against whichever `Storage` backend
/// `main` selected. Monomorphized once per backend.
async fn run<S: Storage>(store: Arc<S>, cli: Cli, license: License) -> Result<()> {
    match cli.cmd {
        Cmd::Migrate => {
            let version = store.migrate().await?;
            println!("schema version: {version}");
        }

        Cmd::Save {
            id,
            platform,
            project,
            agent,
            user,
            session,
            r#type,
            content,
            scope,
            importance,
            confidence,
        } => {
            let id = id.unwrap_or_else(|| {
                // UUIDv7 is monotonic-ish; truncate to 21 chars to match the
                // ApId schema width the rest of the platform uses.
                let raw = uuid::Uuid::now_v7().to_string().replace('-', "");
                raw.chars().take(21).collect()
            });
            let scope_parsed = parse_scope(&scope)?;
            let mut item = MemoryItem::new(id.clone(), platform, r#type, content, scope_parsed);
            item.project_id = project;
            item.chatbot_id = agent;
            item.chat_identity_id = user;
            item.session_key = session;
            item.importance = importance;
            item.confidence = confidence;
            memory_storage::quota::ensure_under_cap(store.as_ref(), license.cap())
                .await
                .map_err(|e| anyhow!("{e}"))?;
            store.save(&item).await?;
            // Index it for semantic search (no-op when embeddings are off).
            memory_storage::embedding::embed_and_store(store.as_ref(), &item.id, &item.content)
                .await;
            println!("{}", id);
        }

        Cmd::Get { id } => {
            let found = store.get(&id).await?;
            match found {
                Some(item) => {
                    store.touch(&item.id).await.ok();
                    println!("{}", serde_json::to_string_pretty(&item)?);
                }
                None => {
                    eprintln!("not found: {id}");
                    std::process::exit(2);
                }
            }
        }

        Cmd::Delete { id, hard } => {
            store.delete(&id, hard).await?;
            println!("{}: {}", if hard { "hard-deleted" } else { "rejected" }, id);
        }

        Cmd::Observe {
            content,
            role,
            platform,
            project,
            user,
            agent,
            session,
            occurred_at,
            json,
        } => {
            let text = match content {
                Some(c) => c,
                None => {
                    use std::io::Read;
                    let mut buf = String::new();
                    std::io::stdin().read_to_string(&mut buf)?;
                    // AI-tool hooks (Claude Code's UserPromptSubmit, etc.) deliver
                    // their input as a JSON envelope on stdin, not the raw prompt —
                    // e.g. {"hook_event_name":"UserPromptSubmit","prompt":"…"}.
                    // Observing the wrapper JSON extracts nothing, so pull the
                    // actual prompt out when we recognize that shape.
                    extract_hook_prompt(&buf).unwrap_or(buf)
                }
            };
            // Drop harness/tool noise before it ever reaches the engine.
            // task-notifications, system-reminders, and command wrappers are
            // machine chatter, not things worth remembering — capturing them
            // pollutes recall and buries the real memories.
            if is_noise(&text) {
                if json {
                    println!("{}", serde_json::json!({"saved": [], "candidateCount": 0, "skipped": "noise"}));
                } else {
                    println!("observed: 0 saved (filtered harness noise)");
                }
                return Ok(());
            }
            let role_parsed = match role.as_str() {
                "user" => Some(memory_core::extraction::ObserveRole::User),
                "assistant" => Some(memory_core::extraction::ObserveRole::Assistant),
                "system" => Some(memory_core::extraction::ObserveRole::System),
                other => return Err(anyhow!("invalid --role {other}")),
            };
            let req = memory_storage::observe::ObserveRequest {
                text,
                role: role_parsed,
                platform_id: Some(platform.unwrap_or_else(|| "local".to_string())),
                project_id: project.or_else(detect_git_project),
                user_id: Some(user.unwrap_or_else(detect_os_user)),
                agent_id: agent,
                session_id: session,
                occurred_at,
            };
            memory_storage::quota::ensure_under_cap(store.as_ref(), license.cap())
                .await
                .map_err(|e| anyhow!("{e}"))?;
            let resp = memory_storage::observe::observe(store.as_ref(), &req).await?;
            if json {
                println!("{}", serde_json::to_string(&resp)?);
            } else if resp.saved.is_empty() {
                println!(
                    "observed: 0 saved ({} candidate(s) considered)",
                    resp.candidate_count
                );
            } else {
                println!("observed: {} saved", resp.saved.len());
                for m in &resp.saved {
                    println!("  [{}] {} ({})", m.scope.as_str(), m.content, m.type_);
                }
            }
        }

        Cmd::Search {
            query,
            platform,
            project,
            agent,
            user,
            scope,
            r#type,
            limit,
            offset,
            format,
        } => {
            let project_for_header = project.clone();
            let filter = MemoryFilter {
                platform_id: platform,
                project_id: project,
                agent_id: agent,
                user_id: user,
                scope: scope.as_deref().map(parse_scope).transpose()?,
                kind: r#type,
                // Free text goes through the hybrid searcher via `query`, not
                // the filter's substring `text_match`.
                text_match: None,
                ..Default::default()
            };
            // Hybrid semantic + lexical + recency ranking. Falls back to the
            // lexical substring path automatically when embeddings are off.
            let mut rows = memory_storage::search::search(
                store.as_ref(),
                &filter,
                query.as_deref(),
                limit,
                offset,
            )
            .await?;
            // For session-start injection (claude-context, typically no query)
            // lead with the highest-value memories — facts / rules / preferences
            // (importance ~8) before raw conversational observations (~3) — so
            // the model sees signal first instead of recency-ordered chatter.
            if format == "claude-context" {
                rows.sort_by(|a, b| {
                    b.importance
                        .partial_cmp(&a.importance)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| b.created.cmp(&a.created))
                });
            }
            match format.as_str() {
                "json" => println!("{}", serde_json::to_string_pretty(&rows)?),
                "claude-context" => {
                    print!("{}", format_claude_context(&rows, project_for_header.as_deref()));
                }
                other => return Err(anyhow!("invalid --format '{other}'. Known: json, claude-context")),
            }
        }

        Cmd::Consolidate {
            platform,
            project,
            agent,
            user,
            scope,
            r#type,
            threshold,
            dry_run,
            json,
        } => {
            let filter = MemoryFilter {
                platform_id: platform,
                project_id: project,
                agent_id: agent,
                user_id: user,
                scope: scope.as_deref().map(parse_scope).transpose()?,
                kind: r#type,
                ..Default::default()
            };
            let report =
                memory_storage::consolidate::consolidate(store.as_ref(), &filter, threshold, dry_run)
                    .await?;
            if json {
                let collapses: Vec<_> = report
                    .collapses
                    .iter()
                    .map(|c| {
                        serde_json::json!({
                            "loserId": c.loser_id,
                            "survivorId": c.survivor_id,
                            "similarity": c.similarity,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "scanned": report.scanned,
                        "threshold": report.threshold,
                        "semantic": report.semantic,
                        "dryRun": report.dry_run,
                        "collapsed": report.collapses.len(),
                        "collapses": collapses,
                    }))?
                );
            } else {
                let mode = if report.dry_run { " (dry run — nothing written)" } else { "" };
                let matcher = if report.semantic { "cosine embeddings" } else { "exact text (no embeddings)" };
                println!(
                    "consolidate: scanned {} item(s), collapsed {} duplicate(s) at threshold {:.2} via {}{}",
                    report.scanned,
                    report.collapses.len(),
                    report.threshold,
                    matcher,
                    mode,
                );
                for c in &report.collapses {
                    println!(
                        "  {} → {} (cosine {:.3})",
                        c.loser_id, c.survivor_id, c.similarity
                    );
                }
            }
        }

        Cmd::Mcp => {
            tracing::info!(db = %cli.db, license = %license.describe(), "starting MCP stdio server");
            memory_server::serve_mcp_stdio(store, license).await?;
        }

        Cmd::Serve { http } => {
            // Kick off a background sync task if SaaS is configured.
            // Lives for the duration of the serve process; cancelled
            // implicitly when serve_http returns or the process exits.
            let cfg = memory_core::config::Config::load_or_default();
            if let Some(sync_cfg) = cfg.sync.clone() {
                let sync_store = store.clone();
                let sync_cfg_clone = sync_cfg.clone();
                tokio::spawn(async move {
                    run_sync_daemon(sync_store, sync_cfg_clone).await;
                });
            }
            tracing::info!(db = %cli.db, addr = %http, "starting HTTP API");
            memory_server::serve_http(store, &http).await?;
        }

        Cmd::Console { http, no_open } => {
            // Same server as `serve` (REST + embedded UI), plus a friendly
            // banner and an auto-opened browser. Kick off background sync if
            // SaaS is configured, exactly like `serve`.
            let cfg = memory_core::config::Config::load_or_default();
            if let Some(sync_cfg) = cfg.sync.clone() {
                let sync_store = store.clone();
                tokio::spawn(async move {
                    run_sync_daemon(sync_store, sync_cfg).await;
                });
            }
            let url = format!("http://{http}");
            println!();
            println!("  MemMesh console");
            println!("  ───────────────");
            println!("  engine : {}", cli.db);
            println!("  logs   : {}", memory_core::config::Config::log_file().display());
            println!("  url    : {url}");
            println!();
            if no_open {
                println!("  Open {url} in your browser.");
            } else {
                // Delay the launch briefly so the listener is bound before the
                // browser requests the page (avoids a first-load failure).
                let url_for_open = url.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                    open_browser(&url_for_open);
                });
                println!("  Opening browser… (Ctrl-C to stop the console)");
            }
            println!();
            tracing::info!(db = %cli.db, addr = %http, "starting web console");
            memory_server::serve_http(store, &http).await?;
        }

        Cmd::ServeMcp { http, token } => {
            let token = token.unwrap_or_else(|| {
                uuid::Uuid::new_v4().to_string().replace('-', "")
            });
            let url = format!("http://{http}/mcp");
            println!();
            println!("  MemMesh remote MCP (Streamable HTTP)");
            println!("  ────────────────────────────────────");
            println!("  engine : {}", cli.db);
            println!("  url    : {url}");
            println!("  token  : {token}");
            println!();
            println!("  Expose it publicly with a tunnel, e.g.:");
            println!("    cloudflared tunnel --url http://{http}");
            println!("  then register the resulting https URL + '/mcp' as a custom");
            println!("  connector in ChatGPT / Claude.ai, with header:");
            println!("    Authorization: Bearer {token}");
            println!();
            tracing::info!(db = %cli.db, addr = %http, "starting remote MCP server");
            memory_server::serve_mcp_http(store, license, &http, token).await?;
        }

        Cmd::Sync { skip_token_check } => {
            let cfg = memory_core::config::Config::load_or_default();
            let sync_cfg = cfg.sync.clone().ok_or_else(|| {
                anyhow!(
                    "sync requested but no [sync] section in config. \
                     Run `memmesh config set-sync ...` first."
                )
            })?;
            let client = memory_sync::SyncClient::new(sync_cfg.url.clone(), sync_cfg.token.clone())
                .map_err(|e| anyhow!("{e}"))?;
            if !skip_token_check {
                client.validate_token().await.map_err(|e| anyhow!("{e}"))?;
                tracing::info!("token validated");
            }
            let stats = memory_sync::run_cycle(store.as_ref(), &client, &sync_cfg)
                .await
                .map_err(|e| anyhow!("{e}"))?;
            println!("{}", serde_json::to_string_pretty(&stats)?);
        }

        Cmd::Eval { fixture, json } => {
            // Bundled fixtures: baked into the binary at build time so
            // `eval` works on installed binaries without needing the
            // source tree. Add more by dropping YAML files into
            // crates/eval/fixtures/ and including them here.
            let bundled: &[(&str, &str)] = &[(
                "01-basic-substring",
                include_str!("../../eval/fixtures/01-basic-substring.yaml"),
            )];

            let fixtures: Vec<memory_eval::Fixture> = match fixture {
                Some(path) => {
                    vec![memory_eval::Fixture::load(std::path::Path::new(&path))?]
                }
                None => bundled
                    .iter()
                    .map(|(name, yaml)| {
                        serde_yaml::from_str::<memory_eval::Fixture>(yaml)
                            .with_context(|| format!("parse bundled fixture {name}"))
                    })
                    .collect::<Result<_>>()?,
            };

            let suite = memory_eval::run_suite(fixtures).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&suite)?);
            } else {
                suite.print_human();
            }
        }

        Cmd::Secret { action } => {
            use memory_storage::vault::Vault;
            let vault = Vault::open(&Vault::default_path())
                .await
                .context("opening secrets vault")?;
            match action {
                SecretAction::Set { name, kind, desc } => {
                    // Hidden TTY prompt for humans; fall back to reading a line
                    // from stdin when there's no terminal (piped / automation).
                    let value = match rpassword::prompt_password(format!("Value for '{name}' (hidden): ")) {
                        Ok(v) => v,
                        Err(_) => {
                            use std::io::BufRead;
                            let mut line = String::new();
                            std::io::stdin().lock().read_line(&mut line)?;
                            line.trim_end_matches(['\n', '\r']).to_string()
                        }
                    };
                    if value.is_empty() {
                        return Err(anyhow!("empty value — nothing stored"));
                    }
                    vault
                        .set(&name, &value, kind.as_deref(), desc.as_deref(), Some("user"), None)
                        .await?;
                    println!("stored secret '{name}' (reference it as {{{{memmesh:{name}}}}})");
                }
                SecretAction::List { json } => {
                    let secrets = vault.list().await?;
                    if json {
                        println!("{}", serde_json::to_string_pretty(&secrets)?);
                    } else if secrets.is_empty() {
                        println!("no secrets stored. Add one with `memmesh secret set <name>`.");
                    } else {
                        for s in &secrets {
                            println!(
                                "  {}{}{}",
                                s.name,
                                s.kind.as_deref().map(|k| format!("  [{k}]")).unwrap_or_default(),
                                s.description.as_deref().map(|d| format!("  — {d}")).unwrap_or_default(),
                            );
                        }
                    }
                }
                SecretAction::Rm { name } => {
                    if vault.delete(&name).await? {
                        println!("removed '{name}'");
                    } else {
                        eprintln!("no secret named '{name}'");
                        std::process::exit(2);
                    }
                }
                SecretAction::Run { command } => {
                    let r = vault.run(&command).await?;
                    if !r.stdout.is_empty() {
                        print!("{}", r.stdout);
                    }
                    if !r.stderr.is_empty() {
                        eprint!("{}", r.stderr);
                    }
                    std::process::exit(r.exit_code);
                }
            }
        }

        Cmd::Skill { action } => match action {
            SkillAction::Print => {
                print!("{SKILL_MD}");
            }
            SkillAction::Install { tool, dir } => {
                install_skill(&tool, dir.as_deref())?;
            }
        },

        Cmd::Bind {
            cwd,
            platform,
            project,
        } => {
            let cwd = resolve_cwd(cwd)?;
            let platform = match platform {
                Some(p) => p,
                None => {
                    let cfg = memory_core::config::Config::load_or_default();
                    cfg.sync
                        .as_ref()
                        .map(|s| s.platform_id.clone())
                        .filter(|p| !p.is_empty())
                        .ok_or_else(|| {
                            anyhow!(
                                "--platform not given and no signed-in platform in \
                                 ~/.memmesh/config.toml. Run `config set-sync` first \
                                 or pass --platform explicitly."
                            )
                        })?
                }
            };
            let now = chrono::Utc::now();
            let binding = memory_core::ProjectBinding {
                cwd: cwd.clone(),
                platform_id: platform,
                project_id: project,
                created: now,
                updated: now,
            };
            store.save_binding(&binding).await?;
            println!("{}", serde_json::to_string_pretty(&binding)?);
        }

        Cmd::Bindings { action } => match action {
            BindingsAction::List => {
                let bindings = store.list_bindings().await?;
                println!("{}", serde_json::to_string_pretty(&bindings)?);
            }
            BindingsAction::Show { cwd } => {
                let cwd = resolve_cwd(cwd)?;
                let found = store.get_binding(&cwd).await?;
                match found {
                    Some(b) => println!("{}", serde_json::to_string_pretty(&b)?),
                    None => {
                        eprintln!("no binding for {cwd}");
                        std::process::exit(2);
                    }
                }
            }
            BindingsAction::Remove { cwd } => {
                let cwd = resolve_cwd(cwd)?;
                store.remove_binding(&cwd).await?;
                println!("removed: {cwd}");
            }
        },

        Cmd::Config { action } => match action {
            ConfigAction::Show => {
                let cfg = memory_core::config::Config::load_or_default();
                // toml::to_string skips None fields; that's the desired
                // behavior — empty sync section means local-only mode.
                let text = toml::to_string_pretty(&cfg)
                    .map_err(|e| anyhow!("serialize config: {e}"))?;
                println!("# resolved from {}", memory_core::config::Config::resolve_path().display());
                println!("{text}");
            }
            ConfigAction::SetSync {
                url,
                token,
                platform_id,
            } => {
                let mut cfg = memory_core::config::Config::load_or_default();
                cfg.sync = Some(memory_core::config::SyncConfig {
                    url,
                    token,
                    platform_id,
                    interval_seconds: 30,
                    binding_policy: memory_core::config::BindingPolicy::AutoCreate,
                });
                cfg.save()?;
                println!("ok");
            }
            ConfigAction::ClearSync => {
                let mut cfg = memory_core::config::Config::load_or_default();
                cfg.sync = None;
                cfg.save()?;
                println!("ok");
            }
        },

        Cmd::Install {
            tool,
            dry_run,
            force,
            skill_only,
            mcp_only,
            no_hooks,
            binary,
        } => {
            if skill_only && mcp_only {
                return Err(anyhow!(
                    "--skill-only and --mcp-only are mutually exclusive"
                ));
            }
            run_install_cmd(installer::InstallOptions {
                tools: tool,
                dry_run,
                force,
                skip_mcp: skill_only,
                skip_skill: mcp_only,
                skip_hooks: no_hooks,
                binary_override: binary,
                db_path: cli.db.clone(),
            })?;
        }

        Cmd::Budget { action } => {
            let resolve_platform = |arg: Option<String>| -> Result<String> {
                if let Some(p) = arg {
                    return Ok(p);
                }
                let cfg = memory_core::config::Config::load_or_default();
                cfg.sync
                    .as_ref()
                    .map(|s| s.platform_id.clone())
                    .filter(|p| !p.is_empty())
                    .ok_or_else(|| {
                        anyhow!(
                            "--platform not given and no signed-in platform in \
                             ~/.memmesh/config.toml. Pass --platform <id> or \
                             run `config set-sync` first."
                        )
                    })
            };
            match action {
                BudgetAction::Show { platform } => {
                    let pid = resolve_platform(platform)?;
                    let state = memory_storage::budget::current(store.as_ref(), &pid)
                        .await
                        .map_err(|e| anyhow!("{e}"))?;
                    let cap_display = state
                        .cap_cents
                        .map(|c| format!("${:.2}", c as f64 / 100.0))
                        .unwrap_or_else(|| "(no cap)".to_string());
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "platform": pid,
                            "periodStart": state.period_start.to_rfc3339(),
                            "spentUsd": state.spent_cents as f64 / 100.0,
                            "capUsd": state.cap_cents.map(|c| c as f64 / 100.0),
                            "capDisplay": cap_display,
                            "remainingUsd": state
                                .cap_cents
                                .map(|c| (c - state.spent_cents).max(0) as f64 / 100.0),
                        }))?
                    );
                }
                BudgetAction::SetCap { platform, usd } => {
                    let pid = resolve_platform(platform)?;
                    let cents = (usd * 100.0).round() as i64;
                    memory_storage::budget::set_cap(store.as_ref(), &pid, Some(cents))
                        .await
                        .map_err(|e| anyhow!("{e}"))?;
                    println!("ok — cap set to ${:.2} for {pid}", cents as f64 / 100.0);
                }
                BudgetAction::ClearCap { platform } => {
                    let pid = resolve_platform(platform)?;
                    memory_storage::budget::set_cap(store.as_ref(), &pid, None)
                        .await
                        .map_err(|e| anyhow!("{e}"))?;
                    println!("ok — cap cleared for {pid}");
                }
                BudgetAction::Record { platform, usd } => {
                    let pid = resolve_platform(platform)?;
                    let cents = (usd * 100.0).round() as i64;
                    let state = memory_storage::budget::record_cost(store.as_ref(), &pid, cents)
                        .await
                        .map_err(|e| anyhow!("{e}"))?;
                    println!(
                        "recorded ${:.2}; total this period: ${:.2}",
                        cents as f64 / 100.0,
                        state.spent_cents as f64 / 100.0,
                    );
                }
            }
        }

        Cmd::Activate { code, endpoint } => {
            println!("▶ Activating against {}...",
                endpoint.as_deref().unwrap_or(memory_license::activate::DEFAULT_ACTIVATION_ENDPOINT));
            match memory_license::activate::activate(&code, endpoint.as_deref()).await {
                Ok(outcome) => {
                    println!("✓ Activated: {} tier", outcome.tier);
                    let cap_display = if outcome.memory_cap == u64::MAX {
                        "unlimited".to_string()
                    } else {
                        outcome.memory_cap.to_string()
                    };
                    println!("  memory cap: {cap_display}");
                    if !outcome.features.is_empty() {
                        println!("  features:   {}", outcome.features.join(", "));
                    }
                    println!("  expires:    {}", outcome.expires_at);
                    println!("✓ License saved to {}", outcome.written_to.display());
                    println!("  Bound to this machine ({}).", memory_license::fingerprint::machine_label());
                    println!();
                    println!("Restart Claude Code (and Cursor / Codex / etc.) to pick up the new tier.");
                }
                Err(err) => {
                    eprintln!("✗ Activation failed: {err}");
                    std::process::exit(2);
                }
            }
        }

        Cmd::License { json } => {
            let count = store.count_items().await.unwrap_or(0);
            if json {
                let payload = serde_json::json!({
                    "tier": license.claims.plan_tier,
                    "customer_id": license.claims.customer_id,
                    "memory_cap": license.claims.memory_cap,
                    "memory_count": count,
                    "features": license.claims.features,
                    "environment": license.claims.environment,
                    "expires_at": license.claims.expires_at,
                    "status": match &license.status {
                        memory_license::LicenseStatus::Valid => "valid",
                        memory_license::LicenseStatus::GracePeriod { .. } => "grace",
                        memory_license::LicenseStatus::Expired => "expired",
                        memory_license::LicenseStatus::Invalid { .. } => "invalid",
                    },
                });
                println!("{}", serde_json::to_string_pretty(&payload)?);
            } else {
                let cap_display = if license.claims.memory_cap == u64::MAX {
                    "unlimited".to_string()
                } else {
                    license.claims.memory_cap.to_string()
                };
                let features = if license.claims.features.is_empty() {
                    "(none)".to_string()
                } else {
                    license.claims.features.join(", ")
                };
                println!("ThinkFleet Memory — License");
                println!("===========================");
                println!("tier:       {}", license.claims.plan_tier.as_str());
                println!("customer:   {}", license.claims.customer_id);
                println!("memory cap: {} (current: {})", cap_display, count);
                println!("features:   {}", features);
                println!("env:        {:?}", license.claims.environment);
                println!("status:     {:?}", license.status);
                if license.claims.plan_tier == memory_license::LicenseTier::Free {
                    println!();
                    println!("Running on the free tier. Upgrade at https://memmesh.ai");
                    println!("then set MEMORY_LICENSE_TOKEN or MEMORY_LICENSE_PATH and restart.");
                }
            }
        }
    }

    Ok(())
}

/// Format memory rows as a compact text block for injection as Claude Code
/// session context. The shape mirrors what a SessionStart hook needs: a
/// short tagged block the model can scan in one read.
fn format_claude_context(rows: &[MemoryItem], project_hint: Option<&str>) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(rows.len() * 96);
    let header_project = project_hint
        .map(|p| format!(" project=\"{}\"", p))
        .unwrap_or_default();
    if rows.is_empty() {
        writeln!(out, "<persistent-memory{header_project}>").ok();
        writeln!(out, "  (no memories yet for this scope)").ok();
        writeln!(out, "</persistent-memory>").ok();
        return out;
    }
    writeln!(
        out,
        "<persistent-memory{header_project} count=\"{}\">",
        rows.len()
    )
    .ok();
    for r in rows {
        let scope = r.scope.as_str();
        let kind = &r.type_;
        // Single-line each — collapse newlines, truncate at 280 chars.
        let mut content: String = r.content.chars().take(280).collect();
        content = content.replace('\n', " ").replace('\r', " ");
        if r.content.chars().count() > 280 {
            content.push_str("…");
        }
        writeln!(out, "  - ({scope}/{kind}) {content}").ok();
    }
    writeln!(out, "</persistent-memory>").ok();
    out
}

/// Background sync daemon — runs inside `serve`. Validates the token
/// once at startup, then loops on `interval_seconds`, calling
/// `run_cycle` each tick. Errors are logged but never panic the loop;
/// the next tick retries.
async fn run_sync_daemon<S: Storage>(
    store: std::sync::Arc<S>,
    sync_cfg: memory_core::config::SyncConfig,
) {
    let client = match memory_sync::SyncClient::new(sync_cfg.url.clone(), sync_cfg.token.clone()) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "sync daemon: client init failed; aborting daemon");
            return;
        }
    };
    match client.validate_token().await {
        Ok(()) => tracing::info!("sync daemon: token validated; entering loop"),
        Err(e) => {
            tracing::warn!(error = %e, "sync daemon: token invalid; staying local-only");
            return;
        }
    }
    let interval = std::time::Duration::from_secs(sync_cfg.interval_seconds.max(5));
    loop {
        match memory_sync::run_cycle(store.as_ref(), &client, &sync_cfg).await {
            Ok(stats) => tracing::debug!(?stats, "sync daemon: cycle complete"),
            Err(e) => tracing::warn!(error = %e, "sync daemon: cycle failed; retrying"),
        }
        tokio::time::sleep(interval).await;
    }
}

fn resolve_cwd(arg: Option<String>) -> Result<String> {
    match arg {
        Some(p) => Ok(p),
        None => {
            let p = std::env::current_dir().context("reading current working directory")?;
            Ok(p.to_string_lossy().into_owned())
        }
    }
}

fn run_install_cmd(opts: installer::InstallOptions) -> Result<()> {
    let home = installer::home_dir()?;
    let binary = match &opts.binary_override {
        Some(b) => std::path::PathBuf::from(b),
        None => std::env::current_exe().context("resolving current binary path")?,
    };
    let db_path = std::path::PathBuf::from(&opts.db_path);

    let tools = if opts.tools.is_empty() {
        Tool::ALL
            .iter()
            .copied()
            .filter(|t| opts.force || t.detect(&home))
            .collect::<Vec<_>>()
    } else {
        opts.tools
            .iter()
            .map(|id| Tool::from_id(id))
            .collect::<Result<Vec<_>>>()?
    };

    if tools.is_empty() {
        println!("no supported AI tools detected.");
        println!("known ids: claude-code, cursor, windsurf, codex, cline, continue, goose, zed");
        println!("re-run with `--force --tool <id>` to install anyway.");
        return Ok(());
    }

    let skill = SkillBundle { markdown: SKILL_MD };
    let mut reports = Vec::with_capacity(tools.len());
    for tool in &tools {
        let report = installer::install_tool(
            *tool,
            &home,
            &binary,
            &db_path,
            &skill,
            opts.dry_run,
            opts.skip_mcp,
            opts.skip_skill,
            opts.skip_hooks,
        )?;
        reports.push(report);
    }

    print_install_summary(&reports, opts.dry_run);
    Ok(())
}

fn print_install_summary(reports: &[installer::InstallReport], dry_run: bool) {
    let header = if dry_run {
        "DRY RUN — no changes written"
    } else {
        "installed"
    };
    println!("{header}");
    for r in reports {
        let detect_note = if r.detected { "" } else { " (not detected)" };
        println!(
            "  {} [{}]{}",
            r.tool.display_name(),
            r.tool.id(),
            detect_note
        );
        println!(
            "    MCP  {} {}",
            action_glyph(r.mcp_action),
            r.mcp_target.display()
        );
        match (&r.skill_target, r.skill_action) {
            (Some(p), action) => println!("    SKILL{} {}", action_glyph(action), p.display()),
            (None, _) => println!("    SKILL  (tool doesn't expose a skills directory; rely on MCP tool descriptions)"),
        }
        if let Some(p) = &r.hook_target {
            println!(
                "    HOOK {} {} (auto-observe + session-start memory injection)",
                action_glyph(r.hook_action),
                p.display()
            );
        }
    }
    if !dry_run {
        println!();
        println!("Restart the host AI tool(s) so they pick up the new config.");
    }
}

fn action_glyph(a: Action) -> &'static str {
    match a {
        Action::Create => "[create]",
        Action::Update => "[update]",
        Action::Skip => "[skip]  ",
    }
}

fn install_skill(tool: &str, custom_dir: Option<&str>) -> Result<()> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let target_dir = match (custom_dir, tool) {
        (Some(d), _) => std::path::PathBuf::from(d),
        (None, "claude-code") => std::path::PathBuf::from(&home)
            .join(".claude")
            .join("skills")
            .join("memmesh"),
        (None, "cursor") => std::path::PathBuf::from(&home)
            .join(".cursor")
            .join("skills")
            .join("memmesh"),
        (None, other) => {
            return Err(anyhow!(
                "unknown tool '{other}'. Use --dir <path> or one of: claude-code, cursor"
            ))
        }
    };
    std::fs::create_dir_all(&target_dir)
        .with_context(|| format!("creating {}", target_dir.display()))?;
    let dest = target_dir.join("SKILL.md");
    std::fs::write(&dest, SKILL_MD).with_context(|| format!("writing {}", dest.display()))?;
    println!("installed skill: {}", dest.display());
    Ok(())
}

/// Best-effort project id detection from the current git repo. Returns
/// the repo's directory basename, which matches how users typically refer
/// to a project. `None` if not in a git repo.
fn detect_git_project() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?;
    let trimmed = path.trim();
    let last = std::path::Path::new(trimmed).file_name()?.to_str()?;
    Some(last.to_string())
}

/// AI-tool hooks pipe a JSON envelope to stdin rather than the raw prompt.
/// Claude Code's `UserPromptSubmit` sends
/// `{"hook_event_name":"UserPromptSubmit","prompt":"…","session_id":…,…}`.
/// Detect that envelope and return the inner prompt so `observe` runs on the
/// user's text, not the wrapper. Returns `None` for plain text (observe as-is)
/// — a bare prompt won't parse as a JSON object with these hook markers.
fn extract_hook_prompt(raw: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    let obj = v.as_object()?;
    let is_hook_envelope = obj.contains_key("hook_event_name")
        || obj.contains_key("session_id")
        || obj.contains_key("transcript_path");
    if !is_hook_envelope {
        return None;
    }
    obj.get("prompt")
        .and_then(|p| p.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
}

/// True for machine/harness chatter that should never be remembered:
/// task-notification and system-reminder blocks injected by the AI tool,
/// command wrappers, and empty input. These arrive through the same hook
/// path as real prompts but carry no durable user intent.
fn is_noise(text: &str) -> bool {
    let t = text.trim_start();
    if t.is_empty() {
        return true;
    }
    const NOISE_PREFIXES: &[&str] = &[
        "<task-notification",
        "<system-reminder",
        "<local-command",
        "<command-name",
        "<command-message",
        "[SYSTEM NOTIFICATION",
    ];
    NOISE_PREFIXES.iter().any(|p| t.starts_with(p))
}

fn detect_os_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

fn parse_scope(s: &str) -> Result<MemoryScope> {
    Ok(match s {
        "platform" => MemoryScope::Platform,
        "project" => MemoryScope::Project,
        "location" => MemoryScope::Location,
        "agent" => MemoryScope::Agent,
        "user" => MemoryScope::User,
        "session" => MemoryScope::Session,
        other => return Err(anyhow!("invalid scope: {other}")),
    })
}

/// Initialize logging. Logs go to **stderr** (the MCP stdio server uses
/// stdout for protocol traffic, so logs must never touch it) **and** are
/// appended to a unified log file at `~/.memmesh/logs/memmesh.log` that the
/// web console tails. Returns the file-writer guard, which the caller must
/// hold for the process lifetime so buffered lines flush on exit.
fn init_tracing(level: &str) -> Result<Option<tracing_appender::non_blocking::WorkerGuard>> {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    let stderr_layer = fmt::layer().with_writer(std::io::stderr);

    // Best-effort file layer — if the log dir can't be created we just log to
    // stderr and carry on rather than failing to start the engine.
    let (file_layer, guard) = match open_log_writer() {
        Some((writer, guard)) => (
            Some(fmt::layer().with_ansi(false).with_writer(writer)),
            Some(guard),
        ),
        None => (None, None),
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        .with(file_layer)
        .init();
    Ok(guard)
}

/// Open the unified log file for appending, wrapped in a non-blocking writer.
/// Every `memmesh` process appends to the same file so the console shows one
/// timeline across `mcp`, `serve`, and `console`.
fn open_log_writer() -> Option<(
    tracing_appender::non_blocking::NonBlocking,
    tracing_appender::non_blocking::WorkerGuard,
)> {
    let dir = memory_core::config::Config::log_dir();
    std::fs::create_dir_all(&dir).ok()?;
    // `never` = no rotation; a single append-only file, which is what makes
    // concurrent appends from multiple processes behave predictably.
    let appender = tracing_appender::rolling::never(&dir, "memmesh.log");
    Some(tracing_appender::non_blocking(appender))
}

/// Best-effort: open `url` in the user's default browser. Never fails the
/// command — if it can't spawn, the URL was already printed for manual use.
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let (bin, args): (&str, Vec<&str>) = ("open", vec![url]);
    #[cfg(target_os = "linux")]
    let (bin, args): (&str, Vec<&str>) = ("xdg-open", vec![url]);
    #[cfg(target_os = "windows")]
    let (bin, args): (&str, Vec<&str>) = ("cmd", vec!["/C", "start", "", url]);
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let (bin, args): (&str, Vec<&str>) = ("true", vec![]);

    if let Err(e) = std::process::Command::new(bin).args(&args).spawn() {
        tracing::warn!(error = %e, "could not launch browser; open the URL manually");
    }
}
