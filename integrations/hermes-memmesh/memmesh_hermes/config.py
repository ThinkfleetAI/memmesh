"""Config loading, the setup-wizard schema, and bank-id scoping."""

from __future__ import annotations

import json
import logging
import os
import re
from pathlib import Path
from typing import Any, Dict, List

logger = logging.getLogger(__name__)

CONFIG_DIRNAME = "memmesh"
CONFIG_FILENAME = "config.json"

DEFAULTS: Dict[str, Any] = {
    "mode": "local",
    "api_url": "",
    "project_id": "",
    "brain_id": "",
    "bank_id": "hermes",
    "bank_id_template": "",
    "recall_limit": 10,
    "recall_indicator": True,
    "recall_preamble": "",
    "auto_observe": True,
    "mirror_builtin_memory": True,
    "memory_mode": "hybrid",
    "db_path": "",
}


def _config_dir() -> Path:
    home = os.environ.get("HERMES_HOME")
    base = Path(home) if home else Path.home() / ".hermes"
    return base / CONFIG_DIRNAME


def load_config() -> Dict[str, Any]:
    """Config file merged over defaults, with env overrides on top.

    Env wins because that is where an operator reaches first when something is
    wrong at 3am, and because a container deployment often has no writable
    config file at all.
    """
    cfg = dict(DEFAULTS)
    path = _config_dir() / CONFIG_FILENAME
    if path.is_file():
        try:
            loaded = json.loads(path.read_text(encoding="utf-8"))
            if isinstance(loaded, dict):
                cfg.update(loaded)
        except (OSError, ValueError) as exc:
            # Never fail startup on a malformed config: the defaults are a
            # working local install, which is a better outcome than an agent
            # that will not boot.
            logger.warning("memmesh: ignoring unreadable config at %s (%s)", path, exc)

    for env_key, cfg_key in (
        ("MEMMESH_MODE", "mode"),
        ("MEMMESH_API_URL", "api_url"),
        ("MEMMESH_PROJECT_ID", "project_id"),
        ("MEMMESH_BRAIN_ID", "brain_id"),
        ("MEMMESH_BANK_ID", "bank_id"),
        ("MEMMESH_DB_PATH", "db_path"),
    ):
        value = os.environ.get(env_key)
        if value:
            cfg[cfg_key] = value
    return cfg


def save_config_file(values: Dict[str, Any], hermes_home: Path) -> None:
    """Write non-secret config. Secrets go to .env via `env_var` in the schema."""
    directory = hermes_home / CONFIG_DIRNAME
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / CONFIG_FILENAME

    existing: Dict[str, Any] = {}
    if path.is_file():
        try:
            loaded = json.loads(path.read_text(encoding="utf-8"))
            if isinstance(loaded, dict):
                existing = loaded
        except (OSError, ValueError):
            existing = {}

    # Merge rather than replace: the wizard only collects the fields it asked
    # about, and overwriting would silently reset everything the user tuned by
    # hand in the same file.
    existing.update({k: v for k, v in values.items() if v is not None})
    path.write_text(json.dumps(existing, indent=2) + "\n", encoding="utf-8")


_PLACEHOLDER_RE = re.compile(r"\{(profile|workspace|platform|user|session)\}")
_COLLAPSE_RE = re.compile(r"[-_]{2,}")


def resolve_bank_id(cfg: Dict[str, Any], init_kwargs: Dict[str, Any]) -> str:
    """Derive the memory bank name for this agent.

    WHY A TEMPLATE. One Hermes install can run several agents — different
    profiles, different workspaces, a gateway serving many platform users. They
    should not share one memory unless someone asked for that. Hermes hands the
    identity in at `initialize()` and nowhere else, so this is the only point
    where the id can be built.

    Empty placeholders collapse cleanly, so `hermes-{user}` with no user becomes
    `hermes` rather than `hermes-`. That matters: a trailing separator produces a
    DIFFERENT bank id from the one an operator expects, and the symptom is an
    agent that appears to have lost its memory.
    """
    template = str(cfg.get("bank_id_template") or "").strip()
    fallback = str(cfg.get("bank_id") or "hermes").strip() or "hermes"
    if not template:
        return fallback

    values = {
        "profile": str(init_kwargs.get("agent_identity") or ""),
        "workspace": str(init_kwargs.get("agent_workspace") or ""),
        "platform": str(init_kwargs.get("platform") or ""),
        "user": str(init_kwargs.get("user_id") or ""),
        "session": str(init_kwargs.get("session_id") or ""),
    }
    rendered = _PLACEHOLDER_RE.sub(lambda m: values.get(m.group(1), ""), template)
    rendered = _COLLAPSE_RE.sub("-", rendered).strip("-_ ")
    return rendered or fallback


CONFIG_SCHEMA: List[Dict[str, Any]] = [
    {
        "key": "mode",
        "description": (
            "local (we run a MemMesh daemon for you), local_external (point at "
            "one you already run), or cloud (app.memmesh.ai)"
        ),
        "choices": ["local", "local_external", "cloud"],
        "default": "local",
        "required": True,
    },
    {
        "key": "api_key",
        "description": "MemMesh API key — cloud mode only",
        "secret": True,
        "env_var": "MEMMESH_API_KEY",
        "url": "https://app.memmesh.ai",
    },
    {
        "key": "api_url",
        "description": (
            "Endpoint. cloud: https://app.memmesh.ai. local_external: your "
            "`memmesh serve-mcp` address, e.g. http://127.0.0.1:7899"
        ),
    },
    {
        "key": "project_id",
        "description": "Project id — cloud mode only",
    },
    {
        "key": "brain_id",
        "description": (
            "Restrict reads and writes to one brain. Leave empty for "
            "project-wide memory."
        ),
    },
    {
        "key": "bank_id",
        "description": "Memory bank name, used when no template is set",
        "default": "hermes",
    },
    {
        "key": "bank_id_template",
        "description": (
            "Derive the bank per agent. Placeholders: {profile} {workspace} "
            "{platform} {user} {session}. Example: hermes-{profile} gives each "
            "Hermes profile its own memory."
        ),
    },
    {
        "key": "recall_limit",
        "description": "How many memories to inject before each turn",
        "type": "integer",
        "minimum": 1,
        "maximum": 50,
        "default": 10,
    },
    {
        "key": "memory_mode",
        "description": (
            "hybrid (auto-inject + tools), context (auto-inject only), or "
            "tools (tools only)"
        ),
        "choices": ["hybrid", "context", "tools"],
        "default": "hybrid",
    },
    {
        "key": "auto_observe",
        "description": "Send each completed turn to MemMesh automatically",
        "type": "boolean",
        "default": True,
    },
    {
        "key": "mirror_builtin_memory",
        "description": (
            "Mirror Hermes' own MEMORY.md / USER.md writes into MemMesh. Those "
            "files are char-capped and never auto-compact, so an entry dropped "
            "to make room is otherwise gone for good."
        ),
        "type": "boolean",
        "default": True,
    },
    {
        "key": "recall_indicator",
        "description": "Show a '\U0001f9e0 MemMesh — recalled N memories' status line",
        "type": "boolean",
        "default": True,
    },
]
