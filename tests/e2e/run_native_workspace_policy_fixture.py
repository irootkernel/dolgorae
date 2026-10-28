#!/usr/bin/env python3
"""Opt-in native sandbox probe using scripted loopback Responses, without an account.

The fixture supplies tool calls, never their results. Native terminal output and
workspace bytes establish the observations. No model or production qualification
is involved. Requests and tool output stay in memory and are never reported.
"""

from __future__ import annotations

import argparse
import gzip
import io
import json
import os
import re
import subprocess
import tempfile
import threading
import uuid
from contextlib import ExitStack
from http.server import BaseHTTPRequestHandler, HTTPServer, ThreadingHTTPServer
from pathlib import Path
from typing import Any

from run_live_instruction_discovery import (
    CORE_PREFIX, ProbeCheckFailure, checked_codex, executable_identity, isolated_environment,
    private_file, verify_core_prefix,
)
from run_live_workspace_policy import TARGET_MODEL, PolicyServer, attempt, start_writable_thread
from run_live_workspace_startup import check_policy, initialize, update

OPT_IN = "DOLGORAE_RUN_NATIVE_WORKSPACE_POLICY_FIXTURE"
MAX_BODY = 2_097_152
SCHEMA = "dolgorae.native-workspace-policy-fixture/v3"


class FixturePolicyServer(PolicyServer):
    ALLOWED_METHODS = PolicyServer.ALLOWED_METHODS | {"thread/name/set", "thread/loaded/list"}


def terminal_output(value: Any) -> tuple[int, str]:
    # The selected 0.164 Code Mode exec result has structured terminal facts.
    if (not isinstance(value, dict) or type(value.get("exit_code")) is not int
            or not isinstance(value.get("output"), str) or len(value["output"].encode()) > MAX_BODY):
        raise ProbeCheckFailure("native_terminal_output_unverified")
    return value["exit_code"], value["output"]


def code_call(request: dict, call_id: str, calls: list[tuple[str, str, dict]]) -> dict:
    advertised = [tool for item in request.get("input", []) if item.get("type") == "additional_tools"
                  for namespace in item.get("tools", []) if namespace.get("name") == "functions"
                  for tool in namespace.get("tools", []) if tool.get("type") == "custom" and tool.get("name") == "exec"]
    if (len(advertised) != 1 or advertised[0].get("format", {}).get("type") != "grammar"
            or any("### `" + name + "`" not in advertised[0].get("description", "") for _, name, _ in calls)):
        raise ProbeCheckFailure("advertised_code_mode_tool_missing")
    return {"type": "custom_tool_call", "call_id": call_id, "name": "exec", "namespace": "functions",
            "input": "\n".join(f"text({{label:{json.dumps(label)},result:await tools.{name}({json.dumps(args)})}});"
                               for label, name, args in calls)}


def code_results(request: dict, call_id: str, labels: list[str]) -> dict:
    # Observed in the same native successor case: a separate completion/header
    # block followed by exactly the JSON text(...) blocks we emitted.
    returned = [item for item in request.get("input", [])
                if item.get("type") == "custom_tool_call_output" and item.get("call_id") == call_id]
    if len(returned) != 1:
        raise ProbeCheckFailure("custom_return_correlation_missing_or_duplicate")
    blocks = returned[0].get("output")
    if (not isinstance(blocks, list) or len(blocks) != len(labels) + 1
            or any(not isinstance(block, dict) or block.get("type") != "input_text" or not isinstance(block.get("text"), str) for block in blocks)
            or re.fullmatch(r"Script completed\nWall time [0-9]+(?:\.[0-9]+)? seconds\nOutput:\n", blocks[0]["text"]) is None):
        raise ProbeCheckFailure("custom_return_not_observed_completed_shape")
    results = {}
    for block in blocks[1:]:
        value = json.loads(block["text"])
        if (not isinstance(value, dict) or set(value) != {"label", "result"} or not isinstance(value.get("label"), str)
                or value["label"] not in labels or value["label"] in results):
            raise ProbeCheckFailure("custom_return_label_missing_or_duplicate")
        results[value["label"]] = value["result"]
    if set(results) != set(labels):
        raise ProbeCheckFailure("custom_return_label_missing_or_duplicate")
    return results


