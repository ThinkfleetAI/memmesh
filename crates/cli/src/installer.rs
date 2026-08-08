// Copyright 2026 Thinkfleet AI, LLC Licensed under the Apache License, Version 2.0.

//! AI-tool installer.
//!
//! For each supported tool (Claude Code, Cursor, Windsurf, Codex CLI):
//!   - Merge an `mcpServers.memmesh` entry into the tool's MCP
//!     configuration file (JSON for most, TOML for Codex).
//!   - Copy the bundled `SKILL.md` into the tool's skills/rules directory
//!     so the agent knows when and how to use the memory tools.
//!
//! Idempotency: re-running `install` replaces the previous entry with the
//! current binary path / args / skill content. Existing MCP servers from
//! other vendors stay untouched.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;
use std::path::{Path, PathBuf};

const MCP_SERVER_NAME: &str = "memmesh";
const SKILL_DIR_NAME: &str = "memmesh";
const SKILL_FILE: &str = "SKILL.md";

/// Skill markdown — owned by the caller so the binary and the disk file
/// can't drift (the caller injects it via `include_str!`).
pub struct SkillBundle<'a> {
    pub markdown: &'a str,
}

/// Options for the `install` CLI command.
pub struct InstallOptions {
    pub tools: Vec<String>,
    pub dry_run: bool,
    pub force: bool,
    pub skip_mcp: bool,
    pub skip_skill: bool,
    /// When true, also skip the Claude Code `UserPromptSubmit` hook that
    /// makes the engine auto-observe every user prompt. Default false
    /// (hooks ON) — the auto-observe flow is the main user value.
    pub skip_hooks: bool,
    pub binary_override: Option<String>,
    pub db_path: String,
}

/// One supported AI tool.
#[derive(Debug, Clone, Copy)]
pub enum Tool {
    ClaudeCode,
    Cursor,
    Windsurf,
    CodexCli,
}

