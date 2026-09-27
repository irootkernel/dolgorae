#!/usr/bin/env python3
"""Isolated fake-provider coverage for the TASK-058 public recovery carrier."""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import os
import pathlib
import signal
import sqlite3
import subprocess
import tempfile
import time
from unittest.mock import patch

import native_codex
from schema_support import assert_valid, validator
from test_scoped_specialist_review_failures import (
    add_profile as fixture_add_profile,
    v3_review_request,
)


def machine(binary: pathlib.Path, home: pathlib.Path, arguments: list[str], *,
            request: dict[str, object] | None = None,
            timeout: float | None = None) -> tuple[int, dict[str, object]]:
    if timeout is None:
        timeout = 180 if arguments[:2] == ["specialist", "review"] else 60
    stdin = {"input": json.dumps(request)} if request is not None else {"stdin": subprocess.DEVNULL}
    completed = subprocess.run(
        [str(binary), *arguments], capture_output=True, text=True, timeout=timeout,
        env={**os.environ, "HOME": str(home)}, **stdin,
    )
    if completed.stderr:
        raise AssertionError(f"unexpected stderr for {arguments}: {completed.stderr!r}")
    return completed.returncode, json.loads(completed.stdout)


def machine_input(binary: pathlib.Path, home: pathlib.Path, arguments: list[str],
                  request: dict[str, object]) -> tuple[int, dict[str, object]]:
    return machine(binary, home, arguments, request=request)


def start_input(binary: pathlib.Path, home: pathlib.Path, arguments: list[str],
                request: dict[str, object]) -> subprocess.Popen[str]:
    payload = json.dumps(request)
    if len(payload.encode()) > 4096:
        raise AssertionError("asynchronous fixture request exceeds one empty pipe write")
    process = subprocess.Popen(
        [str(binary), *arguments], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, text=True, env={**os.environ, "HOME": str(home)},
    )
    process.stdin.write(payload)
    process.stdin.close()
    process.stdin = None
    return process


def add_profile(*arguments, **options) -> pathlib.Path:
    # Keep shared scenario construction, but bound its CLI call too.
    with patch("test_scoped_specialist_review_failures.machine", machine):
        return fixture_add_profile(*arguments, **options)


def terminate_owned(root: pathlib.Path) -> None:
    listing = subprocess.run(
        ["ps", "-axo", "pid=,command="], check=True, capture_output=True, text=True, timeout=5,
        env={**os.environ, "HOME": str(root / "home")},
    ).stdout
    for line in listing.splitlines():
        pid_text, _, command = line.strip().partition(" ")
        if str(root) in command and int(pid_text) != os.getpid():
            with contextlib.suppress(ProcessLookupError):
                os.kill(int(pid_text), signal.SIGTERM)


def git(repository: pathlib.Path, *arguments: str) -> str:
    return subprocess.run(
        ["git", "-C", str(repository), *arguments], check=True, capture_output=True, text=True,
        env={**os.environ, "HOME": str(repository.parent / "home")}, timeout=30,
    ).stdout.strip()


def reference(index: int) -> str:
    # Deterministic caller-owned UUIDv7 values exist before any invocation.
    return f"01990000-0000-7000-8000-{index:012x}"


def credential(binary: pathlib.Path, home: pathlib.Path, path: pathlib.Path) -> None:
    code, envelope = machine(binary, home, [
        "controller", "credential", "create", "--kind", "automation",
        "--instance-id", path.name, "--output", str(path),
    ])
    if code != 0:
        raise AssertionError(f"fixture credential creation failed: {envelope!r}")


def review_arguments(workspace: pathlib.Path, profile: str, request_ref: str,
                     controller: pathlib.Path) -> list[str]:
    return [
        "specialist", "review", "--workspace", str(workspace), "--profile", profile,
        "--request-stdin", "--format", "json", "--request-ref", request_ref,
        "--recovery-controller-file", str(controller), "--temporary-server",
    ]


def recovery_arguments(workspace: pathlib.Path, request_ref: str,
                       controller: pathlib.Path | None = None) -> list[str]:
    arguments = [
        "specialist", "review-inspect" if controller is None else "review-recover",
        "--workspace", str(workspace), "--request-ref", request_ref, "--format", "json",
    ]
    if controller is not None:
        arguments += ["--recovery-controller-file", str(controller), "--action", "cleanup"]
    return arguments


def authority_bytes(root: pathlib.Path) -> dict[str, str]:
    """Detect observation-side writes without reading private state as authority."""
    return {
        str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in root.rglob("*") if path.is_file()
        and not path.name.endswith("-shm")
        and not (path.name.endswith("-wal") and path.stat().st_size == 0)
    } if root.exists() else {}


def messages(root: pathlib.Path, profile: str) -> list[dict[str, object]]:
    transcript = root / f"transcript-{profile}.jsonl"
    return [json.loads(line) for line in transcript.read_text().splitlines()] if transcript.exists() else []


