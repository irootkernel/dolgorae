#!/usr/bin/env python3
"""Static contract checks for the opt-in TASK-047 live acceptance driver."""

from __future__ import annotations

import ast
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
DRIVER = ROOT / "tests" / "e2e" / "run_live_primary_bridge.py"


def main() -> int:
    source = DRIVER.read_text(encoding="utf-8")
    ast.parse(source)
    required = (
        'OPT_IN = "DOLGORAE_RUN_LIVE_PRIMARY_BRIDGE"',
        'PINNED_CODEX_VERSION = "codex-cli 0.153.4"',
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
    print("live Primary bridge driver contract passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
