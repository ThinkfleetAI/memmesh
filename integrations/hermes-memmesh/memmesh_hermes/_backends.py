"""Transport adapters for the two MemMesh deployments.

WHY TWO ADAPTERS AND NOT ONE BASE URL

The obvious design is one HTTP client and a configurable base URL, because both
deployments expose `/observe` and both expose `/search`. That is a trap, and it
is worth writing down exactly where it springs.

The open-source binary serves TWO different search paths:

    POST /search        -> storage.query()            filter only, no ranking
    POST /mcp           -> memory_storage::search()   semantic + lexical + recency

`/search` takes a MemoryFilter — scope, ids, status, kind, and a `text_match`
LIKE — and returns rows in storage order. It is a filter endpoint that happens
to accept text. The hybrid searcher, which is the thing worth integrating with,
is reachable only through the MCP tool surface.

So a plugin that pointed local mode at REST `/search` would work, return
plausible-looking rows, and quietly deliver substring matching where the user
expected semantic recall. Nothing would error. That is why local mode speaks
MCP-over-HTTP (`memmesh serve-mcp`) instead.

The hosted deployment is a different service with project-scoped routes, bearer
auth, and its own richer contract (rerank, KG arms, trust scoring, archival). It
is not the same API behind a different host, and pretending otherwise would mean
writing to the lowest common denominator of both.

Each adapter therefore states plainly what it can and cannot do, and the
provider degrades rather than guesses.
"""

from __future__ import annotations

import json
import logging
import uuid
from typing import Any, Dict, List, Optional, Protocol

import requests

logger = logging.getLogger(__name__)

DEFAULT_TIMEOUT = 10.0
# Writes are queued in a background thread, so they can afford to wait longer
# than a recall that blocks the model's first token.
WRITE_TIMEOUT = 30.0


class Backend(Protocol):
    """What the provider needs from a deployment, and nothing more."""

    def health(self) -> bool: ...

    def search(self, query: str, limit: int) -> List[Dict[str, Any]]: ...

    def observe(self, text: str, *, session_id: str = "", role: str = "") -> None: ...

    def supports_checkpoint(self) -> bool:
        """Can this deployment durably archive a transcript before compaction?"""
        return False

    def archive_transcript(self, *, digest: str, turns: List[Dict[str, str]],
                           session_id: str) -> str:
        raise NotImplementedError

    def supports_traces(self) -> bool:
        """Can this deployment record a reasoning trace (for on_delegation)?"""
        return False

    def record_trace(self, *, task: str, result: str, child_session_id: str,
                     duration_ms: Optional[int] = None) -> None:
        raise NotImplementedError


class LocalBackend:
    """The open-source binary, over its MCP Streamable-HTTP endpoint.

    Start it with:

        memmesh serve-mcp --http 127.0.0.1:7899 --token <token>

    JSON-RPC 2.0 over a single POST to `/mcp`. A request returns 200 with a
    result; a notification returns 202 with no body.
    """

    def __init__(self, base_url: str, token: str) -> None:
        self._url = base_url.rstrip("/") + "/mcp"
        self._health_url = base_url.rstrip("/") + "/health"
        self._session = requests.Session()
        self._session.headers.update({
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
        })

    def health(self) -> bool:
        try:
            r = self._session.get(self._health_url, timeout=DEFAULT_TIMEOUT)
            return r.status_code == 200
        except requests.RequestException:
            return False

    def _call_tool(self, name: str, arguments: Dict[str, Any],
                   timeout: float) -> Optional[Any]:
        payload = {
            "jsonrpc": "2.0",
            "id": str(uuid.uuid4()),
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }
        try:
            r = self._session.post(self._url, json=payload, timeout=timeout)
        except requests.RequestException as exc:
            logger.debug("memmesh local %s failed: %s", name, exc)
            return None
        if r.status_code == 401:
            # Distinct from a transport failure: the token is wrong, and no
            # amount of retrying fixes it.
            logger.warning("memmesh local: unauthorized — check MEMMESH_MCP_TOKEN")
            return None
        if r.status_code >= 400:
            logger.debug("memmesh local %s: HTTP %s", name, r.status_code)
            return None
        try:
            body = r.json()
        except ValueError:
            return None
        if isinstance(body, dict) and "error" in body:
            logger.debug("memmesh local %s: %s", name, body["error"])
            return None
        return (body or {}).get("result")

    @staticmethod
    def _rows_from_result(result: Any) -> List[Dict[str, Any]]:
        """MCP tool results are text content blocks holding a JSON string."""
        if not isinstance(result, dict):
            return []
        for block in result.get("content") or []:
            if not isinstance(block, dict) or block.get("type") != "text":
                continue
            try:
                parsed = json.loads(block.get("text") or "")
            except (ValueError, TypeError):
                continue
            if isinstance(parsed, list):
                return [r for r in parsed if isinstance(r, dict)]
        return []

    def search(self, query: str, limit: int) -> List[Dict[str, Any]]:
        result = self._call_tool(
            "memory_search", {"query": query, "limit": limit}, DEFAULT_TIMEOUT,
        )
        return self._rows_from_result(result)

    def observe(self, text: str, *, session_id: str = "", role: str = "") -> None:
        args: Dict[str, Any] = {"text": text}
        if session_id:
            args["sessionId"] = session_id
        if role:
            args["role"] = role
        self._call_tool("memory_observe", args, WRITE_TIMEOUT)

    def supports_checkpoint(self) -> bool:
        # The OSS binary has no durable transcript archive. Claiming otherwise
        # would let the provider advertise the fail-closed compaction contract
        # and then not honour it — which is worse than not offering it, because
        # the operator would have configured compaction to depend on it.
        return False

    def supports_traces(self) -> bool:
        return False