class ScriptedResponses:
    """One exact call and correlated native return per Turn; twelve POSTs total."""

    def __init__(self, *, concurrent_requests: bool = False) -> None:
        self.posts = 0
        self.steps = 0
        self.command: str | None = None
        self.call_id: str | None = None
        self.issued = False
        self.result: tuple[int, str] | None = None
        self.failure: str | None = None
        self.failure_details: dict | None = None
        self.input_sizes: list[dict] = []
        self.instruction_markers: dict[str, str] = {}
        self.ambient_seen = {name: False for name in ("profile", "project", "ancestor")}
        self.positive_control_seen = self.ambient_seen.copy()
        self.positive_control = False
        self.positive_control_inputs_observed = 0
        self.instruction_inputs_observed = 0
        self.lock = threading.Lock()
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args: object) -> None:
                pass

            def do_POST(self) -> None:
                self.connection.settimeout(5)
                try:
                    if self.path != "/v1/responses" or self.headers.get("Authorization") is not None:
                        raise ProbeCheckFailure("fixture_request_outside_contract")
                    size = int(self.headers.get("Content-Length", "0"))
                    if not 0 < size <= MAX_BODY or self.headers.get("Transfer-Encoding") is not None:
                        raise ProbeCheckFailure("fixture_body_outside_bound")
                    body = self.rfile.read(size)
                    if len(body) != size:
                        raise ProbeCheckFailure("fixture_body_incomplete")
                    encoding = self.headers.get("Content-Encoding", "identity")
                    if encoding == "gzip":
                        with gzip.GzipFile(fileobj=io.BytesIO(body)) as compressed:
                            body = compressed.read(MAX_BODY + 1)
                    elif encoding != "identity":
                        raise ProbeCheckFailure("fixture_encoding_unsupported")
                    if len(body) > MAX_BODY:
                        raise ProbeCheckFailure("fixture_body_outside_bound")
                    request = json.loads(body)
                    with owner.lock:
                        owner.input_sizes.append({"wire_bytes": size, "decoded_bytes": len(body),
                            "model": request.get("model") if isinstance(request, dict) else None})
                    events = owner.respond(request)
                    wire = "".join(f"event: {event['type']}\ndata: {json.dumps(event)}\n\n"
                                   for event in events).encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(wire)))
                    self.end_headers()
                    self.wfile.write(wire)
                    self.wfile.flush()
                except (OSError, ValueError, TypeError, AttributeError, RuntimeError) as error:
                    with owner.lock:
                        if owner.failure is None:
                            owner.failure = error.kind if isinstance(error, ProbeCheckFailure) else "fixture_request_invalid"
                            owner.failure_details = {"class": type(error).__name__[:64], "reason": owner.failure[:128]}
                    self.send_error(400, "fixture request rejected")

        class QuietServer(ThreadingHTTPServer if concurrent_requests else HTTPServer):
            # Join concurrent handlers in server_close, including an ack waiter.
            daemon_threads = False

            def handle_error(self, _request: object, _address: object) -> None:
                with owner.lock:
                    if owner.failure is None:
                        owner.failure = "fixture_transport_failure"
                        owner.failure_details = {"class": "TransportError", "reason": owner.failure}

        self.http = QuietServer(("127.0.0.1", 0), Handler)
        self.worker = threading.Thread(target=self.http.serve_forever, daemon=True)
        self.worker.start()

    @property
    def base_url(self) -> str:
        return f"http://127.0.0.1:{self.http.server_port}/v1"

    def prepare(self, command: str) -> None:
        with self.lock:
            if self.failure is not None or self.steps == 6 or (self.steps and self.result is None):
                raise ProbeCheckFailure("fixture_step_outside_budget")
            self.steps += 1
            self.command = command
            self.call_id = f"bounded-call-{self.steps}"
            self.issued = False
            self.result = None

    def respond(self, request: Any) -> list[dict[str, Any]]:
        with self.lock:
            self.posts += 1
            if self.failure is not None or self.posts > 12 or self.command is None:
                raise ProbeCheckFailure("fixture_request_outside_budget")
            if not isinstance(request, dict) or not isinstance(request.get("input"), list):
                raise ProbeCheckFailure("fixture_input_invalid")
            inputs = request["input"]
            if not self.issued:
                if self.instruction_markers:
                    # Inspect the actual model request, not a model's account of
                    # which instructions it saw. Only marker booleans are retained.
                    def strings(value: Any) -> list[str]:
                        if isinstance(value, str):
                            return [value]
                        if isinstance(value, list):
                            return [text for part in value for text in strings(part)]
                        if isinstance(value, dict):
                            return [text for part in value.values() for text in strings(part)]
                        return []
                    text = "\n".join(strings([request.get("instructions"), inputs]))
                    if not all(self.instruction_markers[name] in text for name in ("common", "role")):
                        raise ProbeCheckFailure("explicit_instruction_input_unverified")
                    seen = self.positive_control_seen if self.positive_control else self.ambient_seen
                    for name in seen:
                        seen[name] |= self.instruction_markers[name] in text
                    if self.positive_control:
                        self.positive_control_inputs_observed += 1
                    else:
                        self.instruction_inputs_observed += 1
                # Admit only the current task, not an earlier call in persisted history.
                users = [item for item in inputs if item.get("type") == "message" and item.get("role") == "user"]
                texts = users[-1].get("content", []) if users else []
                if not any(part.get("type") == "input_text" and
                           f"working directory: {self.command}\n" in part.get("text", "") for part in texts):
                    raise ProbeCheckFailure("fixture_task_command_mismatch")
                self.issued = True
                item = code_call(request, self.call_id, [(self.call_id, "exec_command",
                    {"cmd": self.command, "yield_time_ms": 1000})])
            else:
                if self.result is not None:
                    raise ProbeCheckFailure("fixture_native_return_missing_or_ambiguous")
                self.result = terminal_output(code_results(request, self.call_id, [self.call_id])[self.call_id])
                item = {"type": "message", "role": "assistant", "id": f"bounded-message-{self.steps}",
                        "content": [{"type": "output_text", "text": "Done."}]}
            identity = f"bounded-response-{self.posts}"
            return [{"type": "response.created", "response": {"id": identity}},
                    {"type": "response.output_item.done", "item": item},
                    {"type": "response.completed", "response": {"id": identity, "usage": {
                        "input_tokens": 0, "input_tokens_details": None, "output_tokens": 0,
                        "output_tokens_details": None, "total_tokens": 0}}}]

    def native_terminal(self, command: str) -> tuple[int, str] | None:
        with self.lock:
            if self.failure is not None:
                raise ProbeCheckFailure(self.failure)
            if self.command != command:
                raise ProbeCheckFailure("fixture_command_correlation_mismatch")
            return self.result

    def close(self) -> None:
        self.http.shutdown()
        self.http.server_close()
        self.worker.join(timeout=5)
        self.result = None
        self.command = None
        self.instruction_markers.clear()


