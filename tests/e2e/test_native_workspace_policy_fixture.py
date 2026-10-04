#!/usr/bin/env python3
"""Offline loopback transport, native-result correlation, and isolated campaign checks."""

from __future__ import annotations

import gzip
import hashlib
import http.client
import json
import os
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path
from unittest.mock import patch

import run_native_workspace_policy_fixture as probe
from run_live_instruction_discovery import ProbeCheckFailure


def post(fixture: probe.ScriptedResponses, request: dict, *, compressed: bool = False) -> tuple[int, list[dict]]:
    connection = http.client.HTTPConnection("127.0.0.1", fixture.http.server_port, timeout=5)
    body = json.dumps(request).encode()
    headers = {"Content-Type": "application/json"}
    if compressed:
        body = gzip.compress(body)
        headers["Content-Encoding"] = "gzip"
    try:
        connection.request("POST", "/v1/responses", body, headers)
        response = connection.getresponse()
        data = response.read().decode()
        events = [json.loads(line.removeprefix("data: ")) for line in data.splitlines() if line.startswith("data: ")]
        return response.status, events
    finally:
        connection.close()


def task(command: str) -> dict:
    return {"input": [{"type": "message", "role": "user", "content": [
        {"type": "input_text", "text": f"working directory: {command}\n"}]}]}


def result(call_id: str, output: str | list) -> dict:
    return {"input": [{"type": "function_call_output", "call_id": call_id, "output": output}]}


