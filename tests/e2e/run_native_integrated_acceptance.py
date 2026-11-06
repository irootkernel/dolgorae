#!/usr/bin/env python3
"""Opt-in production CLI qualification with isolated state and bounded loopback Turns."""
from __future__ import annotations

import argparse
import base64
import hashlib
import importlib.util
import json
import os
import shutil
import shlex
import sqlite3
import subprocess
import sys
import tempfile
import tarfile
import threading
import time
import uuid
from pathlib import Path
from typing import Any
from unittest.mock import patch

import run_native_caller_pipeline as caller
from orphan_cleanup import cleanup_removed_root
from run_live_instruction_discovery import (
    CORE_PREFIX, ProbeCheckFailure, checked_codex, executable_identity, isolated_environment,
    private_file, verify_core_prefix,
)
from run_native_workspace_policy_fixture import ScriptedResponses, code_call, code_results, fixture_config, terminal_output
from schema_support import assert_valid, validator

OPT_IN = "DOLGORAE_RUN_NATIVE_INTEGRATED_ACCEPTANCE"
CODEX_VERSION = caller.CODEX_VERSION
CODEX_SHA256 = caller.CODEX_SHA256
MAX_TURNS = 17
MAX_POSTS = MAX_TURNS * 2
SUCCESSOR_READ_PAGES = {"complete_result": 2, "final_response": 2, "before": 1, "after": 1, "package": 1,
                        "record_input": 1, "record_outcome": 1, "accept_input": 2, "accept_outcome": 1, "accept_receipt": 3}
# Original reads batch their second pages together. Accept input adds one
# exchange; the original receipt adds two. These are required pages, not slack.
SUCCESSOR_CONTINUATIONS = 1 + 1 + 2
SUCCESSOR_POSTS = 19 + SUCCESSOR_CONTINUATIONS
TURN_TIMEOUT = "60s"
PROTOCOL = Path(__file__).resolve().parents[2] / "docs" / "protocol"
COMMON = "integrated-common-017"
GENERAL = "integrated-general-017"
REVIEWER = "integrated-reviewer-017"
PROFILE = "integrated-profile-user-017"
AMBIENT = ("integrated-project-forbidden-017", "integrated-ancestor-forbidden-017")
COLLABORATION_RESULT = ("TASK027 large result prefix: 한글.\n"
                        + "한글-0123456789abcdef\n" * 4096
                        + "TASK027 large result tail: 끝.\n")
COLLABORATION_PAGE_LIMIT = 1030


def successor_aquarium_sources(root: Path, archive: Path, *, current_roles: bool = False) -> None:
    """Retain only the selected instruction/helper inputs; never execute on extraction."""
    archive_digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    if archive_digest != "542a7836dee2d412b06f6c78e16712124cce8f89c14413d943c1bd3fab5454e9":
        raise ProbeCheckFailure("selected_aquarium_archive_changed")
    prefix = "plugins/aquarium/"
    names = ["references/ordinary-agent-orchestration.md",
             "references/formal-agent-orchestration-v1.md", "references/executor-guidance.md",
             "skills/goal-evaluator/scripts/prepare_assignment.py",
             "skills/goal-evaluator/scripts/render_dispatch.py",
             "skills/goal-evaluator/scripts/ordinary_assignment.py"]
    if current_roles:
        names.append("skills/goal-evaluator/SKILL.md")
    source = root / "aquarium-source"
    source.mkdir(mode=0o700)
    identities = []
    with tarfile.open(archive) as selected:
        for name in names:
            member = selected.getmember(prefix + name)
            if not member.isfile():
                raise ProbeCheckFailure("selected_aquarium_input_not_regular")
            body = selected.extractfile(member).read()
            destination = source / name
            destination.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            private_file(destination, body)
            identities.append({"member": member.name, "sha256": hashlib.sha256(body).hexdigest()})
    private_file(root / "aquarium-source.json", json.dumps({
        "selection": "db132845624c24bb4d0a889503b35a2a59b2571d",
        "archive_sha256": archive_digest, "files": identities, "current_ordinary_roles": current_roles}).encode())


def prepare_successor_consumers(root: Path, *, adapter_id: str = "dolgorae.managed/1") -> None:
    """Called by the existing Rust fixture before managed assignment admission."""
    from run_native_production_host import podway_fixture
    inputs = json.loads((root / "consumer-inputs.json").read_text())
    facts = json.loads((root / "managed-consumer-setup.json").read_text())
    podway_fixture(root, Path(inputs["podway"]), inputs["podway_sha256"], facts["candidate"],
                   dict(os.environ), time.monotonic() + 30,
                   adapter_id=adapter_id, adapter_version=facts["adapter_version"])
    if inputs.get("producer_only"):
        return
    source = root / "aquarium-source"
    scripts = source / "skills/goal-evaluator/scripts"
    modules = []
    previous = sys.dont_write_bytecode
    sys.dont_write_bytecode = True
    try:
        for name in ("prepare_assignment", "render_dispatch"):
            spec = importlib.util.spec_from_file_location(name, scripts / (name + ".py"))
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            modules.append(module)
        connection = json.loads((root / "connection.json").read_text())
        procedure = {key: connection[key] for key in ("workspace", "session", "attempt", "placement")}
        evidence = [{"path": str(root / "workspace/file.txt"), "basis": "original workspace bytes"}]
        gaps = [{"owner": "independent evaluator", "obligation": "Assess the delivered candidate; no goal pass is supplied by this fixture."}]
        ordinary = {"schema": "aquarium.assignment/v3", "role": "executor",
            "work": {"objective": "Perform the authorized second native write", "sources": "references/ordinary-agent-orchestration.md"},
            "target": {"workspace": str(root / "workspace"), "included": ["file.txt"], "excluded": ["all other paths"]},
            "authority": {"allowed": ["Write file.txt in the isolated fixture"], "excluded": ["Git history or index mutation", "workflow acceptance"]},
            "procedure": None, "native": {"harness": "codex", "agent": "unknown until actual spawn", "lifecycle": "not-dispatched"},
            "evidence": evidence, "gaps": gaps, "purpose": "Bounded ordinary delivery",
            "instructions": (source / "references/ordinary-agent-orchestration.md").read_text(),
            "deliverables": ["Actual changed file and ordinary executor handoff"],
            "success_criteria": ["Return actual evidence and remaining gaps without formal receipt requirements"], "checkpoint": "delivery"}
        prepared = modules[0].prepare(source, ordinary)
        dispatch = modules[1].render_dispatch(prepared)
        private_file(root / "ordinary-assignment.json", json.dumps(prepared, ensure_ascii=False).encode())
        private_file(root / "ordinary-dispatch.txt", dispatch["dispatch_text"].encode())
        formal = {"schema": "aquarium.assignment-request/v1",
            "role": {"path": "references/executor-guidance.md", "dependencies": ["references/formal-agent-orchestration-v1.md"]},
            "node": {"objective": "Perform the authorized first native write", "purpose": "Formal Worker delivery",
                "instructions": "Return the actual result and gaps. Primary must inspect original content, explicitly Record/Accept, then separately complete the Action. No Judge or goal pass is implied.",
                "canonical_sources": ["references/formal-agent-orchestration-v1.md"],
                "context": {"work_identity": "native-managed-writer", "goal_ref": "not-applicable:no-Podway-goal", "goal_revision": "not-applicable",
                    "target": str(root / "workspace/file.txt"), "base": facts["base_commit"],
                    "procedure": procedure["session"], "node": procedure["placement"]},
                "selected_evidence": [], "permitted_effects": {"allowed": ["Write file.txt in the isolated fixture"], "excluded": ["workflow acceptance"]},
                "deliverables": ["Complete native write result"], "success_criteria": ["Preserve actual native bindings, original evidence and remaining gaps"]}}
        prepared = modules[0].prepare(source, formal)
        dispatch = modules[1].render_dispatch(prepared)
        private_file(root / "formal-preparation.json", json.dumps(prepared, ensure_ascii=False).encode())
        private_file(root / "formal-dispatch.txt", dispatch["dispatch_text"].encode())
    finally:
        sys.dont_write_bytecode = previous


