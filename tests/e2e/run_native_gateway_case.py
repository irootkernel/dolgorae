"""Run one prebuilt Rust black-box gateway scenario without invoking Cargo."""
from __future__ import annotations

import argparse
import json
import os
import pathlib
import subprocess
import sys

REPOSITORY = pathlib.Path(__file__).resolve().parents[2]


def run_case(target: str, case: str) -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, default=REPOSITORY / "target/debug/dolgorae")
    parser.add_argument("--artifacts", type=pathlib.Path, default=REPOSITORY / "target/gateway-native-artifacts.json")
    args = parser.parse_args()
    records = [json.loads(line) for line in args.artifacts.read_text().splitlines() if line.strip()]
    executables = {record["executable"] for record in records if record.get("reason") == "compiler-artifact" and record.get("target", {}).get("name") == target and record.get("profile", {}).get("test") and record.get("executable")}
    if len(executables) != 1:
        raise AssertionError(f"expected one prebuilt {target} test artifact, found {len(executables)}")
    executable = pathlib.Path(executables.pop())
    if not executable.is_absolute() or not executable.is_file():
        raise AssertionError(f"native gateway artifact is unavailable: {executable}")
    environment = dict(os.environ, DOLGORAE_BIN=str(args.binary.resolve()), DOLGORAE_TEST_PYTHON=sys.executable)
    listed = subprocess.run(
        [str(executable), "--list", "--format", "terse"],
        cwd=REPOSITORY,
        env=environment,
        check=True,
        capture_output=True,
        text=True,
        timeout=30,
    )
    cases = {
        line.removesuffix(": test")
        for line in listed.stdout.splitlines()
        if line.endswith(": test")
    }
    if case not in cases:
        raise AssertionError(f"native gateway case is unavailable: {target}::{case}")
    subprocess.run([str(executable), "--exact", case, "--nocapture", "--test-threads=1"], cwd=REPOSITORY, env=environment, check=True, timeout=300)


if __name__ == "__main__":
    raise SystemExit("invoke a named gateway scenario wrapper")