def fixture_config(base_url: str) -> bytes:
    # This constructor accepts only our numeric loopback endpoint, never a provider URL.
    if re.fullmatch(r"http://127\.0\.0\.1:[0-9]+/v1", base_url) is None:
        raise ProbeCheckFailure("fixture_endpoint_invalid")
    return (f'model_provider = "bounded_fixture"\ncli_auth_credentials_store = "file"\n'
            f'[model_providers.bounded_fixture]\nname = "Bounded local fixture"\n'
            f'base_url = "{base_url}"\nwire_api = "responses"\n'
            'requires_openai_auth = false\nsupports_websockets = false\n'
            'request_max_retries = 0\nstream_max_retries = 0\n').encode()


def check_quiescent_thread(server: PolicyServer, thread_id: str) -> None:
    def sole_loaded_thread() -> None:
        loaded = server.request("thread/loaded/list", {"cursor": None, "limit": 2})
        if loaded.get("data") != [thread_id] or "nextCursor" not in loaded or loaded["nextCursor"] is not None:
            raise ProbeCheckFailure("loaded_thread_census_not_quiescent")

    sole_loaded_thread()
    thread = server.request("thread/read", {"threadId": thread_id, "includeTurns": False}).get("thread", {})
    if (thread.get("id") != thread_id or "parentThreadId" not in thread or thread["parentThreadId"] is not None
            or thread.get("status", {}).get("type") != "idle"):
        raise ProbeCheckFailure("thread_status_not_quiescent")
    sole_loaded_thread()


