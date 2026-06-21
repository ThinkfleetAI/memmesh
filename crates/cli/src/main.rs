// Copyright 2026 ThinkFleet, Inc. Licensed under the Apache License, Version 2.0.

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
use chrono::Utc;
use clap::{Parser, Subcommand};
use installer::{Action, SkillBundle, Tool};
use memory_core::{MemoryItem, MemoryScope};
use memory_license::License;
use memory_storage::{sqlite::SqliteStore, MemoryFilter, MemoryQuery, Storage};
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

    init_tracing(&cli.log)?;

    let db_url = format!("sqlite://{}?mode=rwc", cli.db);
    let store = SqliteStore::connect(&db_url)
        .await
        .with_context(|| format!("opening sqlite store at {}", cli.db))?;
    store.migrate().await.context("running migrations")?;
    let store = Arc::new(store);

    // Resolve license once per CLI invocation. `tflk_...` license keys
    // trigger an HTTP exchange against the SaaS to receive a JWT; raw
    // JWTs verify locally without network. Kept immutable for the rest
    // of main(). Phase 2 will add runtime refresh.
    let license = License::load_from_env(Utc::now()).await;

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
            json,
        } => {
            let text = match content {
                Some(c) => c,
                None => {
                    use std::io::Read;
                    let mut buf = String::new();
                    std::io::stdin().read_to_string(&mut buf)?;
                    buf
                }
            };
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
                text_match: query,
                ..Default::default()
            };
            let rows = store
                .query(&MemoryQuery {
                    filter,
                    limit: Some(limit),
                    offset: Some(offset),
                })
                .await?;
            match format.as_str() {
                "json" => println!("{}", serde_json::to_string_pretty(&rows)?),
                "claude-context" => {
                    print!("{}", format_claude_context(&rows, project_for_header.as_deref()));
                }
                other => return Err(anyhow!("invalid --format '{other}'. Known: json, claude-context")),
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
async fn run_sync_daemon(
    store: std::sync::Arc<memory_storage::sqlite::SqliteStore>,
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

fn init_tracing(level: &str) -> Result<()> {
    // The MCP stdio server uses stdout for protocol traffic, so logs MUST
    // go to stderr. tracing_subscriber::fmt defaults to stdout — switch
    // explicitly. Honor RUST_LOG if set.
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .init();
    Ok(())
}