def valid_report() -> str:
    return json.dumps({
        "summary": "The captured candidate was reviewed.", "findings": [],
        "criterion_assessments": [{
            "criterion_id": "C-failure-envelope", "status": "met", "explanation": "Captured file checked.",
            "evidence": [{
                "basis": "candidate", "description": "The captured root file.",
                "path": "root.txt", "line_start": 1, "line_end": 1, "context_id": None,
            }],
            "remaining_gap": None,
        }],
        "evidence_limits": [], "overall_assessment": "requirements_met",
    })


def validate(binary: pathlib.Path, protocol_root: pathlib.Path) -> None:
    machine_schema = validator(protocol_root, "dolgorae-machine-v2.schema.json")
    observation_schema = validator(protocol_root, "dolgorae-one-shot-review-observation-v1.schema.json")
    schema_source = native_codex.installed_codex()
    with tempfile.TemporaryDirectory(prefix="dolgorae-task058-") as temporary:
        root = pathlib.Path(temporary).resolve()
        home, workspace = root / "home", root / "workspace"
        home.mkdir(mode=0o700)
        workspace.mkdir(mode=0o700)
        git(workspace, "init", "-b", "main")
        git(workspace, "config", "user.name", "Dolgorae E2E")
        git(workspace, "config", "user.email", "dolgorae@example.invalid")
        (workspace / "root.txt").write_text("root\n")
        git(workspace, "add", "root.txt")
        git(workspace, "commit", "-m", "root")
        code, initialized = machine(binary, home, ["init", str(workspace)])
        if code != 0:
            raise AssertionError(f"fixture initialization failed: {initialized!r}")
        state_root = home / ".dolgorae" / "workspaces" / initialized["data"]["workspace_id"]
        orchestration = state_root / "orchestration"
        controller, wrong_controller = root / "recovery-owner", root / "wrong-owner"
        credential(binary, home, controller)
        credential(binary, home, wrong_controller)
        source_before = authority_bytes(workspace)

        def checked(arguments: list[str], *, timeout: float | None = None) -> tuple[int, dict[str, object]]:
            code, envelope = machine(binary, home, arguments, timeout=timeout)
            assert_valid(envelope, machine_schema, arguments[1])
            return code, envelope

        def inspect(request_ref: str) -> dict[str, object]:
            before = authority_bytes(orchestration)
            code, envelope = checked(recovery_arguments(workspace, request_ref))
            if code != 0:
                raise AssertionError(f"public observation failed: {envelope!r}")
            observation = envelope["data"]
            assert_valid(observation, observation_schema, "one-shot observation")
            after = authority_bytes(orchestration)
            if after != before:
                changed = sorted(key for key in before.keys() | after.keys() if before.get(key) != after.get(key))
                raise AssertionError(f"read-only lookup changed durable orchestration files: {changed!r}")
            if observation["request_ref"] != request_ref:
                raise AssertionError("lookup returned a different request reference")
            serialized = json.dumps(observation)
            if any(str(path) in serialized for path in (controller, wrong_controller, orchestration)):
                raise AssertionError("public observation exposed a private credential or carrier path")
            return observation

        def blocked(request_ref: str, presented: pathlib.Path, reason: str) -> None:
            before = authority_bytes(orchestration)
            code, envelope = checked(recovery_arguments(workspace, request_ref, presented))
            error = envelope.get("error", {})
            if code == 0 or error.get("code") != "REVIEW_RECOVERY_BLOCKED" or error.get("details") != {
                "request_ref": request_ref, "reason": reason,
                "required_action": "inspect_original_operation",
            }:
                raise AssertionError(f"expected bounded recovery refusal: {envelope!r}")
            # Unauthorized and unknown calls must not open a writer. An
            # authorized cleanup may checkpoint existing SQLite WAL pages even
            # when lifecycle evidence subsequently blocks cleanup.
            if reason in ("authority_unavailable", "operation_unknown", "operation_active") and authority_bytes(orchestration) != before:
                raise AssertionError("refused recovery changed original authority")

        def cleanup(request_ref: str) -> dict[str, object]:
            code, envelope = checked(recovery_arguments(workspace, request_ref, controller))
            if code != 0:
                raise AssertionError(f"authorized terminal cleanup failed: {envelope!r}")
            assert_valid(envelope["data"], observation_schema, "cleanup observation")
            return envelope["data"]

        def terminal(observation: dict[str, object], outcome: str, server_status: str) -> None:
            if (
                observation["observation"] != "known" or observation["outcome"] != outcome
                or observation["engagement"]["state"] != "closed"
                or observation["reviewer"]["state"] != "closed"
                or observation["capture"]["state"] != "settled"
                or observation["server"]["status"] != server_status
                or observation["recovery"] != {"status": "not_needed", "blocked_reasons": []}
            ):
                raise AssertionError(f"original operation did not reach independent terminal authorities: {observation!r}")

        def wait_for_turn(process: subprocess.Popen[str], profile: str, request_ref: str) -> dict[str, object]:
            deadline = time.monotonic() + 90
            while time.monotonic() < deadline:
                code, envelope = checked(recovery_arguments(workspace, request_ref),
                                         timeout=min(10, max(0.1, deadline - time.monotonic())))
                if code != 0:
                    raise AssertionError(f"active operation could not be inspected: {envelope!r}")
                active = envelope["data"]
                assert_valid(active, observation_schema, "active operation")
                if active["task"] is not None and active["reviewer"]["state"] == "running" and any(
                    item.get("method") == "turn/start" for item in messages(root, profile)
                ):
                    return active
                if process.poll() is not None:
                    raise AssertionError(f"fixture exited before interruption: {process.communicate(timeout=10)!r}")
                time.sleep(0.05)
            process.kill()
            process.communicate(timeout=10)
            raise AssertionError("review never reached an accepted active Turn")

        try:
            for invalid_ref in ("not-a-uuid", "018f1111-2222-4333-8444-555555555555", reference(99) + "x"):
                before = authority_bytes(home / ".dolgorae")
                for arguments in (
                    recovery_arguments(workspace, invalid_ref),
                    recovery_arguments(workspace, invalid_ref, controller),
                ):
                    code, envelope = checked(arguments)
                    if code != 2 or envelope["error"]["code"] != "INVALID_ARGUMENT":
                        raise AssertionError(f"invalid reference was not rejected: {envelope!r}")
                code, envelope = machine_input(binary, home,
                    review_arguments(workspace, "absent-profile", invalid_ref, controller), v3_review_request())
                assert_valid(envelope, machine_schema, "invalid review reference")
                if code != 2 or envelope["error"]["code"] != "INVALID_ARGUMENT":
                    raise AssertionError(f"invalid review reference reached preparation: {envelope!r}")
                if authority_bytes(home / ".dolgorae") != before:
                    raise AssertionError("invalid reference created or changed runtime state")

            before = authority_bytes(home / ".dolgorae")
            relative_controller = pathlib.Path("relative-controller")
            code, envelope = machine_input(binary, home,
                review_arguments(workspace, "absent-profile", reference(98), relative_controller), v3_review_request())
            assert_valid(envelope, machine_schema, "relative review credential")
            if (code != 2 or envelope["error"]["code"] != "INVALID_ARGUMENT"
                    or envelope["error"]["details"]["argument"] != "--recovery-controller-file"):
                raise AssertionError(f"relative review credential reached preparation: {envelope!r}")
            code, envelope = checked(recovery_arguments(workspace, reference(98), relative_controller))
            if (code != 2 or envelope["error"]["code"] != "INVALID_ARGUMENT"
                    or envelope["error"]["details"]["argument"] != "--recovery-controller-file"):
                raise AssertionError(f"relative recovery credential was not rejected: {envelope!r}")
            if authority_bytes(home / ".dolgorae") != before:
                raise AssertionError("relative credential path created or changed runtime state")

            unknown = inspect(reference(99))
            if unknown["observation"] != "unknown" or unknown["outcome"] != "unknown":
                raise AssertionError("unknown reference invented an operation")
            if any(unknown[field] is not None for field in (
                "profile", "request_sha256", "result", "report", "diagnostic",
                "engagement", "reviewer", "task", "capture", "server",
            )):
                raise AssertionError("unknown reference invented identities")
            blocked(reference(99), controller, "operation_unknown")

            for index, preexisting in ((1, False), (2, True)):
                profile = f"recovery-success-{index}"
                add_profile(binary, home, workspace, root, schema_source, profile,
                            "scoped_specialist_review_v3.json", output_text=valid_report())
                if preexisting:
                    code, started = checked(["profile", "server", "start", profile])
                    if code != 0:
                        raise AssertionError(f"shared fixture server did not start: {started!r}")
                    shared_state = started["data"]["state"]
                arguments = review_arguments(workspace, profile, reference(index), controller)
                if preexisting:
                    code, response = machine_input(binary, home, arguments, v3_review_request())
                    assert_valid(response, machine_schema, "recoverable success")
                    if code != 0:
                        raise AssertionError(f"recoverable review failed: {response!r}")
                else:
                    # The caller retains only its reference and credential;
                    # the initial response is never delivered to the test.
                    environment = {**os.environ, "HOME": str(home)}
                    completed = subprocess.run(
                        [str(binary), *arguments], input=json.dumps(v3_review_request()),
                        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True, env=environment, timeout=180,
                    )
                    if completed.returncode != 0 or completed.stderr:
                        raise AssertionError(f"response-loss fixture failed: {completed.stderr!r}")
                observation = inspect(reference(index))
                terminal(observation, "succeeded", "preexisting" if preexisting else "retired")
                if observation["result"]["verdict"] != json.loads(valid_report()):
                    raise AssertionError("lookup failed to retrieve the original checked verdict")
                if observation["report"]["verdict"] != observation["result"]["verdict"]:
                    raise AssertionError("stored report and original result disagree")
                if preexisting and observation["result"] != response["data"]:
                    raise AssertionError("fresh-process lookup changed the original result")
                if inspect(reference(index)) != observation:
                    raise AssertionError("repeated inspection changed the original observation")
                blocked(reference(index), wrong_controller, "authority_unavailable")
                if cleanup(reference(index)) != observation:
                    raise AssertionError("idempotent cleanup changed the terminal observation")
                code, status = checked(["profile", "server", "status", profile])
                if code != 0 or status["data"]["lifecycle"] != ("ready" if preexisting else "stopped"):
                    raise AssertionError("temporary cleanup stopped the wrong server or retained its owned server")
                if preexisting and status["data"]["state"]["server_epoch"] != shared_state["server_epoch"]:
                    raise AssertionError("temporary review replaced the shared generation")
                transcript_before = messages(root, profile)
                for change in ("none", "request", "credential", "policy", "profile"):
                    repeated_arguments = list(arguments)
                    repeated_request = v3_review_request()
                    if change == "request":
                        repeated_request["brief"] = "Different accepted request."
                    elif change == "credential":
                        repeated_arguments[repeated_arguments.index(str(controller))] = str(wrong_controller)
                    elif change == "policy":
                        repeated_arguments.remove("--temporary-server")
                    elif change == "profile":
                        repeated_arguments[repeated_arguments.index(profile)] = "unregistered-other-profile"
                    code, repeated = machine_input(binary, home, repeated_arguments, repeated_request)
                    assert_valid(repeated, machine_schema, "same-reference refusal")
                    expected = "REVIEW_RECOVERY_BLOCKED" if change == "none" else "IDEMPOTENCY_CONFLICT"
                    if code == 0 or repeated["error"]["code"] != expected:
                        raise AssertionError(f"same reference re-entered execution: {repeated!r}")
                if messages(root, profile) != transcript_before or inspect(reference(index)) != observation:
                    raise AssertionError("same-reference execution changed the original provider operation")

            # The same caller-owned carrier covers the two flag-based input
            # modes, including successful ownership without retirement policy.
            for index, legacy in ((40, True), (41, False)):
                profile = "recovery-legacy" if legacy else "recovery-scoped-persistent"
                add_profile(binary, home, workspace, root, schema_source, profile,
                            "scoped_specialist_review.json")
                arguments = review_arguments(workspace, profile, reference(index), controller)
                arguments.remove("--request-stdin")
                if legacy:
                    arguments += ["--scope", "working-tree"]
                else:
                    arguments.remove("--temporary-server")
                    arguments += ["--target-kind", "head", "--deadline-seconds", "60"]
                code, response = checked(arguments)
                if code != 0:
                    raise AssertionError(f"flag-based recoverable review failed: {response!r}")
                observation = inspect(reference(index))
                if observation["result"] != response["data"] or observation["outcome"] != "succeeded":
                    raise AssertionError("flag-based lookup lost its original result")
                if legacy:
                    if (
                        observation["result"]["operation"] != "review_working_tree_result"
                        or observation["capture"] is not None
                        or observation["engagement"]["state"] != "closed"
                        or observation["reviewer"]["state"] != "closed"
                        or observation["server"]["status"] != "retired"
                        or observation["recovery"] != {"status": "not_needed", "blocked_reasons": []}
                    ):
                        raise AssertionError("Legacy recovery invented a capture or lost terminal authority")
                else:
                    terminal(observation, "succeeded", "owned")
                    if observation["result"]["target"]["request"]["kind"] != "head":
                        raise AssertionError("Scoped recovery lost the requested target")
                code, status = checked(["profile", "server", "status", profile])
                if code != 0 or status["data"]["lifecycle"] != ("stopped" if legacy else "ready"):
                    raise AssertionError("flag-based review violated its server retirement policy")
                if not legacy and status["data"]["state"]["server_epoch"] != observation["server"]["server_epoch"]:
                    raise AssertionError("non-temporary review lost its original owned server generation")
                if cleanup(reference(index)) != observation:
                    raise AssertionError("terminal flag-based cleanup changed the original operation")
                if not legacy:
                    _, after_cleanup = checked(["profile", "server", "status", profile])
                    if after_cleanup["data"] != status["data"]:
                        raise AssertionError("cleanup retired or replaced a non-temporary server")
                code, repeated = checked(arguments)
                if code == 0 or repeated["error"]["code"] != "REVIEW_RECOVERY_BLOCKED":
                    raise AssertionError("same-reference flag-based execution replayed the review")
                if len([item for item in messages(root, profile) if item.get("method") == "turn/start"]) != 1:
                    raise AssertionError("flag-based inspection or cleanup dispatched another Turn")

            other_workspace = root / "other-workspace"
            other_workspace.mkdir()
            git(other_workspace, "init", "-b", "main")
            code, initialized_other = checked(["init", str(other_workspace)])
            if code != 0:
                raise AssertionError(f"second workspace initialization failed: {initialized_other!r}")
            other_authority = home / ".dolgorae" / "workspaces" / initialized_other["data"]["workspace_id"] / "orchestration"
            before_original, before_other = authority_bytes(orchestration), authority_bytes(other_authority)
            code, foreign = checked(recovery_arguments(other_workspace, reference(1)))
            assert_valid(foreign["data"], observation_schema, "cross-workspace observation")
            if code != 0 or foreign["data"] != {**unknown, "request_ref": reference(1)}:
                raise AssertionError("another workspace exposed the original operation")
            code, refused = checked(recovery_arguments(other_workspace, reference(1), controller))
            if code == 0 or refused["error"]["code"] != "REVIEW_RECOVERY_BLOCKED" or refused["error"]["details"] != {
                "request_ref": reference(1), "reason": "operation_unknown",
                "required_action": "inspect_original_operation",
            }:
                raise AssertionError("another workspace accepted the original operation credential")
            if authority_bytes(orchestration) != before_original or authority_bytes(other_authority) != before_other:
                raise AssertionError("cross-workspace lookup or refusal mutated either authority")

            profile = "recovery-invalid-output"
            raw_output = ' {"summary":"private-output-canary"}\n'
            add_profile(binary, home, workspace, root, schema_source, profile,
                        "scoped_specialist_review_v3.json", output_text=raw_output)
            code, failure = machine_input(binary, home,
                review_arguments(workspace, profile, reference(3), controller), v3_review_request())
            assert_valid(failure, machine_schema, "recoverable invalid output")
            if code == 0 or failure["error"]["code"] != "REVIEW_OUTPUT_INVALID":
                raise AssertionError(f"invalid-output fixture did not fail as intended: {failure!r}")
            original_diagnostic = failure["error"]["details"]["cause_details"]["diagnostic"]
            observation = inspect(reference(3))
            terminal(observation, "failed", "retired")
            if observation["diagnostic"] != original_diagnostic or observation["result"] is not None:
                raise AssertionError("fresh process lost the original failed diagnostic")
            if observation["safe_error_code"] != "REVIEW_OUTPUT_INVALID" or "private-output-canary" in json.dumps(observation):
                raise AssertionError("failure lookup lost its safe error or disclosed provider content")
            if cleanup(reference(3)) != observation or len([
                item for item in messages(root, profile) if item.get("method") == "turn/start"
            ]) != 1:
                raise AssertionError("failed-output cleanup replayed the review")

            profile = "recovery-interrupted-closure"
            add_profile(binary, home, workspace, root, schema_source, profile,
                        "scoped_specialist_review_v3.json", output_text=valid_report())
            with contextlib.closing(sqlite3.connect(orchestration / "orchestration.sqlite3")) as connection, connection:
                connection.execute(
                    "CREATE TRIGGER fixture_reject_closure BEFORE UPDATE OF state ON engagements "
                    "WHEN NEW.state='closed' BEGIN SELECT RAISE(ABORT, 'fixture closure interruption'); END"
                )
            try:
                completed = subprocess.run(
                    [str(binary), *review_arguments(workspace, profile, reference(4), controller)],
                    input=json.dumps(v3_review_request()), stdout=subprocess.DEVNULL,
                    stderr=subprocess.PIPE, text=True, env={**os.environ, "HOME": str(home)}, timeout=180,
                )
                if completed.returncode == 0 or completed.stderr:
                    raise AssertionError("closure fault did not interrupt the original operation cleanly")
            finally:
                # Remove the fault, never rewrite the failed operation state.
                with contextlib.closing(sqlite3.connect(orchestration / "orchestration.sqlite3")) as connection, connection:
                    connection.execute("DROP TRIGGER fixture_reject_closure")
            interrupted_closure = inspect(reference(4))
            if (
                interrupted_closure["engagement"]["state"] != "released"
                or interrupted_closure["reviewer"]["state"] != "closed"
                or interrupted_closure["capture"]["state"] != "active"
                or interrupted_closure["server"]["status"] != "owned"
                or interrupted_closure["recovery"]["status"] != "available"
                or interrupted_closure["report"]["verdict"] != json.loads(valid_report())
            ):
                raise AssertionError(f"closure interruption lost the original cleanup opportunity: {interrupted_closure!r}")
            blocked(reference(4), wrong_controller, "authority_unavailable")
            recovered_closure = cleanup(reference(4))
            terminal(recovered_closure, interrupted_closure["outcome"], "retired")
            for field in ("request_ref", "request_sha256", "profile", "outcome", "result", "report", "diagnostic", "safe_error_code", "task"):
                if recovered_closure[field] != interrupted_closure[field]:
                    raise AssertionError(f"authorized cleanup rewrote original {field}")
            for field, identity in (("engagement", "engagement_id"), ("reviewer", "run_id"), ("capture", "capture_ref"), ("server", "server_epoch")):
                if recovered_closure[field][identity] != interrupted_closure[field][identity]:
                    raise AssertionError(f"authorized cleanup replaced original {field} identity")
            if cleanup(reference(4)) != recovered_closure or len([
                item for item in messages(root, profile) if item.get("method") == "turn/start"
            ]) != 1:
                raise AssertionError("resumed cleanup dispatched a replacement review or was not idempotent")

            for case_index, fault in enumerate(("model", "effort", "policy", "credential", "engagement", "capture")):
                for preexisting in (False, True):
                    index = 10 + 2 * case_index + int(preexisting)
                    profile = f"recovery-preparation-{fault}-{index}"
                    models = [{
                        "model": "gpt-5.6" if fault == "model" else "gpt-6-sol",
                        "isDefault": True,
                        "supportedReasoningEfforts": [
                            {"reasoningEffort": "medium" if fault == "effort" else "high"},
                        ],
                    }]
                    add_profile(binary, home, workspace, root, schema_source, profile,
                        "scoped_specialist_review_v3.json", advertised_models=models,
                        recursive_adapter=fault == "policy")
                    if preexisting:
                        code, started = checked(["profile", "server", "start", profile])
                        if code != 0:
                            raise AssertionError(f"shared preparation fixture failed: {started!r}")
                    request = v3_review_request()
                    if fault == "credential":
                        # Deterministic isolated filesystem fault before retained
                        # private carrier creation; never use this as recovery authority.
                        (orchestration / f"one-shot-{reference(index)}").write_text("fixture obstruction")
                    elif fault == "engagement":
                        # Reject only the engagement allocation transaction. The
                        # public receipt still exists and remains inspectable.
                        with contextlib.closing(sqlite3.connect(orchestration / "orchestration.sqlite3")) as connection, connection:
                            connection.execute(
                                "CREATE TRIGGER fixture_reject_engagement BEFORE INSERT ON engagements "
                                "BEGIN SELECT RAISE(ABORT, 'fixture engagement allocation failure'); END"
                            )
                    elif fault == "capture":
                        request["target"] = {"kind": "commit", "revision": "refs/heads/missing-fixture-revision"}
                    try:
                        code, failure = machine_input(binary, home,
                            review_arguments(workspace, profile, reference(index), controller), request)
                    finally:
                        if fault == "engagement":
                            with contextlib.closing(sqlite3.connect(orchestration / "orchestration.sqlite3")) as connection, connection:
                                connection.execute("DROP TRIGGER fixture_reject_engagement")
                    assert_valid(failure, machine_schema, f"{fault} preparation failure")
                    expected_error = {
                        "model": "COMPATIBILITY_REJECTED", "effort": "COMPATIBILITY_REJECTED",
                        "policy": "REVIEW_PROFILE_UNAVAILABLE", "credential": "INTERNAL_ERROR",
                        "engagement": "INTERNAL_ERROR", "capture": "REVIEW_TARGET_REVISION_INVALID",
                    }[fault]
                    if code == 0 or failure["error"]["code"] != expected_error:
                        raise AssertionError(f"pre-Reviewer {fault} fixture failed unexpectedly: {failure!r}")
                    observation = inspect(reference(index))
                    if (
                        observation["outcome"] != "failed"
                        or observation["safe_error_code"] != expected_error
                        or observation["server"]["status"] != ("preexisting" if preexisting else "retired")
                        or observation["server"]["server_epoch"] is None
                        or any(observation[field] is not None for field in ("reviewer", "task"))
                    ):
                        raise AssertionError(f"{fault} failure lost its known server or fabricated Reviewer identity")
                    if fault in ("engagement", "capture"):
                        capture = observation["capture"]
                        if (
                            capture is None or capture["state"] != "reserved"
                            or capture["revision"] is not None or capture["cleanup_pending"] is not False
                        ):
                            raise AssertionError("unpublished capture reservation was lost or claimed settled")
                    elif observation["capture"] is not None:
                        raise AssertionError("failure before capture reservation invented a capture")
                    if fault == "capture":
                        if observation["engagement"] is None or observation["engagement"]["state"] != "closed":
                            raise AssertionError("failed capture did not close its original engagement")
                    elif observation["engagement"] is not None:
                        raise AssertionError("failed preparation invented an engagement")
                    if cleanup(reference(index)) != observation:
                        raise AssertionError("preparation cleanup changed the original failure")
                    if any(item.get("method") in ("thread/start", "turn/start") for item in messages(root, profile)):
                        raise AssertionError("preparation failure allocated a Reviewer conversation")
                    code, status = checked(["profile", "server", "status", profile])
                    if code != 0 or status["data"]["lifecycle"] != ("ready" if preexisting else "stopped"):
                        raise AssertionError("preparation failure violated server retirement ownership")
                    if preexisting and status["data"]["state"]["server_epoch"] != started["data"]["state"]["server_epoch"]:
                        raise AssertionError("preparation failure replaced the pre-existing server generation")

            for index, interrupt in ((7, False), (8, True), (42, False)):
                scoped = index == 42
                profile = f"recovery-interrupted-client-{index}"
                add_profile(binary, home, workspace, root, schema_source, profile,
                            "scoped_specialist_review_timeout.json")
                request = v3_review_request()
                arguments = review_arguments(workspace, profile, reference(index), controller)
                if interrupt:
                    process = start_input(binary, home, arguments, request)
                    active = wait_for_turn(process, profile, reference(index))
                    transcript_before = messages(root, profile)
                    blocked(reference(index), controller, "operation_active")
                    if inspect(reference(index)) != active or messages(root, profile) != transcript_before:
                        raise AssertionError("active recovery refusal changed the operation or provider Turn")
                    process.send_signal(signal.SIGINT)
                    stdout, stderr = process.communicate(timeout=30)
                    if stderr:
                        raise AssertionError(f"cancelled review produced stderr: {stderr!r}")
                    code, response = process.returncode, json.loads(stdout)
                elif scoped:
                    arguments.remove("--request-stdin")
                    arguments += ["--target-kind", "workspace", "--deadline-seconds", "1"]
                    # The bound is below the 600-second default, so silently
                    # dropping this flag cannot pass through eventual timeout.
                    code, response = checked(arguments, timeout=120)
                else:
                    request["deadline_seconds"] = 1
                    code, response = machine_input(binary, home, arguments, request)
                expected_error = "REVIEW_CANCELLED" if interrupt else "REVIEW_TIMEOUT"
                assert_valid(response, machine_schema, "interrupted review response")
                if code == 0 or response["error"]["code"] != expected_error:
                    raise AssertionError(f"interruption changed the original failure: {response!r}")
                observation = inspect(reference(index))
                if (
                    observation["outcome"] != "failed"
                    or observation["safe_error_code"] != expected_error
                    or observation["result"] is not None
                    or observation["engagement"]["state"] != "interrupted_unknown"
                    or observation["capture"]["state"] != "active"
                    or observation["server"]["status"] != "owned"
                    or observation["recovery"]["status"] != "blocked"
                ):
                    raise AssertionError("timeout or cancellation lost original recovery evidence")
                blocked(reference(index), controller, "cleanup_blocked")
                methods = [item.get("method") for item in messages(root, profile)]
                if methods.count("turn/start") != 1 or methods.count("turn/interrupt") != int(interrupt):
                    raise AssertionError("recovery replayed a timed-out task or added an interruption")

            profile = "recovery-killed-client"
            add_profile(binary, home, workspace, root, schema_source, profile,
                        "scoped_specialist_review_timeout.json")
            process = start_input(binary, home,
                review_arguments(workspace, profile, reference(6), controller), v3_review_request())
            active = wait_for_turn(process, profile, reference(6))
            process.kill()
            process.communicate(timeout=10)
            interrupted = inspect(reference(6))
            if (
                interrupted["outcome"] != "unknown"
                or interrupted["recovery"]["status"] != "blocked"
                or interrupted["result"] is not None
                or interrupted["engagement"] != active["engagement"]
                or interrupted["reviewer"]["run_id"] != active["reviewer"]["run_id"]
                or interrupted["capture"]["state"] != "active"
                or interrupted["server"]["status"] != "owned"
            ):
                raise AssertionError(f"termination lost original identities or claimed terminal cleanup: {interrupted!r}")
            blocked(reference(6), wrong_controller, "authority_unavailable")
            blocked(reference(6), controller, "cleanup_blocked")
            methods = [item.get("method") for item in messages(root, profile)]
            if methods.count("turn/start") != 1 or "turn/interrupt" in methods:
                raise AssertionError("inspection or cleanup implicitly replayed or interrupted the active Turn")

            for index, preexisting in ((30, False), (31, True)):
                profile = f"recovery-killed-before-member-{index}"
                add_profile(binary, home, workspace, root, schema_source, profile,
                            "scoped_specialist_review_v3.json", output_text=valid_report())
                if preexisting:
                    code, started = checked(["profile", "server", "start", profile])
                    if code != 0:
                        raise AssertionError(f"shared pre-member fixture failed: {started!r}")
                index_path = workspace / ".git" / "index"
                original_index = index_path.read_bytes()
                index_mode = index_path.stat().st_mode & 0o777
                index_path.unlink()
                os.mkfifo(index_path, 0o600)
                process = None
                readers: list[int] = []
                try:
                    process = start_input(binary, home,
                        review_arguments(workspace, profile, reference(index), controller), v3_review_request())
                    deadline = time.monotonic() + 45
                    while time.monotonic() < deadline:
                        code, envelope = checked(recovery_arguments(workspace, reference(index)),
                                                 timeout=min(10, max(0.1, deadline - time.monotonic())))
                        if code != 0:
                            raise AssertionError(f"pre-member inspection failed: {envelope!r}")
                        before_member = envelope["data"]
                        assert_valid(before_member, observation_schema, "pre-member observation")
                        listing = subprocess.run(
                            ["ps", "-axo", "pid=,command="], check=True, capture_output=True, text=True,
                            env={**os.environ, "HOME": str(home)}, timeout=5,
                        ).stdout
                        readers = [int(line.strip().split(" ", 1)[0]) for line in listing.splitlines()
                                   if str(workspace) in line and " ls-files " in line and "/usr/bin/git " in line]
                        if (
                            before_member["engagement"] is not None
                            and before_member["capture"] is not None
                            and before_member["server"] is not None and readers
                        ):
                            break
                        if process.poll() is not None:
                            raise AssertionError(f"pre-member fixture exited before interruption: {process.communicate(timeout=10)!r}")
                        time.sleep(0.02)
                    else:
                        raise AssertionError("index FIFO blocked discovery or missed the capture boundary")
                    if before_member["reviewer"] is not None or before_member["task"] is not None:
                        raise AssertionError("pre-member barrier allowed Reviewer allocation")
                    process.kill()
                    process.communicate(timeout=10)
                finally:
                    if process is not None and process.poll() is None:
                        process.kill()
                    # Release the owned Git open before restoring the original
                    # index. No replacement index data is supplied to the reader.
                    try:
                        try:
                            writer = os.open(index_path, os.O_WRONLY | os.O_NONBLOCK)
                        except OSError:
                            pass
                        else:
                            os.close(writer)
                    finally:
                        index_path.unlink()
                        index_path.write_bytes(original_index)
                        index_path.chmod(index_mode)
                    for pid in readers:
                        deadline = time.monotonic() + 5
                        while time.monotonic() < deadline:
                            try:
                                os.kill(pid, 0)
                            except ProcessLookupError:
                                break
                            time.sleep(0.01)
                        else:
                            with contextlib.suppress(ProcessLookupError):
                                os.kill(pid, signal.SIGTERM)
                    if process is not None:
                        process.communicate(timeout=10)
                interrupted_capture = inspect(reference(index))
                if (
                    interrupted_capture["outcome"] != "unknown"
                    or interrupted_capture["engagement"]["engagement_id"] != before_member["engagement"]["engagement_id"]
                    or interrupted_capture["reviewer"] is not None or interrupted_capture["task"] is not None
                    or interrupted_capture["capture"]["state"] != "reserved"
                    or interrupted_capture["capture"]["capture_ref"] != before_member["capture"]["capture_ref"]
                    or interrupted_capture["server"]["status"] != ("preexisting" if preexisting else "owned")
                    or interrupted_capture["server"]["server_epoch"] != before_member["server"]["server_epoch"]
                    or interrupted_capture["recovery"]["status"] != "blocked"
                ):
                    raise AssertionError("pre-member interruption lost the original capture or server reservation")
                blocked(reference(index), controller, "cleanup_blocked")
                if any(item.get("method") in ("thread/start", "turn/start") for item in messages(root, profile)):
                    raise AssertionError("pre-member recovery dispatched a Reviewer")

            profile = "recovery-killed-preparation"
            add_profile(binary, home, workspace, root, schema_source, profile,
                        "scoped_specialist_review_v3.json")
            scenario_path = root / f"scenario-{profile}.json"
            scenario = json.loads(scenario_path.read_text())
            for step in scenario["steps"]:
                if step.get("method") == "model/list":
                    step["respond"] = {"silent": True}
            scenario_path.write_text(json.dumps(scenario))
            process = start_input(binary, home,
                review_arguments(workspace, profile, reference(9), controller), v3_review_request())
            deadline = time.monotonic() + 30
            while not any(item.get("method") == "model/list" for item in messages(root, profile)):
                if process.poll() is not None:
                    raise AssertionError(f"preparation exited before launch interruption: {process.communicate(timeout=10)!r}")
                if time.monotonic() >= deadline:
                    process.kill()
                    process.communicate(timeout=10)
                    raise AssertionError("fixture server did not reach its unanswered model handshake")
                time.sleep(0.01)
            process.kill()
            process.communicate(timeout=10)
            interrupted_launch = inspect(reference(9))
            if (
                interrupted_launch["outcome"] != "unknown"
                or interrupted_launch["recovery"]["status"] != "blocked"
                or interrupted_launch["server"]["status"] != "recovery_blocked"
                or interrupted_launch["server"]["reason"] != "launch_identity_incomplete"
                or interrupted_launch["server"]["server_epoch"] is None
                or interrupted_launch["server"]["epoch_id"] is not None
                or any(interrupted_launch[field] is not None for field in ("engagement", "reviewer", "task", "capture"))
            ):
                raise AssertionError(f"launch interruption lost its reserved generation or invented identities: {interrupted_launch!r}")
            blocked(reference(9), controller, "cleanup_blocked")
            if any(item.get("method") in ("thread/start", "turn/start") for item in messages(root, profile)):
                raise AssertionError("uncertain preparation recovery dispatched a Reviewer")
            if authority_bytes(workspace) != source_before:
                raise AssertionError("review or recovery changed source or Git bytes")
        finally:
            terminate_owned(root)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument("--protocol-root", type=pathlib.Path, default="docs/protocol")
    arguments = parser.parse_args()
    validate(arguments.binary.resolve(), arguments.protocol_root.resolve())
    print("one-shot review recovery CLI tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