def resume_caller_thread(server: PolicyServer, thread_id: str, workspace: Path,
                         prefix: str, write: bool) -> dict[str, Any]:
    # Supply immutable inputs and the selected access on every fresh connection,
    # including recovery; native identity storage does not prove policy persistence.
    approval = "on-request" if write else "never"
    resumed = server.request("thread/resume", {
        "threadId": thread_id, "cwd": str(workspace), "model": TARGET_MODEL,
        "modelProvider": "bounded_fixture", "sandbox": "workspace-write" if write else "read-only",
        "approvalPolicy": approval, "developerInstructions": prefix,
        "config": {"project_doc_max_bytes": 0, "sandbox_workspace_write": {
            "writable_roots": [str(workspace)], "network_access": False,
            "exclude_slash_tmp": False, "exclude_tmpdir_env_var": False,
        }},
    })
    matches = {
        "resumed_thread_identity_matches": resumed.get("thread", {}).get("id") == thread_id,
        "resumed_workspace_matches": resumed.get("cwd") == str(workspace),
        "resumed_approval_policy_matches": resumed.get("approvalPolicy") == approval,
        "resumed_provider_matches": resumed.get("modelProvider") == "bounded_fixture",
        "resumed_model_matches": resumed.get("model") == TARGET_MODEL,
    }
    if not all(matches.values()):
        raise ProbeCheckFailure("turnless_restart_identity_mismatch" if write else "restart_thread_identity_mismatch", matches)
    check_policy(resumed.get("sandbox"), write, workspace)
    return resumed


