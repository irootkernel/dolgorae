#!/usr/bin/env python3
"""Exercise native waits and public recovery against an isolated skill install."""

from __future__ import annotations

import argparse
import json
import pathlib
import subprocess
import sys
import tempfile
import time
from unittest.mock import patch

import native_codex
import test_one_shot_partial_cleanup as partial_cleanup
import test_one_shot_recovery_cli as recovery
from schema_support import assert_valid, validator
from test_scoped_specialist_review_failures import v3_review_request

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "validators"))
from validate_agent_skills import validate_installed_resources


def observation_wait(binary: pathlib.Path, protocol: pathlib.Path) -> None:
    checked = validator(protocol, "dolgorae-machine-v2.schema.json")
    schema_source = native_codex.installed_codex()
    with tempfile.TemporaryDirectory(prefix="dolgorae-skill-wait-") as temporary:
        root = pathlib.Path(temporary).resolve()
        home, workspace = root / "home", root / "workspace"
        home.mkdir(mode=0o700)
        workspace.mkdir(mode=0o700)
        recovery.git(workspace, "init", "-b", "main")
        recovery.git(workspace, "config", "user.name", "Dolgorae E2E")
        recovery.git(workspace, "config", "user.email", "dolgorae@example.invalid")
        (workspace / "root.txt").write_text("root\n")
        recovery.git(workspace, "add", "root.txt")
        recovery.git(workspace, "commit", "-m", "root")

        def success(arguments: list[str]) -> dict:
            code, envelope = recovery.machine(binary, home, arguments, timeout=60)
            assert_valid(envelope, checked, "installed workflow envelope")
            if code != 0:
                raise AssertionError(f"installed workflow failed: {envelope!r}")
            return envelope["data"]

        success(["init", str(workspace)])
        controller = root / "controller"
        recovery.credential(binary, home, controller)
        reached, release = root / "reached", root / "release"
        create = native_codex.create_native_codex

        def gated(*args, **kwargs):
            return create(*args, **kwargs, turn_gate=(reached, release))

        profile = "installed-skill-wait"
        with patch.object(native_codex, "create_native_codex", side_effect=gated):
            recovery.add_profile(
                binary, home, workspace, root, schema_source, profile,
                "scoped_specialist_review_v3.json", output_text=recovery.valid_report(),
            )
        source_before = recovery.authority_bytes(workspace)
        request_ref = recovery.reference(90)
        process = recovery.start_input(
            binary, home, recovery.review_arguments(workspace, profile, request_ref, controller),
            v3_review_request(),
        )
        original_pid = process.pid
        try:
            deadline = time.monotonic() + 90
            while not reached.exists():
                if process.poll() is not None:
                    raise AssertionError(f"review exited before wait: {process.communicate(timeout=5)!r}")
                if time.monotonic() >= deadline:
                    raise AssertionError("review did not reach the bounded native Turn gate")
                time.sleep(0.02)
            before = success(recovery.recovery_arguments(workspace, request_ref))
            try:
                process.communicate(timeout=0.05)
            except subprocess.TimeoutExpired:
                pass
            else:
                raise AssertionError("observation wait unexpectedly completed a gated Turn")
            if process.poll() is not None or process.pid != original_pid:
                raise AssertionError("observation wait terminated or replaced the native process")
            after = success(recovery.recovery_arguments(workspace, request_ref))
            if after != before or after["outcome"] != "pending":
                raise AssertionError("observation wait changed the original operation")
            release.touch()
            stdout, stderr = process.communicate(timeout=90)
            if process.returncode != 0 or stderr:
                raise AssertionError(f"original result handle failed: {stderr!r}")
            envelope = json.loads(stdout)
            assert_valid(envelope, checked, "original native result")
            observed = success(recovery.recovery_arguments(workspace, request_ref))
            if (
                observed["result"] != envelope["data"] or observed["outcome"] != "succeeded"
                or observed["reviewer"]["state"] != "closed"
                or observed["engagement"]["state"] != "closed"
                or observed["capture"]["state"] != "settled"
                or observed["capture"]["cleanup_pending"]
                or observed["server"]["status"] != "retired"
            ):
                raise AssertionError("original process did not collect and clean up its checked result")
            traffic = recovery.messages(root, profile)
            methods = [entry.get("method") for entry in traffic]
            if methods.count("turn/start") != 1 or "turn/interrupt" in methods:
                raise AssertionError("observation wait replayed or interrupted the review")
            if recovery.authority_bytes(workspace) != source_before:
                raise AssertionError("installed wait workflow changed source or Git state")
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate(timeout=10)
            recovery.terminate_owned(root)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="dolgorae-skill-workflow-") as temporary:
        installed = pathlib.Path(temporary) / "use-dolgorae"
        subprocess.run(
            [sys.executable, str(ROOT / "tools/validators/package_agent_skill.py"), "install", "--destination", str(installed)],
            cwd=temporary, check=True, capture_output=True, timeout=30,
        )
        validate_installed_resources(installed)
        protocol = installed / "resources" / "protocol"
        observation_wait(binary, protocol)
        recovery.validate(binary, protocol)
        partial_cleanup.validate(binary, protocol)
    print("Installed skill native wait, interruption and public recovery tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
