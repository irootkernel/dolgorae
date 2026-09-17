#!/usr/bin/env python3
"""Black-box orphan CLI contract without touching a live process."""

from __future__ import annotations

import argparse
import json
import pathlib
import subprocess
import tempfile

from schema_support import assert_valid, validator


def invoke(binary: pathlib.Path, *arguments: str) -> tuple[int, dict]:
    completed = subprocess.run(
        [str(binary), "runtime", "orphan", *arguments],
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.stderr:
        raise AssertionError(f"unexpected stderr: {completed.stderr}")
    return completed.returncode, json.loads(completed.stdout)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    binary = parser.parse_args().binary.resolve()
    protocol = pathlib.Path(__file__).resolve().parents[2] / "docs" / "protocol"
    machine = validator(protocol, "dolgorae-machine-v3.schema.json")
    with tempfile.TemporaryDirectory(prefix="dolgorae-orphan-cli-") as temporary:
        owner = str(pathlib.Path(temporary).resolve())
        status, inspected = invoke(binary, "inspect", "--owner-root", owner)
        assert status == 0
        assert_valid(inspected, machine, "orphan inspection")
        assert inspected["data"]["candidates"] == []
        status, stale = invoke(
            binary, "cleanup", "--owner-root", owner,
            "--confirm-selection-sha256", "0" * 64,
        )
        assert status == 4
        assert_valid(stale, machine, "stale orphan selection")
        assert stale["error"]["code"] == "ORPHAN_SELECTION_CHANGED"
        status, cleaned = invoke(
            binary, "cleanup", "--owner-root", owner,
            "--confirm-selection-sha256", inspected["data"]["selection_sha256"],
        )
        assert status == 0
        assert_valid(cleaned, machine, "empty orphan cleanup")
        assert cleaned["data"]["cleaned"] == []
        status, missing = invoke(binary, "cleanup", "--confirm-selection-sha256", "0" * 64)
        assert status == 2
        assert_valid(missing, machine, "unscoped orphan cleanup refusal")
        assert missing["error"]["code"] == "INVALID_ARGUMENT"
    print("orphan CLI contract passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
