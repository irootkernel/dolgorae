#!/usr/bin/env python3
"""Opt-in production CLI qualification with isolated state and bounded loopback Turns."""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any
from unittest.mock import patch

import run_native_caller_pipeline as caller
from orphan_cleanup import cleanup_removed_root
from run_live_instruction_discovery import (
    CORE_PREFIX, ProbeCheckFailure, checked_codex, executable_identity, isolated_environment,
    private_file, verify_core_prefix,
)
from run_native_workspace_policy_fixture import ScriptedResponses, fixture_config, terminal_output
from schema_support import assert_valid, validator

OPT_IN = "DOLGORAE_RUN_NATIVE_INTEGRATED_ACCEPTANCE"
CODEX_VERSION = caller.CODEX_VERSION
CODEX_SHA256 = caller.CODEX_SHA256
MAX_TURNS = 17
MAX_POSTS = MAX_TURNS * 2
TURN_TIMEOUT = "60s"
PROTOCOL = Path(__file__).resolve().parents[2] / "docs" / "protocol"
COMMON = "integrated-common-017"
GENERAL = "integrated-general-017"
REVIEWER = "integrated-reviewer-017"
PROFILE = "integrated-profile-user-017"
AMBIENT = ("integrated-project-forbidden-017", "integrated-ancestor-forbidden-017")


def overwrite_fixture_file(path: Path, body: bytes) -> None:
    """Rewrite an existing file in the private campaign fixture."""
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_NOFOLLOW), "wb") as output:
        output.truncate()
        output.write(body)


def same_captured_configuration(captured: dict, stored: dict) -> bool:
    """Compare the accepted caller snapshot with the serialized schema-3 Run."""
    if stored != captured or stored.get("schema_version") != 3:
        return False
    instructions = stored["instructions"]
    prefix = f"{CORE_PREFIX}\n\n{instructions['common']['body']}\n\n{instructions['role']['body']}"
    return hashlib.sha256(prefix.encode()).hexdigest() == instructions["composition_sha256"]


def require_closed_session(value: dict, run_id: str, *, interrupt: bool = False) -> str:
    close = value.get("session_close")
    if (value.get("run_id") != run_id or value.get("lifecycle") != "closed"
            or value.get("writer_state") != "none" or not isinstance(close, dict)
            or close.get("progress") != ("aborted" if interrupt else "completed")
            or close.get("interrupt") is not interrupt
            or not isinstance(close.get("operation_id"), str) or not close["operation_id"]):
        raise ProbeCheckFailure("modern_aggregate_close_unproved")
    return close["operation_id"]


def assert_no_source_isolation(home: Path) -> None:
    for state_root in (home / ".dolgorae/workspaces").glob("*"):
        if any((state_root / path).exists() for path in
               ("review-targets", "worktrees", "orchestration/isolated")):
            raise ProbeCheckFailure("source_isolation_created")


def stop_fixture_profile(call, root: Path, operator: Path) -> None:
    status = call("profile", "server", "status", "integrated")
    state = status["state"]
    if (status["profile"] != "integrated" or state["server_key"] != status["server_key"]
            or state["snapshot"]["canonical_codex_home"] != str(root / "profile")
            or state["snapshot"]["executable_identity"]["sha256"] != CODEX_SHA256):
        raise ProbeCheckFailure("fixture_profile_ownership_unverified")
    membership = call("profile", "membership", "verify", "integrated")
    if not membership["complete"] or membership["server_key"] != status["server_key"]:
        raise ProbeCheckFailure("fixture_profile_membership_unverified")
    call("profile", "server", "stop", "integrated", "--operator-file", str(operator),
         "--interrupt", "--confirm-server-key", status["server_key"])


def require_fixture_process_absence(observation: dict) -> None:
    if any(candidate["verdict"] != "absent" for candidate in observation["candidates"]):
        raise ProbeCheckFailure("fixture_process_absence_unverified")