impl Tool {
    pub const ALL: &'static [Tool] = &[
        Tool::ClaudeCode,
        Tool::Cursor,
        Tool::Windsurf,
        Tool::CodexCli,
    ];

    pub fn id(&self) -> &'static str {
        match self {
            Tool::ClaudeCode => "claude-code",
            Tool::Cursor => "cursor",
            Tool::Windsurf => "windsurf",
            Tool::CodexCli => "codex",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Tool::ClaudeCode => "Claude Code",
            Tool::Cursor => "Cursor",
            Tool::Windsurf => "Windsurf",
            Tool::CodexCli => "Codex CLI",
        }
    }

    pub fn from_id(id: &str) -> Result<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|t| t.id() == id)
            .ok_or_else(|| {
                anyhow!(
                    "unknown tool '{id}'. Known: {}",
                    Self::ALL
                        .iter()
                        .map(|t| t.id())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }

    /// Tool-specific MCP config file path (relative to $HOME).
    pub fn mcp_config_path(&self, home: &Path) -> PathBuf {
        match self {
            // Claude Code stores user-level MCP servers in ~/.claude.json
            // alongside other settings (history, projects, etc.).
            Tool::ClaudeCode => home.join(".claude.json"),
            // Cursor's MCP config sits in ~/.cursor/mcp.json.
            Tool::Cursor => home.join(".cursor").join("mcp.json"),
            // Windsurf (Codeium) uses ~/.codeium/windsurf/mcp_config.json.
            Tool::Windsurf => home
                .join(".codeium")
                .join("windsurf")
                .join("mcp_config.json"),
            // Codex CLI uses TOML: ~/.codex/config.toml with [mcp_servers.<name>] sections.
            Tool::CodexCli => home.join(".codex").join("config.toml"),
        }
    }

    /// Tool-specific skills directory. None means the tool has no native
    /// skills system — the user installs the skill content into a rules
    /// file or as a system prompt manually.
    pub fn skill_dir(&self, home: &Path) -> Option<PathBuf> {
        match self {
            Tool::ClaudeCode => Some(home.join(".claude").join("skills").join(SKILL_DIR_NAME)),
            // Cursor uses ~/.cursor/rules for user-level rules. We treat it
            // as a "skills directory" — SKILL.md drops in and is loaded.
            Tool::Cursor => Some(home.join(".cursor").join("rules").join(SKILL_DIR_NAME)),
            // Windsurf user-level rules live under ~/.codeium/windsurf/
            // memories/global_rules.md (single file, not a directory). We
            // model it as a synthetic dir whose only "file" is the rules
            // doc; install() handles the special case.
            Tool::Windsurf => Some(
                home.join(".codeium")
                    .join("windsurf")
                    .join("memories")
                    .join(SKILL_DIR_NAME),
            ),
            // Codex CLI loads ~/.codex/AGENTS.md as global agent
            // instructions. Treat it the same way — a synthetic dir whose
            // single file ends up at the right place after install.
            Tool::CodexCli => Some(
                home.join(".codex")
                    .join("instructions")
                    .join(SKILL_DIR_NAME),
            ),
        }
    }

    /// Quick detection — config dir or binary present. False is a soft
    /// signal: the user can still install with `--force`.
    pub fn detect(&self, home: &Path) -> bool {
        let candidates: Vec<PathBuf> = match self {
            Tool::ClaudeCode => vec![home.join(".claude"), home.join(".claude.json")],
            Tool::Cursor => vec![home.join(".cursor")],
            Tool::Windsurf => vec![home.join(".codeium").join("windsurf")],
            Tool::CodexCli => vec![home.join(".codex")],
        };
        candidates.iter().any(|p| p.exists())
    }
}

/// One install action's outcome (used by --dry-run and by the post-install
/// summary).
#[derive(Debug)]
pub struct InstallReport {
    pub tool: Tool,
    pub detected: bool,
    pub mcp_target: PathBuf,
    pub mcp_action: Action,
    pub skill_target: Option<PathBuf>,
    pub skill_action: Action,
    /// For Claude Code: where the auto-observe hook was wired. None for
    /// other tools (no event-hook system).
    pub hook_target: Option<PathBuf>,
    pub hook_action: Action,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Path didn't exist — would create new file/dir.
    Create,
    /// Path existed — would merge / overwrite (idempotent for our entries).
    Update,
    /// Tool doesn't support this surface — nothing to do.
    Skip,
}

/// What gets written into the MCP config for the tool.
fn server_command(binary: &Path, db_path: &Path) -> serde_json::Value {
    json!({
        "command": binary.to_string_lossy().to_string(),
        "args": ["--db", db_path.to_string_lossy().to_string(), "mcp"],
    })
}

/// Install MCP + skill (+ hooks where supported) for a single tool.
#[allow(clippy::too_many_arguments)]
pub fn install_tool(
    tool: Tool,
    home: &Path,
    binary: &Path,
    db_path: &Path,
    skill: &SkillBundle<'_>,
    dry_run: bool,
    skip_mcp: bool,
    skip_skill: bool,
    skip_hooks: bool,
) -> Result<InstallReport> {
    let detected = tool.detect(home);
    let mcp_target = tool.mcp_config_path(home);
    let skill_target = tool.skill_dir(home);

    let mcp_action = if skip_mcp {
        Action::Skip
    } else {
        let exists = mcp_target.exists();
        if !dry_run {
            install_mcp(tool, &mcp_target, binary, db_path)
                .with_context(|| format!("installing MCP for {}", tool.display_name()))?;
        }
        if exists {
            Action::Update
        } else {
            Action::Create
        }
    };

    let skill_action = match (&skill_target, skip_skill) {
        (None, _) => Action::Skip,
        (_, true) => Action::Skip,
        (Some(dir), false) => {
            let dest = dir.join(SKILL_FILE);
            let exists = dest.exists();
            if !dry_run {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("creating skill dir {}", dir.display()))?;
                std::fs::write(&dest, skill.markdown)
                    .with_context(|| format!("writing {}", dest.display()))?;
            }
            if exists {
                Action::Update
            } else {
                Action::Create
            }
        }
    };

    // Hooks — Claude Code only (the others don't have an event-hook
    // system). The hook runs `memmesh observe` on every user
    // prompt so the engine extracts facts automatically.
    let claude_settings = home.join(".claude").join("settings.json");
    let (hook_target, hook_action) = match (tool, skip_hooks) {
        (Tool::ClaudeCode, false) => {
            let existed = claude_settings.exists();
            if !dry_run {
                install_claude_code_hook(&claude_settings, binary)
                    .with_context(|| format!("installing hook to {}", claude_settings.display()))?;
            }
            (
                Some(claude_settings),
                if existed {
                    Action::Update
                } else {
                    Action::Create
                },
            )
        }
        _ => (None, Action::Skip),
    };

    Ok(InstallReport {
        tool,
        detected,
        mcp_target,
        mcp_action,
        skill_target: skill_target.map(|d| d.join(SKILL_FILE)),
        skill_action,
        hook_target,
        hook_action,
    })
}