class CloudBackend:
    """The hosted service (app.memmesh.ai), project-scoped and bearer-authed."""

    def __init__(self, base_url: str, api_key: str, project_id: str,
                 brain_id: str = "") -> None:
        self._base = base_url.rstrip("/")
        self._project_id = project_id
        self._brain_id = brain_id
        self._session = requests.Session()
        self._session.headers.update({
            "Authorization": f"Bearer {api_key}",
            "Content-Type": "application/json",
        })

    def _project_url(self, suffix: str) -> str:
        return f"{self._base}/v1/projects/{self._project_id}/memory{suffix}"

    def _admin_url(self, suffix: str) -> str:
        return f"{self._base}/v1/projects/{self._project_id}/admin/memory{suffix}"

    def _post(self, url: str, body: Dict[str, Any],
              timeout: float) -> Optional[Any]:
        try:
            r = self._session.post(url, json=body, timeout=timeout)
        except requests.RequestException as exc:
            logger.debug("memmesh cloud POST %s failed: %s", url, exc)
            return None
        if r.status_code == 401 or r.status_code == 403:
            logger.warning("memmesh cloud: rejected (%s) — check MEMMESH_API_KEY "
                           "and that the key is entitled to this project",
                           r.status_code)
            return None
        if r.status_code >= 400:
            logger.debug("memmesh cloud POST %s: HTTP %s", url, r.status_code)
            return None
        try:
            return r.json()
        except ValueError:
            return None

    def health(self) -> bool:
        # A zero-limit search is the cheapest authenticated round-trip that
        # proves both reachability AND that the key works for this project —
        # an unauthenticated /health would prove neither.
        return self._post(self._admin_url("/search"),
                          {"query": "health", "limit": 1},
                          DEFAULT_TIMEOUT) is not None

    def search(self, query: str, limit: int) -> List[Dict[str, Any]]:
        body: Dict[str, Any] = {
            "query": query,
            "limit": limit,
            # Automatic recall: let the server skip the query embedding and the
            # cross-encoder entirely on a turn that carries no signal. The
            # client-side gate already catches most of these; this is the
            # backstop for anything its regex does not know about.
            "skipTrivialQueries": True,
        }
        if self._brain_id:
            body["brainId"] = self._brain_id
        rows = self._post(self._admin_url("/search"), body, DEFAULT_TIMEOUT)
        return [r for r in rows if isinstance(r, dict)] if isinstance(rows, list) else []

    def observe(self, text: str, *, session_id: str = "", role: str = "") -> None:
        body: Dict[str, Any] = {"content": text}
        if session_id:
            body["sessionKey"] = session_id
        if self._brain_id:
            body["brainId"] = self._brain_id
        self._post(self._project_url("/observe"), body, WRITE_TIMEOUT)

    def supports_checkpoint(self) -> bool:
        return True

    def archive_transcript(self, *, digest: str, turns: List[Dict[str, str]],
                           session_id: str) -> str:
        """Durably archive evidence before Hermes compacts it away.

        IDEMPOTENT BY CONTENT DIGEST, which is not optional. After a fail-closed
        block Hermes calls on_pre_compress() again with a transcript that has
        grown only slightly, so successive attempts carry largely overlapping
        evidence. Keying the write by digest is what makes a retry a no-op
        instead of a duplicate archive.

        Raises on failure. This is the one path in the plugin that must NOT
        fail soft: under the checkpoint contract, returning normally is a claim
        that the evidence is durably stored, and compaction is irreversible.
        """
        body: Dict[str, Any] = {
            "content": "\n".join(
                f"{t.get('role', 'unknown')}: {t.get('content', '')}" for t in turns
            ),
            "sessionKey": session_id,
            # The digest is both the dedupe key and the audit anchor.
            "idempotencyKey": digest,
        }
        if self._brain_id:
            body["brainId"] = self._brain_id
        try:
            r = self._session.post(self._project_url("/observe"), json=body,
                                   timeout=WRITE_TIMEOUT)
        except requests.RequestException as exc:
            raise RuntimeError(f"memmesh checkpoint failed: {exc}") from exc
        if r.status_code >= 400:
            raise RuntimeError(
                f"memmesh checkpoint rejected: HTTP {r.status_code} {r.text[:200]}"
            )
        return digest

    def supports_traces(self) -> bool:
        return True

    def record_trace(self, *, task: str, result: str, child_session_id: str,
                     duration_ms: Optional[int] = None) -> None:
        """Record a delegation as a reasoning trace.

        `verified` is deliberately OMITTED rather than sent as false. The API is
        explicit that false means "checked and found wrong" and is kept as a
        counterexample, while omitted means "nothing checked it". A subagent
        returning a result is not evidence the result was correct, and sending
        false would mislabel every delegation as a known failure — removing it
        from procedure induction entirely, which is the opposite of the point.
        """
        body: Dict[str, Any] = {
            "question": task,
            "steps": [{
                "text": f"Delegated to a subagent (session {child_session_id})",
                "tool": "delegate",
                "result": result[:2000],
            }],
            "answer": result,
            # Replay guard: the same delegation submitted twice would otherwise
            # be counted twice, and duplicates BIAS induction — one trace seen
            # five times reads as a repeated, reliable pattern.
            "idempotencyKey": f"delegation:{child_session_id}",
        }
        if duration_ms is not None:
            body["durationMs"] = duration_ms
        if self._brain_id:
            body["brainId"] = self._brain_id
        self._post(self._project_url("/traces"), body, WRITE_TIMEOUT)
