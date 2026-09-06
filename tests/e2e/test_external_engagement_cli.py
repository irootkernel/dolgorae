#!/usr/bin/env python3
"""Black-box TASK-022 reusable External Specialist Engagement lifecycle."""

from __future__ import annotations

import argparse
import base64
import json
import os
import pathlib
import signal
import subprocess
import tempfile
import time

import native_codex
from schema_support import assert_valid, validator


PROTOCOL_ROOT = pathlib.Path(__file__).resolve().parents[2] / "docs" / "protocol"
FACADE_SCHEMA = validator(
    PROTOCOL_ROOT, "dolgorae-external-specialist-facade-v2.schema.json"
)


def invoke(
    binary: pathlib.Path,
    home: pathlib.Path,
    arguments: list[str],
) -> tuple[subprocess.CompletedProcess[str], dict[str, object]]:
    environment = os.environ.copy()
    environment["HOME"] = str(home)
    completed = subprocess.run(
        [str(binary), *arguments],
        check=False,
        capture_output=True,
        text=True,
        env=environment,
    )
    if completed.stderr:
        raise AssertionError(f"unexpected stderr for {arguments}: {completed.stderr!r}")
    return completed, json.loads(completed.stdout)


def call(
    binary: pathlib.Path,
    home: pathlib.Path,
    workspace: pathlib.Path,
    owner: pathlib.Path,
    request: dict[str, object],
    new_controller: pathlib.Path | None = None,
    expected_error: str | None = None,
    environment_overrides: dict[str, str] | None = None,
    invalid_request: bool = False,
) -> dict[str, object]:
    if invalid_request:
        if FACADE_SCHEMA.is_valid(request):
            raise AssertionError("negative request unexpectedly conforms to the schema")
    else:
        assert_valid(request, FACADE_SCHEMA, f"{request.get('operation')} request")
    with tempfile.TemporaryFile() as request_file:
        os.fchmod(request_file.fileno(), 0o600)
        request_file.write(json.dumps(request, separators=(",", ":")).encode())
        request_file.flush()
        request_file.seek(0)
        arguments = [
            str(binary),
            "engagement",
            "call",
            "--workspace",
            str(workspace),
            "--controller-file",
            str(owner),
            "--request-fd",
            str(request_file.fileno()),
        ]
        if new_controller is not None:
            arguments.extend(["--new-controller-file", str(new_controller)])
        environment = os.environ.copy()
        environment["HOME"] = str(home)
        if environment_overrides:
            environment.update(environment_overrides)
        completed = subprocess.run(
            arguments,
            check=False,
            capture_output=True,
            text=True,
            env=environment,
            pass_fds=(request_file.fileno(),),
        )
    if completed.stderr:
        raise AssertionError(f"unexpected engagement stderr: {completed.stderr!r}")
    envelope = json.loads(completed.stdout)
    if completed.returncode != 0 and expected_error is None:
        raise AssertionError(f"engagement call failed: {envelope!r}")
    if expected_error is not None:
        if completed.returncode == 0 or envelope.get("error", {}).get("code") != expected_error:
            raise AssertionError(
                f"engagement call did not fail with {expected_error}: {envelope!r}"
            )
        error = envelope["error"]
        assert_valid(
            {
                "operation": "external_specialist_error",
                "code": error["code"],
                "message": error["message"],
                "retryable": error["retryable"],
            },
            FACADE_SCHEMA,
            f"{request.get('operation')} error",
        )
        return envelope["error"]
    if str(owner) in completed.stdout or (
        new_controller is not None and str(new_controller) in completed.stdout
    ):
        raise AssertionError("engagement response disclosed a credential carrier path")
    result = envelope["data"]
    assert_valid(result, FACADE_SCHEMA, f"{request.get('operation')} result")
    return result


def credential(
    binary: pathlib.Path,
    home: pathlib.Path,
    output: pathlib.Path,
    instance: str,
) -> None:
    completed, envelope = invoke(
        binary,
        home,
        [
            "controller",
            "credential",
            "create",
            "--kind",
            "automation",
            "--instance-id",
            instance,
            "--output",
            str(output),
        ],
    )
    if completed.returncode != 0:
        raise AssertionError(f"credential creation failed: {envelope!r}")


def stop_worker(pid: int) -> None:
    os.kill(pid, signal.SIGTERM)
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return
        time.sleep(0.02)
    raise AssertionError(f"Worker {pid} did not stop")


