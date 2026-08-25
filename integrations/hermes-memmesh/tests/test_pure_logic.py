"""Tests for the parts of the provider that need no Hermes runtime and no network.

Deliberately scoped to the logic that is easy to get quietly wrong: bank-id
scoping (whose failure mode is an agent that looks like it lost its memory),
MCP result parsing (whose failure mode is empty recall that reads as "nothing
stored"), and the transcript digest (whose failure mode is duplicate archives
on every fail-closed retry).
"""

from __future__ import annotations

import sys
import types
from pathlib import Path

# The provider package imports `requests` at module scope and the Hermes ABC at
# import time. Neither is needed to exercise the pure logic, so both are stubbed
# before import rather than being made lazy in production code for the sake of a
# test.
if "requests" not in sys.modules:
    stub = types.ModuleType("requests")

    class _RequestException(Exception):
        pass

    class _Session:  # pragma: no cover - never called in these tests
        def __init__(self) -> None:
            self.headers: dict = {}

        def post(self, *a, **k):
            raise _RequestException("no network in tests")

        def get(self, *a, **k):
            raise _RequestException("no network in tests")

    stub.RequestException = _RequestException
    stub.Session = _Session
    stub.get = lambda *a, **k: (_ for _ in ()).throw(_RequestException("no network"))
    sys.modules["requests"] = stub

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

# The Hermes runtime supplies the MemoryProvider ABC and the shared
# `is_trivial_prompt` predicate. The plugin genuinely depends on the host at
# runtime — reusing Hermes' own trivial-prompt definition instead of shipping a
# second copy is a deliberate choice, since two copies of that regex would drift
# and the drift would be invisible. Stub the host so the pure logic is testable
# without a Hermes checkout.
if "agent.memory_provider" not in sys.modules:
    agent_pkg = types.ModuleType("agent")
    mp = types.ModuleType("agent.memory_provider")

    class _MemoryProvider:  # minimal stand-in for the ABC
        pre_compress_checkpoint_api_version = 1

    class _RecallStatus:
        def __init__(self, provider_label: str, count: int, glyph: str = "") -> None:
            self.provider_label = provider_label
            self.count = count
            self.glyph = glyph

    mp.MemoryProvider = _MemoryProvider
    mp.RecallStatus = _RecallStatus
    mp.is_trivial_prompt = lambda text: not (text or "").strip()
    agent_pkg.memory_provider = mp
    sys.modules["agent"] = agent_pkg
    sys.modules["agent.memory_provider"] = mp



from memmesh_hermes._backends import LocalBackend  # noqa: E402
from memmesh_hermes.config import resolve_bank_id  # noqa: E402


class TestBankScoping:
    def test_template_fills_from_init_kwargs(self) -> None:
        assert resolve_bank_id(
            {"bank_id_template": "hermes-{profile}"}, {"agent_identity": "coder"},
        ) == "hermes-coder"
        assert resolve_bank_id(
            {"bank_id_template": "{workspace}-{profile}"},
            {"agent_workspace": "acme", "agent_identity": "coder"},
        ) == "acme-coder"

    def test_empty_placeholders_collapse_without_a_dangling_separator(self) -> None:
        # This is the bug worth a test. `hermes-{user}` with no user must be
        # `hermes`, not `hermes-`: a trailing separator is a DIFFERENT bank id,
        # and the symptom is an agent that appears to have lost its memory
        # rather than an error anyone can see.
        assert resolve_bank_id({"bank_id_template": "hermes-{user}"}, {}) == "hermes"
        assert resolve_bank_id({"bank_id_template": "{workspace}-{profile}"}, {}) == "hermes"
        assert resolve_bank_id(
            {"bank_id_template": "{workspace}-{profile}"}, {"agent_workspace": "acme"},
        ) == "acme"

    def test_falls_back_to_static_bank_id(self) -> None:
        assert resolve_bank_id({}, {}) == "hermes"
        assert resolve_bank_id({"bank_id": "custom"}, {}) == "custom"
        # A template that renders empty must fall back rather than produce "".
        assert resolve_bank_id(
            {"bank_id_template": "{user}", "bank_id": "fallback"}, {},
        ) == "fallback"


class TestMcpResultParsing:
    def test_extracts_rows_from_a_text_content_block(self) -> None:
        result = {"content": [{"type": "text", "text": '[{"content": "hello"}]'}]}
        assert LocalBackend._rows_from_result(result) == [{"content": "hello"}]

    def test_returns_empty_rather_than_raising_on_junk(self) -> None:
        # An exception here would surface as a broken turn. Empty recall is the
        # correct degradation: the agent proceeds without memory.
        assert LocalBackend._rows_from_result(None) == []
        assert LocalBackend._rows_from_result({}) == []
        assert LocalBackend._rows_from_result({"content": [{"type": "text", "text": "nope"}]}) == []
        assert LocalBackend._rows_from_result({"content": [{"type": "image"}]}) == []
        assert LocalBackend._rows_from_result({"content": "not-a-list"}) == []

    def test_drops_non_dict_rows(self) -> None:
        result = {"content": [{"type": "text", "text": '[{"a": 1}, "junk", null]'}]}
        assert LocalBackend._rows_from_result(result) == [{"a": 1}]


class TestTranscriptDigest:
    def _digest(self, *args, **kwargs):
        from memmesh_hermes import _transcript_digest
        return _transcript_digest(*args, **kwargs)

    def test_is_stable_for_identical_input(self) -> None:
        # Idempotency of the checkpoint depends entirely on this. After a
        # fail-closed block Hermes re-calls with the same transcript, and an
        # unstable digest turns every retry into a duplicate archive.
        turns = [{"role": "user", "content": "hi"}, {"role": "assistant", "content": "hello"}]
        assert self._digest(turns, "bank") == self._digest(list(turns), "bank")

    def test_separates_banks_holding_identical_transcripts(self) -> None:
        turns = [{"role": "user", "content": "hi"}]
        assert self._digest(turns, "bank-a") != self._digest(turns, "bank-b")

    def test_is_not_confusable_across_field_boundaries(self) -> None:
        # Without delimiters, ("ab", "c") and ("a", "bc") hash identically —
        # two different transcripts would share one archive.
        a = [{"role": "ab", "content": "c"}]
        b = [{"role": "a", "content": "bc"}]
        assert self._digest(a, "bank") != self._digest(b, "bank")
