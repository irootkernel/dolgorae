#!/usr/bin/env python3
"""Black-box validation of executable output against the Machine v1 schema."""

from __future__ import annotations

import argparse
import json
import pathlib
import subprocess
import sys

from schema_support import assert_valid, validator


def validate(binary: pathlib.Path, protocol_root: pathlib.Path) -> None:
    machine = validator(protocol_root, "dolgorae-machine-v1.schema.json")
    cases = (("--help",), ("--version",), ("runtime", "capabilities"))
    for arguments in cases:
        completed = subprocess.run(
            [str(binary), *arguments],
            check=True,
            capture_output=True,
            text=True,
        )
        if completed.stderr:
            raise AssertionError(f"unexpected stderr for {arguments}: {completed.stderr}")
        instance = json.loads(completed.stdout)
        assert_valid(instance, machine, f"output for {arguments}")
        if not completed.stdout.endswith("\n"):
            raise AssertionError(f"machine output lacks final LF for {arguments}")

    unknown = subprocess.run(
        [str(binary), "definitely-unknown"],
        check=False,
        capture_output=True,
        text=True,
    )
    if unknown.returncode != 2 or unknown.stderr:
        raise AssertionError(
            f"unknown command boundary failed: status={unknown.returncode} "
            f"stderr={unknown.stderr!r}"
        )
    unknown_envelope = json.loads(unknown.stdout)
    assert_valid(unknown_envelope, machine, "unknown-command Machine envelope")
    if unknown_envelope["command"] != "unknown":
        raise AssertionError("unknown command used the wrong command identifier")
    if unknown_envelope["error"]["code"] != "INVALID_ARGUMENT":
        raise AssertionError("unknown command used the wrong error code")

    human = subprocess.run(
        [str(binary), "--human", "--version"],
        check=True,
        capture_output=True,
        text=True,
    )
    if human.stderr or not human.stdout.startswith("dolgorae "):
        raise AssertionError(f"human version boundary failed: {human!r}")
    try:
        json.loads(human.stdout)
    except json.JSONDecodeError:
        pass
    else:
        raise AssertionError("--human --version unexpectedly emitted JSON")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument(
        "--protocol-root", type=pathlib.Path, default=pathlib.Path("docs/protocol")
    )
    arguments = parser.parse_args()
    validate(arguments.binary.resolve(), arguments.protocol_root.resolve())
    print("Machine CLI validation passed: envelopes, errors, human boundary")
    return 0


if __name__ == "__main__":
    sys.exit(main())