class IntegratedResponses(ScriptedResponses):
    """Issue a tool call; accept only its correlated native or checked host return."""

    def prepare(self, marker: str, role: str, *, command: str | None = None,
                expected: bytes | None = None, side: str = "after", denied: bool = False,
                receipt_fault: str | None = None, drift: tuple[Path, bytes] | None = None,
                request_specialist: bool = False) -> None:
        with self.lock:
            if getattr(self, "pending", False):
                raise ProbeCheckFailure("unfinished_fixture_turn")
            if self.steps >= MAX_TURNS:
                raise ProbeCheckFailure("fixture_turn_outside_budget")
            self.pending = True
            self.marker, self.role = marker, role
            self.command, self.expected, self.side, self.denied = command, expected, side, denied
            self.receipt_fault, self.drift = receipt_fault, drift
            self.request_specialist = request_specialist
            self.specialist_run_id = None
            self.issued = False
            self.call_id = f"integrated-call-{self.steps + 1}"

    def respond(self, request: Any) -> list[dict[str, Any]]:
        with self.lock:
            if hasattr(self, "source_check"):
                self.source_check()
            self.posts += 1
            if self.failure or self.posts > MAX_POSTS or not getattr(self, "pending", False):
                raise ProbeCheckFailure("fixture_request_outside_budget")
            if not isinstance(request, dict) or not isinstance(request.get("input"), list):
                raise ProbeCheckFailure("fixture_input_invalid")
            inputs = request["input"]
            if not self.issued:
                text = json.dumps([request.get("instructions"), inputs], ensure_ascii=False)
                if not all(marker in text for marker in (CORE_PREFIX, COMMON, self.role)):
                    raise ProbeCheckFailure("captured_instructions_missing")
                if any(marker in text for marker in AMBIENT):
                    raise ProbeCheckFailure("ambient_instructions_admitted")
                users = [item for item in inputs if isinstance(item, dict)
                         and item.get("type") == "message" and item.get("role") == "user"]
                if PROFILE not in json.dumps(users):
                    raise ProbeCheckFailure("profile_user_guidance_missing")
                if not users or self.marker not in json.dumps(users[-1]):
                    raise ProbeCheckFailure("fixture_task_mismatch")
                self.issued = True
                if self.request_specialist:
                    arguments = {"operation": "request_specialist", "role_ref": "reviewer",
                                 "objective": "Validate the captured child configuration",
                                 "expected_output": ["An initialized read-only Specialist"],
                                 "requested_access": "read_only", "deadline_seconds": 60}
                    name = "dolgorae_orchestration"
                elif self.command is None:
                    arguments = {"side": self.side, "path": "file.txt", "byte_offset": 0,
                                 "byte_length": len(self.expected or b"")}
                    name = "dolgorae_candidate_read"
                else:
                    arguments = {"cmd": self.command, "yield_time_ms": 10000}
                    name = "exec_command"
                item = {"type": "function_call", "call_id": self.call_id, "name": name,
                        "arguments": json.dumps(arguments)}
            else:
                returns = [item for item in inputs if isinstance(item, dict)
                           and item.get("type") == "function_call_output"
                           and item.get("call_id") == self.call_id]
                if len(returns) != 1:
                    raise ProbeCheckFailure("fixture_return_correlation_invalid")
                output = returns[0].get("output")
                if self.request_specialist:
                    if isinstance(output, list) and len(output) == 1:
                        output = output[0].get("text")
                    if not isinstance(output, str) or len(output) > 65536:
                        raise ProbeCheckFailure("primary_tool_return_missing")
                    result = json.loads(output)
                    if not validator(PROTOCOL, "dolgorae-orchestration-tool-v1.schema.json",
                                     "#/$defs/request_specialist_result").is_valid(result):
                        raise ProbeCheckFailure("primary_tool_return_invalid")
                    operation = result["specialist_operation"]
                    if (operation["state"] != "ready" or operation["specialist_run_id"] is None
                            or operation["role_ref"] != "reviewer" or operation["reused"]
                            or operation["approval_request_id"] is not None
                            or operation["safe_error_code"] is not None):
                        raise ProbeCheckFailure("primary_child_request_unverified")
                    self.specialist_run_id = operation["specialist_run_id"]
                    answer = "Done."
                elif self.command is None:
                    if isinstance(output, list) and len(output) == 1:
                        output = output[0].get("text")
                    if not isinstance(output, str) or len(output) > 65536:
                        raise ProbeCheckFailure("candidate_return_missing")
                    result = json.loads(output)
                    if isinstance(result, dict) and result.get("error") is not None:
                        raise ProbeCheckFailure("candidate_read_refused")
                    receipt = result.get("receipt", {})
                    if (base64.b64decode(result.get("content", ""), validate=True) != self.expected
                            or receipt.get("path") != "file.txt" or receipt.get("side") != self.side
                            or receipt.get("byte_offset") != 0
                            or receipt.get("byte_length") != len(self.expected or b"")):
                        raise ProbeCheckFailure("candidate_return_substituted")
                    verdict = {"summary": "Checked the admitted candidate", "findings": [],
                               "criterion_assessments": [{"criterion_id": "C-1", "status": "met",
                                   "explanation": "The host read returned the expected candidate bytes",
                                   "evidence": [{"basis": "candidate", "description": "Checked file bytes",
                                       "path": "file.txt", "line_start": 1, "line_end": 1,
                                       "context_id": None, "read_receipt_id": receipt["receipt_id"]}],
                                   "remaining_gap": None}], "evidence_limits": [],
                               "overall_assessment": "requirements_met"}
                    evidence = verdict["criterion_assessments"][0]["evidence"][0]
                    if self.receipt_fault == "forged":
                        evidence["read_receipt_id"] = "018f0000-0000-7000-8000-ffffffffffff"
                    elif self.receipt_fault == "missing":
                        del evidence["read_receipt_id"]
                    if self.drift is not None:
                        path, content = self.drift
                        overwrite_fixture_file(path, content)
                    answer = json.dumps(verdict)
                else:
                    code, text = terminal_output(output)
                    if self.denied:
                        if code == 0:
                            raise ProbeCheckFailure("readonly_write_was_admitted")
                    elif code != 0 or text.encode() != (self.expected or b""):
                        raise ProbeCheckFailure("native_tool_return_unverified")
                    answer = "Done."
                self.pending = False
                self.steps += 1
                item = {"type": "message", "role": "assistant", "id": f"integrated-message-{self.steps}",
                        "content": [{"type": "output_text", "text": answer}]}
            identity = f"integrated-response-{self.posts}"
            return [{"type": "response.created", "response": {"id": identity}},
                    {"type": "response.output_item.done", "item": item},
                    {"type": "response.completed", "response": {"id": identity, "usage": {
                        "input_tokens": 0, "input_tokens_details": None, "output_tokens": 0,
                        "output_tokens_details": None, "total_tokens": 0}}}]


