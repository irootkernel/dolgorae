"""Reap verified Dolgorae processes from removed E2E fixture roots."""

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


def cleanup_removed_root(
    binary: pathlib.Path, owner_root: pathlib.Path, *, allow_existing_root: bool = False
) -> None:
    owner_root = owner_root.resolve()
    # The suite root remains while its removed child fixtures are reaped.
    if owner_root.exists() and not allow_existing_root:
        raise RuntimeError(f"owner root must be removed before orphan cleanup: {owner_root}")
    last_observation: dict | None = None
    for _ in range(50):
        inspected = _call(binary, "inspect", owner_root)["data"]
        last_observation = inspected
        if any(candidate["verdict"] == "unverifiable" for candidate in inspected["candidates"]):
            # A process can exit between the inventory's BSD identity and
            # live-process probes. Never clean an uncertain candidate; allow
            # a bounded re-observation to prove its final state.
            time.sleep(0.1)
            continue
        result = _call(
            binary, "cleanup", owner_root,
            "--confirm-selection-sha256", inspected["selection_sha256"],
        )
        last_observation = result
        if result["ok"]:
            remaining = _call(binary, "inspect", owner_root)["data"]
            last_observation = remaining
            if not any(candidate["verdict"] in {"orphan", "unverifiable"} for candidate in remaining["candidates"]):
                return
        time.sleep(0.1)
    raise RuntimeError(f"Dolgorae process did not reach verified absence: {last_observation}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument("--owner-root-under", type=pathlib.Path, required=True)
    parser.add_argument("--allow-existing-root", action="store_true")
    arguments = parser.parse_args()
    cleanup_removed_root(
        arguments.binary.resolve(),
        arguments.owner_root_under.resolve(),
        allow_existing_root=arguments.allow_existing_root,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