def main() -> int:
    # Exercise the real request guard without spawning a native process. The
    # fixture's metadata scope must not broaden the live-account probe.
    with patch("run_live_instruction_discovery.ProbeServer.request", return_value={}):
        fixture_client = object.__new__(probe.FixturePolicyServer)
        live_client = object.__new__(probe.PolicyServer)
        for method in ("thread/name/set", "thread/loaded/list"):
            assert fixture_client.request(method, {}) == {}
            try:
                live_client.request(method, {})
            except ProbeCheckFailure as error:
                assert error.kind == "request_outside_policy_probe"
            else:
                raise AssertionError("fixture-only metadata scope leaked into live probe")
        for method in ("command/exec", "thread/archive", "thread/fork"):
            try:
                fixture_client.request(method, {})
            except ProbeCheckFailure as error:
                assert error.kind == "request_outside_policy_probe"
            else:
                raise AssertionError("unexpected fixture operation admitted")
    fixture = probe.ScriptedResponses()
    try:
        config = tomllib.loads(probe.fixture_config(fixture.base_url).decode())
        provider = config["model_providers"]["bounded_fixture"]
        assert provider["requires_openai_auth"] is False and provider["supports_websockets"] is False
        assert provider["request_max_retries"] == provider["stream_max_retries"] == 0
        assert set(provider) == {"name", "base_url", "wire_api", "requires_openai_auth",
                                 "supports_websockets", "request_max_retries", "stream_max_retries"}
        for number in range(1, 7):
            command = f"cat policy-target && printf %s policy-{number} >> policy-target"
            fixture.prepare(command)
            status, events = post(fixture, task(command), compressed=number % 2 == 0)
            assert status == 200 and len(events) == 3
            call = events[1]["item"]
            assert call["name"] == "exec_command" and json.loads(call["arguments"]) == {
                "cmd": command, "yield_time_ms": 1000}
            assert fixture.native_terminal(command) is None
            text = "Wall time: 0.1 seconds\nProcess exited with code 1\nOutput:\nread-canary\nPermission denied\n"
            output = text if number % 2 else [{"type": "input_text", "text": text}]
            status, events = post(fixture, result(call["call_id"], output))
            assert status == 200 and events[1]["item"]["type"] == "message"
            assert fixture.native_terminal(command) == (1, "read-canary\nPermission denied\n")
        assert fixture.posts == 12
        try:
            fixture.prepare("seventh")
        except ProbeCheckFailure as error:
            assert error.kind == "fixture_step_outside_budget"
        else:
            raise AssertionError("seventh scripted Turn admitted")
        assert post(fixture, task(command))[0] == 400
    finally:
        fixture.close()
    assert not fixture.worker.is_alive() and fixture.result is None
    native_text = "Chunk ID: synthetic-chunk\nWall time: 0.0010 seconds\nProcess exited with code 0\nOutput:\nread-canary"
    assert probe.terminal_output(native_text) == (0, "read-canary")
    for bad in ("exec_command failed: private-canary", "Permission denied", [{"type": "input_text", "text": ""}],
                [1], "Process exited with code 0\nProcess exited with code 1\nOutput:\nx",
                "Process running with session ID 123\nOutput:\nx",
                "Process exited with code 0\nFinal output:\nx",
                "Process exited with code 0\nProcess running with session ID 123\nOutput:\nx"):
        try:
            probe.terminal_output(bad)
        except ProbeCheckFailure as error:
            assert "private-canary" not in str(error)
        else:
            raise AssertionError("unverified native terminal return accepted")
    for bad in ("https://provider.example/v1", "http://localhost:123/v1", "http://127.0.0.1:123/v1\napi_key=x"):
        try:
            probe.fixture_config(bad)
        except ProbeCheckFailure:
            pass
        else:
            raise AssertionError("non-fixture endpoint admitted")
    for bad_kind in ("foreign_return", "nonterminal_return", "wrong_task", "duplicate_return"):
        fixture = probe.ScriptedResponses()
        try:
            fixture.prepare("bounded-command")
            if bad_kind == "wrong_task":
                assert post(fixture, task("private-command-canary"))[0] == 400
            else:
                assert post(fixture, task("bounded-command"))[0] == 200
                call_id = "foreign" if bad_kind == "foreign_return" else "bounded-call-1"
                text = "Process exited with code 0\nOutput:\nok" if bad_kind != "nonterminal_return" else "private-output-canary"
                returned = result(call_id, text)
                if bad_kind == "duplicate_return":
                    returned["input"] *= 2
                assert post(fixture, returned)[0] == 400
            assert "canary" not in str(fixture.failure)
        finally:
            fixture.close()
    for missing in ("common", "role"):
        fixture = probe.ScriptedResponses()
        try:
            fixture.instruction_markers = {name: f"private-marker-{name}" for name in
                ("common", "role", "profile", "project", "ancestor")}
            fixture.prepare("bounded-command")
            request = task("bounded-command")
            other = "role" if missing == "common" else "common"
            request["instructions"] = fixture.instruction_markers[other]
            assert post(fixture, request)[0] == 400
            assert fixture.failure == "explicit_instruction_input_unverified"
            assert fixture.instruction_inputs_observed == 0
        finally:
            fixture.close()

    with tempfile.TemporaryDirectory(prefix="dolgorae-native-policy-offline-") as temporary:
        root = Path(temporary).resolve()
        executable = root / "codex"
        invoked = root / "invoked"
        executable.write_text("#!/usr/bin/env python3\nimport pathlib,sys\n"
                              f"pathlib.Path({str(invoked)!r}).write_text(' '.join(sys.argv[1:]))\n"
                              "print('codex-cli 0.159.3')\n")
        executable.chmod(0o700)
        digest = hashlib.sha256(executable.read_bytes()).hexdigest()
        refused = subprocess.run([sys.executable, probe.__file__, "--codex", str(executable),
            "--expected-version", "codex-cli 0.159.3", "--expected-sha256", digest],
            env={key: value for key, value in os.environ.items() if key != probe.OPT_IN},
            capture_output=True, timeout=10)
        assert refused.returncode == 2 and not invoked.exists()
        servers: list[FakeServer] = []
        histories: list[dict] = []

        class FakeServer:
            false_denial = False
            missing_readback = False
            replace_thread = False
            malformed_terminal = False
            denial_status_mismatch = False
            denial_text_missing = False
            write_after_release = False
            valid_history = False
            history_exit_mismatch = False
            history_output_mismatch = False
            duplicate_history = False
            missing_baseline_return = False
            name_response_invalid = False
            hidden_child = False
            lowercase_denial = False
            ignore_recovery_policy = False
            recovery_write_after_release = False
            recovery_history_corrupt = False

            def __init__(self, _codex: Path, profile: Path, environment: dict) -> None:
                assert not (profile / "auth.json").exists()
                assert environment["HOME"] != os.environ.get("HOME") and "OPENAI_API_KEY" not in environment
                self.profile = profile
                self.config = tomllib.loads((profile / "config.toml").read_text())
                self.workspace = profile.parent / "home" / "workspace"
                self.write = False
                self.approval = "never"
                self.discovery_enabled = False
                self.thread_id = "thread"
                self.messages: list[dict] = []
                self.closed = False
                servers.append(self)

            def send(self, frame: dict) -> None:
                assert frame["method"] == "initialized"

            def handle_request(self, frame: dict) -> None:
                assert "id" not in frame

            def close(self) -> None:
                assert self.profile.is_dir(), "home removed before server exit"
                self.closed = True

            def request(self, method: str, params: dict) -> dict:
                if method == "initialize":
                    return {"codexHome": str(self.profile)}
                if method in {"thread/start", "thread/resume"}:
                    if method == "thread/start":
                        assert params["modelProvider"] == "bounded_fixture"
                        limit = params["config"]["project_doc_max_bytes"]
                        assert limit in (0, 65_536)
                        self.discovery_enabled = limit != 0
                        self.thread_id = "baseline" if self.discovery_enabled else "thread"
                        self.write = True
                        self.approval = params["approvalPolicy"]
                        assert self.approval == "on-request"
                        if not self.discovery_enabled:
                            histories.clear()
                        self.prefix = params["developerInstructions"]
                    else:
                        assert set(params) == {"threadId", "cwd", "model", "modelProvider",
                            "sandbox", "approvalPolicy", "developerInstructions", "config"}
                        assert params["threadId"] == "thread" and params["cwd"] == str(self.workspace)
                        assert params["model"] == probe.TARGET_MODEL and params["modelProvider"] == "bounded_fixture"
                        assert params["config"] == {"project_doc_max_bytes": 0, "sandbox_workspace_write": {
                            "writable_roots": [str(self.workspace)], "network_access": False,
                            "exclude_slash_tmp": False, "exclude_tmpdir_env_var": False}}
                        assert params["developerInstructions"] == servers[-2].prefix
                        self.prefix = params["developerInstructions"]
                        if not histories:
                            # Turnless identity storage alone has no permission
                            # snapshot. Require the accepted launch inputs again.
                            assert params["sandbox"] == "workspace-write" and params["approvalPolicy"] == "on-request"
                            self.write = True
                            self.approval = params["approvalPolicy"]
                        else:
                            # Model a stale native writable snapshot. Recovery's
                            # explicit selection must override it before a Turn.
                            assert params["sandbox"] == "read-only" and params["approvalPolicy"] == "never"
                            self.write = self.ignore_recovery_policy
                            self.approval = "never"
                    return {"thread": {"id": "foreign" if self.replace_thread and method == "thread/resume" else self.thread_id, "turns": []},
                        "cwd": str(self.workspace), "model": probe.TARGET_MODEL, "modelProvider": "bounded_fixture",
                        "approvalPolicy": self.approval, "sandbox": {"type": "workspaceWrite" if self.write else "readOnly",
                        **({"writableRoots": []} if self.write else {})}}
                if method == "thread/settings/update":
                    self.write = params["sandboxPolicy"]["type"] == "workspaceWrite"
                    self.approval = params["approvalPolicy"]
                    assert self.approval == ("on-request" if self.write else "never")
                    self.messages.append({"method": "thread/settings/updated", "params": {"threadId": "thread",
                        "threadSettings": {"cwd": str(self.workspace), "approvalPolicy": self.approval,
                                           "sandboxPolicy": params["sandboxPolicy"]}}})
                    return {}
                if method == "thread/name/set":
                    assert params == {"threadId": "thread", "name": "Bounded turnless persistence probe"}
                    assert not histories
                    return {"unexpected": True} if self.name_response_invalid else {}
                if method == "thread/loaded/list":
                    assert params == {"cursor": None, "limit": 2}
                    return {"data": [self.thread_id], "nextCursor": "hidden-child" if self.hidden_child else None}
                if method == "thread/read":
                    persisted = histories.copy()
                    if self.recovery_history_corrupt and len(servers) == 4:
                        persisted = persisted[:-1]
                    return {"thread": {"id": self.thread_id, "turns": persisted,
                        "parentThreadId": None, "status": {"type": "idle"}}}
                assert method == "turn/start" and set(params) == {"threadId", "input", "model", "effort"}
                assert self.model_turns_requested < 5
                self.model_turns_requested += 1
                fixture = active_fixture[0]
                command = params["input"][0]["text"].split("working directory: ", 1)[1].split("\n", 1)[0]
                request = task(command)
                request["input"].insert(0, {"type": "message", "role": "developer", "content": [
                    {"type": "input_text", "text": self.prefix}]})
                # The positive control loads all three sources; controlled requests
                # suppress project/ancestor guidance while retaining Profile input.
                request["input"].insert(0, {"type": "message", "role": "user", "content": [
                    {"type": "input_text", "text": (self.profile / "AGENTS.md").read_text()}]})
                if self.discovery_enabled:
                    for path in (self.workspace / "AGENTS.md", self.workspace.parent / "AGENTS.md"):
                        request["input"].insert(0, {"type": "message", "role": "user", "content": [
                            {"type": "input_text", "text": path.read_text()}]})
                status, events = post(fixture, request)
                assert status == 200
                call = events[1]["item"]
                if self.write or self.write_after_release or (self.recovery_write_after_release and len(servers) == 4):
                    executed = subprocess.run(["/bin/sh", "-c", json.loads(call["arguments"])["cmd"]],
                        cwd=self.workspace, capture_output=True, timeout=5)
                    code, output = executed.returncode, executed.stdout.decode()
                else:
                    code = 0 if self.false_denial else 1
                    output = "" if self.missing_readback else (self.workspace / "policy-target").read_text()
                    output += ("\nprivate-error-canary\n" if self.denial_text_missing else
                               "\nzsh:1: operation not permitted: policy-target\n" if self.lowercase_denial else
                               "\nPermission denied\n")
                returned = f"Process exited with code {code}\nOutput:\n{output}"
                if self.malformed_terminal:
                    returned = returned.replace("\nOutput:\n", "\nFinal output:\n")
                returned_status = post(fixture, result(call["call_id"], [{"type": "input_text", "text": returned}]))[0]
                assert returned_status == (400 if self.malformed_terminal else 200)
                turn = {"id": f"turn-{len(histories) + 1}", "status": "completed", "items": []}
                if (self.valid_history or self.missing_baseline_return or (not self.write and (
                        self.denial_status_mismatch or self.history_exit_mismatch
                        or self.history_output_mismatch or self.duplicate_history))):
                    turn["items"] = [{"type": "commandExecution", "command": command,
                        "cwd": str(self.workspace),
                        "status": "completed" if self.denial_status_mismatch or code == 0 else "failed",
                        "exitCode": code + 1 if self.history_exit_mismatch else code,
                        "aggregatedOutput": output}]
                    if self.history_output_mismatch:
                        turn["items"][0]["aggregatedOutput"] += "private-history-canary"
                    if self.duplicate_history:
                        turn["items"].append(turn["items"][0].copy())
                if self.missing_baseline_return and self.discovery_enabled:
                    fixture.result = None
                if self.malformed_terminal:
                    turn["status"] = "failed"
                histories.append(turn)
                self.messages.append({"method": "turn/completed", "params": {"threadId": self.thread_id, "turn": turn}})
                return {"turn": turn}

        active_fixture: list[probe.ScriptedResponses] = []
        real_fixture = probe.ScriptedResponses

        def create_fixture() -> probe.ScriptedResponses:
            fixture = real_fixture()
            active_fixture[:] = [fixture]
            return fixture

        with patch.object(probe, "FixturePolicyServer", FakeServer), patch.object(probe, "ScriptedResponses", create_fixture):
            successes = {None, "valid_history", "lowercase_denial"}
            for failure in (None, "valid_history", "lowercase_denial", "false_denial", "missing_readback", "replace_thread", "malformed_terminal",
                            "denial_status_mismatch", "denial_text_missing", "write_after_release",
                            "history_exit_mismatch", "history_output_mismatch", "duplicate_history", "missing_baseline_return",
                            "name_response_invalid", "hidden_child", "ignore_recovery_policy",
                            "recovery_write_after_release", "recovery_history_corrupt"):
                servers.clear()
                histories.clear()
                if failure:
                    setattr(FakeServer, failure, True)
                observed = probe.campaign(executable, "codex-cli 0.159.3", digest)
                assert observed["outcome"] == ("observed" if failure in successes else "unqualified"), observed
                assert observed["credential_access"] is False and observed["external_provider_requests"] == 0
                assert observed["production_qualification"] == "not_changed"
                assert observed["instruction_discovery"] == (
                    "unverified" if failure in {"malformed_terminal", "missing_baseline_return", "replace_thread", "name_response_invalid", "hidden_child"}
                    else "project_ancestor_suppression_observed")
                assert observed["ambient_instruction_markers_seen"] == {
                    "profile": failure not in {"malformed_terminal", "missing_baseline_return", "replace_thread", "name_response_invalid", "hidden_child"}, "project": False, "ancestor": False}
                assert "canary" not in json.dumps(observed)
                if failure == "malformed_terminal":
                    assert observed["fixture_failure_kind"] == "native_terminal_output_unverified"
                    assert observed["native_turns_requested"] == 1
                    assert observed["loopback_responses_posts"] == 2
                if failure == "replace_thread":
                    assert observed["failure_kind"] == "turnless_restart_identity_mismatch"
                    assert observed["resumed_thread_identity_matches"] is False
                    assert all(observed[name] is True for name in (
                        "resumed_workspace_matches", "resumed_approval_policy_matches",
                        "resumed_provider_matches", "resumed_model_matches"))
                if failure in {"false_denial", "denial_status_mismatch", "denial_text_missing", "write_after_release"}:
                    assert observed["failure_kind"] == "read_only_denial_unverified"
                    assert observed["native_turns_requested"] == 3 and observed["loopback_responses_posts"] == 6
                    assert observed["access_evidence_source"] == (
                        "command_history" if failure == "denial_status_mismatch" else "correlated_tool_return")
                    assert observed["command_exit_zero"] == (failure in {"false_denial", "write_after_release"})
                    assert observed["command_status"] == (
                        "completed" if failure in {"denial_status_mismatch", "false_denial", "write_after_release"} else "failed")
                    assert observed["permission_denial_observed"] == (failure in {"false_denial", "denial_status_mismatch"})
                    assert observed["expected_workspace_bytes_match"] == (failure != "write_after_release")
                    assert observed["workspace_read_observed"] is True
                    assert observed["native_terminal_observed"] is True
                if failure == "missing_readback":
                    assert observed["failure_kind"] == "workspace_read_unverified"
                    assert observed["workspace_read_observed"] is False
                    assert observed["command_exit_zero"] is False
                    assert observed["permission_denial_observed"] is True
                    assert observed["expected_workspace_bytes_match"] is True
                if failure in {"history_exit_mismatch", "history_output_mismatch"}:
                    assert observed["failure_kind"] == "native_history_result_mismatch"
                    assert observed["native_history_exit_matches"] == (failure != "history_exit_mismatch")
                    assert observed["native_history_output_matches"] == (failure != "history_output_mismatch")
                    assert observed["native_terminal_observed"] is True
                    assert observed["native_turns_requested"] == 3 and observed["loopback_responses_posts"] == 6
                if failure == "duplicate_history":
                    assert observed["failure_kind"] == "command_attempt_missing_or_ambiguous"
                    assert observed["history_command_count"] == 2
                if failure == "missing_baseline_return":
                    assert observed["failure_kind"] == "fixture_native_return_missing_or_ambiguous"
                    assert observed["history_command_count"] == 1 and observed["native_terminal_observed"] is False
                    assert observed["native_turns_requested"] == 1 and observed["loopback_responses_posts"] == 2
                if failure in {"name_response_invalid", "hidden_child"}:
                    assert observed["failure_kind"] == ("thread_name_response_mismatch" if failure == "name_response_invalid"
                                                        else "loaded_thread_census_not_quiescent")
                    assert observed["native_turns_requested"] == 1 and observed["loopback_responses_posts"] == 2
                if failure in {"ignore_recovery_policy", "recovery_history_corrupt"}:
                    assert observed["failure_kind"] == ("policy_metadata_mismatch" if failure == "ignore_recovery_policy"
                                                        else "restart_history_mismatch")
                    assert observed["failed_phase"] == "restart_resume"
                    assert observed["native_turns_requested"] == 5 and observed["loopback_responses_posts"] == 10
                    assert observed["verified_access_steps"] == 4
                if failure == "recovery_write_after_release":
                    assert observed["failure_kind"] == "read_only_denial_unverified"
                    assert observed["failed_phase"] == "post_restart_read_only"
                    assert observed["native_turns_requested"] == 6 and observed["loopback_responses_posts"] == 12
                    assert observed["verified_access_steps"] == 4
                    assert observed["expected_workspace_bytes_match"] is False
                    assert observed["command_exit_zero"] is True
                assert all(server.closed and not server.profile.exists() for server in servers)
                assert not active_fixture[0].worker.is_alive()
                if failure:
                    setattr(FakeServer, failure, False)
                if failure in successes:
                    assert len(servers) == 4 and observed["native_turns_requested"] == 6
                    assert observed["turnless_restart"] == "same_thread_empty_history"
                    assert observed["writable_approval_policy"] == "on-request"
                    assert observed["quiescence_metadata_checks"] == 4
                    assert observed["loopback_responses_posts"] == 12 and observed["verified_access_steps"] == 5
                    assert observed["instruction_inputs_observed"] == 5
                    assert observed["ambient_positive_control_inputs_observed"] == 1
                    assert all(observed["ambient_positive_control_markers_seen"].values())
            rejected = probe.campaign(executable, "codex-cli 0.159.3", "0" * 64)
            assert rejected["failure_kind"] == "executable_digest_mismatch"
    print("native workspace policy fixture offline checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