def successor_public(root: Path, *args: str, payload: dict | None = None, expected_error: str | None = None) -> dict:
    connection = json.loads((root / "connection.json").read_text())
    executable = Path(connection["executable"])
    if "sha256:" + hashlib.sha256(executable.read_bytes()).hexdigest() != connection["executable_sha256"]:
        raise ProbeCheckFailure("selected_podway_executable_changed")
    argv = [str(executable), "--contract-generation", "1", "--worktree", connection["worktree"], "--json", *args]
    case_input = root / "input.json"
    unbounded = case_input.exists() and json.loads(case_input.read_text()).get("ordinary_unbounded", False)
    result = subprocess.run(argv, cwd=connection["worktree"], env=isolated_environment(root),
                            input=json.dumps(payload, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode() if payload is not None else None,
                            capture_output=True, timeout=None if unbounded else 10)
    with (root / "consumer-public.jsonl").open("a") as log:
        log.write(json.dumps({"argv": argv, "exit": result.returncode,
            "stdout": result.stdout.decode(), "stderr": result.stderr.decode()}) + "\n")
    value = json.loads(result.stdout)
    if expected_error is not None:
        if result.returncode == 0 or value.get("schema") != "podway.error/v2" or value.get("code") != expected_error:
            raise ProbeCheckFailure("selected_public_consumer_wrong_refusal", {"expected": expected_error, "actual": value})
    elif result.returncode:
        raise ProbeCheckFailure("selected_public_consumer_read_or_completion_failed")
    return value


def finish_successor_consumer(root: Path) -> None:
    """A separate public Action completion after the Primary's delivery decision."""
    accepted = json.loads((root / "consumer-prepared-accept.json").read_text())
    connection = json.loads((root / "connection.json").read_text())
    receipt = successor_public(root, "delegation", "receipt", "--session", connection["session"], "--attempt", connection["attempt"])
    if receipt["result"]["applicability"] != "accepted" or receipt["result"]["receipt"]["result_digest"] != accepted["result_digest"]:
        raise ProbeCheckFailure("formal_receipt_not_current_before_separate_completion")
    before = successor_public(root, "observe")
    if before["result"]["status"]["session"]["lifecycle"] != "running":
        raise ProbeCheckFailure("acceptance_already_completed_action")
    completed = successor_public(root, "complete", "--idempotency-key", "primary-action-complete")
    after = successor_public(root, "observe")
    if after["result"]["status"]["session"]["lifecycle"] != "completed":
        raise ProbeCheckFailure("separate_action_completion_unproved")
    private_file(root / "consumer-completion.json", json.dumps({"before": before, "complete": completed,
        "after": after, "semantic_goal_pass": False}).encode())


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


def accepted_profile_state(root: Path, workspace_id: str, run_id: str) -> dict:
    manifest=json.loads((root / "home/.dolgorae/workspaces" / workspace_id / "runs" / run_id / "manifest.json").read_text())
    binding=manifest["global_profile_binding"]
    state=json.loads((root / "home/.dolgorae/profiles" / binding["server_key"] / "state.json").read_text())
    if (state["server_key"]!=binding["server_key"] or state["snapshot"]!=binding["launch_snapshot"]
            or state["snapshot"]["canonical_codex_home"]!=str(root / "profile")
            or state["snapshot"]["executable_identity"]["sha256"]!=CODEX_SHA256):
        raise ProbeCheckFailure("accepted_fixture_profile_binding_changed")
    return state


def stop_fixture_profile(driver: Path, root: Path, environment: dict, *, binary: Path | None = None) -> None:
    selected = {} if binary is None else {"DOLGORAE_NATIVE_ORDINARY_BIN": str(binary)}
    result=subprocess.run([str(driver),"--retire-profiles",str(root)],env={**environment,caller.OPT_IN:"1",**selected},capture_output=True,text=True,timeout=None if binary is not None else 30)
    if result.returncode or json.loads(result.stdout).get("retained_profiles_retired") is not True:
        raise ProbeCheckFailure("accepted_fixture_profile_retirement_failed")


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
                if len([user for user in users if self.marker in json.dumps(user)]) != 1:
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
                    arguments = {"cmd": self.command, "yield_time_ms": 10000, "shell": "/bin/sh", "login": False}
                    name = "exec_command"
                self.native_calls = [(self.call_id, name, arguments)]
                if self.request_specialist and getattr(self, "managed_native_sources", None) is not None:
                    self.native_calls.extend([
                        (self.call_id + "-native-0", "dolgorae_specialists_native", {"schema": "dolgorae.external-specialist-request/v5", "operation": "validate_provisioning", "sources": self.managed_native_sources}),
                        (self.call_id + "-native-1", "dolgorae_specialist_consumer", {"schema": "dolgorae.consumer-control/v1", "action": "status", "target": {"kind": "context"}})])
                item = code_call(request, self.call_id, self.native_calls)
            else:
                outputs = code_results(request, self.call_id, [label for label, _, _ in self.native_calls])
                output = outputs[self.call_id]
                if self.request_specialist:
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
            if self.request_specialist and getattr(self,"managed_native_sources",None) is not None and not self.pending:
                headers=[]
                for index in range(2):
                    output=outputs[self.call_id+"-native-"+str(index)]
                    if not isinstance(output,str) or not output.isascii() or len(output.encode())>4096: raise ProbeCheckFailure("managed_native_metadata_missing")
                    headers.append(json.loads(output))
                candidate=headers[0].get("facts",{}).get("candidate",{})
                if (headers[0].get("schema")!="dolgorae.native-specialist-response/v1" or headers[0].get("ok") is not True
                        or not candidate.get("agent_configuration_digest","").startswith("sha256:") or candidate.get("installed") is not False
                        or headers[1].get("schema")!="dolgorae.consumer-control-response/v1" or headers[1].get("ok") is not True
                        or headers[1].get("data")!={"kind":"context","mode":"native_only","context_ref":None}):
                    raise ProbeCheckFailure("managed_native_product_result_unverified")
                self.managed_native_seen=True
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
    foreground = None
    census_checked = False
    environment = isolated_environment(root)
    passed = False
    runs: list[tuple[Path, str, Path]] = []
    try:
        for name in ("home", "tmp", "config", "cache", "profile", "workspace", "git-workspace"):
            (root / name).mkdir(mode=0o700)
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
        foreground = subprocess.Popen([str(binary), "serve", "--socket", str(root / "gateway.sock")],
                                      env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        import select
        if not select.select([foreground.stdout], [], [], 10)[0]:
            raise ProbeCheckFailure("integrated_foreground_readiness_timeout")
        ready_line = foreground.stdout.readline()
        private_file(root / "foreground-ready.json", ready_line.encode())
        ready = json.loads(ready_line)
        if not ready.get("ok") or ready.get("command") != "serve":
            raise ProbeCheckFailure("integrated_foreground_not_ready")
        overwrite_fixture_file(root / "home/.dolgorae/home/AGENTS.md", (COMMON + "\n").encode())
        overwrite_fixture_file(root / "home/.dolgorae/roles/general/AGENTS.md", (GENERAL + "\n").encode())
        overwrite_fixture_file(root / "home/.dolgorae/roles/reviewer/AGENTS.md", (REVIEWER + "\n").encode())
        private_file(root / "fixture-owned", b"native-integrated-acceptance/v1\n")
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
        fixture_view=call("workspace","inspect","--workspace",str(workspace))
        profile = accepted_profile_state(root,fixture_view["workspace_id"],writer["run_id"])
        features = profile["snapshot"]["process_static_configuration"]["features"]
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
            stop_fixture_profile(caller_driver, root, environment, binary=binary)
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
        # Explicit setup for the existing Primary Turn; no additional Turn/POST.
        host_root = root / "managed-native-host"
        host_root.mkdir(mode=0o700)
        host_controller = host_root / "controller"
        private_file(host_controller, primary_controller.read_bytes())
        journal=host_root / "journal";journal.mkdir(mode=0o700)
        draft=host_root / "draft.json"
        root_manifest=json.loads((state_root / "runs" / primary["run_id"] / "manifest.json").read_text())
        private_file(draft,json.dumps({"route":"codex-0.160.0","executable":{"path":str(codex),"version":CODEX_VERSION,"sha256":CODEX_SHA256},
            "owner":{"kind":"managed","session_ref":primary["run_id"]},"controller_generation":root_manifest["controller"]["identity"]["generation"],
            "controller_file":str(host_controller),"native_host_root":str(journal),"allowed_scopes":[{"kind":"orchestrated_session","reference":primary["run_id"]}],
            "tools":["dolgorae_specialists_native","dolgorae_specialist_consumer"],"caller_scope":{"binding_ref":"native-managed","podway_workspace_ref":"native-workspace","podway_session_ref":"native-session","podway_attempt_ref":"native-attempt","delegated_placement_ref":"native-placement","max_rounds":3},
            "adapter_id":"dolgorae.managed/1","adapter_version":root_manifest["dolgorae"]["version"].removeprefix("v")}).encode())
        owner_config=host_root / "owner.json"
        def native_setup(operation, extra=()):
            with operator.open("rb") as op,primary_controller.open("rb") as controller:
                completed=subprocess.run([str(binary),"__trusted-specialist-host",operation,"--owner-config",str(owner_config),"--operator-fd",str(op.fileno()),"--controller-fd",str(controller.fileno()),*extra],env=environment,pass_fds=(op.fileno(),controller.fileno()),capture_output=True,timeout=30)
            if completed.returncode or completed.stdout or completed.stderr: raise ProbeCheckFailure("managed_native_setup_failed")
        native_setup("owner-config-create",("--workspace",str(workspace),"--request",str(draft)))
        native_setup("namespace-setup");native_setup("host-state-init");native_setup("preparation-init");native_setup("managed-connect")
        mode=host_root / "consumer-mode.json";private_file(mode,b'{"kind":"native_only"}')
        native_setup("consumer-init",("--request",str(mode)))
        selected=host_root / "managed-provisioning.json";projected=root / "managed-projection.json"
        private_file(selected,json.dumps({"role_name":"reviewer","role":{"kind":"file","value":["roles","reviewer"]},"dependencies":[],"selected_guidance":[],"profile_binding":root_manifest["global_profile_binding"],
            "admission":{"model":"gpt-5.6-luna","effort":"low","purpose_kind":"review","purpose_label":None,"required_capabilities":[],"execution_lane":"dedicated","requested_access":"read_only","required_assurance":"best_effort_personal_alpha","native_subagent_policy":"disabled"}}).encode())
        native_setup("provisioning-prepare",("--request",str(selected),"--output",str(projected)))
        fixture.managed_native_sources=json.loads(projected.read_text())
        fixture.prepare("native-primary-child-request", GENERAL, request_specialist=True)
        call("run", "send", primary["run_id"], "--workspace", str(workspace),
             "--controller-file", str(primary_controller), "--message", "native-primary-child-request",
             "--idempotency-key", "native-primary-child", "--timeout", TURN_TIMEOUT)
        if getattr(fixture,"managed_native_seen",False) is not True: raise ProbeCheckFailure("managed_native_current_http_unproved")
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
        stop_fixture_profile(caller_driver, root, environment, binary=binary)
        cleanup_removed_root(binary, root, allow_existing_root=True)
        foreground.terminate()
        out, err = foreground.communicate(timeout=7)
        private_file(root / "foreground.stdout", out.encode())
        private_file(root / "foreground.stderr", err.encode())
        if foreground.returncode or out:
            raise ProbeCheckFailure("integrated_foreground_shutdown_failed")
        require_fixture_process_absence(call("runtime", "orphan", "inspect",
                                            "--owner-root-under", str(root)))
        census_checked = True
        with patch.dict(os.environ, {caller.OPT_IN: "1"}):
            recovery = caller.campaign(caller_driver, codex, binary=binary)
        passed = True
        return {"schema": "dolgorae.native-integrated-acceptance/v1", "passed": True,
                "codex_version": CODEX_VERSION, "executable_sha256": CODEX_SHA256,
                "loopback_posts": fixture.posts, "native_turns": fixture.steps,
                "external_provider_requests": 0, "account_login_performed": False,
                "real_account_credentials_copied": False,
                "codex_child_account_home_and_platform_tmpdir_exception": True,
                "real_home_file_access_absence_proven": False,
                "foreground_runtime_owned": True,
                "production_cli_defaults": True, "same_thread_access_transitions": True,
                "readonly_native_write_denied": True, "captured_role_replay": True,
                "profile_guidance_observed_as_user_input": True,
                "reusable_review_tasks": 2, "primary_policy_capture_and_replay": True,
                "rejected_review_failures": ["native_read", "forged_receipt", "missing_receipt", "target_drift"],
                "broker_child_accepted_configuration": True,"managed_native_current_http":True,
                "direct_review_scopes": [case[1] for case in cases],
                "copy_and_worktree_creation": False, "caller_service_recovery": recovery,
                "owned_operator_cleanup": True, "owned_process_absence_verified": True,
                "normal_whole_session_close_qualified": True,
                "aggregate_close_replay_and_recovery": True,
                "aggregate_child_retired_and_writer_released": True,
                "idle_writable_primary_abort_and_recovery": True}
    finally:
        cleanup_errors = []
        try:
            if foreground is not None and foreground.poll() is None:
                foreground.terminate()
                out, err = foreground.communicate(timeout=7)
                private_file(root / "foreground.stdout", out.encode())
                private_file(root / "foreground.stderr", err.encode())
                if foreground.returncode:
                    cleanup_errors.append({"stage":"foreground_stop", "exit_code":foreground.returncode})
        except Exception as error:
            cleanup_errors.append({"stage":"foreground_stop", "error":type(error).__name__})
        try:
            if fixture is not None:
                fixture.close()
        except Exception as error:
            cleanup_errors.append({"stage":"fixture_close", "error":type(error).__name__})
        try:
            if not census_checked:
                inspected = subprocess.run([str(binary), "runtime", "orphan", "inspect", "--owner-root-under", str(root)],
                                           env=environment, capture_output=True, text=True, timeout=30)
                private_file(root / "cleanup-census.stdout", inspected.stdout.encode())
                private_file(root / "cleanup-census.stderr", inspected.stderr.encode())
                if inspected.returncode:
                    raise ProbeCheckFailure("integrated_owned_process_inspection_failed")
                require_fixture_process_absence(json.loads(inspected.stdout)["data"])
        except Exception as error:
            cleanup_errors.append({"stage":"strict_census", "error":type(error).__name__})
        private_file(root / "cleanup-errors.json", json.dumps(cleanup_errors).encode())
        if passed and cleanup_errors:
            raise ProbeCheckFailure("integrated_cleanup_incomplete", {"cleanup_errors":cleanup_errors})
        if passed:
            shutil.rmtree(root)
        else:
            print(json.dumps({"schema": "dolgorae.native-integrated-acceptance-failure/v1",
                              "owned_fixture_retained": str(root), "passed": False, "cleanup_errors":cleanup_errors,
                              "loopback_posts": fixture.posts if fixture is not None else 0,
                              "fixture_failure": fixture.failure if fixture is not None else None,
                              "completed_fixture_turns": fixture.steps if fixture is not None else 0}),
                  file=sys.stderr)


class SuccessorResponses(IntegratedResponses):
    """Serve the existing two Writer tasks and three actual Primary Turns."""

    def __init__(self) -> None:
        self.assignment_seen = threading.Event()
        self.writer_steps = 0
        super().__init__(concurrent_requests=True)

    def primary_user(self, request: dict, phase: dict) -> dict | None:
        marker = "native-primary-" + phase["phase"] + "\n"
        users = [v for v in request.get("input", []) if v.get("type") == "message" and v.get("role") == "user"
                 and any(b.get("text", "").startswith(marker) for b in v.get("content", []))]
        if len(users) > 1:
            raise ProbeCheckFailure("duplicate_current_primary_phase_message")
        return users[0] if users else None

    def writer_task(self, request: dict, writer_step: int) -> dict:
        expected = json.loads((self.consumer_root / ("formal-task.json" if writer_step == 0 else "ordinary-task.json")).read_text())
        suffix = (f"\n\nExecution intent: {expected['execution_intent']}\n"
                  f"Task deadline: {expected['deadline_seconds']} seconds after durable acceptance")
        tasks = []
        for message in request.get("input", []):
            if message.get("type") != "message" or message.get("role") != "user":
                continue
            for block in message.get("content", []):
                text = block.get("text", "")
                if "\n\nAccepted task:\n" not in text:
                    continue
                tail = text.split("\n\nAccepted task:\n", 1)[1]
                try:
                    task, end = json.JSONDecoder().raw_decode(tail)
                except json.JSONDecodeError as error:
                    raise ProbeCheckFailure("writer_accepted_task_json_invalid") from error
                if tail[end:] != suffix:
                    raise ProbeCheckFailure("writer_execution_intent_or_deadline_mismatch")
                if task != expected["task"]:
                    raise ProbeCheckFailure("writer_accepted_task_mismatch")
                tasks.append(task)
        if len(tasks) != 1:
            raise ProbeCheckFailure("writer_accepted_task_missing_or_duplicate")
        return tasks[0]

    def check_prepared_input(self, kind: str, phase: dict) -> None:
        raw = self.finished_reads[kind + "_input"]
        value = json.loads(raw)
        if uuid.UUID(value["idempotency_key"]).version != 7:
            raise ProbeCheckFailure("prepared_original_key_invalid")
        connection = json.loads((self.consumer_root / "connection.json").read_text())
        expected = {"workspace_uuid": connection["workspace"], "session_id": connection["session"],
                    "attempt_id": connection["attempt"], "idempotency_key": value["idempotency_key"]}
        if kind == "record":
            observed = self.record_observation["result"]
            item = next(v for v in observed["active_items"] if v["item_id"] == "result")
            expected.update(schema="podway.item-record-many-input/v1", session_revision=observed["status"]["session"]["revision"],
                operations=[{"item_id": "result", "expected_item_revision": item["revision"],
                    "record": {"type": "text", "value": "Primary inspected the original managed delivery and retained its gaps."}}])
        else:
            expected.update(schema="podway.delegation-accept-input/v1", session_revision=self.accept_package["result"]["session_revision"],
                package_digest=self.accept_package["result"]["package_digest"], execution=phase["input"]["execution"],
                rounds_used=1, result_digest=phase["input"]["result_digest"], worker_disposition="idle", accepted=True,
                summary="Primary verified the complete managed delivery, original content, bindings and retained assessment gaps.")
        expected_raw = json.dumps(expected, ensure_ascii=False, sort_keys=True,
                                  separators=(",", ":")).encode("utf-8")
        if raw != expected_raw:
            raise ProbeCheckFailure("prepared_input_not_exact_selected_contract")
        setattr(self, kind + "_input", value)
        private_file(self.consumer_root / f"consumer-prepared-{kind}.json", raw)

    def primary_return(self, request: dict, call_id: str) -> tuple[dict, str | None]:
        if call_id not in self.primary_pending:
            raise ProbeCheckFailure("managed_primary_return_label_not_pending")
        output = code_results(request, self.primary_outer_call, list(self.primary_pending))[call_id]
        if not isinstance(output, str):
            raise ProbeCheckFailure("managed_primary_return_shape_changed")
        header, separator, original = output.partition("\n")
        if not header.isascii() or len(header.encode()) > 4096:
            raise ProbeCheckFailure("managed_primary_metadata_unbounded")
        value = json.loads(header)
        if value.get("ok") is not True:
            raise ProbeCheckFailure("managed_primary_product_refused", value)
        return value, original if separator else None

    def primary_calls(self, requests: list[tuple[str, dict, dict | None]]) -> list[dict]:
        self.primary_pending = {}
        calls = []
        for index, (tool, body, page) in enumerate(requests):
            call_id = f"primary-{self.primary_phase}-{self.posts}-{index}"
            self.primary_pending[call_id] = (body, page)
            calls.append((call_id, tool, body))
        self.primary_outer_call = f"primary-{self.primary_phase}-{self.posts}"
        item = code_call(self.primary_carrier_request, self.primary_outer_call, calls)
        if any(body.get("operation") == "await_specialist_task" for _, body, _ in requests):
            item["input"] = '// @exec: {"yield_time_ms": 65000}\n' + item["input"]
        identity = f"primary-response-{self.posts}"
        return [{"type": "response.created", "response": {"id": identity}},
                {"type": "response.output_item.done", "item": item},
                {"type": "response.completed", "response": {"id": identity, "usage": {
                    "input_tokens": 0, "input_tokens_details": None, "output_tokens": 0,
                    "output_tokens_details": None, "total_tokens": 0}}}]

    def read_view(self, label: str, selection: str, view: dict, expected: bytes | None,
                  declared: dict | None = None) -> tuple[str, dict, dict]:
        page = {"label": label, "selection": selection, "view": view, "expected": expected,
                "declared": declared, "offset": 0, "body": b""}
        return self.read_call(page, None)

    def read_call(self, page: dict, cursor: str | None) -> tuple[str, dict, dict]:
        return ("dolgorae_specialist_consumer", {"schema": "dolgorae.consumer-control/v1", "action": "inspect",
            "selection_ref": page["selection"], "view": page["view"],
            "range": {"kind": "continue", "cursor": cursor} if cursor else {"kind": "start", "offset": 0}}, page)

    def check_primary_page(self, header: dict, original: str | None, page: dict) -> None:
        data = header["data"]
        raw = original.encode() if isinstance(original, str) else None
        if (raw is None or data.get("kind") != "page" or data.get("encoding") != "utf8"
                or data["offset"] != page["offset"] or data["returned_length"] != len(raw) or len(raw) > 1024):
            raise ProbeCheckFailure("managed_original_page_metadata_changed")
        if (data["source_length"] + 1023) // 1024 != SUCCESSOR_READ_PAGES[page["label"]]:
            raise ProbeCheckFailure("frozen_managed_read_trace_changed")
        expected = page["expected"]
        if expected is not None:
            end = min(page["offset"] + 1024, len(expected))
            while True:
                try: expected[page["offset"]:end].decode(); break
                except UnicodeDecodeError: end -= 1
            if raw != expected[page["offset"]:end]:
                raise ProbeCheckFailure("managed_original_next_http_bytes_changed")
        page["body"] += raw
        page["offset"] += len(raw)
        declared = page["declared"]
        if declared is not None and (data["source_digest"] != declared["digest"] or data["source_length"] != declared["byte_length"]):
            raise ProbeCheckFailure("managed_prepared_or_outcome_source_changed")
        if expected is None and (raw is None or not raw.isascii() or len(raw) != min(1024, data["source_length"] - data["offset"])):
            raise ProbeCheckFailure("managed_frozen_json_page_length_changed")
        self.primary_pages.append({"label": page["label"], "post": self.posts, "offset": data["offset"],
            "outer_call_id": getattr(self, "primary_outer_call", None), "view": page["view"], "selection_ref": page["selection"],
            "bytes": len(raw), "bytes_sha256": hashlib.sha256(raw).hexdigest(),
            "source_bytes": data["source_length"], "source_digest": data["source_digest"]})
        if data["continuation"] is not None:
            if page["offset"] >= data["source_length"]:
                raise ProbeCheckFailure("managed_invented_continuation")
            self.continuing.append(self.read_call(page, data["continuation"]))
            return
        if (page["offset"] != data["source_length"] or "sha256:" + hashlib.sha256(page["body"]).hexdigest() != data["source_digest"]
                or (expected is not None and page["body"] != expected)):
            raise ProbeCheckFailure("managed_incomplete_original_read")
        self.finished_reads[page["label"]] = page["body"]

    def primary_response(self, request: dict, phase: dict) -> list[dict]:
        from run_native_production_host import encoded, Responses
        with self.lock:
            self.posts += 1
            if phase["phase"] == "cleanup":
                self.cleanup_posts = getattr(self, "cleanup_posts", 0) + 1
                if self.cleanup_posts > 2:
                    raise ProbeCheckFailure("managed_original_release_cleanup_trace_exceeded")
            if self.posts - getattr(self, "cleanup_posts", 0) > SUCCESSOR_POSTS:
                raise ProbeCheckFailure("successor_exact_trace_budget_exceeded")
            self.primary_posts = getattr(self, "primary_posts", 0) + 1
            name = phase["phase"]
            private_file(self.consumer_root / f"primary-http-{name}-{self.primary_posts}.json", encoded(request))
            if getattr(self, "primary_phase", None) != name:
                if getattr(self, "primary_pending", {}):
                    raise ProbeCheckFailure("unfinished_primary_phase")
                self.primary_phase, self.primary_step = name, 0
                self.primary_results, self.primary_pending = [], {}
                self.primary_pages, self.finished_reads = [], {}
                self.continuation_posts = getattr(self, "continuation_posts", 0)
                self.continuing = []
                text = json.dumps([request.get("instructions"), request.get("input")], ensure_ascii=False)
                if not all(marker in text for marker in (CORE_PREFIX, COMMON, GENERAL, PROFILE)) or any(marker in text for marker in AMBIENT):
                    raise ProbeCheckFailure("managed_primary_captured_instructions_missing_or_ambient")
                user = self.primary_user(request, phase)
                original_instruction = "".join(v.get("text", "") for v in user.get("content", [])) if user else ""
                if (not getattr(self, "producer_only", False)
                        and (self.consumer_root / "aquarium-source/references/formal-agent-orchestration-v1.md").read_text() not in original_instruction):
                    raise ProbeCheckFailure("selected_formal_primary_instruction_not_delivered")
                self.primary_carrier_request = {"input": [v for v in request["input"] if v.get("type") == "additional_tools"]}
            for call_id, (body, page) in self.primary_pending.items():
                header, original = self.primary_return(request, call_id)
                if (("operation" in body and (header.get("schema") != "dolgorae.native-specialist-response/v1" or header.get("operation") != body["operation"]))
                        or ("action" in body and (header.get("schema") != "dolgorae.consumer-control-response/v1" or header.get("action") != body["action"]))
                        or ("request_id" in body and header.get("request_id") != body["request_id"])):
                    raise ProbeCheckFailure("managed_primary_return_operation_or_request_changed")
                self.primary_results.append({"outer_call_id": self.primary_outer_call, "label": call_id, "request": body, "header": header})
                if page is not None:
                    self.check_primary_page(header, original, page)
                if body.get("operation") == "assign_specialist_task":
                    self.managed_task = header["facts"]["task"]
                    if (self.managed_task["member_ref"] != phase["input"]["member_ref"]
                            or self.managed_task["accepted_task_digest"] != phase["input"]["accepted_task_digest"]):
                        raise ProbeCheckFailure("actual_managed_assignment_binding_changed")
                    self.assignment_seen.set()
                if body.get("operation") == "spawn_specialist":
                    member = header["facts"]["member"]
                    if (member["scope_kind"] != "orchestrated_session" or member["scope_ref"] != phase["root_run"]
                            or member["run_ref"] != member["member_ref"]
                            or member["agent_configuration_digest"] != phase["input"]["candidate"]["agent_configuration_digest"]
                            or member["role_snapshot_digest"] != phase["input"]["candidate"]["role_snapshot_digest"]):
                        raise ProbeCheckFailure("actual_managed_spawn_binding_changed")
            self.primary_pending = {}
            if self.continuing:
                calls, self.continuing = self.continuing, []
                self.continuation_posts += 1
                return self.primary_calls(calls)
            semantic = "dolgorae_specialists_native"
            control = "dolgorae_specialist_consumer"
            schema = "dolgorae.external-specialist-request/v5"
            def call(action: str, **fields) -> tuple[str, dict, None]:
                return control, {"schema": "dolgorae.consumer-control/v1", "action": action, **fields}, None
            def status(action: str) -> dict:
                return next(v["header"]["data"] for v in reversed(self.primary_results) if v["request"].get("action") == action)
            step = self.primary_step
            self.primary_step += 1
            data = phase["input"]
            calls = []
            if name == "spawn" and step == 0 and "sources" in data:
                calls = [(semantic, {"schema": schema, "operation": "install_provisioning",
                    "sources": data["sources"], "idempotency_key": "native-install", "expected_revision": None}, None)]
            elif name == "spawn" and step == (1 if "sources" in data else 0):
                candidate = data["candidate"]
                calls = [(semantic, {"schema": schema, "operation": "spawn_specialist", "scope_kind": "orchestrated_session",
                    "scope_ref": phase["root_run"], "provisioning_ref": candidate["provisioning_ref"], "role_ref": "general",
                    "agent_configuration_digest": candidate["agent_configuration_digest"], "idempotency_key": "native-spawn", "expected_revision": None}, None)]
            elif name == "assignment":
                if step == 0: calls = [(semantic, data["request"], None)]
                elif step == 1:
                    calls = [(semantic, {"schema": schema, "operation": "await_specialist_task", "scope_kind": "orchestrated_session",
                        "scope_ref": phase["root_run"], "member_ref": data["member_ref"], "task_ref": self.managed_task["task_ref"], "timeout_seconds": 60}, None)]
                else:
                    last = self.primary_results[-1]["header"]["facts"]
                    if last["task"]["state"] != "completed" or last["wait_timed_out"] is not False:
                        raise ProbeCheckFailure("actual_managed_task_did_not_complete")
            elif name == "cleanup":
                if step == 0:
                    calls = [(semantic, {"schema": schema, "operation": "release_specialist",
                        "scope_kind": "orchestrated_session", "scope_ref": phase["root_run"],
                        "member_ref": data["member_ref"], "expected_revision": data["release_revision"]}, None)]
                elif (self.primary_results[-1]["header"]["facts"]["member"]["member_ref"] != data["member_ref"]
                      or self.primary_results[-1]["header"]["facts"]["member"]["state"] != "released"):
                    raise ProbeCheckFailure("managed_original_release_cleanup_incomplete")
            elif name == "consumer":
                if step == 0:
                    calls = [(semantic, {"schema": schema, "operation": "collect_specialist_task", "scope_kind": "orchestrated_session",
                        "scope_ref": phase["root_run"], "member_ref": data["member_ref"], "task_ref": data["task_ref"]}, None), call("status", target={"kind": "context"})]
                elif step == 1:
                    collected = next(v["header"] for v in self.primary_results if v["request"].get("operation") == "collect_specialist_task")
                    if (collected["evidence"]["mapping_contract"] != "dolgorae.podway-managed/v1"
                            or collected["facts"]["task"]["task_ref"] != data["task_ref"]
                            or collected["facts"]["task"]["member_ref"] != data["member_ref"]
                            or collected["evidence"]["result_digest"] != data["result_digest"]):
                        raise ProbeCheckFailure("actual_managed_collection_mapping_changed")
                    self.result_selection = collected["evidence"]["selection_ref"]
                    self.context_selection = status("status")["context_ref"]
                    package = json.loads((self.consumer_root / "package.json").read_text())["result"]["package"]
                    originals = [("complete_result", {"kind": "complete_result"}, data["originals"]["complete_result"].encode()),
                        ("final_response", {"kind": "final_response"}, self.managed_original),
                        ("before", {"kind": "change", "index": 0, "side": "before"}, data["originals"]["before"].encode()),
                        ("after", {"kind": "change", "index": 0, "side": "after"}, data["originals"]["after"].encode())]
                    calls = [self.read_view(label, self.result_selection, view, expected) for label, view, expected in originals]
                    calls.append(self.read_view("package", self.context_selection, {"kind": "package"}, encoded(package)))
                elif step == 2:
                    self.record_observation = successor_public(self.consumer_root, "observe")
                    calls = [call("prepare_record", request_id="primary-record", context_ref=self.context_selection,
                        evidence_ref=self.result_selection, operations=[{"item_id": "result", "record": {"type": "text", "value": "Primary inspected the original managed delivery and retained its gaps."}}])]
                elif step == 3:
                    prepared = status("prepare_record")["input"]
                    self.record_selection = prepared["selection_ref"]
                    calls = [self.read_view("record_input", self.record_selection, {"kind": "prepared_input"}, None, prepared)]
                elif step == 4:
                    self.check_prepared_input("record", phase)
                    calls = [call("record", request_id="primary-record", decision="record")]
                elif step == 5:
                    record = status("record")
                    if record["effect"] != "committed": raise ProbeCheckFailure("actual_record_not_committed")
                    self.accept_package = successor_public(self.consumer_root, "delegation", "package")
                    original = successor_public(self.consumer_root, "--idempotency-key", self.record_input["idempotency_key"], "outcome", "lookup")
                    calls = [call("prepare_accept", request_id="primary-accept", evidence_ref=self.result_selection,
                        summary="Primary verified the complete managed delivery, original content, bindings and retained assessment gaps."),
                        self.read_view("record_outcome", self.record_selection, {"kind": "outcome"}, encoded(original["result"]["outcome"]["response"]), record["outcome"])]
                elif step == 6:
                    prepared = status("prepare_accept")["input"]
                    self.accept_selection = prepared["selection_ref"]
                    calls = [self.read_view("accept_input", self.accept_selection, {"kind": "prepared_input"}, None, prepared)]
                elif step == 7:
                    self.check_prepared_input("accept", phase)
                    calls = [call("accept", request_id="primary-accept", decision="accept")]
                elif step == 8:
                    accepted = status("accept")
                    if accepted["effect"] != "committed": raise ProbeCheckFailure("actual_acceptance_not_committed")
                    original = successor_public(self.consumer_root, "--idempotency-key", self.accept_input["idempotency_key"], "outcome", "lookup")
                    calls = [self.read_view("accept_outcome", self.accept_selection, {"kind": "outcome"}, encoded(original["result"]["outcome"]["response"]), accepted["outcome"]),
                        self.read_view("accept_receipt", self.accept_selection, {"kind": "receipt"}, None, accepted["receipt"])]
                else:
                    receipt = json.loads(self.finished_reads["accept_receipt"])
                    connection = json.loads((self.consumer_root / "connection.json").read_text())
                    observed = successor_public(self.consumer_root, "delegation", "receipt", "--session", connection["session"], "--attempt", connection["attempt"])
                    if receipt["result"]["receipt"] != observed["result"]["receipt"] or observed["result"]["applicability"] != "accepted":
                        raise ProbeCheckFailure("actual_original_receipt_not_current_or_matching")
                    if data.get("release_member"):
                        releases = [v["header"]["facts"] for v in self.primary_results
                                    if v["request"].get("operation") == "release_specialist"]
                        if not releases:
                            calls = [(semantic, {"schema": schema, "operation": "release_specialist",
                                "scope_kind": "orchestrated_session", "scope_ref": phase["root_run"],
                                "member_ref": data["member_ref"], "expected_revision": data["release_revision"]}, None)]
                        elif (len(releases) != 1 or releases[0]["member"]["member_ref"] != data["member_ref"]
                              or releases[0]["member"]["state"] != "released"):
                            raise ProbeCheckFailure("original_managed_member_release_incomplete")
            if calls:
                return self.primary_calls(calls)
            private_file(self.consumer_root / f"primary-{name}-results.json", encoded(self.primary_results))
            private_file(self.consumer_root / f"primary-{name}-pages.json", encoded(self.primary_pages))
            if name == "consumer" and (set(self.finished_reads) != set(SUCCESSOR_READ_PAGES)
                    or self.continuation_posts != SUCCESSOR_CONTINUATIONS):
                raise ProbeCheckFailure("required_managed_reads_or_continuation_count_missing")
            self.steps += 1
            return Responses.codex_events({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Primary delivery checks finished; no semantic goal pass is claimed."}]}, self.posts)

    def respond(self, request: Any) -> list[dict[str, Any]]:
        phase_path = self.consumer_root / "primary-phase.json"
        phase = json.loads(phase_path.read_text()) if phase_path.is_file() else None
        if phase is not None and phase["phase"] == "spawn":
            with self.lock:
                if not getattr(self, "first_request_captured", False):
                    # Preserve the actual decoded HTTP input before classification.
                    private_file(self.consumer_root / "successor-first-request.json",
                                 json.dumps(request, ensure_ascii=False).encode())
                    self.first_request_captured = True
        if phase is not None and self.primary_user(request, phase) is not None:
            return self.primary_response(request, phase)
        writer_step = getattr(self, "writer_steps", 0)
        if not getattr(self, "pending", False):
            markers = ["native-managed-writer", "native-external-writer"]
            if writer_step >= len(markers):
                raise ProbeCheckFailure("unexpected_successor_turn")
            marker = markers[writer_step]
            self.prepare(marker, GENERAL, command=f"printf '{writer_step + 1} after\\n' > file.txt; printf 'written\\n'",
                         expected=b"written\n")
            task = self.writer_task(request, writer_step)
            if not getattr(self, "producer_only", False):
                dispatch = (self.consumer_root / ("formal-dispatch.txt" if writer_step == 0 else "ordinary-dispatch.txt")).read_text()
                if not any(context.get("content") == dispatch for context in task.get("contexts", [])):
                    raise ProbeCheckFailure("selected_aquarium_assignment_not_delivered")
        private_file(self.consumer_root / f"writer-http-{writer_step}-{self.posts}.json", json.dumps(request, ensure_ascii=False).encode())
        if writer_step == 0 and not getattr(self, "managed_input", None):
            self.managed_input = request
        events = super().respond(request)
        if not self.pending:
            self.writer_steps = writer_step + 1
        if writer_step == 0 and not self.pending:
            self.managed_dispatch_finished()
            if getattr(self, "producer_only", False):
                self.managed_original = b"Complete managed producer delivery. Native write verified; no Aquarium Role or semantic goal acceptance is claimed.\n" * 14
                for event in events:
                    item = event.get("item", {})
                    if item.get("type") == "message" and item.get("role") == "assistant":
                        item["content"][0]["text"] = self.managed_original.decode()
                return events
            # Wait for the genuine assignment acknowledgement, never a scraped
            # task ID. The HTTP server can serve the concurrent Root request.
            if not self.assignment_seen.wait(5):
                raise ProbeCheckFailure("actual_assignment_ack_not_observed")
            phase = json.loads((self.consumer_root / "primary-phase.json").read_text())
            facts = phase["input"]
            task = self.managed_task
            connection = json.loads((self.consumer_root / "connection.json").read_text())
            package = json.loads((self.consumer_root / "package.json").read_text())["result"]["package"]
            preparation = json.loads((self.consumer_root / "formal-preparation.json").read_text())
            setup = json.loads((self.consumer_root / "managed-consumer-setup.json").read_text())
            handoff = {"schema": "aquarium.worker-handoff/v1", "work": {"objective": "Bounded formal native delivery",
                    "unit": "native-managed-writer", "sources": "aquarium-source.json"},
                "target": {"path": "file.txt", "sha256": hashlib.sha256((self.consumer_root / "workspace/file.txt").read_bytes()).hexdigest(),
                    "base_revision": setup["base_commit"]},
                "procedure": {"session": connection["session"], "attempt": connection["attempt"], "placement": connection["placement"],
                    "procedure_digest": package["procedure_digest"], "goal": package["goal"]},
                "native": {"backend": "codex", "adapter": "dolgorae.managed/1", "adapter_version": connection["adapter_version"],
                    "primary_run_ref": phase["root_run"], "supervisor_ref": facts["controller_ref"],
                    "worker_ref": facts["member_ref"], "task_ref": task["task_ref"]},
                "configuration": {"role_snapshot_digest": facts["candidate"]["role_snapshot_digest"],
                    "agent_configuration_digest": facts["candidate"]["agent_configuration_digest"], "accepted_task_digest": task["accepted_task_digest"],
                    "aquarium_local_role_sha256": preparation["local_prepared_role_snapshot_sha256"]},
                "authority": {"allowed": ["write file.txt"], "excluded": ["goal acceptance"]},
                "evidence": [{"basis": "correlated native command", "exit": 0, "output": "written\n"}],
                "gaps": [{"owner": "independent evaluator", "obligation": "Assess candidate against original requirements"}],
                "deliverables": ["file.txt"], "changed_artifacts": ["file.txt"],
                "complete_result": {"owner": "managed task", "task_ref": task["task_ref"], "binding": "Primary reads the authoritative terminal result"},
                "settlement": "Terminal publication and Primary fresh idle proof still required", "next_owner": "Primary"}
            self.managed_original = json.dumps(handoff, ensure_ascii=True, sort_keys=True, separators=(",", ":")).encode()
            for event in events:
                item = event.get("item", {})
                if item.get("type") == "message" and item.get("role") == "assistant":
                    item["content"][0]["text"] = self.managed_original.decode()
        if writer_step == 1 and not self.pending:
            assignment = json.loads((self.consumer_root / "ordinary-assignment.json").read_text())
            body = (self.consumer_root / "workspace/file.txt").read_bytes()
            handoff = {key: assignment[key] for key in ("work", "target", "authority", "procedure", "native", "evidence", "gaps")}
            handoff.update(schema="aquarium.executor-handoff/v3", assignment="ordinary-assignment.json",
                deliverables={"delivered": ["file.txt"], "missing": []},
                changed_artifacts=[{"path": "file.txt", "sha256": hashlib.sha256(body).hexdigest()}],
                results=[{"basis": "firsthand correlated native exec_command", "exit": 0, "output": "written\n"}],
                settlement="Native result publication and owner close must still be observed", next_owner="Primary")
            handoff["native"] = {"harness": "codex", "agent": "unknown in this response fixture", "lifecycle": "returning-delivery"}
            self.ordinary_handoff = handoff
            for event in events:
                item = event.get("item", {})
                if item.get("type") == "message" and item.get("role") == "assistant":
                    item["content"][0]["text"] = json.dumps(handoff, ensure_ascii=False)
        if self.posts > SUCCESSOR_POSTS:
            raise ProbeCheckFailure("successor_exact_trace_budget_exceeded")
        return events


def assert_successor_dispatch(root: Path, fixture: SuccessorResponses) -> None:
    assert_no_source_isolation(root / "home")
    states = list((root / "home/.dolgorae/workspaces").glob("*"))
    if len(states) != 1:
        raise ProbeCheckFailure("successor_workspace_not_unique")
    state = states[0]
    with sqlite3.connect(f"file:{state / 'orchestration/orchestration.sqlite3'}?mode=ro", uri=True) as db:
        rows = db.execute("SELECT task_id,run_ref FROM dispatch_indexes WHERE EXISTS (SELECT 1 FROM result_content_refs r WHERE r.owner_kind='dispatch_index' AND r.owner_id=dispatch_indexes.task_id AND r.representation='index') AND task_id IN "
            "(SELECT assignment_id FROM caller_scope_assignments WHERE accounting_state IN "
            "('counted_unresolved','counted_accepted'))").fetchall()
    writer = json.loads((state / "runtime/writer.json").read_bytes())
    if (writer["state"] != "active" or writer.get("holder") is None
            or not any(run == writer["holder"]["run_id"] for _, run in rows)):
        raise ProbeCheckFailure("successor_submit_preceded_index_or_writer")
    fixture.dispatch_checks = getattr(fixture, "dispatch_checks", 0) + 1


def successor_campaign(binary: Path, codex: Path, podway: Path | None = None,
                       aquarium_source: Path | None = None, *, frozen_driver: Path | None = None,
                       frozen_driver_sha256: str | None = None, producer_only: bool = False) -> dict[str, Any]:
    if os.environ.get(OPT_IN) != "1":
        raise ValueError("explicit integrated native opt-in required")
    if (frozen_driver is None) != (frozen_driver_sha256 is None):
        raise ValueError("frozen successor driver and SHA256 must be supplied together")
    if executable_identity(codex)["sha256"] != CODEX_SHA256:
        raise ValueError("successor acceptance requires the pinned executable")
    if podway is None or aquarium_source is None:
        raise ValueError("successor acceptance requires the selected --podway and --aquarium-source inputs")
    podway = podway.resolve(strict=True)
    aquarium_source = aquarium_source.resolve(strict=True)
    root = Path(tempfile.mkdtemp(prefix="dsn-", dir=os.environ["TMPDIR"])).resolve()
    root.chmod(0o700)
    fixture = None
    driver = None
    foreground = None
    environment = None
    passed = False

    def stop_foreground() -> None:
        nonlocal foreground
        if foreground is None:
            return
        foreground.terminate()
        out, err = foreground.communicate(timeout=7)
        private_file(root / "foreground.stdout", out.encode())
        private_file(root / "foreground.stderr", err.encode())
        code = foreground.returncode
        foreground = None
        if code or out:
            raise ProbeCheckFailure("integrated_foreground_shutdown_failed")

    try:
        for name in ("home", "tmp", "config", "cache", "profile", "profile-members", "workspace"):
            (root / name).mkdir(mode=0o700)
        environment = isolated_environment(root)
        environment[OPT_IN] = "1"
        if producer_only:
            from run_native_production_host import admitted_native_input
            environment["DOLGORAE_NATIVE_MANAGED_PRODUCER"] = "1"
        environment.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null")
        successor_aquarium_sources(root, aquarium_source)
        private_file(root / "consumer-inputs.json", json.dumps({
            "python": sys.executable, "scripts": str(Path(__file__).resolve().parent),
            "podway": str(podway), "podway_sha256": "dece7fecd90eaf6130b330e2cb70e98532f3e6b73950227cc2ff935a8be8e5ae",
            "producer_only": producer_only}).encode())
        checked_codex(codex, f"codex-cli {CODEX_VERSION}", environment)
        driver = root / "fixture-successor-driver"
        if frozen_driver is not None:
            frozen_driver = frozen_driver.resolve(strict=True)
            if not frozen_driver_sha256 or hashlib.sha256(frozen_driver.read_bytes()).hexdigest() != frozen_driver_sha256:
                raise ProbeCheckFailure("frozen_successor_driver_changed")
            shutil.copyfile(frozen_driver, driver)
        else:
            command = shlex.split(os.environ.get("CARGO", "cargo")) + [
                "rustc", "--locked", "--release", "--example", "native_successor_pipeline", "--",
                "--cfg", "dolgorae_native_acceptance", "--cfg", "test",
                "-C", "debug-assertions=on", "-C", "overflow-checks=on"]
            build = subprocess.run(command, cwd=PROTOCOL.parents[1], capture_output=True,
                                   text=True, timeout=300)
            private_file(root / "build.stdout", build.stdout.encode())
            private_file(root / "build.stderr", build.stderr.encode())
            if build.returncode:
                raise ProbeCheckFailure("successor_driver_build_failed")
            shutil.copyfile(Path(os.environ.get("CARGO_TARGET_DIR",str(PROTOCOL.parents[1] / "target"))) / "release/examples/native_successor_pipeline", driver)
        driver.chmod(0o700)
        fixture = SuccessorResponses()
        fixture.producer_only = producer_only
        fixture.consumer_root = root
        fixture.managed_dispatch_finished = stop_foreground
        fixture.source_check = lambda: assert_successor_dispatch(root, fixture)
        private_file(root / "profile/config.toml", b'model = "gpt-5.6-luna"\nmodel_reasoning_effort = "low"\n'
                     + fixture_config(fixture.base_url) + b"\n[features]\nplugins = false\nshell_snapshot = false\n")
        private_file(root / "profile/AGENTS.md", (PROFILE + "\n").encode())
        private_file(root / "profile-members/config.toml", (root / "profile/config.toml").read_bytes())
        private_file(root / "profile-members/AGENTS.md", (root / "profile/AGENTS.md").read_bytes())
        private_file(root / "fixture-owned", b"native-successor-pipeline/v1\n")
        subprocess.run(["/usr/bin/git", "-C", str(root / "workspace"), "init", "-q"],
                       env=environment, check=True, capture_output=True, timeout=10)
        initialized = subprocess.run([str(binary), "init", str(root / "workspace")],
                                     env=environment, capture_output=True, text=True, timeout=10)
        private_file(root / "control-plane-init.json", initialized.stdout.encode())
        if initialized.returncode or not json.loads(initialized.stdout).get("ok"):
            raise ProbeCheckFailure("integrated_foreground_not_ready")
        foreground = subprocess.Popen([str(driver), "--control-plane", str(root)],
                                      env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        import select
        if not select.select([foreground.stdout], [], [], 10)[0]:
            raise ProbeCheckFailure("integrated_foreground_readiness_timeout")
        ready_line = foreground.stdout.readline()
        private_file(root / "foreground-ready.json", ready_line.encode())
        ready = json.loads(ready_line)
        if not ready.get("ok") or ready.get("command") != "serve":
            raise ProbeCheckFailure("integrated_foreground_not_ready")
        result = subprocess.run([str(driver), str(root), str(codex)], env=environment,
                                capture_output=True, text=True, timeout=300)
        private_file(root / "driver.stdout", result.stdout.encode())
        private_file(root / "driver.stderr", result.stderr.encode())
        if result.returncode or fixture.failure:
            raise ProbeCheckFailure("successor_driver_failed", {"exit": result.returncode,
                "loopback_posts": fixture.posts, "fixture_failure": fixture.failure,
                "http_posts": len(fixture.input_sizes), "first_fixture_error": fixture.failure_details})
        value = json.loads(result.stdout)
        if (value.get("schema") != "dolgorae.native-successor-pipeline/v1"
                or not value.get("passed") or fixture.steps != (4 if producer_only else 5)
                or fixture.posts != SUCCESSOR_POSTS - (2 if producer_only else 0)
                or len(fixture.input_sizes) != SUCCESSOR_POSTS - (2 if producer_only else 0)
                or getattr(fixture, "primary_posts", 0) != 15 + SUCCESSOR_CONTINUATIONS
                or getattr(fixture, "writer_steps", 0) != (1 if producer_only else 2)
                or getattr(fixture, "dispatch_checks", 0) != (2 if producer_only else 4)
                or (root / "cleanup-result").read_bytes() != b"complete\n"):
            raise ProbeCheckFailure("successor_evidence_incomplete", {"native_turns":fixture.steps,"loopback_posts":fixture.posts,"dispatch_checks":getattr(fixture,"dispatch_checks",0)})
        stop_foreground()
        with patch.dict(os.environ, environment, clear=True):
            cleanup_removed_root(binary, root, allow_existing_root=True)
        inspected = subprocess.run([str(binary), "runtime", "orphan", "inspect",
            "--owner-root-under", str(root)], env=environment, capture_output=True,
            text=True, timeout=30)
        observation = json.loads(inspected.stdout)
        if inspected.returncode or not observation.get("ok"):
            raise ProbeCheckFailure("successor_process_inventory_failed")
        require_fixture_process_absence(observation["data"])
        value.update(native_turns=fixture.steps, loopback_posts=fixture.posts,
                     primary_posts=fixture.primary_posts, continuation_posts=fixture.continuation_posts,
                     original_pages=fixture.primary_pages, http_input_sizes=fixture.input_sizes,
                     primary_turns={phase: json.loads((root / f"primary-{phase}-turn.json").read_text())
                                    for phase in ("spawn", "assignment", "consumer")},
                     primary_returns={phase: [{"outer_call_id": v["outer_call_id"], "logical_label": v["label"],
                         "operation": v["request"].get("operation", v["request"].get("action")), "header": v["header"]}
                         for v in json.loads((root / f"primary-{phase}-results.json").read_text())]
                         for phase in ("spawn", "assignment", "consumer")},
                     action_completion=json.loads((root / "consumer-completion.json").read_text()),
                     ordinary_delivery=None if producer_only else json.loads((root / "ordinary-consumed.json").read_text()),
                     external_provider_requests=0, real_account_or_credentials_used=False,
                     codex_sha256=CODEX_SHA256, owned_process_absence_verified=True)
        if producer_only:
            exported = json.loads((root / "managed-producer-export.json").read_text())
            facts = {"schema": "dolgorae.native-producer-facts/v1", **exported,
                "source_kind": "genuine managed Root production dispatch, authorized source-owner collect and original native page returns",
                "originals": {key: body.decode() for key, body in fixture.finished_reads.items()},
                "native_pages": fixture.primary_pages, "ordinary_aquarium_role": False, "semantic_goal_pass": False}
            facts["native_admitted_input"] = admitted_native_input(fixture.managed_input, exported["accepted_configuration"], exported["original_task"])
            private_file(root / "producer-facts.json", json.dumps(facts, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode())
            value.update(producer_facts=str(root / "producer-facts.json"), owned_fixture_retained=str(root), current_ordinary_consumer_pending=True)
        passed = True
        return value
    finally:
        original_failure = sys.exc_info()[0] is not None
        cleanup_errors = []
        try:
            stop_foreground()
        except Exception as error:
            passed = False
            cleanup_errors.append({"stage":"foreground_stop", "error":type(error).__name__})
        try:
            if not passed and driver is not None and driver.is_file():
                cleanup = subprocess.run([str(driver), "--cleanup", str(root)], env=environment,
                                         capture_output=True, text=True, timeout=120)
                private_file(root / "cleanup.stdout", cleanup.stdout.encode())
                private_file(root / "cleanup.stderr", cleanup.stderr.encode())
                private_file(root / "cleanup-status.json", json.dumps({"exit": cleanup.returncode}).encode())
                if cleanup.returncode == 0:
                    with patch.dict(os.environ, environment, clear=True):
                        cleanup_removed_root(binary, root, allow_existing_root=True)
                else:
                    cleanup_errors.append({"stage":"owner_cleanup", "exit_code":cleanup.returncode})
        except Exception as error:
            cleanup_errors.append({"stage":"owner_cleanup", "error":type(error).__name__})
        try:
            if fixture is not None:
                fixture.close()
        except Exception as error:
            cleanup_errors.append({"stage":"fixture_close", "error":type(error).__name__})
        if cleanup_errors:
            passed = False
            private_file(root / "cleanup-errors.json", json.dumps(cleanup_errors).encode())
        if passed and not producer_only:
            shutil.rmtree(root)
        elif not passed:
            print(json.dumps({"schema":"dolgorae.native-successor-pipeline-failure/v1",
                              "owned_fixture_retained":str(root)}), file=sys.stderr)
        if cleanup_errors and not original_failure:
            raise ProbeCheckFailure("successor_cleanup_failed", {"errors":cleanup_errors})


class CollaborationResponses(ScriptedResponses):
    """Normal Caller Primary, two policy members, and correlated current pages."""
    def __init__(self, root: Path, approval_policy: str):
        super().__init__(concurrent_requests=True)
        self.root, self.approval_policy = root, approval_policy
        self.primary_stage = self.recovery_primary_stage = 0
        self.source_stage = self.recovery_stage = 0
        self.target_posts = self.blocker_posts = self.tail_posts = 0
        self.source = self.target = self.exchange_id = None
        self.initial_collection = self.recovery_collection = None
        self.await_started = self.await_seconds = None
        self.blocker_started = threading.Event()
        self.page_witnesses = []

    @staticmethod
    def checked_output(request, call, label, operation, *, primary=False, values=None):
        output = (code_results(request, call, [label]) if values is None else values)[label]
        if not isinstance(output, str) or len(output.encode()) > 65_536:
            raise ProbeCheckFailure("native_collaboration_return_shape_changed")
        value = json.loads(output)
        name = "dolgorae-orchestration-tool-v1.schema.json" if primary else "dolgorae-collaboration-tool-v1.schema.json"
        assert_valid(value, validator(PROTOCOL, name), "current native tool result")
        if value.get("operation") != operation:
            raise ProbeCheckFailure("native_collaboration_operation_refused", {"operation": value.get("operation"), "code": value.get("code")})
        return value

    def primary_output(self, request, call, labels, operation):
        values = code_results(request, call, labels)
        return [self.checked_output(request, call, label, operation, primary=True, values=values) for label in labels]

    @staticmethod
    def page_spec(recovery):
        body = COLLABORATION_RESULT.encode()
        offset = len(body) - COLLABORATION_PAGE_LIMIT if recovery else 0
        while body[offset] & 0xc0 == 0x80:
            offset += 1
        end = min(offset + COLLABORATION_PAGE_LIMIT, len(body))
        while end < len(body) and body[end] & 0xc0 == 0x80:
            end -= 1
        return offset, body[offset:end], end < len(body)

    def page_output(self, request, call, label):
        value = self.checked_output(request, call, label, "read_collaboration_result_result")
        body = COLLABORATION_RESULT.encode()
        if (call, label) not in (("a1-read", "initial-page"), ("a2-read", "recovered-page")):
            raise ProbeCheckFailure("current_source_page_call_changed")
        offset, content, truncated = self.page_spec(call == "a2-read")
        if (value["exchange_id"] != self.exchange_id or value["result_artifact_ref"] != self.artifact
                or value["content"].encode() != content
                or value["length"] != len(body) or value["sha256"] != hashlib.sha256(body).hexdigest()
                or value["offset"] != offset or value["truncated"] != truncated):
            raise ProbeCheckFailure("current_source_page_identity_changed")
        # Select the exact new output call and per-Turn label, not prior history.
        witness = {"call_id":call, "label":label, "exchange_id":self.exchange_id,
                   "result_artifact_ref":self.artifact, "byte_length":value["length"],
                   "sha256":value["sha256"], "offset":offset,
                   "content_byte_length":len(content), "content_sha256":hashlib.sha256(content).hexdigest(),
                   "truncated":truncated}
        self.page_witnesses.append(witness)
        private_file(self.root / f"current-page-{label}.json", json.dumps(witness).encode())
        return value

    def respond(self, request):
        users = [item for item in request.get("input", []) if item.get("type") == "message" and item.get("role") == "user"]
        text = "".join(block.get("text", "") for block in users[-1].get("content", [])) if users else ""
        accepted = json.JSONDecoder().raw_decode(text.split("\n\nAccepted task:\n", 1)[1])[0] if "\n\nAccepted task:\n" in text else {}
        marker = accepted.get("objective", "")
        target = "TASK027_NATIVE_TARGET" in text
        blocker = marker == "TASK027_B_BLOCKER"
        tail = marker == "TASK027_B_TAIL"
        with self.lock:
            self.posts += 1
            ordinal = self.posts
            bound = 18 if self.approval_policy == "user_approval_required" else 17
            if ordinal > bound:
                raise ProbeCheckFailure("native_collaboration_post_bound")
        if blocker:
            self.blocker_posts += 1
            if self.blocker_posts != 1:
                raise ProbeCheckFailure("native_blocker_replayed")
            self.blocker_started.set()
            deadline = time.monotonic() + 10
            while not (self.root / "source-await-registered.json").exists():
                if time.monotonic() >= deadline:
                    raise ProbeCheckFailure("native_collaboration_wait_barrier_timeout")
                time.sleep(.01)
            item = self.final("blocker-finished", "Original blocker completed.")
        elif target:
            self.target_posts += 1
            if self.target_posts != 1 or self.tail_posts:
                raise ProbeCheckFailure("waiting_consultation_did_not_precede_tail")
            item = self.final("consultation-final", COLLABORATION_RESULT)
        elif tail:
            self.tail_posts += 1
            if self.tail_posts != 1 or self.target_posts != 1:
                raise ProbeCheckFailure("original_tail_order_changed")
            item = self.final("tail-final", "Original tail completed.")
        else:
            with self.lock:
                needs_approval = (text == "TASK027_PRIMARY_INITIAL" and self.primary_stage == 1
                                  and self.approval_policy == "user_approval_required")
                if needs_approval:
                    admitted = self.primary_output(request, "p1-spawn", ["source-member", "target-member"], "request_specialist_result")
                needs_blocker = text == "TASK027_PRIMARY_INITIAL" and self.primary_stage == 3
                if needs_blocker:
                    self.primary_output(request, "p1-blocker", ["blocker"], "assign_specialist_task_result")
            if needs_approval:
                self.wait_approved_members([value["specialist_operation"] for value in admitted])
            if needs_blocker and not self.blocker_started.wait(10):
                raise ProbeCheckFailure("target_was_not_actually_busy")
            with self.lock:
                item = self.step(request, text, marker)
        identity = "collaboration-response-" + str(ordinal)
        return [{"type":"response.created", "response":{"id":identity}},
                {"type":"response.output_item.done", "item":item},
                {"type":"response.completed", "response":{"id":identity,
                 "usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0}}}]

    def wait_approved_members(self, operations):
        if any(operation["state"] != "awaiting_approval" for operation in operations):
            raise ProbeCheckFailure("user_approval_was_bypassed")
        deadline = json.loads((self.root / "primary-phase-deadline.json").read_text())["deadline_monotonic_ns"]
        if not isinstance(deadline, int) or isinstance(deadline, bool) or deadline <= 0:
            raise ProbeCheckFailure("original_approval_deadline_changed")
        while time.clock_gettime_ns(time.CLOCK_MONOTONIC) < deadline:
            members = []
            for operation in operations:
                path = self.root / f"approval-{operation['approval_request_id']}.json"
                if not path.exists():
                    break
                receipt = json.loads(path.read_text())
                if (receipt.get("approval_id") != operation["approval_request_id"]
                        or receipt.get("operation_id") != operation["operation_id"]
                        or receipt.get("protected_current_response") is not True
                        or receipt.get("operation_state") != "ready"
                        or uuid.UUID(receipt.get("original_sql_receipt", "")).version != 7
                        or uuid.UUID(receipt.get("member_run_id", "")).version != 7):
                    raise ProbeCheckFailure("original_approval_readiness_witness_changed")
                members.append(receipt["member_run_id"])
            if len(members) == len(operations):
                if len(set(members)) != len(operations):
                    raise ProbeCheckFailure("two_distinct_members_not_admitted")
                return
            time.sleep(.01)
        raise ProbeCheckFailure("original_approval_phase_expired")

    @staticmethod
    def final(identity, text):
        return {"type":"message", "role":"assistant", "id":identity,
                "content":[{"type":"output_text", "text":text}]}

    def assignment(self, member, objective):
        return {"operation":"assign_specialist_task", "target":{"run_id":member}, "objective":objective,
                "context_refs":[], "expected_output":["Bounded final answer"], "execution_intent":"read_only",
                "blocking":False, "deadline_seconds":60}

    def step(self, request, text, marker):
        if text == "TASK027_PRIMARY_INITIAL":
            stage = self.primary_stage
            if stage == 0:
                body = {"operation":"request_specialist", "role_ref":"general", "objective":"Execute bounded evidence task.",
                        "expected_output":["Bounded final answer"], "requested_access":"read_only", "deadline_seconds":60}
                result = code_call(request, "p1-spawn", [("source-member", "dolgorae_orchestration", body), ("target-member", "dolgorae_orchestration", body)])
            elif stage == 1:
                operations = self.primary_output(request, "p1-spawn", ["source-member", "target-member"], "request_specialist_result")
                self.operations = [v["specialist_operation"] for v in operations]
                if self.approval_policy == "user_approval_required":
                    if any(v["state"] != "awaiting_approval" for v in self.operations):
                        raise ProbeCheckFailure("user_approval_was_bypassed")
                    result = code_call(request, "p1-approved", [("approved-members", "dolgorae_orchestration", {"operation":"await_specialist_operations", "operation_ids":[v["operation_id"] for v in self.operations], "return_when":"all", "transport_wait_seconds":10})])
                else:
                    result = self.start_blocker(request)
                    self.primary_stage += 1
            elif stage == 2:
                value = self.primary_output(request, "p1-approved", ["approved-members"], "await_specialist_operations_result")[0]
                if value["pending"]:
                    raise ProbeCheckFailure("approved_members_not_ready")
                self.operations = value["operations"]
                result = self.start_blocker(request)
            elif stage == 3:
                self.primary_output(request, "p1-blocker", ["blocker"], "assign_specialist_task_result")
                if not self.blocker_started.is_set():
                    raise ProbeCheckFailure("target_was_not_actually_busy")
                result = code_call(request, "p1-queues", [("tail", "dolgorae_orchestration", self.assignment(self.target, "TASK027_B_TAIL")),
                                                        ("source", "dolgorae_orchestration", self.assignment(self.source, "TASK027_A_INITIAL"))])
            elif stage == 4:
                receipts = self.primary_output(request, "p1-queues", ["tail", "source"], "assign_specialist_task_result")
                if any(v["state"] != "accepted" for v in receipts):
                    raise ProbeCheckFailure("original_primary_admission_receipt_changed")
                result = self.final("primary-initial-final", "Original Primary assignments admitted.")
            else:
                raise ProbeCheckFailure("primary_initial_post_bound")
            self.primary_stage += 1
            return result
        if text == "TASK027_PRIMARY_RECOVERY":
            if self.recovery_primary_stage == 0:
                result = code_call(request, "p2-recovery", [("prior-tasks", "dolgorae_orchestration", {"operation":"collect_specialist_results", "after_sequence":0, "limit":16}),
                                                           ("recovery-source", "dolgorae_orchestration", self.assignment(self.source, "TASK027_A_RECOVERY"))])
            elif self.recovery_primary_stage == 1:
                values = code_results(request, "p2-recovery", ["prior-tasks", "recovery-source"])
                self.checked_output(request, "p2-recovery", "prior-tasks", "collect_specialist_results_result", primary=True, values=values)
                self.checked_output(request, "p2-recovery", "recovery-source", "assign_specialist_task_result", primary=True, values=values)
                result = self.final("primary-recovery-final", "Original results retained and recovery admitted.")
            else:
                raise ProbeCheckFailure("primary_recovery_post_bound")
            self.recovery_primary_stage += 1
            return result
        recovery = marker == "TASK027_A_RECOVERY"
        if marker not in ("TASK027_A_INITIAL", "TASK027_A_RECOVERY"):
            raise ProbeCheckFailure("accepted_original_task_missing")
        stage = self.recovery_stage if recovery else self.source_stage
        if not recovery and stage == 0:
            result = code_call(request, "a1-submit", [("submitted", "dolgorae_collaboration", {"operation":"submit_consultation", "target":{"run_id":self.target}, "objective":"TASK027_NATIVE_TARGET: return the bounded retained answer.", "context_refs":[], "expected_output":["A retained UTF-8 answer with distinct prefix and tail markers."], "blocking":False, "deadline_seconds":60})])
        elif not recovery and stage == 1:
            value = self.checked_output(request, "a1-submit", "submitted", "submit_consultation_result")
            self.exchange_id = value["exchange_id"]
            self.await_started = time.monotonic()
            result = code_call(request, "a1-await", [("awaited", "dolgorae_collaboration", {"operation":"await_consultations", "exchange_ids":[self.exchange_id], "return_when":"all", "transport_wait_seconds":10})])
        elif not recovery and stage == 2:
            value = self.checked_output(request, "a1-await", "awaited", "await_consultations_result")
            self.await_seconds = time.monotonic() - self.await_started
            completed = value["completed"]
            if (value["pending"] or len(completed) != 1 or completed[0]["exchange_id"] != self.exchange_id
                    or completed[0]["execution_state"] != "succeeded" or completed[0]["delivery_state"] != "pending"
                    or completed[0]["result_artifact_ref"] is None or self.await_seconds >= 9.5):
                raise ProbeCheckFailure("native_pending_wait_not_notified")
            self.artifact = completed[0]["result_artifact_ref"]
            result = code_call(request, "a1-collect", [("collected", "dolgorae_collaboration", {"operation":"collect_collaboration_results", "limit":1})])
        elif (not recovery and stage == 3) or (recovery and stage == 1):
            call, label = ("a2-collect", "recovered") if recovery else ("a1-collect", "collected")
            value = self.checked_output(request, call, label, "collect_collaboration_results_result")
            if value["results"] != [{"exchange_id":self.exchange_id,"execution_state":"succeeded","delivery_state":"acknowledged","result_artifact_ref":self.artifact,"safe_error_code":None}]:
                raise ProbeCheckFailure("original_collection_identity_changed")
            if recovery:
                self.recovery_collection = value
                if value != self.initial_collection:
                    raise ProbeCheckFailure("restart_original_collection_changed")
            else:
                self.initial_collection = value
            call, label = ("a2-read", "recovered-page") if recovery else ("a1-read", "initial-page")
            offset, _, _ = self.page_spec(recovery)
            result = code_call(request, call, [(label, "dolgorae_collaboration", {"operation":"read_collaboration_result", "exchange_id":self.exchange_id, "offset":offset, "limit":COLLABORATION_PAGE_LIMIT})])
        elif (not recovery and stage == 4) or (recovery and stage == 2):
            call, label = ("a2-read", "recovered-page") if recovery else ("a1-read", "initial-page")
            self.page_output(request, call, label)
            result = self.final("source-recovered-final" if recovery else "source-initial-final", "Verified the current correlated result page.")
        elif recovery and stage == 0:
            result = code_call(request, "a2-collect", [("recovered", "dolgorae_collaboration", {"operation":"collect_collaboration_results", "limit":1})])
        else:
            raise ProbeCheckFailure("source_post_bound")
        if recovery:
            self.recovery_stage += 1
        else:
            self.source_stage += 1
        return result

    def start_blocker(self, request):
        if any(v["state"] != "ready" for v in self.operations):
            raise ProbeCheckFailure("policy_member_not_ready")
        self.source, self.target = [v["specialist_run_id"] for v in self.operations]
        if self.source == self.target:
            raise ProbeCheckFailure("two_distinct_members_not_admitted")
        return code_call(request, "p1-blocker", [("blocker", "dolgorae_orchestration", self.assignment(self.target, "TASK027_B_BLOCKER"))])


class CallerPolicyWriterResponses(ScriptedResponses):
    """Private seven-POST owner/lifecycle case; task calls are synthetic HOST inputs."""
    def __init__(self, root: Path):
        super().__init__(concurrent_requests=True)
        self.root = root
        self.primary_a = self.primary_b = 0
        self.target_stages = {"A": 0, "B": 0}

    def marker(self, name, value):
        path = self.root / name
        pending = path.with_suffix(".pending")
        private_file(pending, json.dumps(value).encode())
        pending.replace(path)

    def wait_release(self, phase):
        deadline = json.loads((self.root / "primary-phase-deadline.json").read_text())["deadline_monotonic_ns"]
        if not isinstance(deadline, int) or isinstance(deadline, bool) or deadline <= 0:
            raise ProbeCheckFailure("writer_source_deadline_invalid")
        while time.clock_gettime_ns(time.CLOCK_MONOTONIC) < deadline:
            path = self.root / f"writer-{phase}-release.json"
            if path.exists():
                value = json.loads(path.read_text())
                if value != {"phase": phase, "task_terminal": True}:
                    raise ProbeCheckFailure("writer_source_release_changed")
                return
            time.sleep(.01)
        raise ProbeCheckFailure("writer_source_barrier_timeout")

    def respond(self, request):
        users = [v for v in request.get("input", []) if v.get("type") == "message" and v.get("role") == "user"]
        text = "".join(v.get("text", "") for v in users[-1].get("content", [])) if users else ""
        accepted = json.JSONDecoder().raw_decode(text.split("\n\nAccepted task:\n", 1)[1])[0] if "\n\nAccepted task:\n" in text else {}
        marker = accepted.get("objective")
        hold = None
        with self.lock:
            self.posts += 1
            ordinal = self.posts
            if ordinal > 7:
                raise ProbeCheckFailure("policy_writer_post_bound")
            if marker in ("TASK027_WRITER_TASK_A", "TASK027_WRITER_TASK_B"):
                phase = marker[-1]
                if (self.primary_a != 2 or (phase == "B" and (self.primary_b != 1 or self.target_stages["A"] != 2))):
                    raise ProbeCheckFailure("policy_writer_task_order_changed")
                stage = self.target_stages[phase]
                call = f"policy-writer-{phase}"
                if stage == 0:
                    item = code_call(request, call, [(call, "exec_command", {"cmd":f"printf 'writer-{phase}\\n' > caller-policy-writer-{phase}.txt; printf 'writer-{phase}\\n'", "yield_time_ms":1000})])
                elif stage == 1:
                    if terminal_output(code_results(request, call, [call])[call]) != (0, f"writer-{phase}\n"):
                        raise ProbeCheckFailure("policy_writer_native_command_unverified")
                    item = CollaborationResponses.final(f"policy-writer-final-{phase}", f"Bounded writer {phase} completed.")
                else:
                    raise ProbeCheckFailure("policy_writer_task_replayed")
                self.target_stages[phase] += 1
            elif text == "TASK027_WRITER_PRIMARY_A":
                if self.primary_a == 0:
                    item = code_call(request, "policy-writer-spawn", [("writer-member", "dolgorae_orchestration", {"operation":"request_specialist", "role_ref":"general", "objective":"Perform only the two accepted bounded Writer tasks.", "expected_output":["Bounded marker output"], "requested_access":"canonical_workspace_write", "deadline_seconds":60})])
                    self.primary_a = 1
                elif self.primary_a == 1:
                    value = CollaborationResponses.checked_output(request,"policy-writer-spawn","writer-member","request_specialist_result",primary=True)
                    operation = value["specialist_operation"]
                    if operation["state"] != "ready" or not operation.get("specialist_run_id") or operation.get("approval_request_id"):
                        raise ProbeCheckFailure("policy_writer_member_not_ready")
                    self.primary_a = 2
                    self.marker("writer-A-ready.json", {"phase":"A", "member":operation["specialist_run_id"], "operation":operation["operation_id"]})
                    item = CollaborationResponses.final("policy-writer-root-A", "First original Writer task completed.")
                    hold = "A"
                else:
                    raise ProbeCheckFailure("policy_writer_primary_replayed")
            elif text == "TASK027_WRITER_PRIMARY_B" and self.primary_a == 2 and self.target_stages["A"] == 2 and self.primary_b == 0:
                self.primary_b = 1
                self.marker("writer-B-ready.json", {"phase":"B"})
                item = CollaborationResponses.final("policy-writer-root-B", "Second task-attributed Writer completed.")
                hold = "B"
            else:
                raise ProbeCheckFailure("policy_writer_source_order_changed")
        # Source responses never retain the common request lock while targets run.
        if hold:
            self.wait_release(hold)
        identity = f"policy-writer-response-{ordinal}"
        return [{"type":"response.created", "response":{"id":identity}},
                {"type":"response.output_item.done", "item":item},
                {"type":"response.completed", "response":{"id":identity, "usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0}}}]


def caller_policy_writer_component_campaign(binary: Path, codex: Path) -> dict[str, Any]:
    return collaboration_component_campaign(binary, codex, _writer_case=True)


def collaboration_component_campaign(binary: Path, codex: Path, *, _writer_case: bool = False) -> dict[str, Any]:
    if os.environ.get(OPT_IN) != "1" or executable_identity(codex)["sha256"] != CODEX_SHA256:
        raise ValueError("collaboration component requires the opted-in qualified executable")
    root = Path(tempfile.mkdtemp(prefix="dcc-", dir=os.environ["TMPDIR"])).resolve()
    root.chmod(0o700)
    cases = []
    cargo = shlex.split(os.environ.get("CARGO", "cargo"))
    for name, command in [("main", cargo + ["build", "--locked", "--bin", "dolgorae"]),
                          ("driver", cargo + ["rustc", "--locked", "--example", "native_successor_pipeline", "--",
                                              "--cfg", "dolgorae_native_acceptance", "--cfg", "test"])]:
        build = subprocess.run(command, cwd=PROTOCOL.parents[1], capture_output=True, text=True, timeout=600)
        private_file(root / f"build-{name}.stdout", build.stdout.encode())
        private_file(root / f"build-{name}.stderr", build.stderr.encode())
        if build.returncode:
            raise ProbeCheckFailure("collaboration_component_build_failed", {"target":name})
    target = Path(os.environ.get("CARGO_TARGET_DIR", str(PROTOCOL.parents[1] / "target"))).resolve()
    if binary != (target / "debug/dolgorae").resolve():
        raise ValueError("collaboration census must use the current candidate main binary")
    for case_name in (("caller_policy_writer",) if _writer_case else ("fully_delegated", "user_approval_required")):
        approval = "fully_delegated" if _writer_case else case_name
        case = root / case_name
        case.mkdir(mode=0o700)
        fixture = foreground = None
        passed = False
        for name in ("home", "tmp", "config", "cache", "profile", "workspace"):
            (case / name).mkdir(mode=0o700)
        private_file(case / "fixture-owned", b"native-successor-pipeline/v1\n")
        private_file(case / "component-policy-case.json", json.dumps({"approval_policy":approval, **({"variation":"caller_policy_writer"} if _writer_case else {})}).encode())
        environment = isolated_environment(case)
        environment.update({OPT_IN:"1", "DOLGORAE_NATIVE_COLLABORATION_COMPONENT":"1",
                            "GIT_CONFIG_NOSYSTEM":"1", "GIT_CONFIG_GLOBAL":"/dev/null"})
        driver, current_main = case / "fixture-collaboration-driver", case / "current-main"
        for source, destination in ((target / "debug/examples/native_successor_pipeline",driver), (binary,current_main)):
            shutil.copyfile(source,destination)
            destination.chmod(0o700)
        try:
            checked_codex(codex, f"codex-cli {CODEX_VERSION}", environment)
            fixture = CallerPolicyWriterResponses(case) if _writer_case else CollaborationResponses(case,approval)
            private_file(case / "profile/config.toml", b'model = "gpt-5.6-luna"\nmodel_reasoning_effort = "low"\n'
                         + fixture_config(fixture.base_url) + b"\n[features]\nplugins = false\nshell_snapshot = false\n")
            subprocess.run(["/usr/bin/git","-C",str(case / "workspace"),"init","-q"],env=environment,check=True,capture_output=True,timeout=10)
            initialized = subprocess.run([str(current_main),"init",str(case / "workspace")],env=environment,capture_output=True,text=True,timeout=30)
            private_file(case / "init.stdout",initialized.stdout.encode())
            private_file(case / "init.stderr",initialized.stderr.encode())
            if initialized.returncode:
                raise ProbeCheckFailure("collaboration_main_initialization_failed")
            # This exact Child owns the foreground gateway and centralized runtime.
            foreground = subprocess.Popen([str(current_main),"serve","--socket",str(case / "gateway.sock")],env=environment,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
            import select
            if not select.select([foreground.stdout],[],[],10)[0]:
                raise ProbeCheckFailure("collaboration_foreground_readiness_timeout")
            ready_line = foreground.stdout.readline()
            ready = json.loads(ready_line)
            private_file(case / "foreground-ready.json",ready_line.encode())
            if not ready.get("ok") or ready.get("command") != "serve":
                raise ProbeCheckFailure("collaboration_foreground_not_ready")
            result = subprocess.run([str(driver),str(case),str(codex)],env=environment,capture_output=True,text=True,timeout=180)
            private_file(case / "driver.stdout",result.stdout.encode())
            private_file(case / "driver.stderr",result.stderr.encode())
            if result.returncode or fixture.failure:
                raise ProbeCheckFailure("collaboration_component_driver_failed",{"exit":result.returncode,"posts":fixture.posts,"fixture_failure":fixture.failure,"approval_policy":approval})
            value = json.loads(result.stdout)
            if _writer_case:
                if (not value.get("passed") or fixture.posts != 7 or fixture.primary_a != 2 or fixture.primary_b != 1
                        or fixture.target_stages != {"A":2,"B":2} or value.get("authority_kind") != "synthetic_trusted_host_fixture"):
                    raise ProbeCheckFailure("policy_writer_component_evidence_incomplete")
            else:
                expected = 18 if approval == "user_approval_required" else 17
                if (not value.get("passed") or fixture.posts != expected or fixture.target_posts != 1
                        or fixture.blocker_posts != 1 or fixture.tail_posts != 1 or fixture.source_stage != 5
                        or fixture.recovery_stage != 3 or fixture.recovery_primary_stage != 2
                        or fixture.recovery_collection != fixture.initial_collection or len(fixture.page_witnesses) != 2):
                    raise ProbeCheckFailure("collaboration_component_evidence_incomplete")
            # SIGTERM is sent only through the actual foreground Child handle.
            foreground.terminate()
            out, err = foreground.communicate(timeout=7)
            private_file(case / "foreground.stdout",out.encode())
            private_file(case / "foreground.stderr",err.encode())
            if foreground.returncode or out:
                raise ProbeCheckFailure("collaboration_foreground_shutdown_failed")
            value.update(loopback_posts=fixture.posts,external_provider_requests=0,
                         account_login_performed=False,real_account_credentials_copied=False,
                         codex_child_account_home_and_platform_tmpdir_exception=True,
                         real_home_file_access_absence_proven=False,
                         codex_sha256=CODEX_SHA256,driver_sha256=hashlib.sha256(driver.read_bytes()).hexdigest(),
                         main_sha256=hashlib.sha256(current_main.read_bytes()).hexdigest(),foreground_pid=foreground.pid,
                         owned_fixture_retained=str(case),
                         source_authority_kind="synthetic_trusted_host_fixture" if _writer_case else "native_model_call")
            if not _writer_case:
                value.update(notified_await_seconds=fixture.await_seconds,current_page_outputs=fixture.page_witnesses,
                         large_result_byte_length=len(COLLABORATION_RESULT.encode()),
                         large_result_sha256=hashlib.sha256(COLLABORATION_RESULT.encode()).hexdigest(),
                         model_page_scope="prefix_before_source_restart_and_tail_after",
                         scripted_model_input_causality_verified=True,external_model_cognition_proven=False)
            passed = True
        finally:
            cleanup_errors = []
            try:
                if not passed:
                    cleanup = subprocess.run([str(driver), "--cleanup", str(case)], env=environment,
                                             capture_output=True, text=True, timeout=120)
                    private_file(case / "failure-cleanup.stdout", cleanup.stdout.encode())
                    private_file(case / "failure-cleanup.stderr", cleanup.stderr.encode())
                    if cleanup.returncode:
                        cleanup_errors.append({"stage":"owner_cleanup", "exit_code":cleanup.returncode})
            except Exception as error:
                cleanup_errors.append({"stage":"owner_cleanup", "error":type(error).__name__})
            try:
                if foreground is not None and foreground.poll() is None:
                    foreground.terminate()
                    out, err = foreground.communicate(timeout=7)
                    private_file(case / "foreground.stdout", out.encode())
                    private_file(case / "foreground.stderr", err.encode())
                    if foreground.returncode:
                        cleanup_errors.append({"stage":"foreground_stop", "exit_code":foreground.returncode})
            except Exception as error:
                cleanup_errors.append({"stage":"foreground_stop", "error":type(error).__name__})
            try:
                if fixture is not None:
                    fixture.close()
            except Exception as error:
                cleanup_errors.append({"stage":"fixture_close", "error":type(error).__name__})
            try:
                inspected = subprocess.run([str(current_main), "runtime", "orphan", "inspect", "--owner-root-under", str(case)],
                                           env=environment, capture_output=True, text=True, timeout=30)
                private_file(case / "cleanup-census.stdout", inspected.stdout.encode())
                private_file(case / "cleanup-census.stderr", inspected.stderr.encode())
                if inspected.returncode:
                    raise ProbeCheckFailure("collaboration_owned_process_inspection_failed")
                observation = json.loads(inspected.stdout)
                require_fixture_process_absence(observation["data"])
                private_file(case / "cleanup-process-absence.json", json.dumps(observation).encode())
            except Exception as error:
                cleanup_errors.append({"stage":"strict_census", "error":type(error).__name__})
            private_file(case / "cleanup-errors.json", json.dumps(cleanup_errors).encode())
            if not passed:
                print(json.dumps({"schema":"dolgorae.native-collaboration-component-failure/v3",
                                  "owned_fixture_retained":str(case), "cleanup_errors":cleanup_errors}), file=sys.stderr)
            elif cleanup_errors:
                raise ProbeCheckFailure("collaboration_component_cleanup_incomplete", {"cleanup_errors":cleanup_errors})
        value["owned_process_absence_verified"] = True
        private_file(case / "result.json", json.dumps(value).encode())
        cases.append(value)
    combined = {"schema":"dolgorae.native-collaboration-component-cases/v1","passed":len(cases)==(1 if _writer_case else 2),"cases":cases,"owned_fixture_retained":str(root)}
    private_file(root / "result.json",json.dumps(combined).encode())
    return combined

def successor_startup_component_campaign(binary: Path, codex: Path) -> dict[str, Any]:
    """Actual supported successor birth, with no model Turn or consumer run."""
    if os.environ.get(OPT_IN) != "1" or executable_identity(codex)["sha256"] != CODEX_SHA256:
        raise ValueError("successor startup requires the opted-in qualified executable")
    # Physical short root is required by native Unix socket pathname limits.
    root = Path(tempfile.mkdtemp(prefix="dsu-", dir="/private/tmp")).resolve()
    root.chmod(0o700)
    fixture = None
    driver = None
    environment = None
    passed = False
    try:
        for name in ("home", "tmp", "config", "cache", "profile", "workspace"):
            (root / name).mkdir(mode=0o700)
        private_file(root / "fixture-owned", b"native-successor-pipeline/v1\n")
        environment = isolated_environment(root)
        environment.update({OPT_IN:"1", "DOLGORAE_NATIVE_SUCCESSOR_STARTUP_COMPONENT":"1",
                            "GIT_CONFIG_NOSYSTEM":"1", "GIT_CONFIG_GLOBAL":"/dev/null"})
        checked_codex(codex, f"codex-cli {CODEX_VERSION}", environment)
        cargo = shlex.split(os.environ.get("CARGO", "cargo"))
        for name, command in [("main", cargo + ["build", "--locked", "--bin", "dolgorae"]),
                              ("driver", cargo + ["rustc", "--locked", "--example", "native_successor_pipeline", "--",
                                                   "--cfg", "dolgorae_native_acceptance", "--cfg", "test"])]:
            build = subprocess.run(command, cwd=PROTOCOL.parents[1], capture_output=True, text=True, timeout=600)
            private_file(root / f"build-{name}.stdout", build.stdout.encode())
            private_file(root / f"build-{name}.stderr", build.stderr.encode())
            if build.returncode:
                raise ProbeCheckFailure("successor_startup_build_failed", {"target":name})
        target = Path(os.environ.get("CARGO_TARGET_DIR", str(PROTOCOL.parents[1] / "target"))).resolve()
        if binary != (target / "debug/dolgorae").resolve():
            raise ValueError("startup census must use the current candidate main binary")
        driver = root / "fixture-startup-driver"
        shutil.copyfile(target / "debug/examples/native_successor_pipeline", driver)
        driver.chmod(0o700)
        fixture = ScriptedResponses()
        private_file(root / "profile/config.toml", b'model = "gpt-5.6-luna"\nmodel_reasoning_effort = "low"\n'
                     + fixture_config(fixture.base_url) + b"\n[features]\nplugins = false\nshell_snapshot = false\n")
        result = subprocess.run([str(driver), str(root), str(codex)], env=environment,
                                capture_output=True, text=True, timeout=180)
        private_file(root / "driver.stdout", result.stdout.encode())
        private_file(root / "driver.stderr", result.stderr.encode())
        if result.returncode or fixture.failure:
            raise ProbeCheckFailure("successor_startup_driver_failed", {"exit":result.returncode,"posts":fixture.posts,"fixture_failure":fixture.failure})
        value = json.loads(result.stdout)
        if not value.get("passed") or fixture.posts != 0:
            raise ProbeCheckFailure("successor_startup_evidence_incomplete")
        inspected = subprocess.run([str(binary), "runtime", "orphan", "inspect", "--owner-root-under", str(root)],
                                   env=environment, capture_output=True, text=True, timeout=30)
        private_file(root / "owned-process-absence.stdout", inspected.stdout.encode())
        private_file(root / "owned-process-absence.stderr", inspected.stderr.encode())
        if inspected.returncode:
            raise ProbeCheckFailure("successor_startup_census_failed")
        observation = json.loads(inspected.stdout)
        require_fixture_process_absence(observation["data"])
        value.update(loopback_posts=fixture.posts, external_provider_requests=0,
                     real_account_or_credentials_used=False, codex_sha256=CODEX_SHA256,
                     driver_sha256=hashlib.sha256(driver.read_bytes()).hexdigest(),
                     census_binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                     owned_fixture_retained=str(root), owned_process_absence_verified=True)
        private_file(root / "result.json", json.dumps(value).encode())
        passed = True
        return value
    finally:
        if not passed and driver is not None and environment is not None:
            cleanup = subprocess.run([str(driver), "--cleanup", str(root)], env=environment,
                                     capture_output=True, text=True, timeout=120)
            private_file(root / "cleanup.stdout", cleanup.stdout.encode())
            private_file(root / "cleanup.stderr", cleanup.stderr.encode())
            private_file(root / "cleanup-status.json", json.dumps({"exit":cleanup.returncode}).encode())
        if fixture is not None:
            fixture.close()
        if not passed:
            print(json.dumps({"schema":"dolgorae.native-successor-startup-failure/v1",
                              "owned_fixture_retained":str(root)}), file=sys.stderr)


class SchedulerResponses(ScriptedResponses):
    """Four accepted tasks, with actual native writes and retained instructions."""
    def __init__(self, root: Path):
        super().__init__(concurrent_requests=True)
        self.root = root
        self.issued = set()
        self.completed = set()
        self.markers = {"scheduler-managed-write", "scheduler-external-write",
                        "scheduler-pool-sibling", "scheduler-pool-resume"}

    def respond(self, request):
        with self.lock:
            self.posts += 1
            if self.posts > 6:
                raise ProbeCheckFailure("scheduler_component_post_bound")
            users = [item for item in request.get("input", [])
                     if item.get("type") == "message" and item.get("role") == "user"]
            text = "".join(block.get("text", "") for block in users[-1].get("content", [])) if users else ""
            if "\n\nAccepted task:\n" not in text:
                raise ProbeCheckFailure("scheduler_accepted_task_missing")
            task, end = json.JSONDecoder().raw_decode(text.split("\n\nAccepted task:\n", 1)[1])
            marker = task.get("brief")
            if marker not in self.markers or marker in self.completed:
                raise ProbeCheckFailure("scheduler_task_identity_changed")
            captured = json.dumps([request.get("instructions"), request.get("input")])
            if GENERAL not in captured or "Edited authoring source" in captured:
                raise ProbeCheckFailure("scheduler_retained_role_not_used")
            private_file(self.root / f"scheduler-http-{self.posts}.json", json.dumps(request).encode())
            call = "scheduler-call-" + marker
            if marker.endswith("-write") and marker not in self.issued:
                self.issued.add(marker)
                path = "scheduler-managed.txt" if marker == "scheduler-managed-write" else "scheduler-external.txt"
                item = code_call(request, call, [(call, "exec_command", {"cmd":f"printf 'written\\n' > {path}; printf 'written\\n'", "yield_time_ms":1000})])
            else:
                if marker.endswith("-write"):
                    if terminal_output(code_results(request, call, [call])[call]) != (0, "written\n"):
                        raise ProbeCheckFailure("scheduler_native_write_unverified")
                self.completed.add(marker)
                item = {"type":"message", "role":"assistant", "id":call,
                        "content":[{"type":"output_text", "text":"Bounded accepted task completed."}]}
            identity = "scheduler-response-" + str(self.posts)
            return [{"type":"response.created", "response":{"id":identity}},
                    {"type":"response.output_item.done", "item":item},
                    {"type":"response.completed", "response":{"id":identity,
                     "usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0}}}]


def scheduler_component_campaign(binary: Path, codex: Path) -> dict[str, Any]:
    if os.environ.get(OPT_IN) != "1" or executable_identity(codex)["sha256"] != CODEX_SHA256:
        raise ValueError("scheduler component requires the opted-in qualified executable")
    correction = _scheduler_component_phase(binary, codex, correction=True)
    normal = _scheduler_component_phase(binary, codex, correction=False)
    combined = dict(normal, correction_subphase=correction, both_subphases_verified=True)
    private_file(Path(normal["owned_fixture_retained"]) / "combined-result.json", json.dumps(combined).encode())
    return combined


def _scheduler_component_phase(binary: Path, codex: Path, *, correction: bool) -> dict[str, Any]:
    if os.environ.get(OPT_IN) != "1" or executable_identity(codex)["sha256"] != CODEX_SHA256:
        raise ValueError("scheduler component requires the opted-in qualified executable")
    # Native Unix socket paths require a short physical fixture root.
    root = Path(tempfile.mkdtemp(prefix="dsc-", dir="/private/tmp")).resolve()
    root.chmod(0o700)
    fixture = None
    driver = None
    environment = None
    passed = False
    try:
        for name in ("home", "tmp", "config", "cache", "profile", "profile-members", "workspace"):
            (root / name).mkdir(mode=0o700)
        private_file(root / "fixture-owned", b"native-successor-pipeline/v1\n")
        if correction:
            private_file(root / "scheduler-correction-phase", b"dolgorae.scheduler-correction-phase/v1\n")
        environment = isolated_environment(root)
        environment.update({OPT_IN:"1", "DOLGORAE_NATIVE_SCHEDULER_COMPONENT":"1",
                            "GIT_CONFIG_NOSYSTEM":"1", "GIT_CONFIG_GLOBAL":"/dev/null"})
        checked_codex(codex, f"codex-cli {CODEX_VERSION}", environment)
        cargo = shlex.split(os.environ.get("CARGO", "cargo"))
        for name, command in [("main", cargo + ["build", "--locked", "--bin", "dolgorae"]),
                              ("driver", cargo + ["rustc", "--locked", "--example", "native_successor_pipeline", "--",
                                                   "--cfg", "dolgorae_native_acceptance", "--cfg", "test"])]:
            build = subprocess.run(command, cwd=PROTOCOL.parents[1], capture_output=True, text=True, timeout=600)
            private_file(root / f"build-{name}.stdout", build.stdout.encode())
            private_file(root / f"build-{name}.stderr", build.stderr.encode())
            if build.returncode:
                raise ProbeCheckFailure("scheduler_component_build_failed", {"target":name})
        target = Path(os.environ.get("CARGO_TARGET_DIR", str(PROTOCOL.parents[1] / "target"))).resolve()
        if binary != (target / "debug/dolgorae").resolve():
            raise ValueError("scheduler census must use the current candidate main binary")
        driver = root / "fixture-scheduler-driver"
        shutil.copyfile(target / "debug/examples/native_successor_pipeline", driver)
        driver.chmod(0o700)
        fixture = SchedulerResponses(root)
        private_file(root / "profile/config.toml", b'model = "gpt-5.6-luna"\nmodel_reasoning_effort = "low"\n'
                     + fixture_config(fixture.base_url) + b"\n[features]\nplugins = false\nshell_snapshot = false\n")
        private_file(root / "profile-members/config.toml", (root / "profile/config.toml").read_bytes())
        result = subprocess.run([str(driver), str(root), str(codex)], env=environment,
                                capture_output=True, text=True, timeout=300)
        private_file(root / "driver.stdout", result.stdout.encode())
        private_file(root / "driver.stderr", result.stderr.encode())
        if result.returncode or fixture.failure:
            raise ProbeCheckFailure("scheduler_component_driver_failed", {"exit":result.returncode,"posts":fixture.posts,"fixture_failure":fixture.failure})
        value = json.loads(result.stdout)
        if correction:
            if not value.get("passed") or value.get("phase") != "correction" or not value.get("activation_compensation", {}).get("passed") or fixture.posts != 0:
                raise ProbeCheckFailure("scheduler_correction_evidence_incomplete")
        elif not value.get("passed") or value.get("phase") != "normal" or fixture.posts != 6 or fixture.completed != fixture.markers:
            raise ProbeCheckFailure("scheduler_component_evidence_incomplete")
        inspected = subprocess.run([str(binary), "runtime", "orphan", "inspect", "--owner-root-under", str(root)], env=environment, capture_output=True, text=True, timeout=30)
        if inspected.returncode:
            raise ProbeCheckFailure("scheduler_owned_process_inspection_failed")
        observation=json.loads(inspected.stdout)
        require_fixture_process_absence(observation["data"])
        private_file(root / "owned-process-absence.json", json.dumps(observation).encode())
        value.update(loopback_posts=fixture.posts, external_provider_requests=0,
                     real_account_or_credentials_used=False, codex_sha256=CODEX_SHA256,
                     driver_sha256=hashlib.sha256(driver.read_bytes()).hexdigest(),
                     census_binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                     owned_fixture_retained=str(root), owned_process_absence_verified=True)
        private_file(root / "result.json", json.dumps(value).encode())
        passed = True
        return value
    finally:
        if not passed and driver is not None and driver.is_file() and environment is not None:
            cleanup = subprocess.run([str(driver), "--cleanup", str(root)], env=environment,
                                     capture_output=True, text=True, timeout=120)
            private_file(root / "cleanup.stdout", cleanup.stdout.encode())
            private_file(root / "cleanup.stderr", cleanup.stderr.encode())
            private_file(root / "cleanup-status.json", json.dumps({"exit":cleanup.returncode}).encode())
        if fixture is not None:
            fixture.close()
        if not passed:
            print(json.dumps({"schema":"dolgorae.native-scheduler-component-failure/v1",
                              "owned_fixture_retained":str(root)}), file=sys.stderr)

def main() -> int:
    if os.environ.get(OPT_IN) != "1":
        print("integrated native acceptance requires explicit opt-in")
        return 2
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--codex", type=Path, required=True)
    parser.add_argument("--caller-driver", type=Path, required=True)
    parser.add_argument("--podway", type=Path, default=Path(os.environ["DOLGORAE_TEST_PODWAY_BIN"]) if os.environ.get("DOLGORAE_TEST_PODWAY_BIN") else None)
    parser.add_argument("--aquarium-source", type=Path, default=Path(os.environ["DOLGORAE_TEST_AQUARIUM_SOURCE"]) if os.environ.get("DOLGORAE_TEST_AQUARIUM_SOURCE") else None)
    parser.add_argument("--default-feature-startup-only", action="store_true")
    parser.add_argument("--ordinary-close-only", action="store_true")
    parser.add_argument("--successor-only", action="store_true")
    parser.add_argument("--managed-producer-only", action="store_true")
    parser.add_argument("--successor-driver", type=Path)
    parser.add_argument("--successor-driver-sha256")
    parser.add_argument("--collaboration-component-only", action="store_true")
    parser.add_argument("--successor-startup-only", action="store_true")
    parser.add_argument("--scheduler-component-only", action="store_true")
    args = parser.parse_args()
    try:
        if args.managed_producer_only:
            if args.successor_startup_only or args.scheduler_component_only or args.collaboration_component_only or args.default_feature_startup_only or args.successor_only or args.ordinary_close_only:
                raise ValueError("managed producer export is a distinct mode")
            result = successor_campaign(args.binary.resolve(strict=True), args.codex.resolve(strict=True), args.podway, args.aquarium_source,
                frozen_driver=args.successor_driver, frozen_driver_sha256=args.successor_driver_sha256, producer_only=True)
        elif args.successor_startup_only:
            if args.scheduler_component_only or args.collaboration_component_only or args.default_feature_startup_only or args.successor_only or args.ordinary_close_only:
                raise ValueError("successor startup is a distinct component mode")
            result = successor_startup_component_campaign(args.binary.resolve(strict=True), args.codex.resolve(strict=True))
        elif args.scheduler_component_only:
            if args.collaboration_component_only or args.default_feature_startup_only or args.successor_only or args.ordinary_close_only:
                raise ValueError("scheduler component is a distinct mode")
            result = scheduler_component_campaign(args.binary.resolve(strict=True), args.codex.resolve(strict=True))
        elif args.collaboration_component_only:
            if args.default_feature_startup_only or args.successor_only or args.ordinary_close_only:
                raise ValueError("collaboration component is a distinct mode")
            result = collaboration_component_campaign(args.binary.resolve(strict=True), args.codex.resolve(strict=True))
        elif args.default_feature_startup_only:
            if args.successor_only or args.ordinary_close_only:
                raise ValueError("default-feature startup is a distinct mode")
            with patch.dict(os.environ, {caller.OPT_IN:"1"}):
                result=caller.default_feature_campaign(args.caller_driver.resolve(strict=True),args.codex.resolve(strict=True),binary=args.binary.resolve(strict=True))
        elif args.successor_only:
            result = successor_campaign(args.binary.resolve(strict=True), args.codex.resolve(strict=True), args.podway, args.aquarium_source, frozen_driver=args.successor_driver, frozen_driver_sha256=args.successor_driver_sha256)
        else:
            result = campaign(args.binary.resolve(strict=True), args.codex.resolve(strict=True),
                              args.caller_driver.resolve(strict=True), ordinary_close_only=args.ordinary_close_only)
            if not args.ordinary_close_only:
                result["successor_writer_acceptance"] = successor_campaign(args.binary.resolve(strict=True), args.codex.resolve(strict=True), args.podway, args.aquarium_source, frozen_driver=args.successor_driver, frozen_driver_sha256=args.successor_driver_sha256)
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
