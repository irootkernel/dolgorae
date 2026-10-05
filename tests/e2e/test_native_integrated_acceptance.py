#!/usr/bin/env python3
"""Offline authorization, instruction and return-correlation checks for the integrated probe."""
from __future__ import annotations

import base64
import json
import os
import threading
import tempfile
from pathlib import Path
from unittest.mock import patch

import run_native_integrated_acceptance as probe
from schema_support import assert_valid, validator


def rejected(operation) -> None:
    try:
        operation()
    except (ValueError, RuntimeError):
        return
    raise AssertionError("unsafe native fixture operation was admitted")


def fixture(*, request_specialist: bool = False) -> probe.IntegratedResponses:
    value = object.__new__(probe.IntegratedResponses)
    value.lock = threading.Lock()
    value.posts, value.steps, value.failure = 0, 0, None
    value.prepare("bounded-task", probe.REVIEWER, expected=b"candidate\n",
                  request_specialist=request_specialist)
    return value


def first_request() -> dict:
    return {"instructions": "\n".join((probe.CORE_PREFIX, probe.COMMON, probe.REVIEWER)),
            "input": [{"type": "message", "role": "user", "content": [
                {"type": "input_text", "text": probe.PROFILE},
                {"type": "input_text", "text": "bounded-task"}]}]}


def returned(value: probe.IntegratedResponses, *, content: bytes = b"candidate\n",
             call_id: str | None = None) -> dict:
    output = {"content": base64.b64encode(content).decode(), "receipt": {
        "path": "file.txt", "side": "after", "byte_offset": 0, "byte_length": 10,
        "receipt_id": "018f0000-0000-7000-8000-000000000001"}}
    return {"input": [{"type": "function_call_output", "call_id": call_id or value.call_id,
                       "output": json.dumps(output)}]}