/// Wire the engine's auto-observe hook into Claude Code by merging into
/// `~/.claude/settings.json`. Adds a `UserPromptSubmit` hook that pipes
/// the prompt text into `memmesh observe`. Existing hooks
/// (from other vendors) are preserved.
fn install_claude_code_hook(settings_path: &Path, binary: &Path) -> Result<()> {
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut doc: serde_json::Value = if settings_path.exists() {
        let raw = std::fs::read_to_string(settings_path)?;
        if raw.trim().is_empty() {
            serde_json::json!({})
        } else {
            serde_json::from_str(&raw)
                .with_context(|| format!("parsing {}", settings_path.display()))?
        }
    } else {
        serde_json::json!({})
    };
    if !doc.is_object() {
        bail!("{} root is not an object", settings_path.display());
    }
    let root = doc.as_object_mut().unwrap();
    let hooks = root
        .entry("hooks".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !hooks.is_object() {
        bail!("{}::hooks is not an object", settings_path.display());
    }
    let hooks_obj = hooks.as_object_mut().unwrap();
    let bin = binary.to_string_lossy();

    // 1. CAPTURE — UserPromptSubmit: pipe every prompt into the engine.
    //    matcher "*" catches every prompt; runs synchronously (Claude Code
    //    waits), heuristic-only so it finishes in <50ms even cold. stderr is
    //    dropped so a failure never dirties Claude Code's output.
    let observe_hook = serde_json::json!({
        "matcher": "*",
        "hooks": [ {
            "type": "command",
            "command": format!("{bin} observe --role user --json 2>/dev/null || true"),
        } ]
    });
    upsert_owned_hook(hooks_obj, "UserPromptSubmit", observe_hook, "memmesh observe")?;

    // 2. RECALL — SessionStart: inject relevant memories into context at the
    //    start of each session. The hook's stdout is added to the session
    //    context, so `search --format claude-context` surfaces the memory
    //    block automatically (no dependency on the model choosing to search).
    let recall_hook = serde_json::json!({
        "hooks": [ {
            "type": "command",
            "command": format!("{bin} search --format claude-context --limit 30 2>/dev/null || true"),
        } ]
    });
    upsert_owned_hook(hooks_obj, "SessionStart", recall_hook, "memmesh search")?;

    let serialized = serde_json::to_string_pretty(&doc)?;
    std::fs::write(settings_path, serialized)?;
    Ok(())
}

/// Append-or-replace a hook we own under `event`, identified by `marker`
/// appearing in one of its command strings. Other vendors' hooks are left
/// untouched, and re-running install replaces our entry rather than
/// duplicating it.
fn upsert_owned_hook(
    hooks_obj: &mut serde_json::Map<String, serde_json::Value>,
    event: &str,
    our_hook: serde_json::Value,
    marker: &str,
) -> Result<()> {
    let ev = hooks_obj
        .entry(event.to_string())
        .or_insert_with(|| serde_json::json!([]));
    let arr = ev
        .as_array_mut()
        .ok_or_else(|| anyhow!("hooks.{event} is not an array"))?;
    for entry in arr.iter_mut() {
        let is_ours = entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|hs| {
                hs.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .is_some_and(|s| s.contains(marker))
                })
            })
            .unwrap_or(false);
        if is_ours {
            *entry = our_hook;
            return Ok(());
        }
    }
    arr.push(our_hook);
    Ok(())
}

