#!/usr/bin/env python3
"""Check the TASK-060 v3 acceptance path with an isolated native fake provider."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import time

import native_codex
import test_one_shot_recovery_cli as recovery
from schema_support import assert_valid, validator

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "validators"))
from validate_agent_skills import validate_installed_resources


def request() -> dict[str, object]:
    return {
        "schema": "dolgorae-specialist-review-request/v3",
        "operation": "review_target",
        "target": {"kind": "head"},
        "purpose": "completion",
        "brief": "Check the committed one-line program against C-1. Do not repair it.",
        "contexts": [{
            "id": "requirement", "content": "C-1 requires the program to print Hello, world!",
            "provenance": "TASK-060 live acceptance criterion",
        }],
        "criteria": [{
            "id": "C-1", "statement": "The program prints Hello, world!",
            "source_context_ids": ["requirement"],
        }],
        "expected_output": "structured_review_v3",
        "deadline_seconds": 600,
    }


def fake_report() -> dict[str, object]:
    return {
        "summary": "The committed program omits the required comma.",
        "findings": [],
        "criterion_assessments": [{
            "criterion_id": "C-1", "status": "unmet",
            "explanation": "The captured line prints Hello world! without the required comma.",
            "evidence": [{
                "basis": "candidate", "description": "The print statement omits the comma.",
                "path": "hello.py", "line_start": 1, "line_end": 1,
                "context_id": None,
            }],
            "remaining_gap": "Print Hello, world! instead.",
        }],
        "evidence_limits": [],
        "overall_assessment": "requirements_not_met",
    }


def source_identity(workspace: pathlib.Path) -> tuple[str, str, str, str]:
    return (
        recovery.git(workspace, "rev-parse", "HEAD"),
        recovery.git(workspace, "status", "--porcelain=v1"),
        hashlib.sha256((workspace / "hello.py").read_bytes()).hexdigest(),
        hashlib.sha256((workspace / ".git" / "index").read_bytes()).hexdigest(),
    )


def checked_review(result: dict[str, object], review_schema) -> None:
    assert_valid(result, review_schema, "v3 independent review result")
    verdict = result["verdict"]
    assessments = verdict["criterion_assessments"]
    if (
        result["schema"] != "dolgorae-specialist-review-result/v3"
        or result["reviewer"]["model"] != "gpt-6-sol"
        or result["reviewer"]["effort"] != "high"
        or result["reviewer"]["state"] != "closed"
        or result["engagement"]["state"] != "closed"
        or result["settlement"]["state"] != "settled"
        or result["workflow_issued_source_mutation"] is not False
        or verdict["overall_assessment"] != "requirements_not_met"
        or len(assessments) != 1
        or assessments[0]["criterion_id"] != "C-1"
        or assessments[0]["status"] != "unmet"
        or not any(e["basis"] == "candidate"
                   and e["path"] in {"hello.py", "current/hello.py"}
                   and e["line_start"] == e["line_end"] == 1
                   for e in assessments[0]["evidence"])
    ):
        raise AssertionError("review did not report the defective committed line without repair")


def fill_pipe(write_fd: int) -> None:
    """Make the original CLI block on response delivery after durable cleanup."""
    os.set_blocking(write_fd, False)
    try:
        while True:
            os.write(write_fd, b"x" * 4096)
    except BlockingIOError:
        os.set_blocking(write_fd, True)


def validate(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(prefix="dolgorae-independent-fake-") as temporary:
        root = pathlib.Path(temporary).resolve()
        home, workspace = root / "home", root / "workspace"
        home.mkdir(mode=0o700)
        workspace.mkdir(mode=0o700)
        installed = root / "installed-skill"
        subprocess.run(
            [sys.executable, str(ROOT / "tools/validators/package_agent_skill.py"),
             "install", "--destination", str(installed)],
            cwd=root, check=True, capture_output=True, timeout=30,
        )
        validate_installed_resources(installed)
        protocol = installed / "resources" / "protocol"
        machine_schema = validator(protocol, "dolgorae-machine-v2.schema.json")
        observation_schema = validator(protocol, "dolgorae-one-shot-review-observation-v1.schema.json")
        review_schema = validator(protocol, "dolgorae-specialist-review-tool-v3.schema.json", "#/$defs/review_result")
        assert_valid(request(), validator(protocol, "dolgorae-specialist-review-tool-v3.schema.json",
                                          "#/$defs/review_request"), "v3 acceptance request")
        recovery.git(workspace, "init", "-b", "main")
        recovery.git(workspace, "config", "user.name", "Dolgorae E2E")
        recovery.git(workspace, "config", "user.email", "dolgorae@example.invalid")
        (workspace / "hello.py").write_text('print("Hello world!")\n')
        recovery.git(workspace, "add", "hello.py")
        recovery.git(workspace, "commit", "-m", "defective hello")

        def checked(args: list[str], *, request_body=None, timeout=60):
            code, envelope = recovery.machine(binary, home, args, request=request_body, timeout=timeout)
            assert_valid(envelope, machine_schema, "independent acceptance envelope")
            return code, envelope

        def success(args: list[str], *, request_body=None, timeout=60):
            code, envelope = checked(args, request_body=request_body, timeout=timeout)
            if code:
                raise AssertionError(f"fixture command failed: {args[:2]} {envelope.get('error', {}).get('code')}")
            return envelope["data"]

        def inspect(reference: str):
            value = success(recovery.recovery_arguments(workspace, reference))
            assert_valid(value, observation_schema, "public one-shot observation")
            return value

        try:
            success(["init", str(workspace)])
            controller = root / "owner-controller"
            recovery.credential(binary, home, controller)
            wrong_controller = root / "wrong-controller"
            recovery.credential(binary, home, wrong_controller)
            identity = source_identity(workspace)
            shared = "independent-shared"
            temporary_profile = "independent-temporary"
            for profile in (shared, temporary_profile):
                recovery.add_profile(
                    binary, home, workspace, root, native_codex.installed_codex(),
                    profile, "scoped_specialist_review_v3.json",
                    output_text=json.dumps(fake_report()),
                )
            shared_server = success(["profile", "server", "start", shared])
            shared_epoch = shared_server["state"]["server_epoch"]
            shared_ref = recovery.reference(110)
            shared_args = recovery.review_arguments(workspace, shared, shared_ref, controller)
            shared_args.remove("--temporary-server")
            result = success(shared_args, request_body=request(), timeout=180)
            checked_review(result, review_schema)
            shared_observation = inspect(shared_ref)
            if shared_observation["result"] != result or shared_observation["server"]["status"] != "preexisting":
                raise AssertionError("normal review did not preserve the shared server and original result")

            def lost_response(profile: str, reference: str, expected_outcome: str) -> dict:
                args = recovery.review_arguments(workspace, profile, reference, controller)
                read_fd, write_fd = os.pipe()
                process = None
                try:
                    fill_pipe(write_fd)
                    process = subprocess.Popen(
                        [str(binary), *args], stdin=subprocess.PIPE, stdout=write_fd,
                        stderr=subprocess.PIPE, text=True,
                        env={**os.environ, "HOME": str(home)},
                    )
                    os.close(write_fd)
                    write_fd = -1
                    process.stdin.write(json.dumps(request()))
                    process.stdin.close()
                    process.stdin = None
                    deadline = time.monotonic() + 180
                    while True:
                        if process.poll() is not None:
                            raise AssertionError("original CLI exited before its blocked response could be lost")
                        observed = inspect(reference)
                        if (
                            observed["outcome"] == expected_outcome
                            and (observed["result"] is not None if expected_outcome == "succeeded"
                                 else observed["diagnostic"] is not None)
                            and observed["reviewer"]["state"] == "closed"
                            and observed["engagement"]["state"] == "closed"
                            and observed["capture"]["state"] == "settled"
                            and observed["server"]["status"] == "retired"
                            # The operation lock may outlive server retirement.
                            and observed["recovery"] == {"status": "not_needed", "blocked_reasons": []}
                        ):
                            break
                        if time.monotonic() >= deadline:
                            raise AssertionError("review did not reach durable terminal state before response loss")
                        time.sleep(0.05)
                    process.kill()
                    process.communicate(timeout=10)
                    if process.returncode != -9:
                        raise AssertionError("test did not terminate only its original CLI")
                finally:
                    if process is not None and process.poll() is None:
                        process.kill()
                        process.communicate(timeout=10)
                    os.close(read_fd)
                    if write_fd != -1:
                        os.close(write_fd)
                restored = inspect(reference)
                if restored != observed:
                    raise AssertionError("fresh public lookup changed the original checked observation")
                if success(recovery.recovery_arguments(workspace, reference, controller)) != restored:
                    raise AssertionError("authorized cleanup changed the original observation")
                return restored

            lost_ref = recovery.reference(111)
            restored = lost_response(temporary_profile, lost_ref, "succeeded")
            checked_review(restored["result"], review_schema)
            code, refused = checked(recovery.recovery_arguments(workspace, lost_ref, wrong_controller))
            if code != 4 or refused["error"]["code"] != "REVIEW_RECOVERY_BLOCKED":
                raise AssertionError("wrong Controller acquired recovery authority")
            unknown = inspect(recovery.reference(112))
            if unknown["observation"] != "unknown" or unknown["result"] is not None:
                raise AssertionError("unknown reference invented a completed review")
            failed_profile = "independent-invalid"
            recovery.add_profile(
                binary, home, workspace, root, native_codex.installed_codex(),
                failed_profile, "scoped_specialist_review_v3.json",
                output_text='{"summary":"private-output-canary"}',
            )
            failed = lost_response(failed_profile, recovery.reference(113), "failed")
            if (
                failed["safe_error_code"] != "REVIEW_OUTPUT_INVALID"
                or failed["result"] is not None
                or "private-output-canary" in json.dumps(failed)
            ):
                raise AssertionError("lost failure response was not safely redelivered")
            after_shared = success(["profile", "server", "status", shared])
            after_temporary = success(["profile", "server", "status", temporary_profile])
            after_failed = success(["profile", "server", "status", failed_profile])
            if (
                after_shared["lifecycle"] != "ready"
                or after_shared["state"]["server_epoch"] != shared_epoch
                or after_temporary["lifecycle"] != "stopped"
                or after_failed["lifecycle"] != "stopped"
            ):
                raise AssertionError("temporary retirement affected the independent shared server")
            listing = subprocess.run(
                ["ps", "-axo", "pid=,command="], check=True, capture_output=True, text=True,
                timeout=5,
            ).stdout
            if str(root / f"bin-{shared}" / "codex") not in listing:
                raise AssertionError("process scan could not observe the pre-existing shared server")
            for profile in (temporary_profile, failed_profile):
                if str(root / f"bin-{profile}" / "codex") in listing:
                    raise AssertionError("retired temporary server process remained alive")
            for profile in (shared, temporary_profile, failed_profile):
                methods = [item.get("method") for item in recovery.messages(root, profile)]
                if methods.count("turn/start") != 1 or "turn/interrupt" in methods:
                    raise AssertionError("recovery started another Turn or interrupted the original")
            if source_identity(workspace) != identity:
                raise AssertionError("independent review changed committed source or Git state")
        finally:
            recovery.terminate_owned(root)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    args = parser.parse_args()
    validate(args.binary.resolve(strict=True))
    print("Isolated v3 defect detection, response loss, recovery and server safety passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