def task_request(engagement_id: str, run_id: str, sequence: int) -> dict[str, object]:
    return {
        "operation": "assign_external_specialist_task",
        "engagement_id": engagement_id,
        "specialist_run_id": run_id,
        "external_request_ref": {
            "namespace": "dolgorae.e2e",
            "kind": "task",
            "id": str(sequence),
        },
        "objective": f"Return reusable answer {sequence}",
        "context_refs": [],
        "expected_output": ["one concise answer"],
        "execution_intent": "read_only",
        "deadline_seconds": 60,
        "idempotency_key": f"task-{sequence}",
    }


def validate(binary: pathlib.Path) -> None:
    schema_source = native_codex.installed_codex()
    with tempfile.TemporaryDirectory(prefix="dolgorae-task022-") as temporary:
        root = pathlib.Path(temporary).resolve()
        home = root / "home"
        workspace = root / "workspace"
        codex_home = root / "codex-home"
        isolated_codex_home = root / "isolated-codex-home"
        interaction_codex_home = root / "interaction-codex-home"
        bin_root = root / "bin"
        isolated_bin_root = root / "isolated-bin"
        interaction_bin_root = root / "interaction-bin"
        for directory in (
            home,
            workspace,
            codex_home,
            isolated_codex_home,
            interaction_codex_home,
            bin_root,
            isolated_bin_root,
            interaction_bin_root,
        ):
            directory.mkdir(mode=0o700)
        subprocess.run(["git", "-C", str(workspace), "init", "-b", "main"], check=True, capture_output=True)
        subprocess.run(["git", "-C", str(workspace), "config", "user.name", "Dolgorae E2E"], check=True)
        subprocess.run(["git", "-C", str(workspace), "config", "user.email", "dolgorae@example.invalid"], check=True)
        (workspace / "root.txt").write_text("root\n", encoding="utf-8")
        subprocess.run(["git", "-C", str(workspace), "add", "root.txt"], check=True)
        subprocess.run(["git", "-C", str(workspace), "commit", "-m", "root"], check=True, capture_output=True)

        initialized, envelope = invoke(binary, home, ["init", str(workspace)])
        if initialized.returncode != 0:
            raise AssertionError(f"workspace initialization failed: {envelope!r}")
        workspace_id = str(envelope["data"]["workspace_id"])
        state_root = home / ".dolgorae" / "workspaces" / workspace_id
        owner = root / "owner.json"
        child = root / "child.json"
        abort_child = root / "abort-child.json"
        nested_child = root / "nested-child.json"
        isolated_child = root / "isolated-child.json"
        canonical_child = root / "canonical-child.json"
        interaction_child = root / "interaction-child.json"
        operator = root / "operator.json"
        credential(binary, home, owner, "external-host")
        credential(binary, home, child, "external-specialist")
        credential(binary, home, abort_child, "abort-specialist")
        credential(binary, home, nested_child, "nested-specialist")
        credential(binary, home, isolated_child, "isolated-specialist")
        credential(binary, home, canonical_child, "canonical-specialist")
        credential(binary, home, interaction_child, "interaction-specialist")
        initialized_operator, operator_envelope = invoke(
            binary,
            home,
            ["operator", "credential", "initialize", "--output", str(operator)],
        )
        if initialized_operator.returncode != 0:
            raise AssertionError(f"operator initialization failed: {operator_envelope!r}")

        scenario = root / "scenario.json"
        scenario.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "name": "reusable_external_engagement",
                    "description": "One durable thread answers repeated tasks across Worker generations.",
                    "codex_home": str(codex_home),
                    "steps": [
                        {"method": "initialize", "respond": {"result": {"codexHome": "${codex_home}", "userAgent": "fake-app-server/1", "capabilities": {"experimentalApi": False}}}},
                        {"method": "account/read", "respond": {"result": {"requiresOpenaiAuth": False}}},
                        {"method": "model/list", "respond": {"result": {"data": [{"model": "gpt-5.6", "isDefault": True, "supportedReasoningEfforts": [{"reasoningEffort": "medium"}]}], "nextCursor": None}}},
                        {"method": "thread/start", "occurrence": 1, "respond": {"result": {"thread": {"id": "thread-reusable"}}}},
                        {"method": "thread/start", "occurrence": 2, "respond": {"result": {"thread": {"id": "thread-abort"}}}},
                        {"method": "thread/read", "respond": {"error": {"code": -32600, "message": "thread not found"}}},
                        {"method": "turn/start", "occurrence": 1, "respond": {"result": {"turn": {"id": "turn-one"}}}, "emit": [{"kind": "notification", "method": "turn/completed", "params": {"threadId": "thread-reusable", "turn": {"id": "turn-one", "status": "completed", "items": [{"type": "agentMessage", "phase": "final_answer", "threadId": "thread-reusable", "turnId": "turn-one", "status": "completed", "text": "first reusable answer"}]}}}]},
                        {"method": "turn/start", "occurrence": 2, "respond": {"result": {"turn": {"id": "turn-two"}}}, "emit": [{"kind": "notification", "method": "turn/completed", "params": {"threadId": "thread-reusable", "turn": {"id": "turn-two", "status": "completed", "items": [{"type": "agentMessage", "phase": "final_answer", "threadId": "thread-reusable", "turnId": "turn-two", "status": "completed", "text": "second reusable answer"}]}}}]},
                        {"method": "turn/start", "occurrence": 3, "respond": {"result": {"turn": {"id": "turn-abort"}}}},
                        {"method": "turn/interrupt", "respond": {"result": {"interrupted": True}}, "emit": [{"kind": "notification", "method": "turn/completed", "params": {"threadId": "thread-abort", "turn": {"id": "turn-abort", "status": "interrupted", "items": []}}}]},
                        {"method": "thread/archive", "respond": {"result": {"thread": {"id": "thread-reusable", "archived": True}}}},
                    ],
                },
                separators=(",", ":"),
            ),
            encoding="utf-8",
        )
        codex = bin_root / "codex"
        native_codex.create_native_codex(
            codex,
            scenario=scenario,
            codex_home=codex_home,
            schema_source=schema_source,
        )
        isolated_scenario = root / "isolated-scenario.json"
        isolated_transcript = root / "isolated-transcript.jsonl"
        isolated_scenario.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "name": "isolated_external_engagement",
                    "description": "One dedicated Specialist returns an isolated-write result.",
                    "codex_home": str(isolated_codex_home),
                    "steps": [
                        {"method": "initialize", "respond": {"result": {"codexHome": "${codex_home}", "userAgent": "fake-app-server/1", "capabilities": {"experimentalApi": False}}}},
                        {"method": "account/read", "respond": {"result": {"requiresOpenaiAuth": False}}},
                        {"method": "model/list", "respond": {"result": {"data": [{"model": "gpt-5.6", "isDefault": True, "supportedReasoningEfforts": [{"reasoningEffort": "medium"}]}], "nextCursor": None}}},
                        {"method": "thread/read", "respond": {"error": {"code": -32600, "message": "thread not found"}}},
                        {"method": "thread/start", "respond": {"result": {"thread": {"id": "thread-isolated"}}}},
                        {"method": "turn/start", "respond": {"result": {"turn": {"id": "turn-isolated"}}}, "emit": [{"kind": "notification", "method": "turn/completed", "params": {"threadId": "thread-isolated", "turn": {"id": "turn-isolated", "status": "completed", "items": [{"type": "agentMessage", "phase": "final_answer", "threadId": "thread-isolated", "turnId": "turn-isolated", "status": "completed", "text": "isolated reusable answer"}]}}}]},
                        {"method": "thread/archive", "respond": {"result": {"thread": {"id": "thread-isolated", "archived": True}}}},
                    ],
                },
                separators=(",", ":"),
            ),
            encoding="utf-8",
        )
        isolated_codex = isolated_bin_root / "codex"
        native_codex.create_native_codex(
            isolated_codex,
            scenario=isolated_scenario,
            codex_home=isolated_codex_home,
            schema_source=schema_source,
            transcript=isolated_transcript,
        )
        interaction_scenario = root / "interaction-scenario.json"
        interaction_scenario.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "name": "external_engagement_interaction",
                    "description": "One Specialist requests unsupported user input and is interrupted.",
                    "codex_home": str(interaction_codex_home),
                    "steps": [
                        {"method": "initialize", "respond": {"result": {"codexHome": "${codex_home}", "userAgent": "fake-app-server/1", "capabilities": {"experimentalApi": False}}}},
                        {"method": "account/read", "respond": {"result": {"requiresOpenaiAuth": False}}},
                        {"method": "model/list", "respond": {"result": {"data": [{"model": "gpt-5.6", "isDefault": True, "supportedReasoningEfforts": [{"reasoningEffort": "medium"}]}], "nextCursor": None}}},
                        {"method": "thread/read", "respond": {"error": {"code": -32600, "message": "thread not found"}}},
                        {"method": "thread/start", "respond": {"result": {"thread": {"id": "thread-interaction"}}}},
                        {"method": "turn/start", "respond": {"result": {"turn": {"id": "turn-interaction"}}}, "emit": [{"kind": "request", "id": 8001, "method": "item/tool/requestUserInput", "params": {"threadId": "thread-interaction", "turnId": "turn-interaction", "questions": [{"id": "confirmation", "header": "Confirm", "question": "Continue?", "isSecret": False}]}}]},
                        {"method": "turn/interrupt", "respond": {"result": {"interrupted": True}}, "emit": [{"kind": "notification", "method": "turn/completed", "params": {"threadId": "thread-interaction", "turn": {"id": "turn-interaction", "status": "interrupted", "items": []}}}]},
                        {"method": "thread/archive", "respond": {"result": {"thread": {"id": "thread-interaction", "archived": True}}}},
                    ],
                },
                separators=(",", ":"),
            ),
            encoding="utf-8",
        )
        interaction_codex = interaction_bin_root / "codex"
        native_codex.create_native_codex(
            interaction_codex,
            scenario=interaction_scenario,
            codex_home=interaction_codex_home,
            schema_source=schema_source,
        )
        profile = "task022-specialist"
        added, added_envelope = invoke(
            binary,
            home,
            [
                "profile", "add", profile,
                "--codex-home", str(codex_home), "--native-subagents", "enabled",
                "--env", "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
                "--env", "LANG=en_US.UTF-8", "--env", "LC_ALL=en_US.UTF-8",
                "--", str(codex),
            ],
        )
        if added.returncode != 0:
            raise AssertionError(f"profile add failed: {added_envelope!r}")
        isolated_profile = "task022-isolated"
        isolated_added, isolated_envelope = invoke(
            binary,
            home,
            [
                "profile", "add", isolated_profile,
                "--codex-home", str(isolated_codex_home), "--native-subagents", "enabled",
                "--env", "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
                "--env", "LANG=en_US.UTF-8", "--env", "LC_ALL=en_US.UTF-8",
                "--", str(isolated_codex),
            ],
        )
        if isolated_added.returncode != 0:
            raise AssertionError(f"isolated profile add failed: {isolated_envelope!r}")
        interaction_profile = "task022-interaction"
        interaction_added, interaction_envelope = invoke(
            binary,
            home,
            [
                "profile", "add", interaction_profile,
                "--codex-home", str(interaction_codex_home), "--native-subagents", "enabled",
                "--env", "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
                "--env", "LANG=en_US.UTF-8", "--env", "LC_ALL=en_US.UTF-8",
                "--", str(interaction_codex),
            ],
        )
        if interaction_added.returncode != 0:
            raise AssertionError(
                f"interaction profile add failed: {interaction_envelope!r}"
            )

        try:
            opened = call(binary, home, workspace, owner, {
                "operation": "open_external_engagement",
                "external_controller_ref": {"namespace": "dolgorae.e2e", "kind": "host", "id": "one"},
                "label": "reusable lifecycle",
                "idempotency_key": "open",
            })
            engagement_id = str(opened["engagement_id"])
            open_request = {
                "operation": "open_external_engagement",
                "external_controller_ref": {"namespace": "dolgorae.e2e", "kind": "host", "id": "one"},
                "label": "reusable lifecycle",
                "idempotency_key": "open",
            }
            replayed_open = call(binary, home, workspace, owner, open_request)
            if replayed_open["engagement_id"] != engagement_id:
                raise AssertionError("open replay changed engagement identity")
            call(
                binary,
                home,
                workspace,
                child,
                {"operation": "get_external_engagement", "engagement_id": engagement_id},
                expected_error="CONTROLLER_MISMATCH",
            )
            hire_request = {
                "operation": "hire_external_specialist",
                "engagement_id": engagement_id,
                "role_ref": "researcher",
                "agent_configuration": {
                    "schema_version": 2,
                    "selected_profile": profile,
                    "model": "gpt-5.6",
                    "default_effort": "medium",
                    "purpose": "research",
                    "purpose_label": None,
                    "required_capabilities": [],
                    "instructions": "Answer each assigned task concisely.",
                    "execution_lane": "shared_readonly",
                    "required_assurance": "best_effort_personal_alpha",
                    "native_subagent_policy": "enabled",
                },
                "objective": "Serve repeated external tasks",
                "requested_access": "read_only",
                "idempotency_key": "hire",
            }
            for selected_profile in (None, "", ".invalid", "Invalid", "a" * 129):
                rejected = json.loads(json.dumps(hire_request))
                if selected_profile is None:
                    del rejected["agent_configuration"]["selected_profile"]
                else:
                    rejected["agent_configuration"]["selected_profile"] = selected_profile
                call(
                    binary, home, workspace, owner, rejected, child,
                    expected_error="INVALID_ARGUMENT",
                    invalid_request=not FACADE_SCHEMA.is_valid(rejected),
                )
            rejected = json.loads(json.dumps(hire_request))
            rejected["agent_configuration"]["global_profile_binding_sha256"] = "0" * 64
            error = call(
                binary, home, workspace, owner, rejected, child,
                expected_error="INVALID_ARGUMENT",
            )
            if error["details"]["argument"] != "global_profile_binding_sha256":
                raise AssertionError(f"stale binding rejected at the wrong boundary: {error!r}")
            engagement = call(binary, home, workspace, owner, {
                "operation": "get_external_engagement", "engagement_id": engagement_id,
            })
            if engagement["specialists"] or list((state_root / "runs").glob("*")):
                raise AssertionError("rejected Profile input admitted a Specialist or Run")

            hired = call(binary, home, workspace, owner, hire_request, child)
            run_id = str(hired["specialist_run_id"])
            replayed_hire = call(binary, home, workspace, owner, hire_request, child)
            if replayed_hire["specialist_run_id"] != run_id:
                raise AssertionError("hire replay changed Specialist identity")
            changed_hire = dict(hire_request)
            changed_hire["objective"] = "Changed input under the same key"
            call(
                binary,
                home,
                workspace,
                owner,
                changed_hire,
                child,
                expected_error="IDEMPOTENCY_CONFLICT",
            )

            cursor = 0
            for sequence in (1, 2):
                request = task_request(engagement_id, run_id, sequence)
                assigned = call(binary, home, workspace, owner, request)
                task_id = str(assigned["task_id"])
                replayed = call(binary, home, workspace, owner, request)
                if replayed["task_id"] != task_id:
                    raise AssertionError("task replay changed task identity")
                changed = dict(request)
                changed["objective"] = "Changed objective under the same key"
                call(
                    binary,
                    home,
                    workspace,
                    owner,
                    changed,
                    expected_error="IDEMPOTENCY_CONFLICT",
                )
                waited = call(binary, home, workspace, owner, {
                    "operation": "await_external_specialist_tasks",
                    "engagement_id": engagement_id,
                    "task_ids": [task_id],
                    "return_when": "all",
                    "transport_wait_seconds": 10,
                })
                if waited["pending"] or waited["tasks"][0]["state"] != "completed_not_delivered":
                    raise AssertionError(f"task did not become collectable: {waited!r}")
                collected = call(binary, home, workspace, owner, {
                    "operation": "collect_external_specialist_results",
                    "engagement_id": engagement_id,
                    "after_sequence": cursor,
                    "limit": 8,
                })
                if not collected["tasks"] or collected["tasks"][-1]["state"] != "delivered":
                    raise AssertionError(f"task was not durably delivered: {collected!r}")
                cursor = int(collected["next_after_sequence"])

                if sequence == 1:
                    runtime = state_root / "runtime" / "runs" / f"{run_id}.json"
                    record = json.loads(runtime.read_text(encoding="utf-8"))
                    pid = int(record["identity"]["pid"])
                    stop_worker(pid)

            snapshot = call(binary, home, workspace, owner, {
                "operation": "get_external_engagement",
                "engagement_id": engagement_id,
            })
            if snapshot["state"] != "active" or len(snapshot["specialists"]) != 1:
                raise AssertionError(f"reconnected snapshot lost engagement state: {snapshot!r}")
            call(
                binary,
                home,
                workspace,
                owner,
                {
                    "operation": "hire_external_specialist",
                    "engagement_id": engagement_id,
                    "role_ref": "nested",
                    "agent_configuration": hire_request["agent_configuration"],
                    "objective": "must be denied from a Specialist thread",
                    "requested_access": "read_only",
                    "idempotency_key": "nested-hire",
                },
                nested_child,
                expected_error="SPECIALIST_POLICY_DENIED",
                environment_overrides={"CODEX_THREAD_ID": "thread-reusable"},
            )
            denied_write = task_request(engagement_id, run_id, 99)
            denied_write["execution_intent"] = "isolated_write"
            call(
                binary,
                home,
                workspace,
                owner,
                denied_write,
                expected_error="SPECIALIST_POLICY_DENIED",
            )
            release_request = {
                "operation": "release_external_specialist",
                "engagement_id": engagement_id,
                "specialist_run_id": run_id,
                "reason": "E2E complete",
                "idempotency_key": "release",
            }
            released = call(binary, home, workspace, owner, release_request)
            if released["state"] != "released":
                raise AssertionError(f"member was not released: {released!r}")
            if call(binary, home, workspace, owner, release_request)["state"] != "released":
                raise AssertionError("release replay did not preserve released state")
            isolated_hired = call(
                binary,
                home,
                workspace,
                owner,
                {
                    "operation": "hire_external_specialist",
                    "engagement_id": engagement_id,
                    "role_ref": "implementer",
                    "agent_configuration": {
                        "schema_version": 2,
                        "selected_profile": isolated_profile,
                        "model": "gpt-5.6",
                        "default_effort": "medium",
                        "purpose": "implementation",
                        "purpose_label": None,
                        "required_capabilities": [],
                        "instructions": "Return the isolated task result.",
                        "execution_lane": "dedicated",
                        "required_assurance": "best_effort_personal_alpha",
                        "native_subagent_policy": "enabled",
                    },
                    "objective": "Exercise isolated-write lifecycle",
                    "requested_access": "isolated_write",
                    "idempotency_key": "hire-isolated",
                },
                isolated_child,
            )
            isolated_run_id = str(isolated_hired["specialist_run_id"])
            isolated_request = task_request(engagement_id, isolated_run_id, 100)
            isolated_request["execution_intent"] = "isolated_write"
            isolated_assigned = call(binary, home, workspace, owner, isolated_request)
            isolated_task_id = str(isolated_assigned["task_id"])
            isolated_waited = call(binary, home, workspace, owner, {
                "operation": "await_external_specialist_tasks",
                "engagement_id": engagement_id,
                "task_ids": [isolated_task_id],
                "return_when": "all",
                "transport_wait_seconds": 10,
            })
            if isolated_waited["tasks"][0]["state"] != "completed_not_delivered":
                raise AssertionError(f"isolated task did not complete: {isolated_waited!r}")
            isolated_collected = call(binary, home, workspace, owner, {
                "operation": "collect_external_specialist_results",
                "engagement_id": engagement_id,
                "after_sequence": cursor,
                "limit": 8,
            })
            isolated_result = isolated_collected["tasks"][0]["result"]["isolated_change"]
            if isolated_result["format"] != "git_diff_binary_base64":
                raise AssertionError(f"isolated result used the wrong format: {isolated_result!r}")
            base64.b64decode(isolated_result["patch_base64"], validate=True)
            cursor = int(isolated_collected["next_after_sequence"])
            isolated_release = call(binary, home, workspace, owner, {
                "operation": "release_external_specialist",
                "engagement_id": engagement_id,
                "specialist_run_id": isolated_run_id,
                "reason": "isolated E2E complete",
                "idempotency_key": "release-isolated",
            })
            if isolated_release["state"] != "released":
                raise AssertionError(f"isolated member was not released: {isolated_release!r}")
            isolated_root = (
                state_root
                / "orchestration"
                / "isolated"
                / engagement_id
                / isolated_run_id
            )
            if isolated_root.exists():
                raise AssertionError("isolated worktree survived member release")
            if call(binary, home, workspace, owner, {
                "operation": "release_external_specialist",
                "engagement_id": engagement_id,
                "specialist_run_id": isolated_run_id,
                "reason": "isolated E2E complete",
                "idempotency_key": "release-isolated",
            })["state"] != "released":
                raise AssertionError("isolated release replay did not converge")

            canonical_hired = call(
                binary,
                home,
                workspace,
                owner,
                {
                    "operation": "hire_external_specialist",
                    "engagement_id": engagement_id,
                    "role_ref": "implementer",
                    "agent_configuration": {
                        "schema_version": 2,
                        "selected_profile": isolated_profile,
                        "model": "gpt-5.6",
                        "default_effort": "medium",
                        "purpose": "implementation",
                        "purpose_label": None,
                        "required_capabilities": [],
                        "instructions": "Return the canonical task result.",
                        "execution_lane": "dedicated",
                        "required_assurance": "best_effort_personal_alpha",
                        "native_subagent_policy": "enabled",
                    },
                    "objective": "Exercise canonical-write lifecycle",
                    "requested_access": "canonical_workspace_write",
                    "idempotency_key": "hire-canonical",
                },
                canonical_child,
            )
            canonical_run_id = str(canonical_hired["specialist_run_id"])
            dedicated_messages = [
                json.loads(line)
                for line in isolated_transcript.read_text(encoding="utf-8").splitlines()
                if line.strip()
            ]
            starts = [
                message
                for message in dedicated_messages
                if message.get("method") == "thread/start"
            ]
            if len(starts) != 1:
                raise AssertionError(f"dedicated launch count is invalid: {starts!r}")
            if starts[0]["params"]["sandbox"] != "workspace-write":
                raise AssertionError("isolated member did not start in its writable worktree")
            canonical_request = task_request(engagement_id, canonical_run_id, 101)
            canonical_request["execution_intent"] = "canonical_workspace_write"
            canonical_assigned = call(binary, home, workspace, owner, canonical_request)
            dedicated_messages = [
                json.loads(line)
                for line in isolated_transcript.read_text(encoding="utf-8").splitlines()
                if line.strip()
            ]
            starts = [
                message
                for message in dedicated_messages
                if message.get("method") == "thread/start"
            ]
            if len(starts) != 2 or starts[1]["params"]["sandbox"] != "workspace-write":
                raise AssertionError(
                    "canonical member did not gain write sandbox with writer authority"
                )
            canonical_task_id = str(canonical_assigned["task_id"])
            canonical_waited = call(binary, home, workspace, owner, {
                "operation": "await_external_specialist_tasks",
                "engagement_id": engagement_id,
                "task_ids": [canonical_task_id],
                "return_when": "all",
                "transport_wait_seconds": 10,
            })
            if canonical_waited["tasks"][0]["state"] != "completed_not_delivered":
                raise AssertionError(f"canonical task did not complete: {canonical_waited!r}")
            writer_status, writer_envelope = invoke(
                binary,
                home,
                ["workspace", "writer", "status", "--workspace", str(workspace)],
            )
            if writer_status.returncode != 0:
                raise AssertionError(f"writer status failed: {writer_envelope!r}")
            if writer_envelope["data"]["authority_state"] != "none":
                raise AssertionError(
                    f"idle canonical Writer was not reconciled: {writer_envelope!r}"
                )
            canonical_collected = call(binary, home, workspace, owner, {
                "operation": "collect_external_specialist_results",
                "engagement_id": engagement_id,
                "after_sequence": cursor,
                "limit": 8,
            })
            if canonical_collected["tasks"][0]["state"] != "delivered":
                raise AssertionError(f"canonical result was not delivered: {canonical_collected!r}")
            canonical_release = call(binary, home, workspace, owner, {
                "operation": "release_external_specialist",
                "engagement_id": engagement_id,
                "specialist_run_id": canonical_run_id,
                "reason": "canonical E2E complete",
                "idempotency_key": "release-canonical",
            })
            if canonical_release["state"] != "released":
                raise AssertionError(f"canonical member was not released: {canonical_release!r}")
            closed = call(binary, home, workspace, owner, {
                "operation": "close_external_engagement",
                "engagement_id": engagement_id,
                "mode": "complete",
                "reason": "E2E complete",
                "idempotency_key": "close",
            })
            if closed["state"] != "completed":
                raise AssertionError(f"engagement was not completed: {closed!r}")
            completed_runtime = state_root / "runtime" / "runs" / f"{run_id}.json"
            completed_record = json.loads(completed_runtime.read_text(encoding="utf-8"))
            stop_worker(int(completed_record["identity"]["pid"]))

            interaction_opened = call(binary, home, workspace, owner, {
                "operation": "open_external_engagement",
                "external_controller_ref": {
                    "namespace": "dolgorae.e2e", "kind": "host", "id": "interaction"
                },
                "label": "unsupported interaction",
                "idempotency_key": "open-interaction",
            })
            interaction_engagement_id = str(interaction_opened["engagement_id"])
            interaction_hired = call(binary, home, workspace, owner, {
                "operation": "hire_external_specialist",
                "engagement_id": interaction_engagement_id,
                "role_ref": "researcher",
                "agent_configuration": {
                    "schema_version": 2, "selected_profile": interaction_profile, "model": "gpt-5.6",
                    "default_effort": "medium", "purpose": "research", "purpose_label": None,
                    "required_capabilities": [], "instructions": "Request an unsupported approval.",
                    "execution_lane": "shared_readonly",
                    "required_assurance": "best_effort_personal_alpha",
                    "native_subagent_policy": "enabled",
                },
                "objective": "Exercise unsupported interaction reconciliation",
                "requested_access": "read_only",
                "idempotency_key": "hire-interaction",
            }, interaction_child)
            interaction_run_id = str(interaction_hired["specialist_run_id"])
            interaction_assigned = call(
                binary,
                home,
                workspace,
                owner,
                task_request(interaction_engagement_id, interaction_run_id, 4),
            )
            interaction_task_id = str(interaction_assigned["task_id"])
            interaction_waited = call(binary, home, workspace, owner, {
                "operation": "await_external_specialist_tasks",
                "engagement_id": interaction_engagement_id,
                "task_ids": [interaction_task_id],
                "return_when": "all",
                "transport_wait_seconds": 10,
            })
            if interaction_waited["pending"]:
                interaction_waited = call(binary, home, workspace, owner, {
                    "operation": "await_external_specialist_tasks",
                    "engagement_id": interaction_engagement_id,
                    "task_ids": [interaction_task_id],
                    "return_when": "all",
                    "transport_wait_seconds": 10,
                })
            if interaction_waited["pending"] or len(interaction_waited["tasks"]) != 1:
                raise AssertionError(
                    f"unsupported interaction did not settle exactly once: {interaction_waited!r}"
                )
            interaction_task = interaction_waited["tasks"][0]
            if (
                interaction_task["state"] != "failed"
                or interaction_task["safe_error_code"]
                != "SPECIALIST_INTERACTION_UNSUPPORTED"
            ):
                raise AssertionError(
                    f"unsupported interaction was not terminalized safely: {interaction_waited!r}"
                )
            abort_opened = call(binary, home, workspace, owner, {
                "operation": "open_external_engagement",
                "external_controller_ref": {"namespace": "dolgorae.e2e", "kind": "host", "id": "abort"},
                "label": "abort lifecycle",
                "idempotency_key": "open-abort",
            })
            abort_engagement_id = str(abort_opened["engagement_id"])
            abort_hired = call(binary, home, workspace, owner, {
                "operation": "hire_external_specialist",
                "engagement_id": abort_engagement_id,
                "role_ref": "researcher",
                "agent_configuration": {
                    "schema_version": 2, "selected_profile": profile, "model": "gpt-5.6",
                    "default_effort": "medium", "purpose": "research", "purpose_label": None,
                    "required_capabilities": [], "instructions": "Wait until interrupted.",
                    "execution_lane": "shared_readonly",
                    "required_assurance": "best_effort_personal_alpha",
                    "native_subagent_policy": "enabled",
                },
                "objective": "Exercise durable abort",
                "requested_access": "read_only",
                "idempotency_key": "hire-abort",
            }, abort_child)
            abort_run_id = str(abort_hired["specialist_run_id"])
            assigned = call(
                binary,
                home,
                workspace,
                owner,
                task_request(abort_engagement_id, abort_run_id, 3),
            )
            if assigned["state"] != "running":
                raise AssertionError(f"abort fixture did not remain running: {assigned!r}")
            call(
                binary,
                home,
                workspace,
                owner,
                {
                    "operation": "release_external_specialist",
                    "engagement_id": abort_engagement_id,
                    "specialist_run_id": abort_run_id,
                    "reason": "must refuse active work",
                    "idempotency_key": "release-active",
                },
                expected_error="ENGAGEMENT_STATE_CONFLICT",
            )
            cancel_request = {
                "operation": "cancel_external_specialist_task",
                "engagement_id": abort_engagement_id,
                "task_id": str(assigned["task_id"]),
                "reason": "exercise cancellation",
                "idempotency_key": "cancel-running",
            }
            cancelled = call(binary, home, workspace, owner, cancel_request)
            if cancelled["state"] != "cancelled":
                raise AssertionError(f"running task was not durably cancelled: {cancelled!r}")
            if call(binary, home, workspace, owner, cancel_request) != cancelled:
                raise AssertionError("cancel replay changed its durable result")
            aborted = call(binary, home, workspace, owner, {
                "operation": "close_external_engagement",
                "engagement_id": abort_engagement_id,
                "mode": "abort",
                "reason": "host abort test",
                "idempotency_key": "abort",
            })
            if aborted["state"] != "aborted":
                raise AssertionError(f"engagement was not durably aborted: {aborted!r}")
            call(binary, home, workspace, owner, {
                "operation": "release_external_specialist",
                "engagement_id": interaction_engagement_id,
                "specialist_run_id": interaction_run_id,
                "reason": "interaction E2E complete",
                "idempotency_key": "release-interaction",
            })
            interaction_closed = call(binary, home, workspace, owner, {
                "operation": "close_external_engagement",
                "engagement_id": interaction_engagement_id,
                "mode": "complete",
                "reason": "interaction E2E complete",
                "idempotency_key": "close-interaction",
            })
            if interaction_closed["state"] != "completed":
                raise AssertionError(
                    f"interaction engagement did not close: {interaction_closed!r}"
                )

        finally:
            invoke(binary, home, [
                "profile", "server", "stop", profile,
                "--operator-file", str(operator),
            ])
            invoke(binary, home, [
                "profile", "server", "stop", isolated_profile,
                "--operator-file", str(operator),
            ])
            invoke(binary, home, [
                "profile", "server", "stop", interaction_profile,
                "--operator-file", str(operator),
            ])


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    arguments = parser.parse_args()
    validate(arguments.binary.resolve())
    print("external engagement CLI lifecycle passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