fn install_mcp(tool: Tool, path: &Path, binary: &Path, db_path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent dir {}", parent.display()))?;
    }
    match tool {
        Tool::ClaudeCode | Tool::Cursor | Tool::Windsurf => install_mcp_json(path, binary, db_path),
        Tool::CodexCli => install_mcp_toml(path, binary, db_path),
    }
}

/// JSON merge for tools that use the standard `{ mcpServers: { name: ... } }`
/// shape. Loads the existing file (or starts empty), inserts/updates our
/// entry, writes back with pretty formatting. Other servers untouched.
fn install_mcp_json(path: &Path, binary: &Path, db_path: &Path) -> Result<()> {
    let mut root: serde_json::Value = if path.exists() {
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        if raw.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&raw)
                .with_context(|| format!("parsing existing JSON config at {}", path.display()))?
        }
    } else {
        json!({})
    };

    if !root.is_object() {
        bail!(
            "{} doesn't contain a JSON object at the root — refusing to overwrite",
            path.display()
        );
    }
    let obj = root.as_object_mut().unwrap();
    let servers = obj
        .entry("mcpServers".to_string())
        .or_insert_with(|| json!({}));
    if !servers.is_object() {
        bail!(
            "{}::mcpServers is not an object — refusing to overwrite",
            path.display()
        );
    }
    servers
        .as_object_mut()
        .unwrap()
        .insert(MCP_SERVER_NAME.into(), server_command(binary, db_path));

    let serialized = serde_json::to_string_pretty(&root)?;
    std::fs::write(path, serialized).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// TOML merge for Codex CLI. The config uses `[mcp_servers.<name>]` tables.
/// `toml_edit` preserves comments + formatting on existing keys.
fn install_mcp_toml(path: &Path, binary: &Path, db_path: &Path) -> Result<()> {
    let mut doc: toml_edit::DocumentMut = if path.exists() {
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        raw.parse()
            .with_context(|| format!("parsing existing TOML at {}", path.display()))?
    } else {
        toml_edit::DocumentMut::new()
    };

    // Ensure [mcp_servers.memmesh] exists.
    let mcp_servers = doc
        .entry("mcp_servers")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    let mcp_servers = mcp_servers.as_table_mut().ok_or_else(|| {
        anyhow!(
            "{}::mcp_servers is not a TOML table — refusing to overwrite",
            path.display()
        )
    })?;
    mcp_servers.set_implicit(true); // print as `[mcp_servers.memmesh]`, not `[mcp_servers]`

    let entry = mcp_servers
        .entry(MCP_SERVER_NAME)
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    let entry = entry.as_table_mut().ok_or_else(|| {
        anyhow!(
            "{}::mcp_servers.{MCP_SERVER_NAME} is not a table",
            path.display()
        )
    })?;

    entry.insert(
        "command",
        toml_edit::Item::Value(binary.to_string_lossy().to_string().into()),
    );
    let mut args = toml_edit::Array::new();
    args.push("--db");
    args.push(db_path.to_string_lossy().to_string());
    args.push("mcp");
    entry.insert(
        "args",
        toml_edit::Item::Value(toml_edit::Value::Array(args)),
    );

    std::fs::write(path, doc.to_string()).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Resolve `$HOME` once for the whole install operation.
pub fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME env var not set"))
}