def campaign(codex: Path, version: str, digest: str) -> dict[str, object]:
    phase = "executable_admission"
    server: PolicyServer | None = None
    fixture: ScriptedResponses | None = None
    used = 0
    baseline_used = 0
    observations: dict[str, object] = {"schema": SCHEMA, "credential_access": False,
        "external_provider_requests": 0, "production_qualification": "not_changed",
        "profile_capability_mutated": False}
    try:
        if re.fullmatch(r"[0-9a-f]{64}", digest) is None:
            raise ProbeCheckFailure("executable_digest_invalid")
        verify_core_prefix()
        with ExitStack() as cleanup:
            root = Path(cleanup.enter_context(tempfile.TemporaryDirectory(prefix="dolgorae-native-policy-"))).resolve()
            for name in ("home", "profile", "tmp", "config", "cache"):
                (root / name).mkdir(mode=0o700)
            environment = isolated_environment(root)
            workspace = root / "home" / "workspace"
            workspace.mkdir(mode=0o700)
            # Anchor ancestor discovery inside the isolated HOME, so no project
            # guidance outside this campaign's private tree is consulted.
            subprocess.run(["git", "init", "--quiet", str(root / "home")],
                env={**environment, "GIT_CONFIG_NOSYSTEM": "1"}, check=True,
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
            selected = checked_codex(codex, version, environment)
            if selected["sha256"] != digest:
                raise ProbeCheckFailure("executable_digest_mismatch")
            fixture = ScriptedResponses()
            cleanup.callback(fixture.close)
            cleanup.callback(lambda: server.close() if server is not None else None)
            private_file(root / "profile" / "config.toml", fixture_config(fixture.base_url))
            expected = f"read-canary-{uuid.uuid4().hex}".encode()
            private_file(workspace / "policy-target", expected)
            token = uuid.uuid4().hex
            fixture.instruction_markers = {name: f"instruction-{name}-{token}" for name in
                ("common", "role", "profile", "project", "ancestor")}
            for name, directory in (("profile", root / "profile"), ("project", workspace), ("ancestor", root / "home")):
                private_file(directory / "AGENTS.md", (
                    f"Conflicting ambient instruction marker: {fixture.instruction_markers[name]}\n"
                ).encode())
            prefix = CORE_PREFIX + "\n\n" + fixture.instruction_markers["common"] + "\n\n" + fixture.instruction_markers["role"]

            def start_server() -> PolicyServer:
                if executable_identity(codex) != {key: selected[key] for key in ("metadata", "sha256")}:
                    raise ProbeCheckFailure("executable_changed")
                fresh = FixturePolicyServer(codex, root / "profile", environment)
                fresh.model_turns_requested = used
                return fresh

            phase = "ambient_positive_control"
            server = start_server()
            initialize(server, root / "profile")
            fixture.positive_control = True
            baseline_thread = start_writable_thread(server, workspace, provider="bounded_fixture",
                developer_instructions=prefix, project_doc_max_bytes=65_536, approval_policy="on-request")
            marker = f"positive-{token}"
            command = f"cat policy-target && printf %s {marker} >> policy-target"
            fixture.prepare(command)
            before = expected
            expected += marker.encode()
            attempt(server, baseline_thread, workspace, marker, True, expected,
                readback=before, native_terminal=fixture.native_terminal)
            if (fixture.positive_control_inputs_observed != 1
                    or not all(fixture.positive_control_seen.values())):
                raise ProbeCheckFailure("ambient_positive_control_unverified")
            baseline_used = server.model_turns_requested
            server.close()
            server = None
            fixture.positive_control = False
            phase = "turnless_start"
            server = start_server()
            initialize(server, root / "profile")
            thread_id = start_writable_thread(server, workspace, provider="bounded_fixture",
                developer_instructions=prefix, project_doc_max_bytes=0, approval_policy="on-request")
            if thread_id == baseline_thread:
                raise ProbeCheckFailure("controlled_thread_identity_reused")
            phase = "turnless_persistence_barrier"
            if server.request("thread/name/set", {"threadId": thread_id, "name": "Bounded turnless persistence probe"}) != {}:
                raise ProbeCheckFailure("thread_name_response_mismatch")
            history = server.request("thread/read", {"threadId": thread_id, "includeTurns": True})
            if history.get("thread", {}).get("id") != thread_id or history.get("thread", {}).get("turns") != []:
                raise ProbeCheckFailure("turnless_readback_mismatch")
            server.close()
            server = None
            phase = "turnless_restart_resume"
            server = start_server()
            initialize(server, root / "profile")
            resume_caller_thread(server, thread_id, workspace, prefix, True)
            history = server.request("thread/read", {"threadId": thread_id, "includeTurns": True})
            if history.get("thread", {}).get("id") != thread_id or history.get("thread", {}).get("turns") != []:
                raise ProbeCheckFailure("turnless_restart_history_mismatch")
            observations["turnless_restart"] = "same_thread_empty_history"
            observations["writable_approval_policy"] = "on-request"
            phase = "initial_quiescence_metadata"
            check_quiescent_thread(server, thread_id)
            observations["quiescence_metadata_checks"] = 1
            turn_ids: list[str] = []
            for number, write in enumerate((True, False, True, False, False), 1):
                phase = f"access_step_{number}"
                if number == 5:
                    used = server.model_turns_requested
                    server.close()
                    server = None
                    phase = "restart_resume"
                    server = start_server()
                    initialize(server, root / "profile")
                    resume_caller_thread(server, thread_id, workspace, prefix, False)
                    history = server.request("thread/read", {"threadId": thread_id, "includeTurns": True})
                    persisted = history.get("thread", {})
                    if (persisted.get("id") != thread_id
                            or [turn.get("id") for turn in persisted.get("turns", [])] != turn_ids
                            or any(turn.get("status") != "completed" for turn in persisted.get("turns", []))):
                        raise ProbeCheckFailure("restart_history_mismatch")
                    phase = "post_restart_read_only"
                elif number > 1:
                    check_quiescent_thread(server, thread_id)
                    observations["quiescence_metadata_checks"] += 1
                    update(server, thread_id, workspace, write,
                           approval_policy="on-request" if write else "never")
                marker = f"policy-{token}-{number}"
                fixture.prepare(f"cat policy-target && printf %s {marker} >> policy-target")
                before = expected
                if write:
                    expected += marker.encode()
                turn_ids.append(attempt(server, thread_id, workspace, marker, write, expected,
                                        readback=before, native_terminal=fixture.native_terminal))
                observations["verified_access_steps"] = number
            if len(set(turn_ids)) != 5 or fixture.posts != 12:
                raise ProbeCheckFailure("fixture_identity_or_request_count_mismatch")
            observations.update(outcome="observed", codex_version=version, codex_sha256=digest,
                native_turns_requested=baseline_used + server.model_turns_requested, loopback_responses_posts=fixture.posts,
                writable_effects_observed=2, read_only_denials_observed=3,
                same_thread_settings_updates=3, restart="same_thread_read_only_metadata_history_and_io")
    except (OSError, ValueError, RuntimeError, TimeoutError, subprocess.SubprocessError) as error:
        observations.update(outcome="unqualified", failed_phase=phase,
            native_turns_requested=baseline_used + getattr(server, "model_turns_requested", used),
            loopback_responses_posts=fixture.posts if fixture is not None else 0,
            failure_kind=error.kind if isinstance(error, ProbeCheckFailure) else "runtime_failure")
        if isinstance(error, ProbeCheckFailure):
            observations.update(error.observations)
    if fixture is not None:
        observations["fixture_failure_kind"] = fixture.failure
        observations["instruction_inputs_observed"] = fixture.instruction_inputs_observed
        observations["ambient_instruction_markers_seen"] = fixture.ambient_seen.copy()
        observations["ambient_positive_control_markers_seen"] = fixture.positive_control_seen.copy()
        observations["ambient_positive_control_inputs_observed"] = fixture.positive_control_inputs_observed
        observations["instruction_discovery"] = (
            "project_ancestor_not_suppressed" if any(fixture.ambient_seen[name] for name in ("project", "ancestor")) else
            "project_ancestor_suppression_observed" if fixture.instruction_inputs_observed
                and all(fixture.positive_control_seen.values()) else
            "unverified")
    return observations


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--codex", type=Path, required=True)
    parser.add_argument("--expected-version", required=True)
    parser.add_argument("--expected-sha256", required=True)
    args = parser.parse_args()
    if os.environ.get(OPT_IN) != "1":
        parser.error(f"{OPT_IN}=1 and explicit native-runtime authorization are required")
    try:
        result = campaign(args.codex.expanduser().resolve(strict=True), args.expected_version, args.expected_sha256)
    except (OSError, ValueError):
        result = {"schema": SCHEMA, "outcome": "unqualified", "failure_kind": "executable_admission_failed",
                  "credential_access": False, "external_provider_requests": 0,
                  "native_turns_requested": 0, "production_qualification": "not_changed"}
    print(json.dumps(result, sort_keys=True))
    return 0 if result["outcome"] == "observed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
