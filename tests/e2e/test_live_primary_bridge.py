#!/usr/bin/env python3
"""Static contract checks for the opt-in TASK-047 live acceptance driver."""

from __future__ import annotations

import ast
import json
import sqlite3
import tempfile
from pathlib import Path

from run_live_primary_bridge import tool_receipt


ROOT = Path(__file__).resolve().parents[2]
DRIVER = ROOT / "tests" / "e2e" / "run_live_primary_bridge.py"


def main() -> int:
    source = DRIVER.read_text(encoding="utf-8")
    ast.parse(source)
    required = (
        'OPT_IN = "DOLGORAE_RUN_LIVE_PRIMARY_BRIDGE"',
        'PINNED_CODEX_VERSION = "codex-cli 0.158.0"',
        '"--orchestration-policy"',
        "list_specialists",
        '"list_specialists_result"',
        "brokered_tool_results",
        '"dolgorae.brokered-tool-result/v1"',
        'receipt.get("outcome") != "ok"',
        '"isolated_codex_home": True',
        '"shared_profile_mutated": False',
    )
    missing = [fragment for fragment in required if fragment not in source]
    if missing:
        raise AssertionError(f"live Primary bridge driver lost required barriers: {missing!r}")
    with tempfile.TemporaryDirectory(prefix="dolgorae-primary-receipt-") as temporary:
        state_root = Path(temporary)
        database = state_root / "orchestration" / "orchestration.sqlite3"
        database.parent.mkdir()
        connection = sqlite3.connect(database)
        connection.execute(
            "CREATE TABLE brokered_tool_results ("
            "session_id TEXT,source_run_id TEXT,source_turn_id TEXT,"
            "source_tool_call_id TEXT,idempotency_key TEXT,request_sha256 TEXT,"
            "response_json TEXT,created_at_ms INTEGER)"
        )
        envelope = {
            "schema_version": "dolgorae.brokered-tool-result/v1",
            "outcome": "ok",
            "value": {"operation": "list_specialists_result", "specialists": []},
        }
        def receipt(value: dict[str, object], source_run: str = "primary") -> None:
            connection.execute("DELETE FROM brokered_tool_results")
            connection.execute(
                "INSERT INTO brokered_tool_results VALUES (?,?,?,?,?,?,?,?)",
                ("primary", source_run, "turn-1", "call-1", "key-1", "a" * 64,
                 json.dumps(value), 1),
            )
            connection.commit()

        def rejects(expected: str) -> None:
            try:
                tool_receipt(state_root, "primary")
            except RuntimeError as error:
                if expected not in str(error):
                    raise
            else:
                raise AssertionError(f"durable tool receipt accepted {expected}")

        receipt(envelope)
        checked = tool_receipt(state_root, "primary")
        if checked["operation"] != "list_specialists_result" or checked["specialist_count"] != 0:
            raise AssertionError("valid durable Primary receipt was not projected")
        receipt(dict(envelope, outcome="error"))
        rejects("unexpected durable Primary tool envelope")
        receipt(dict(envelope, schema_version="unknown"))
        rejects("unexpected durable Primary tool envelope")
        receipt(envelope, source_run="other")
        rejects("not bound to the Primary Run")
        connection.close()
    print("live Primary bridge driver contract passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
