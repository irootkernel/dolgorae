#!/usr/bin/env python3
"""Native faults preserve partially completed one-shot cleanup for public recovery."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import signal
import stat
import subprocess
import tempfile
import time

import native_codex
from schema_support import assert_valid, validator
from test_one_shot_recovery_cli import (
    authority_bytes, messages, recovery_arguments, reference, review_arguments, valid_report,
)
from test_scoped_specialist_review_failures import v3_review_request


def validate(binary: pathlib.Path, protocol_root: pathlib.Path) -> None:
    machine_schema = validator(protocol_root, "dolgorae-machine-v2.schema.json")
    observation_schema = validator(protocol_root, "dolgorae-one-shot-review-observation-v1.schema.json")
    schema_source = native_codex.installed_codex()
    with tempfile.TemporaryDirectory(prefix="dolgorae-partial-cleanup-") as temporary:
        root = pathlib.Path(temporary).resolve()
        home, workspace = root / "home", root / "workspace"
        home.mkdir(mode=0o700)
        workspace.mkdir(mode=0o700)
        environment = {**os.environ, "HOME": str(home)}

        def git(*arguments: str) -> None:
            subprocess.run(["git", "-C", str(workspace), *arguments], check=True,
                           capture_output=True, text=True, env=environment, timeout=30)

        def machine(arguments: list[str], timeout: float = 90) -> tuple[int, dict[str, object]]:
            completed = subprocess.run([str(binary), *arguments], capture_output=True,
                                       text=True, env=environment, timeout=timeout)
            if completed.stderr:
                raise AssertionError(f"unexpected CLI stderr: {completed.stderr!r}")
            envelope = json.loads(completed.stdout)
            assert_valid(envelope, machine_schema, arguments[:2])
            return completed.returncode, envelope

        def success(arguments: list[str]) -> dict[str, object]:
            code, envelope = machine(arguments)
            if code:
                raise AssertionError(f"fixture command failed: {arguments!r}: {envelope!r}")
            return envelope["data"]

        git("init", "-b", "main")
        git("config", "user.name", "Dolgorae E2E")
        git("config", "user.email", "dolgorae@example.invalid")
        (workspace / "root.txt").write_text("root\n")
        git("add", "root.txt")
        git("commit", "-m", "root")
        initialized = success(["init", str(workspace)])
        state_root = home / ".dolgorae" / "workspaces" / initialized["workspace_id"]
        orchestration = state_root / "orchestration"
        controller = root / "recovery-owner"
        success(["controller", "credential", "create", "--kind", "automation",
                 "--instance-id", "partial-cleanup", "--output", str(controller)])
        source_before = authority_bytes(workspace)

        def inspect(request_ref: str) -> dict[str, object]:
            before = authority_bytes(orchestration)
            observed = success(recovery_arguments(workspace, request_ref))
            assert_valid(observed, observation_schema, "partial cleanup observation")
            if authority_bytes(orchestration) != before:
                raise AssertionError("public inspection changed durable orchestration state")
            return observed

        try:
            for index, fault in ((70, "reviewer-credential"), (71, "immutable-source"), (72, "stop-prepare"), (73, "delivered-client-loss")):
                profile = f"partial-cleanup-{fault}"
                codex_home = root / f"codex-home-{profile}"
                bin_root = root / f"bin-{profile}"
                codex_home.mkdir(mode=0o700)
                bin_root.mkdir(mode=0o700)
                scenario = root / f"scenario-{profile}.json"
                fixture = json.loads(native_codex.scenario_path("scoped_specialist_review_v3.json").read_text())
                turn = next(step for step in fixture["steps"] if step["method"] == "turn/start")
                turn["emit"][0]["params"]["turn"]["items"][0]["text"] = valid_report()
                scenario.write_text(json.dumps(fixture))
                reached, release = root / f"reached-{index}", root / f"release-{index}"
                codex = bin_root / "codex"
                native_codex.create_native_codex(
                    codex, scenario=scenario, codex_home=codex_home, schema_source=schema_source,
                    transcript=root / f"transcript-{profile}.jsonl", turn_gate=(reached, release),
                )
                success(["profile", "add", profile, "--codex-home", str(codex_home),
                         "--native-subagents", "enabled", "--env", "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
                         "--env", "LANG=en_US.UTF-8", "--env", "LC_ALL=en_US.UTF-8", "--", str(codex)])
                request_ref = reference(index)
                carriers = orchestration / f"one-shot-{request_ref}"
                credential = carriers / "reviewer.json"
                hidden_credential = carriers / "reviewer.fixture-hidden"
                immutable_paths: list[pathlib.Path] = []
                process = subprocess.Popen(
                    [str(binary), *review_arguments(workspace, profile, request_ref, controller)],
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    text=True, env=environment,
                )
                try:
                    assert process.stdin is not None
                    process.stdin.write(json.dumps(v3_review_request()))
                    process.stdin.close()
                    process.stdin = None
                    deadline = time.monotonic() + 60
                    while not reached.exists():
                        if process.poll() is not None:
                            raise AssertionError(f"review exited before Turn gate: {process.communicate(timeout=5)!r}")
                        if time.monotonic() >= deadline:
                            raise AssertionError("review did not reach its bounded Turn gate")
                        time.sleep(0.02)
                    active = inspect(request_ref)
                    capture_root = state_root / "review-targets" / active["capture"]["capture_ref"]
                    if fault in ("reviewer-credential", "delivered-client-loss"):
                        if fault == "delivered-client-loss":
                            credential_bytes = credential.read_bytes()
                        credential.rename(hidden_credential)
                        if fault == "delivered-client-loss":
                            os.mkfifo(credential, 0o600)
                    elif fault == "immutable-source":
                        captured = list((capture_root / "source").rglob("root.txt"))
                        if len(captured) != 1:
                            raise AssertionError(f"fixture captured unexpected source paths: {captured!r}")
                        relative = captured[0].relative_to(capture_root / "source")
                        immutable_paths = [captured[0], capture_root / ".settlement-source" / relative]
                        os.chflags(captured[0], stat.UF_IMMUTABLE)
                    else:
                        original_status = success(["profile", "server", "status", profile])
                        original_server = original_status["state"]
                        if original_status["lifecycle"] != "ready" or original_server["server_epoch"] != active["server"]["server_epoch"]:
                            raise AssertionError("stop fixture did not locate its original running generation")
                        canonical_home = original_server["snapshot"]["canonical_codex_home"]
                        if pathlib.Path(canonical_home) != codex_home.resolve():
                            raise AssertionError("stop fixture resolved a home outside its isolated provider")
                        home_hash = hashlib.sha256(b"dolgorae-home-v1\0" + canonical_home.encode()).hexdigest()
                        active_path = home / ".dolgorae" / "homes" / home_hash / "active.json"
                        active_bytes = active_path.read_bytes()
                        ownership_root = home / ".dolgorae" / "profiles" / original_status["server_key"] / "review-owners"
                        ownership_before = authority_bytes(ownership_root)
                        if len(ownership_before) != 1:
                            raise AssertionError("stop fixture did not locate one original ownership record")
                        immutable_paths = [active_path]
                        os.chflags(active_path, stat.UF_IMMUTABLE)
                    release.touch()
                    if fault == "delivered-client-loss":
                        # Opening the retained Reviewer FIFO holds close after
                        # collection commits delivery, before terminal receipt.
                        deadline = time.monotonic() + 40
                        while True:
                            code, observed = machine(recovery_arguments(workspace, request_ref),
                                timeout=min(5, max(0.1, deadline - time.monotonic())))
                            if code:
                                raise AssertionError(f"delivery observation failed: {observed!r}")
                            delivered = observed["data"]
                            assert_valid(delivered, observation_schema, "delivered active review")
                            if delivered["task"]["state"] == "delivered" and delivered["report"] is not None:
                                break
                            if process.poll() is not None:
                                raise AssertionError(f"review exited before delivery: {process.communicate(timeout=5)!r}")
                            if time.monotonic() >= deadline:
                                raise AssertionError("review did not reach its bounded delivery barrier")
                            time.sleep(0.02)
                        if (process.poll() is not None or delivered["outcome"] != "pending"
                                or delivered["reviewer"]["state"] != "idle"
                                or delivered["capture"]["state"] != "active"):
                            raise AssertionError("FIFO missed the post-collection, pre-close boundary")
                        process.kill()
                        process.communicate(timeout=10)
                        credential.unlink()
                        hidden_credential.rename(credential)
                        if credential.read_bytes() != credential_bytes:
                            raise AssertionError("fixture changed retained Reviewer credential bytes")
                    else:
                        stdout, stderr = process.communicate(timeout=90)
                        if stderr or process.returncode == 0:
                            raise AssertionError(f"native cleanup fault was not surfaced: {stdout!r} {stderr!r}")
                        failed = json.loads(stdout)
                        assert_valid(failed, machine_schema, "partial cleanup failure")
                    before = inspect(request_ref)
                    if (before["task"]["state"] != "delivered"
                            or before["report"]["verdict"] != json.loads(valid_report())
                            or before["server"]["status"] != "owned"):
                        raise AssertionError(f"fault lost the checked terminal review: {before!r}")
                    receipt = carriers / "terminal-receipt.json"
                    receipt_bytes = None if fault == "delivered-client-loss" else receipt.read_bytes()
                    if fault == "reviewer-credential":
                        if before["reviewer"]["state"] != "idle" or "authority_unavailable" not in before["recovery"]["blocked_reasons"]:
                            raise AssertionError(f"credential fault did not leave terminal Reviewer for recovery: {before!r}")
                        hidden_credential.rename(credential)
                    elif fault == "immutable-source":
                        if (before["reviewer"]["state"] != "closed"
                                or before["capture"]["state"] != "settled"
                                or before["capture"]["cleanup_pending"] is not True
                                or not immutable_paths[1].is_file()):
                            raise AssertionError(f"source fault did not reach committed pending settlement: {before!r}")
                        os.chflags(immutable_paths[1], 0)
                    elif fault == "delivered-client-loss":
                        if (before["outcome"] != "unknown" or before["result"] is not None
                                or before["recovery"] != {"status": "available", "blocked_reasons": []}
                                or receipt.exists()):
                            raise AssertionError("client loss invented a terminal result or settlement receipt")
                        for field in ("engagement", "reviewer", "task", "capture", "server", "report"):
                            if before[field] != delivered[field]:
                                raise AssertionError(f"client loss changed original {field}")
                    else:
                        expected_details = {
                            "request_ref": request_ref, "reason": "cleanup_blocked",
                            "required_action": "inspect_original_operation",
                        }
                        if failed["error"]["code"] != "REVIEW_RECOVERY_BLOCKED" or failed["error"]["details"] != expected_details:
                            raise AssertionError(f"stop preparation fault lost its checked cleanup error: {failed!r}")
                        if (before["outcome"] != "succeeded"
                                or before["reviewer"]["state"] != "closed"
                                or before["engagement"]["state"] != "closed"
                                or before["capture"]["state"] != "settled"
                                or before["capture"]["cleanup_pending"] is not False):
                            raise AssertionError(f"stop preparation fault interrupted earlier cleanup: {before!r}")
                        for field, identity in (("engagement", "engagement_id"), ("reviewer", "run_id"), ("capture", "capture_ref"), ("server", "server_epoch")):
                            if before[field][identity] != active[field][identity]:
                                raise AssertionError(f"stop preparation fault replaced original {field}")
                        for attempt in range(2):
                            if attempt:
                                code, refused = machine(recovery_arguments(workspace, request_ref, controller))
                                if code == 0 or refused["error"]["code"] != "REVIEW_RECOVERY_BLOCKED" or refused["error"]["details"] != expected_details:
                                    raise AssertionError(f"immutable stop preparation was not refused: {refused!r}")
                            if inspect(request_ref) != before:
                                raise AssertionError("stop preparation refusal changed the public original operation")
                            if authority_bytes(ownership_root) != ownership_before or active_path.read_bytes() != active_bytes:
                                raise AssertionError("failed stop preparation changed original ownership or active record")
                            status = success(["profile", "server", "status", profile])
                            if status["lifecycle"] != "ready" or any(
                                status["state"][field] != original_server[field]
                                for field in ("server_key", "server_epoch", "epoch_id", "pid", "process_fingerprint")
                            ):
                                raise AssertionError("failed stop preparation lost the running original generation")
                            os.kill(original_server["pid"], 0)
                        os.chflags(active_path, 0)
                    recovered = success(recovery_arguments(workspace, request_ref, controller))
                    assert_valid(recovered, observation_schema, "recovered partial cleanup")
                    if (recovered["reviewer"]["state"] != "closed"
                            or recovered["engagement"]["state"] != "closed"
                            or recovered["capture"]["state"] != "settled"
                            or recovered["capture"]["cleanup_pending"] is not False
                            or recovered["server"]["status"] != "retired"
                            or recovered["recovery"] != {"status": "not_needed", "blocked_reasons": []}):
                        raise AssertionError(f"public recovery did not complete original cleanup: {recovered!r}")
                    for field in ("request_ref", "request_sha256", "profile", "outcome", "result", "report", "diagnostic", "safe_error_code", "task"):
                        if recovered[field] != before[field]:
                            raise AssertionError(f"recovery changed original {field}")
                    for field, identity in (("engagement", "engagement_id"), ("reviewer", "run_id"), ("capture", "capture_ref"), ("server", "server_epoch")):
                        if recovered[field][identity] != before[field][identity]:
                            raise AssertionError(f"recovery replaced original {field}")
                    if fault == "delivered-client-loss":
                        receipt_bytes = receipt.read_bytes()
                        terminal_receipt = json.loads(receipt_bytes)
                        if (terminal_receipt["terminal_state"] != "completed"
                                or terminal_receipt["backend_lifecycle_id"] != before["engagement"]["engagement_id"]):
                            raise AssertionError("delivered report did not authorize completed settlement")
                        if recovered["server"]["epoch_id"] != before["server"]["epoch_id"]:
                            raise AssertionError("delivered recovery retired a replacement generation")
                    elif receipt.read_bytes() != receipt_bytes:
                        raise AssertionError("cleanup replaced the accepted settlement receipt")
                    if (capture_root / "source").exists() or (capture_root / ".settlement-source").exists():
                        raise AssertionError("recovery left captured source bytes")
                    if success(recovery_arguments(workspace, request_ref, controller)) != recovered:
                        raise AssertionError("completed cleanup was not idempotent")
                    if fault == "delivered-client-loss" and receipt.read_bytes() != receipt_bytes:
                        raise AssertionError("repeated cleanup replaced the completed settlement receipt")
                    if fault == "stop-prepare":
                        if recovered["server"]["epoch_id"] != original_server["epoch_id"]:
                            raise AssertionError("recovery retired a replacement process generation")
                        if success(["profile", "server", "status", profile])["lifecycle"] != "stopped":
                            raise AssertionError("recovery retained the owned server after removing the stop fault")
                    traffic = messages(root, profile)
                    if sum(item.get("method") == "turn/start" for item in traffic) != 1 or any(item.get("method") == "turn/interrupt" for item in traffic):
                        raise AssertionError("cleanup replayed or interrupted a Turn")
                finally:
                    try:
                        if fault == "delivered-client-loss" and process.poll() is None:
                            process.kill()
                        if hidden_credential.exists():
                            if fault == "delivered-client-loss" and credential.exists():
                                if not stat.S_ISFIFO(credential.lstat().st_mode):
                                    raise AssertionError("fixture refuses to replace an unexpected credential file")
                                credential.unlink()
                            hidden_credential.rename(credential)
                    finally:
                        try:
                            for path in immutable_paths:
                                if path.exists():
                                    os.chflags(path, 0)
                        finally:
                            try:
                                release.touch()
                            finally:
                                if process.poll() is None:
                                    process.kill()
                                process.communicate(timeout=10)
            if authority_bytes(workspace) != source_before:
                raise AssertionError("partial cleanup changed source or Git bytes")
        finally:
            listing = subprocess.run(["ps", "-axo", "pid=,command="], check=True,
                                     capture_output=True, text=True, timeout=10)
            for line in listing.stdout.splitlines():
                pid, _, command = line.strip().partition(" ")
                if str(root) in command and int(pid) != os.getpid():
                    try:
                        os.kill(int(pid), signal.SIGTERM)
                    except ProcessLookupError:
                        pass


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument("--protocol-root", type=pathlib.Path, default="docs/protocol")
    arguments = parser.parse_args()
    validate(arguments.binary.resolve(), arguments.protocol_root.resolve())
    print("one-shot partial cleanup recovery CLI tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
