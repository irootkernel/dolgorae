"""Reap only verified Dolgorae processes from a removed E2E owner root."""

from __future__ import annotations

import argparse
import json
import pathlib
import subprocess
import time


def _call(binary: pathlib.Path, command: str, owner_root: pathlib.Path, *extra: str) -> dict:
    completed = subprocess.run(
        [str(binary), "runtime", "orphan", command, "--owner-root-under", str(owner_root), *extra],
        capture_output=True,
        text=True,
        check=False,
    )
    envelope = json.loads(completed.stdout)
    changed = envelope.get("error", {}).get("code") == "ORPHAN_SELECTION_CHANGED"
    if completed.returncode != 0 and not (command == "cleanup" and changed):
        raise RuntimeError(f"orphan {command} failed: {completed.stdout} {completed.stderr}")
    if not envelope.get("ok") and not changed:
        raise RuntimeError(f"orphan {command} failed: {envelope}")
    return envelope


def cleanup_removed_root(binary: pathlib.Path, owner_root: pathlib.Path) -> None:
    owner_root = owner_root.resolve()
    if owner_root.exists():
        raise RuntimeError(f"owner root must be removed before orphan cleanup: {owner_root}")
    for _ in range(20):
        inspected = _call(binary, "inspect", owner_root)["data"]
        if any(candidate["verdict"] == "unverifiable" for candidate in inspected["candidates"]):
            raise RuntimeError(f"unverifiable Dolgorae process in E2E scope: {inspected}")
        result = _call(
            binary, "cleanup", owner_root,
            "--confirm-selection-sha256", inspected["selection_sha256"],
        )
        if result["ok"]:
            remaining = _call(binary, "inspect", owner_root)["data"]
            if not any(candidate["verdict"] in {"orphan", "unverifiable"} for candidate in remaining["candidates"]):
                return
        time.sleep(0.1)
    raise RuntimeError(f"Dolgorae process did not reach verified absence: {remaining if result['ok'] else result}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument("--owner-root-under", type=pathlib.Path, required=True)
    arguments = parser.parse_args()
    cleanup_removed_root(arguments.binary.resolve(), arguments.owner_root_under.resolve())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