def main() -> int:
    root = Path("/owned-integrated-fixture")
    status = {"profile": "integrated", "server_key": "a" * 64, "state": {
        "server_key": "a" * 64, "snapshot": {"canonical_codex_home": str(root / "profile"),
        "executable_identity": {"sha256": probe.CODEX_SHA256}}}}
    membership = {"complete": True, "server_key": "a" * 64}
    effects = []

    def cleanup_call(*args):
        if args[:3] == ("profile", "server", "status"):
            return status
        if args[:3] == ("profile", "membership", "verify"):
            return membership
        effects.append(args)
        return {}

    probe.stop_fixture_profile(cleanup_call, root, root / "operator")
    assert effects == [("profile", "server", "stop", "integrated", "--operator-file",
                        str(root / "operator"), "--interrupt", "--confirm-server-key", "a" * 64)]
    effects.clear()
    for field, replacement in (("canonical_codex_home", "/other/profile"),
                               ("executable_identity", {"sha256": "0" * 64})):
        snapshot = status["state"]["snapshot"]
        original = snapshot[field]
        snapshot[field] = replacement
        rejected(lambda: probe.stop_fixture_profile(cleanup_call, root, root / "operator"))
        snapshot[field] = original
        assert effects == []
    status["state"]["server_key"] = "b" * 64
    rejected(lambda: probe.stop_fixture_profile(cleanup_call, root, root / "operator"))
    status["state"]["server_key"] = "a" * 64
    for field, replacement in (("complete", False), ("server_key", "b" * 64)):
        original = membership[field]
        membership[field] = replacement
        rejected(lambda: probe.stop_fixture_profile(cleanup_call, root, root / "operator"))
        membership[field] = original
        assert effects == []
    probe.require_fixture_process_absence({"candidates": []})
    probe.require_fixture_process_absence({"candidates": [{"verdict": "absent"}]})
    for verdict in ("owned", "orphan", "unverifiable"):
        rejected(lambda: probe.require_fixture_process_absence({"candidates": [{"verdict": verdict}]}))

    instructions = json.loads((probe.PROTOCOL / "examples/workspace-execution-instructions.valid.json").read_text())
    captured = {"schema_version": 3, "selected_profile": "fixture", "global_profile_binding_sha256": "a" * 64,
                "model": "fixture-model", "default_effort": "low",
                "purpose": {"kind": "review", "external_label": None}, "required_capabilities": [],
                "role_reference": "general", "instructions": instructions,
                "execution_lane": "dedicated", "required_assurance": "best_effort_personal_alpha",
                "native_subagent_policy": "enabled"}
    configuration_schema = validator(probe.PROTOCOL, "dolgorae-workspace-execution-v1.schema.json",
                                     "#/$defs/agent_configuration")
    assert_valid(captured, configuration_schema, "serialized caller child configuration")
    stored = json.loads(json.dumps(captured))
    assert probe.same_captured_configuration(captured, stored)
    for field, replacement in (("selected_profile", "other"), ("model", "other-model"),
                               ("schema_version", 2)):
        assert not probe.same_captured_configuration(captured, dict(stored, **{field: replacement}))
    wrong = dict(stored)
    del wrong["global_profile_binding_sha256"]
    assert not probe.same_captured_configuration(captured, wrong)
    assert not probe.same_captured_configuration(captured, dict(stored, extra="unaccepted"))
    wrong = json.loads(json.dumps(stored))
    wrong["instructions"]["role"]["body"] = "Changed Role instructions."
    assert not probe.same_captured_configuration(wrong, wrong)
    with tempfile.TemporaryDirectory(prefix="integrated-files-", dir=os.environ["TMPDIR"]) as temporary:
        home = Path(temporary) / "home"
        state_root = home / ".dolgorae/workspaces/fixture"
        state_root.mkdir(parents=True)
        probe.assert_no_source_isolation(home)
        for forbidden in ("review-targets", "worktrees", "orchestration/isolated"):
            path = state_root / forbidden
            path.mkdir(parents=True)
            rejected(lambda: probe.assert_no_source_isolation(home))
            path.rmdir()
            probe.assert_no_source_isolation(home)
        path = Path(temporary) / "candidate"
        probe.private_file(path, b"original long candidate\n")
        for content in (b"indexed\n", b"worktree\n", b"changed\n", b"workspace\n"):
            probe.overwrite_fixture_file(path, content)
            assert path.read_bytes() == content and path.stat().st_mode & 0o777 == 0o600
        try:
            probe.overwrite_fixture_file(Path(temporary) / "missing", b"unapproved\n")
        except FileNotFoundError:
            pass
        else:
            raise AssertionError("fixture overwrite created a missing file")
        link = Path(temporary) / "link"
        link.symlink_to(path)
        try:
            probe.overwrite_fixture_file(link, b"unapproved\n")
        except OSError:
            pass
        else:
            raise AssertionError("fixture overwrite followed a symlink")
        assert path.read_bytes() == b"workspace\n"
    review_schema = validator(probe.PROTOCOL, "dolgorae-specialist-review-tool-v4.schema.json",
                              "#/$defs/review_request")
    for kind, revision in (("workspace", None), ("staged", None), ("dirty", None),
                           ("head", None), ("commit", "HEAD"),
                           ("range", "HEAD~1..HEAD"), ("range", "HEAD~1...HEAD")):
        assert_valid(probe.review_request(kind, "bounded-task", revision), review_schema,
                     "integrated production review request")
    assert not review_schema.is_valid(probe.review_request("workspace", "bounded-task", "HEAD"))
    busy = json.loads((probe.PROTOCOL / "examples/error-contract-v4-writer_busy.valid.json").read_text())
    error_schema = validator(probe.PROTOCOL, "dolgorae-error-contract-v4.json")
    close_error = {"code": "OUTCOME_UNKNOWN", "message": "Explicit reconciliation required",
        "retryable": False, "details": {"schema": "dolgorae-session-close-error-details/v1",
            "run_id": "018f0000-0000-7000-8000-000000000002", "operation": "run.reconcile",
            "operation_id": "018f0000-0000-7000-8000-000000000003", "progress": "outcome_unknown",
            "interrupt": False, "required_action": "reconcile_run"}}
    assert_valid(close_error, error_schema, "modern retained aggregate error")
    for field in ("operation_id", "progress", "interrupt", "required_action"):
        wrong = json.loads(json.dumps(close_error))
        del wrong["details"][field]
        assert not error_schema.is_valid(wrong)
    wrong = json.loads(json.dumps(close_error))
    wrong["details"]["operation_id"] = None
    assert not error_schema.is_valid(wrong)
    wrong = json.loads(json.dumps(close_error))
    wrong["details"]["private_diagnostic"] = "provider text"
    assert not error_schema.is_valid(wrong)
    closed = {"run_id": close_error["details"]["run_id"], "lifecycle": "closed", "writer_state": "none",
        "session_close": {"operation_id": close_error["details"]["operation_id"],
                          "interrupt": False, "progress": "completed"}}
    modern_schema = validator(probe.PROTOCOL, "dolgorae-machine-v4.schema.json")
    envelope = json.loads((probe.PROTOCOL / "examples/workspace-machine-v4-start.valid.json").read_bytes())
    for verb in ("status", "close", "recover", "reconcile"):
        envelope["command"] = f"run.{verb}"
        envelope["data"].update(lifecycle="closed", session_close=closed["session_close"],
                                effective_access="unknown", policy_verification="unverified",
                                writer_state="none", writer_generation=None)
        assert_valid(envelope, modern_schema, "modern aggregate terminal command")
    assert probe.require_closed_session(closed, closed["run_id"]) == close_error["details"]["operation_id"]
    aborted = json.loads(json.dumps(closed))
    aborted["session_close"].update(interrupt=True, progress="aborted")
    assert probe.require_closed_session(aborted, closed["run_id"], interrupt=True) == close_error["details"]["operation_id"]
    rejected(lambda: probe.require_closed_session(aborted, closed["run_id"]))
    for field, replacement in (("lifecycle", "idle"), ("writer_state", "active"),
                               ("session_close", None), ("run_id", "foreign")):
        wrong = dict(closed, **{field: replacement})
        rejected(lambda: probe.require_closed_session(wrong, closed["run_id"]))
    for field, replacement in (("operation_id", None), ("interrupt", True), ("progress", "settling")):
        wrong = json.loads(json.dumps(closed))
        wrong["session_close"][field] = replacement
        rejected(lambda: probe.require_closed_session(wrong, closed["run_id"]))
    assert_valid(busy, error_schema, "caller writer-busy error")
    for field in ("requested_run_id", "writer_generation", "controller_kind"):
        wrong = json.loads(json.dumps(busy))
        del wrong["details"][field]
        assert not error_schema.is_valid(wrong)
    wrong = json.loads(json.dumps(busy))
    wrong["details"]["instruction_body"] = "private instructions"
    assert not error_schema.is_valid(wrong)
    wrong = json.loads(json.dumps(busy))
    wrong["retryable"] = False
    assert not error_schema.is_valid(wrong)
    startup = json.loads((probe.PROTOCOL / "examples/error-contract-v4-caller_startup_internal.valid.json").read_text())
    assert_valid(startup, error_schema, "caller post-allocation internal refusal")
    startup_schema = validator(probe.PROTOCOL, "dolgorae-error-contract-v4.json", "#/$defs/caller_startup_internal")
    for field in ("invariant", "run_id"):
        wrong = json.loads(json.dumps(startup))
        del wrong["details"][field]
        assert not startup_schema.is_valid(wrong)
    wrong = json.loads(json.dumps(startup))
    wrong["details"]["reason"] = "private storage diagnostic"
    assert not startup_schema.is_valid(wrong)
    for name, fragment in (("external-engagement-v4-hire.valid.json", "hire_request"),
                           ("external-engagement-v4-assign.valid.json", "assign_request")):
        request = json.loads((probe.PROTOCOL / "examples" / name).read_text())
        assert_valid(request, validator(probe.PROTOCOL,
                     "dolgorae-external-specialist-facade-v4.schema.json", f"#/$defs/{fragment}"),
                     "integrated facade request")
    with patch.dict(os.environ, {}, clear=True), patch.object(probe, "executable_identity") as identity:
        assert probe.main() == 2
        rejected(lambda: probe.campaign(Path("unused"), Path("unused"), Path("unused")))
        identity.assert_not_called()
    with patch.dict(os.environ, {probe.OPT_IN: "1"}), patch.object(
        probe, "executable_identity", return_value={"sha256": "0" * 64}
    ), patch.object(probe.tempfile, "mkdtemp") as allocation:
        rejected(lambda: probe.campaign(Path("unused"), Path("unused"), Path("unused")))
        allocation.assert_not_called()
    value = fixture()
    request = first_request()
    request["instructions"] += "\n" + probe.AMBIENT[0]
    rejected(lambda: value.respond(request))
    value = fixture()
    rejected(lambda: value.respond({"input": first_request()["input"]}))
    value = fixture()
    request = first_request()
    request["input"][0]["content"] = [{"type": "input_text", "text": "bounded-task"}]
    rejected(lambda: value.respond(request))
    value = fixture()
    value.respond(first_request())
    rejected(lambda: value.respond(returned(value, call_id="foreign-call")))
    value = fixture()
    value.respond(first_request())
    rejected(lambda: value.respond(returned(value, content=b"substitute")))
    value = fixture()
    issued = value.respond(first_request())[1]["item"]
    assert issued["name"] == "dolgorae_candidate_read"
    completed = value.respond(returned(value))[1]["item"]
    verdict = json.loads(completed["content"][0]["text"])
    assert verdict["criterion_assessments"][0]["evidence"][0]["read_receipt_id"]
    assert value.steps == 1 and value.posts == 2 and value.pending is False
    rejected(lambda: value.respond(returned(value)))
    primary_result = {"operation": "request_specialist_result", "specialist_operation": {
        "operation_id": "018f0000-0000-7000-8000-000000000002", "role_ref": "reviewer",
        "state": "ready", "specialist_run_id": "018f0000-0000-7000-8000-000000000003",
        "approval_request_id": None, "reused": False, "safe_error_code": None}}
    value = fixture(request_specialist=True)
    issued = value.respond(first_request())[1]["item"]
    assert issued["name"] == "dolgorae_orchestration"
    assert_valid(json.loads(issued["arguments"]), validator(probe.PROTOCOL,
        "dolgorae-orchestration-tool-v1.schema.json", "#/$defs/request_specialist_request"),
        "integrated Primary child request")

    def primary_return(value, response):
        return {"input": [{"type": "function_call_output", "call_id": value.call_id,
                           "output": json.dumps(response)}]}

    value.respond(primary_return(value, primary_result))
    assert value.specialist_run_id == primary_result["specialist_operation"]["specialist_run_id"]
    assert value.steps == 1 and value.posts == 2 and value.pending is False
    for field, replacement in (("state", "awaiting_approval"), ("specialist_run_id", None),
                               ("role_ref", "foreign"), ("reused", True),
                               ("approval_request_id", "018f0000-0000-7000-8000-000000000004"),
                               ("safe_error_code", "INTERNAL_ERROR")):
        value = fixture(request_specialist=True)
        value.respond(first_request())
        response = json.loads(json.dumps(primary_result))
        response["specialist_operation"][field] = replacement
        rejected(lambda: value.respond(primary_return(value, response)))
        assert value.steps == 0 and value.pending is True
    value = fixture(request_specialist=True)
    value.respond(first_request())
    rejected(lambda: value.respond(primary_return(value, {"operation": "orchestration_error",
        "code": "ORCHESTRATION_TRANSPORT_UNAVAILABLE", "retryable": True, "message": "Unavailable"})))
    for fault in ("forged", "missing"):
        value = fixture()
        value.receipt_fault = fault
        value.respond(first_request())
        completed = value.respond(returned(value))[1]["item"]
        evidence = json.loads(completed["content"][0]["text"])["criterion_assessments"][0]["evidence"][0]
        if fault == "forged":
            assert evidence["read_receipt_id"] == "018f0000-0000-7000-8000-ffffffffffff"
        else:
            assert "read_receipt_id" not in evidence
    value = fixture()
    value.posts = probe.MAX_POSTS
    rejected(lambda: value.respond(first_request()))
    value = fixture()
    value.pending = False
    value.steps = probe.MAX_TURNS
    rejected(lambda: value.prepare("outside-budget", probe.GENERAL, request_specialist=True))
    assert value.pending is False and value.steps == probe.MAX_TURNS
    print("Integrated native offline admission and correlation checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
