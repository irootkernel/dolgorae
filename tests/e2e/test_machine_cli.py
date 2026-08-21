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


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument(
        "--protocol-root", type=pathlib.Path, default=pathlib.Path("docs/protocol")
    )
    arguments = parser.parse_args()
    validate(arguments.binary.resolve(), arguments.protocol_root.resolve())
    print("Machine CLI validation passed: help, version, capabilities")
    return 0


if __name__ == "__main__":
    sys.exit(main())