def review_request(kind: str, marker: str, revision: str | None = None) -> dict[str, Any]:
    target = {"kind": kind}
    if revision is not None:
        target["revision"] = revision
    return {"schema": "dolgorae-specialist-review-request/v4", "operation": "review_target",
            "target": target, "purpose": "completion", "brief": marker,
            "contexts": [], "criteria": [{"id": "C-1", "statement": "Inspect the expected file bytes",
                                           "source_context_ids": []}],
            "expected_output": "structured_review_v4", "deadline_seconds": 60}


def campaign(binary: Path, codex: Path, caller_driver: Path, *, ordinary_close_only: bool = False) -> dict[str, Any]:
    if os.environ.get(OPT_IN) != "1":
        raise ValueError("explicit integrated native opt-in required")
    if executable_identity(codex)["sha256"] != CODEX_SHA256:
        raise ValueError("integrated acceptance requires the pinned executable")
    verify_core_prefix()
    root = Path(tempfile.mkdtemp(prefix="dia-", dir=os.environ["TMPDIR"])).resolve()
    root.chmod(0o700)
    retained_binary = root / "fixture-dolgorae"
    shutil.copyfile(binary, retained_binary)
    retained_binary.chmod(0o700)
    binary = retained_binary
    fixture: IntegratedResponses | None = None
    passed = False
    runs: list[tuple[Path, str, Path]] = []
    try:
        for name in ("home", "tmp", "config", "cache", "profile", "workspace", "git-workspace"):
            (root / name).mkdir(mode=0o700)
        environment = isolated_environment(root)
        environment.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null")
        checked_codex(codex, f"codex-cli {CODEX_VERSION}", environment)
        fixture = IntegratedResponses()
        fixture.source_check = lambda: assert_no_source_isolation(root / "home")
        private_file(root / "profile/config.toml", b'model = "gpt-5.6-luna"\nmodel_reasoning_effort = "low"\n'
                     + fixture_config(fixture.base_url)
                     + b"\n[features]\nplugins = false\n")
        private_file(root / "profile/AGENTS.md", (PROFILE + "\n").encode())
        private_file(root / "AGENTS.md", (AMBIENT[1] + "\n").encode())
        workspace = root / "workspace"
        private_file(workspace / "AGENTS.md", (AMBIENT[0] + "\n").encode())
        private_file(workspace / "file.txt", b"workspace\n")
        private_file(root / "git-workspace/AGENTS.md", (AMBIENT[0] + "\n").encode())

        def call(*args: str, request: dict | None = None, expected_error: str | None = None,
                 timeout: int = 120, pass_fds: tuple[int, ...] = ()) -> dict:
            result = subprocess.run([str(binary), *args], env=environment,
                                    input=json.dumps(request) if request is not None else None,
                                    text=True, capture_output=True, timeout=timeout, pass_fds=pass_fds)
            value = json.loads(result.stdout)
            assert_no_source_isolation(root / "home")
            assert_valid(value, validator(PROTOCOL, f"dolgorae-machine-v{value['schema_version']}.schema.json"),
                         "production CLI envelope")
            diagnostic = {"command": args[:2], "code": value.get("error", {}).get("code"),
                "expected_code": expected_error,
                "fixture_turn": fixture.steps + 1 if getattr(fixture, "pending", False) else None,
                "fixture_stage": getattr(fixture, "marker", None), "fixture_failure": fixture.failure}
            if value.get("error", {}).get("code") == "DEDICATED_SERVER_START_FAILED":
                diagnostic["startup_reason"] = value["error"]["details"]["reason"]
            if result.stderr or (expected_error is None and (result.returncode or not value["ok"])):
                raise ProbeCheckFailure("production_cli_failed", diagnostic)
            if expected_error is not None:
                if result.returncode == 0 or value.get("error", {}).get("code") != expected_error:
                    raise ProbeCheckFailure("production_failure_guard_invalid", diagnostic)
                return value
            return value["data"]

        subprocess.run(["/usr/bin/git", "-C", str(root / "git-workspace"), "init", "-q"],
                       env=environment, check=True, capture_output=True, timeout=10)
        call("init", "--non-git", str(workspace))
        call("init", str(root / "git-workspace"))
        overwrite_fixture_file(root / "home/.dolgorae/home/AGENTS.md", (COMMON + "\n").encode())
        overwrite_fixture_file(root / "home/.dolgorae/roles/general/AGENTS.md", (GENERAL + "\n").encode())
        overwrite_fixture_file(root / "home/.dolgorae/roles/reviewer/AGENTS.md", (REVIEWER + "\n").encode())
        operator = root / "operator"
        call("operator", "credential", "initialize", "--output", str(operator))
        owner = root / "controller"
        call("controller", "credential", "create", "--kind", "automation",
             "--instance-id", "integrated-fixture", "--output", str(owner))
        call("profile", "add", "integrated", "--codex-home", str(root / "profile"),
             "--native-subagents", "enabled", "--env", "PATH=/usr/bin:/bin",
             "--env", "LANG=C", "--env", "LC_ALL=C", "--", str(codex))

        def start(key: str, access: str | None = None, error: str | None = None) -> dict:
            args = ["run", "start", "--workspace", str(workspace), "--controller-file", str(owner),
                    "--profile", "integrated", "--control-mode", "managed-agent",
                    "--purpose", "implementation", "--idempotency-key", key]
            if access:
                args.extend(["--access", access])
            return call(*args, expected_error=error)

        writer = start("default-writer")
        profile = call("profile", "server", "status", "integrated")
        features = profile["state"]["snapshot"]["process_static_configuration"]["features"]
        if (not isinstance(features, dict) or features.get("plugins") is not False
                or (root / "profile/.tmp/plugins").exists()):
            raise ProbeCheckFailure("fixture_plugin_startup_isolation_unverified")
        if writer["requested_access"] != "writable" or writer["selected_role"] != "general" \
                or writer["effective_access"] != "write" or writer["policy_verification"] != "verified":
            raise ProbeCheckFailure("startup_defaults_not_verified")
        writer_id = writer["run_id"]
        observed_writer = call("workspace", "writer", "status", "--workspace", str(workspace))
        if observed_writer["writer_run_id"] != writer_id or observed_writer["authority_state"] != "active":
            raise ProbeCheckFailure("modern_holder_writer_status_unreadable")
        if ordinary_close_only:
            foreign = root / "foreign-controller"
            call("controller", "credential", "create", "--kind", "automation",
                 "--instance-id", "foreign-close", "--output", str(foreign))
            control = ["--workspace", str(workspace), "--controller-file", str(owner)]
            before = call("run", "status", writer_id, "--workspace", str(workspace))
            call("run", "close", writer_id, "--workspace", str(workspace),
                 "--controller-file", str(foreign), expected_error="CONTROLLER_MISMATCH")
            after = call("run", "status", writer_id, "--workspace", str(workspace))
            if after != before:
                raise ProbeCheckFailure("foreign_close_mutated_writable_run")

            for interrupt in (False, True):
                if interrupt:
                    writer = start("idle-writer-interrupt")
                    writer_id = writer["run_id"]
                closed = call("run", "close", writer_id, *control,
                              *(["--interrupt"] if interrupt else []))
                observed = [closed, call("run", "status", writer_id,
                                        "--workspace", str(workspace))]
                observed.append(call("run", "close", writer_id, *control))
                for verb in ("recover", "reconcile"):
                    refused = call("run", verb, writer_id, *control,
                                   expected_error="RUN_STATE_CONFLICT")
                    if refused["error"]["details"].get("state") != "closed":
                        raise ProbeCheckFailure("closed_recovery_projection_unreadable")
                observed.append(call("run", "status", writer_id, "--workspace", str(workspace)))
                if any(value["run_id"] != writer_id
                       or value["writer_state"] != "none" or value["effective_access"] == "write"
                       for value in observed):
                    raise ProbeCheckFailure("ordinary_writable_close_projection_unverified")
                listed = call("run", "list", "--workspace", str(workspace))["items"]
                if not any(item["run_id"] == writer_id and item["writer_state"] == "none"
                           and item["effective_access"] != "write" for item in listed):
                    raise ProbeCheckFailure("closed_ordinary_run_list_unreadable")
                writer_state = call("workspace", "writer", "status", "--workspace", str(workspace))
                if writer_state["authority_state"] != "none" or writer_state["writer_run_id"] is not None:
                    raise ProbeCheckFailure("ordinary_writable_close_retained_writer")
            if fixture.posts != 0 or fixture.failure is not None:
                raise ProbeCheckFailure("ordinary_close_started_unapproved_turn")
            stop_fixture_profile(call, root, operator)
            cleanup_removed_root(binary, root, allow_existing_root=True)
            require_fixture_process_absence(call("runtime", "orphan", "inspect",
                                                "--owner-root-under", str(root)))
            passed = True
            return {"schema": "dolgorae.native-ordinary-writable-close/v1", "passed": True,
                    "binary_sha256": executable_identity(binary)["sha256"],
                    "codex_sha256": CODEX_SHA256, "native_turns": 0, "loopback_posts": 0,
                    "normal_idle_close": True, "idle_interrupt_close": True,
                    "foreign_controller_rejected_without_mutation": True,
                    "closed_status_and_close_replay": True,
                    "closed_run_list_contract": True,
                    "closed_recovery_reconciliation_refused": True,
                    "writer_released": True, "owned_operator_cleanup": True,
                    "owned_process_absence_verified": True}

        runs.append((workspace, writer_id, owner))
        start("busy-writer", error="WRITER_BUSY")
        reader = start("explicit-reader", "read-only")
        reader_id = reader["run_id"]
        runs.append((workspace, reader_id, owner))
        thread = reader["thread_id"]
        control = ["--workspace", str(workspace), "--controller-file", str(owner)]
        fixture.prepare("readonly-native-denial", GENERAL,
                        command="printf forbidden > denied.txt", denied=True)
        call("run", "send", reader_id, *control, "--message", "readonly-native-denial",
             "--idempotency-key", "readonly-denial", "--timeout", TURN_TIMEOUT)
        if (workspace / "denied.txt").exists():
            raise ProbeCheckFailure("readonly_workspace_mutated")
        call("run", "release-write", writer_id, *control)
        acquired = call("run", "acquire-write", reader_id, *control)
        if acquired["thread_id"] != thread or acquired["effective_access"] != "write":
            raise ProbeCheckFailure("same_thread_write_acquisition_unverified")
        fixture.prepare("writable-native-write", GENERAL,
                        command="printf accepted > written.txt; cat written.txt", expected=b"accepted")
        call("run", "send", reader_id, *control, "--write", "--message", "writable-native-write",
             "--idempotency-key", "native-write", "--timeout", TURN_TIMEOUT)
        if (workspace / "written.txt").read_bytes() != b"accepted":
            raise ProbeCheckFailure("native_workspace_write_unverified")
        released = call("run", "release-write", reader_id, *control)
        if released["thread_id"] != thread or released["effective_access"] != "read":
            raise ProbeCheckFailure("same_thread_read_release_unverified")
        overwrite_fixture_file(root / "home/.dolgorae/roles/general/AGENTS.md", b"changed-authoring-role\n")
        replay = start("explicit-reader", "read-only")
        if replay["run_id"] != reader_id or replay["instruction_identity"] != reader["instruction_identity"]:
            raise ProbeCheckFailure("accepted_start_rebound_to_authoring")
        fixture.prepare("captured-role-after-edit", GENERAL, command="cat file.txt", expected=b"workspace\n")
        call("run", "send", reader_id, *control, "--message", "captured-role-after-edit",
             "--idempotency-key", "captured-read", "--timeout", TURN_TIMEOUT)
        for selected, run_id, credential in runs:
            call("run", "close", run_id, "--workspace", str(selected), "--controller-file", str(credential))
        runs.clear()

        git_root = root / "git-workspace"
        def git(*args: str) -> str:
            return subprocess.run(["/usr/bin/git", "-C", str(git_root), *args], env=environment,
                                  check=True, capture_output=True, text=True, timeout=10).stdout.strip()
        git("init", "-q")
        git("config", "user.name", "Integrated native fixture")
        git("config", "user.email", "fixture@example.invalid")
        private_file(git_root / "file.txt", b"first\n")
        git("add", "file.txt")
        git("commit", "-qm", "first")
        first = git("rev-parse", "HEAD")
        overwrite_fixture_file(git_root / "file.txt", b"second\n")
        git("add", "file.txt")
        git("commit", "-qm", "second")
        second = git("rev-parse", "HEAD")
        overwrite_fixture_file(git_root / "file.txt", b"indexed\n")
        git("add", "file.txt")
        overwrite_fixture_file(git_root / "file.txt", b"worktree\n")
        cases = [(workspace, "workspace", None, "current", b"workspace\n"),
                 (git_root, "staged", None, "after", b"indexed\n"),
                 (git_root, "dirty", None, "after", b"worktree\n"),
                 (git_root, "head", None, "current", b"second\n"),
                 (git_root, "commit", first, "after", b"first\n"),
                 (git_root, "range", f"{first}..{second}", "after", b"second\n"),
                 (git_root, "range", f"{first}...{second}", "after", b"second\n")]
        for index, (selected, kind, revision, side, expected) in enumerate(cases):
            marker = f"checked-review-{index}"
            fixture.prepare(marker, REVIEWER, expected=expected, side=side)
            result = call("specialist", "review", "--workspace", str(selected), "--profile", "integrated",
                          "--request-stdin", "--format", "json",
                          request=review_request(kind, marker, revision))
            if result["verdict"]["overall_assessment"] != "requirements_met" \
                    or result["source_assurance"]["candidate_provenance"] != "verified" \
                    or result.get("workflow_issued_source_mutation") is not False:
                raise ProbeCheckFailure("checked_review_result_unverified")

        def engage(request: dict, child: Path | None = None) -> dict:
            with tempfile.TemporaryFile(dir=root) as descriptor:
                os.fchmod(descriptor.fileno(), 0o600)
                descriptor.write(json.dumps(request).encode())
                descriptor.seek(0)
                args = ["engagement", "call", "--workspace", str(workspace),
                        "--controller-file", str(owner), "--request-fd", str(descriptor.fileno())]
                if child is not None:
                    args.extend(["--new-controller-file", str(child)])
                return call(*args, pass_fds=(descriptor.fileno(),))

        opened = engage({"operation": "open_external_engagement", "external_controller_ref": {
            "namespace": "fixture", "kind": "workflow", "id": "integrated"},
            "label": "Bounded integrated fixture", "idempotency_key": "integrated-engagement"})
        engagement_id = opened["engagement_id"]
        child = root / "reviewer-controller"
        call("controller", "credential", "create", "--kind", "automation",
             "--instance-id", "integrated-reviewer", "--output", str(child))
        hire = json.loads((PROTOCOL / "examples/external-engagement-v4-hire.valid.json").read_text())
        hire.update(engagement_id=engagement_id, role_ref="fixture-reviewer", idempotency_key="reusable-reviewer")
        hire["agent_configuration"].update(selected_profile="integrated", model="gpt-5.6-luna", default_effort="low")
        hired = engage(hire, child)
        run_id = hired["specialist_run_id"]
        if hired["state"] != "ready":
            raise ProbeCheckFailure("reusable_reviewer_start_unverified")
        original_thread = call("run", "status", run_id, "--workspace", str(workspace))["thread_id"]
        overwrite_fixture_file(root / "home/.dolgorae/roles/reviewer/AGENTS.md", b"changed-reviewer-authoring\n")
        if engage(hire, child)["specialist_run_id"] != run_id:
            raise ProbeCheckFailure("reusable_hire_replay_changed_identity")
        receipts = []
        for index in range(2):
            marker = f"reusable-review-{index}"
            request = review_request("workspace", marker)
            task = {name: request[name] for name in ("purpose", "brief", "contexts", "criteria", "expected_output")}
            fixture.prepare(marker, REVIEWER, expected=b"workspace\n", side="current")
            assigned = engage({"schema": "dolgorae-external-specialist-facade/v4",
                "operation": "assign_external_specialist_task", "engagement_id": engagement_id,
                "specialist_run_id": run_id, "external_request_ref": {
                    "namespace": "fixture", "kind": "review", "id": str(index)},
                "task": task, "target": {"kind": "workspace"}, "execution_intent": "read_only",
                "deadline_seconds": 60, "idempotency_key": marker})
            waited = engage({"operation": "await_external_specialist_tasks", "engagement_id": engagement_id,
                "task_ids": [assigned["task_id"]], "return_when": "all", "transport_wait_seconds": 60})
            summary = next(task for task in waited["tasks"] if task["task_id"] == assigned["task_id"])
            if summary["state"] not in ("completed_not_delivered", "delivered") \
                    or summary["result"]["overall_assessment"] != "requirements_met":
                raise ProbeCheckFailure("reusable_checked_task_unverified")
            receipts.append(summary["result"]["criterion_assessments"][0]["evidence"][0]["read_receipt_id"])
            if call("run", "status", run_id, "--workspace", str(workspace))["thread_id"] != original_thread:
                raise ProbeCheckFailure("reusable_reviewer_changed_thread")
        if len(set(receipts)) != 2:
            raise ProbeCheckFailure("reusable_task_receipts_aliased")
        collected = engage({"operation": "collect_external_specialist_results", "engagement_id": engagement_id,
                           "after_sequence": 0, "limit": 16})
        if len(collected["tasks"]) != 2:
            raise ProbeCheckFailure("reusable_task_collection_missing")
        engage({"operation": "release_external_specialist", "engagement_id": engagement_id,
            "specialist_run_id": run_id, "reason": "Fixture complete", "idempotency_key": "release-reviewer"})
        engage({"operation": "close_external_engagement", "engagement_id": engagement_id,
                "mode": "complete", "reason": "Fixture complete", "idempotency_key": "complete-engagement"})

        overwrite_fixture_file(root / "home/.dolgorae/roles/reviewer/AGENTS.md", (REVIEWER + "\n").encode())
        for fault, code in (("native", "REVIEW_CANDIDATE_PROVENANCE_INVALID"),
                            ("forged", "REVIEW_CANDIDATE_PROVENANCE_INVALID"),
                            ("missing", "REVIEW_OUTPUT_INVALID"),
                            ("drift", "REVIEW_TARGET_DRIFTED")):
            marker = f"adversarial-review-{fault}"
            fixture.prepare(marker, REVIEWER, expected=b"workspace\n", side="current",
                command="cat file.txt" if fault == "native" else None,
                receipt_fault=fault if fault in ("forged", "missing") else None,
                drift=(workspace / "file.txt", b"changed\n") if fault == "drift" else None)
            call("specialist", "review", "--workspace", str(workspace), "--profile", "integrated",
                 "--request-stdin", "--format", "json", request=review_request("workspace", marker),
                 expected_error=code)
            overwrite_fixture_file(workspace / "file.txt", b"workspace\n")

        overwrite_fixture_file(root / "home/.dolgorae/roles/general/AGENTS.md", (GENERAL + "\n").encode())
        policy = json.loads((PROTOCOL / "examples/workspace-policy-input-v3.valid.json").read_text())
        policy.update(policy_name="integrated-session", approval_policy="fully_delegated")
        configuration = policy["roles"][0]["agent_configuration"]
        configuration.update(selected_profile="integrated", model="gpt-5.6-luna", default_effort="low")
        configuration.pop("global_profile_binding_sha256", None)
        policy_path = root / "policy.json"
        private_file(policy_path, json.dumps(policy).encode())
        call("specialist", "policy", "add", policy["policy_name"], "--workspace", str(workspace),
             "--file", str(policy_path))
        primary_controller = root / "primary-controller"
        call("controller", "credential", "create", "--kind", "interactive-client",
             "--instance-id", "integrated-primary", "--orchestration-policy", policy["policy_name"],
             "--output", str(primary_controller))
        primary_args = ["run", "start", "--workspace", str(workspace), "--controller-file", str(primary_controller),
            "--profile", "integrated", "--control-mode", "direct-interactive", "--purpose", "interactive",
            "--idempotency-key", "integrated-primary"]
        primary = call(*primary_args)
        view = call("workspace", "inspect", "--workspace", str(workspace))
        state_root = root / "home/.dolgorae/workspaces" / view["workspace_id"]
        database = state_root / "orchestration/orchestration.sqlite3"
        with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as connection:
            capture = connection.execute("SELECT receipt_json FROM caller_policy_session_acceptances WHERE root_run_id=?",
                                         (primary["run_id"],)).fetchone()
        if capture is None:
            raise ProbeCheckFailure("primary_policy_capture_missing")
        captures = json.loads(capture[0])["captures"]
        role_ref = policy["roles"][0]["role_ref"]
        accepted_child = captures["configurations"][role_ref]
        fixture.prepare("native-primary-child-request", GENERAL, request_specialist=True)
        call("run", "send", primary["run_id"], "--workspace", str(workspace),
             "--controller-file", str(primary_controller), "--message", "native-primary-child-request",
             "--idempotency-key", "native-primary-child", "--timeout", TURN_TIMEOUT)
        child_path = state_root / "runs" / str(fixture.specialist_run_id) / "manifest.json"
        if not child_path.is_file():
            raise ProbeCheckFailure("broker_child_manifest_missing")
        child = json.loads(child_path.read_text())
        binding = child.get("aggregate_binding") or {}
        if (child["run_id"] != fixture.specialist_run_id
                or binding.get("aggregate_id") != primary["run_id"]
                or binding.get("aggregate_kind") != "orchestrated_session"
                or binding.get("member_kind") != "specialist"
                or binding.get("role_reference") != role_ref
                or not same_captured_configuration(accepted_child, child["agent_configuration"])):
            raise ProbeCheckFailure("broker_child_did_not_use_captured_configuration")
        (root / "home/.dolgorae/roles/reviewer/AGENTS.md").unlink()
        (root / "home/.dolgorae/home/AGENTS.md").unlink()
        call("specialist", "policy", "remove", policy["policy_name"], "--workspace", str(workspace))
        if call(*primary_args)["run_id"] != primary["run_id"]:
            raise ProbeCheckFailure("accepted_primary_replay_reopened_sources")
        primary_state = state_root / "runs" / primary["run_id"] / "state.json"
        before_refusal = primary_state.read_bytes()
        call("run", "close", primary["run_id"], "--workspace", str(workspace),
             "--controller-file", str(owner), expected_error="CONTROLLER_MISMATCH")
        if primary_state.read_bytes() != before_refusal:
            raise ProbeCheckFailure("unauthorized_close_changed_primary")
        closed = call("run", "close", primary["run_id"], "--workspace", str(workspace),
                      "--controller-file", str(primary_controller))
        close_operation_id = require_closed_session(closed, primary["run_id"])
        unauthorized = call("run", "close", primary["run_id"], "--workspace", str(workspace),
                            "--controller-file", str(owner), expected_error="CONTROLLER_MISMATCH")
        if "operation_id" in unauthorized["error"]["details"]:
            raise ProbeCheckFailure("unauthorized_close_exposed_retained_intent")
        child_state = json.loads(child_path.with_name("state.json").read_bytes())
        if child_state["lifecycle"] != "closed":
            raise ProbeCheckFailure("aggregate_child_not_closed")
        observed_writer = call("workspace", "writer", "status", "--workspace", str(workspace))
        if observed_writer["authority_state"] != "none" or observed_writer["writer_run_id"] is not None:
            raise ProbeCheckFailure("aggregate_writer_not_released")
        with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as connection:
            retained = connection.execute(
                "SELECT operation_id,interrupt,progress FROM orchestrated_session_closes WHERE session_id=?",
                (primary["run_id"],)).fetchone()
            members = connection.execute(
                "SELECT membership_state FROM brokered_members WHERE session_id=?",
                (primary["run_id"],)).fetchall()
            status = connection.execute("SELECT status FROM orchestrated_sessions WHERE session_id=?",
                                        (primary["run_id"],)).fetchone()
        if (retained != (close_operation_id, 0, "completed") or status != ("completed",)
                or not members or any(member != ("retired",) for member in members)):
            raise ProbeCheckFailure("aggregate_durable_close_incomplete")
        for verb in ("status", "close", "recover", "reconcile"):
            observed = call("run", verb, primary["run_id"], "--workspace", str(workspace),
                            "--controller-file", str(primary_controller))
            if require_closed_session(observed, primary["run_id"]) != close_operation_id:
                raise ProbeCheckFailure("aggregate_close_operation_replaced")
        changed_choice = call("run", "close", primary["run_id"], "--workspace", str(workspace),
            "--controller-file", str(primary_controller), "--interrupt", expected_error="RUN_STATE_CONFLICT")
        if changed_choice["error"]["details"]["operation_id"] != close_operation_id:
            raise ProbeCheckFailure("aggregate_interrupt_choice_not_retained")
        private_file(root / "home/.dolgorae/home/AGENTS.md", (COMMON + "\n").encode())
        private_file(root / "home/.dolgorae/roles/reviewer/AGENTS.md", (REVIEWER + "\n").encode())
        call("specialist", "policy", "add", policy["policy_name"], "--workspace", str(workspace),
             "--file", str(policy_path))
        abort_args = [*primary_args[:-1], "integrated-idle-abort"]
        abort_root = call(*abort_args)
        aborted = call("run", "close", abort_root["run_id"], "--workspace", str(workspace),
                       "--controller-file", str(primary_controller), "--interrupt")
        abort_operation = require_closed_session(aborted, abort_root["run_id"], interrupt=True)
        recovered_abort = call("run", "recover", abort_root["run_id"], "--workspace", str(workspace),
                               "--controller-file", str(primary_controller))
        if require_closed_session(recovered_abort, abort_root["run_id"], interrupt=True) != abort_operation:
            raise ProbeCheckFailure("aggregate_abort_operation_replaced")
        call("review-target", "capture", "--workspace", str(workspace), "--kind", "workspace",
             "--backend-kind", "fixture", "--backend-lifecycle-id", "retired",
             "--settlement-owner-file", str(root / "forbidden-owner"), expected_error="SOURCE_ISOLATION_RETIRED")
        if (root / "forbidden-owner").exists():
            raise ProbeCheckFailure("retired_capture_created_carrier")
        if fixture.failure or fixture.pending or fixture.posts != MAX_POSTS or fixture.steps != MAX_TURNS:
            raise ProbeCheckFailure("integrated_turn_budget_mismatch")
        assert_no_source_isolation(root / "home")
        stop_fixture_profile(call, root, operator)
        cleanup_removed_root(binary, root, allow_existing_root=True)
        require_fixture_process_absence(call("runtime", "orphan", "inspect",
                                            "--owner-root-under", str(root)))
        with patch.dict(os.environ, {caller.OPT_IN: "1"}):
            recovery = caller.campaign(caller_driver, codex)
        passed = True
        return {"schema": "dolgorae.native-integrated-acceptance/v1", "passed": True,
                "codex_version": CODEX_VERSION, "executable_sha256": CODEX_SHA256,
                "loopback_posts": fixture.posts, "native_turns": fixture.steps,
                "external_provider_requests": 0, "real_account_or_credentials_used": False,
                "production_cli_defaults": True, "same_thread_access_transitions": True,
                "readonly_native_write_denied": True, "captured_role_replay": True,
                "profile_guidance_observed_as_user_input": True,
                "reusable_review_tasks": 2, "primary_policy_capture_and_replay": True,
                "rejected_review_failures": ["native_read", "forged_receipt", "missing_receipt", "target_drift"],
                "broker_child_accepted_configuration": True,
                "direct_review_scopes": [case[1] for case in cases],
                "copy_and_worktree_creation": False, "caller_service_recovery": recovery,
                "owned_operator_cleanup": True, "owned_process_absence_verified": True,
                "normal_whole_session_close_qualified": True,
                "aggregate_close_replay_and_recovery": True,
                "aggregate_child_retired_and_writer_released": True,
                "idle_writable_primary_abort_and_recovery": True}
    finally:
        if fixture is not None:
            fixture.close()
        if passed:
            shutil.rmtree(root)
        else:
            print(json.dumps({"schema": "dolgorae.native-integrated-acceptance-failure/v1",
                              "owned_fixture_retained": str(root), "passed": False,
                              "loopback_posts": fixture.posts if fixture is not None else 0,
                              "fixture_failure": fixture.failure if fixture is not None else None,
                              "completed_fixture_turns": fixture.steps if fixture is not None else 0}),
                  file=sys.stderr)


def main() -> int:
    if os.environ.get(OPT_IN) != "1":
        print("integrated native acceptance requires explicit opt-in")
        return 2
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--codex", type=Path, required=True)
    parser.add_argument("--caller-driver", type=Path, required=True)
    parser.add_argument("--ordinary-close-only", action="store_true")
    args = parser.parse_args()
    try:
        result = campaign(args.binary.resolve(strict=True), args.codex.resolve(strict=True),
                          args.caller_driver.resolve(strict=True),
                          ordinary_close_only=args.ordinary_close_only)
    except ProbeCheckFailure as error:
        print(json.dumps({"schema": "dolgorae.native-integrated-acceptance-failure/v1",
                          "passed": False, "check": error.kind,
                          "observations": error.observations}))
        return 1
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(json.dumps({"schema": "dolgorae.native-integrated-acceptance-failure/v1",
                          "passed": False, "failure_type": type(error).__name__}))
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
