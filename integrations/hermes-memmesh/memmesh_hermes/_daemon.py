"""Manage a local `memmesh serve-mcp` daemon on behalf of the agent.

WHY THE PLUGIN RUNS THE DAEMON

Local mode has to cost the user nothing to try. "Install a binary, run a
server, come back and configure a URL" loses most people at step two, and the
whole argument for a local-first memory is that it should be the easy option
rather than the principled-but-inconvenient one.

WHAT MAKES THIS SAFE TO DO AUTOMATICALLY

- It binds loopback only. A memory daemon that listens on 0.0.0.0 because a
  plugin started it for you is a data-exfiltration hole with a friendly setup
  wizard in front of it.
- It is token-gated even on loopback, because every other local process on the
  machine can reach loopback.
- One daemon is shared across agents on the same HERMES_HOME, refcounted, and
  left running briefly after the last release so a `/reset` does not pay a
  cold start.
"""

from __future__ import annotations

import atexit
import logging
import os
import secrets
import shutil
import socket
import subprocess
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, Optional

import requests

logger = logging.getLogger(__name__)

STARTUP_TIMEOUT_SECONDS = 20.0
HEALTH_POLL_SECONDS = 0.25


@dataclass
class DaemonHandle:
    url: str
    token: str
    _managed: bool = False

    def release(self) -> None:
        if self._managed:
            _registry.release()


def _free_loopback_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


class _DaemonRegistry:
    """Refcounted single daemon per process."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._proc: Optional[subprocess.Popen] = None
        self._handle: Optional[DaemonHandle] = None
        self._refs = 0

    def acquire(self, hermes_home: Path, cfg: Dict[str, Any]) -> DaemonHandle:
        with self._lock:
            if self._handle is not None and self._proc is not None \
                    and self._proc.poll() is None:
                self._refs += 1
                return self._handle

            binary = shutil.which("memmesh")
            if binary is None:
                raise RuntimeError("the `memmesh` binary is not on PATH")

            port = _free_loopback_port()
            token = secrets.token_urlsafe(32)
            addr = f"127.0.0.1:{port}"

            args = [binary, "serve-mcp", "--http", addr, "--token", token]
            db_path = str(cfg.get("db_path") or "").strip()
            if db_path:
                # --db is a global flag, so it precedes the subcommand.
                args = [binary, "--db", db_path, "serve-mcp",
                        "--http", addr, "--token", token]

            log_dir = hermes_home / "logs"
            log_dir.mkdir(parents=True, exist_ok=True)
            log_file = (log_dir / "memmesh-daemon.log").open("ab")

            env = dict(os.environ)
            # Do not let an inherited value fight the token we just minted.
            env.pop("MEMMESH_MCP_TOKEN", None)

            logger.info("memmesh: starting local daemon on %s", addr)
            proc = subprocess.Popen(  # noqa: S603 - fixed argv, no shell
                args,
                stdout=log_file,
                stderr=subprocess.STDOUT,
                stdin=subprocess.DEVNULL,
                env=env,
                start_new_session=True,
            )

            url = f"http://{addr}"
            if not _await_health(url, proc):
                proc.terminate()
                raise RuntimeError(
                    f"memmesh daemon did not become healthy within "
                    f"{STARTUP_TIMEOUT_SECONDS:.0f}s — see "
                    f"{log_dir / 'memmesh-daemon.log'}"
                )

            self._proc = proc
            self._handle = DaemonHandle(url=url, token=token, _managed=True)
            self._refs = 1
            return self._handle

    def release(self) -> None:
        with self._lock:
            self._refs = max(0, self._refs - 1)
            # Deliberately NOT stopping at zero. A /reset or /new releases and
            # immediately re-acquires, and tearing the daemon down in between
            # would pay a cold start on every one. The atexit hook is what
            # actually reclaims it.

    def stop(self) -> None:
        with self._lock:
            proc, self._proc, self._handle, self._refs = self._proc, None, None, 0
        if proc is None or proc.poll() is not None:
            return
        proc.terminate()
        try:
            proc.wait(timeout=5.0)
        except subprocess.TimeoutExpired:
            proc.kill()


_registry = _DaemonRegistry()
atexit.register(_registry.stop)


def _await_health(url: str, proc: subprocess.Popen) -> bool:
    deadline = time.monotonic() + STARTUP_TIMEOUT_SECONDS
    health = url.rstrip("/") + "/health"
    while time.monotonic() < deadline:
        # Check the process first: if it has already exited, polling for the
        # full timeout just delays a failure we can report now.
        if proc.poll() is not None:
            return False
        try:
            if requests.get(health, timeout=1.0).status_code == 200:
                return True
        except requests.RequestException:
            pass
        time.sleep(HEALTH_POLL_SECONDS)
    return False


def ensure_local_daemon(hermes_home: Path, cfg: Dict[str, Any]) -> DaemonHandle:
    return _registry.acquire(hermes_home, cfg)
