#!/usr/bin/env python3
"""Black-box TASK-022 reusable External Specialist Engagement lifecycle."""

from __future__ import annotations

import argparse
import base64
import json
import os
import pathlib
import signal
import sqlite3
import subprocess
import tempfile
import time

import native_codex
from schema_support import assert_valid, validator


PROTOCOL_ROOT = pathlib.Path(__file__).resolve().parents[2] / "docs" / "protocol"
FACADE_SCHEMA = validator(
    PROTOCOL_ROOT, "dolgorae-external-specialist-facade-v2.schema.json"
)
FACADE_V3_SCHEMA = validator(
    PROTOCOL_ROOT, "dolgorae-external-specialist-facade-v3.schema.json"
)
FACADE_V3_TASK_SUMMARY_SCHEMA = validator(
    PROTOCOL_ROOT,
    "dolgorae-external-specialist-facade-v3.schema.json",
    "#/$defs/structured_task_summary",
)
FACADE_V3_HOMOGENEOUS_RESULT_SCHEMAS = {
    "await_external_specialist_tasks_result": validator(
        PROTOCOL_ROOT,
        "dolgorae-external-specialist-facade-v3.schema.json",
        "#/$defs/structured_await_result",
    ),
    "collect_external_specialist_results_result": validator(
        PROTOCOL_ROOT,
        "dolgorae-external-specialist-facade-v3.schema.json",
        "#/$defs/structured_collect_result",
    ),
}
MACHINE_SCHEMA = validator(PROTOCOL_ROOT, "dolgorae-machine-v2.schema.json")


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
    v3_contract: bool = False,
    structured_task_ids: set[str] | None = None,
) -> dict[str, object]:
    facade_schema = (
        FACADE_V3_SCHEMA
        if v3_contract
        or request.get("schema") == "dolgorae-external-specialist-facade/v3"
        else FACADE_SCHEMA
    )
    if invalid_request:
        if facade_schema.is_valid(request):
            raise AssertionError("negative request unexpectedly conforms to the schema")
    else:
        assert_valid(request, facade_schema, f"{request.get('operation')} request")
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
    assert_valid(envelope, MACHINE_SCHEMA, f"{request.get('operation')} Machine envelope")
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
            facade_schema,
            f"{request.get('operation')} error",
        )
        return envelope["error"]
    if str(owner) in completed.stdout or (
        new_controller is not None and str(new_controller) in completed.stdout
    ):
        raise AssertionError("engagement response disclosed a credential carrier path")
    result = envelope["data"]
    assert_valid(result, facade_schema, f"{request.get('operation')} result")
    if structured_task_ids is not None:
        tasks = {
            str(task["task_id"]): task
            for task in result.get("tasks", [])
        }
        missing = structured_task_ids - tasks.keys()
        if missing:
            raise AssertionError(
                f"structured v3 result omitted requested tasks: {sorted(missing)!r}"
            )
        for task_id in sorted(structured_task_ids):
            assert_valid(
                tasks[task_id],
                FACADE_V3_TASK_SUMMARY_SCHEMA,
                f"structured v3 task {task_id}",
            )
        if tasks.keys() == structured_task_ids:
            operation = str(result.get("operation"))
            page_schema = FACADE_V3_HOMOGENEOUS_RESULT_SCHEMAS.get(operation)
            if page_schema is None:
                raise AssertionError(
                    f"missing homogeneous v3 result schema for {operation}"
                )
            assert_valid(result, page_schema, f"homogeneous v3 {operation}")
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
        reusable_transcript = root / "reusable-transcript.jsonl"
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
                        {"method": "turn/start", "occurrence": 3, "respond": {"result": {"turn": {"id": "turn-v3"}}}, "emit": [{"kind": "notification", "method": "turn/completed", "params": {"threadId": "thread-reusable", "turn": {"id": "turn-v3", "status": "completed", "items": [{"type": "agentMessage", "phase": "final_answer", "threadId": "thread-reusable", "turnId": "turn-v3", "status": "completed", "text": json.dumps({"summary": "structured reusable review", "findings": [], "criterion_assessments": [{"criterion_id": "C-reusable", "status": "met", "explanation": "accepted context and candidate were checked", "evidence": [{"basis": "context", "description": "caller requirements", "path": None, "line_start": None, "line_end": None, "context_id": "requirements"}], "remaining_gap": None}], "evidence_limits": [], "overall_assessment": "requirements_met"}, separators=(",", ":"), ensure_ascii=False)}]}}}]},
                        {"method": "turn/start", "occurrence": 4, "respond": {"result": {"turn": {"id": "turn-invalid-v3"}}}, "emit": [{"kind": "notification", "method": "turn/completed", "params": {"threadId": "thread-reusable", "turn": {"id": "turn-invalid-v3", "status": "completed", "items": [{"type": "agentMessage", "phase": "final_answer", "threadId": "thread-reusable", "turnId": "turn-invalid-v3", "status": "completed", "text": json.dumps({"summary": "incomplete report", "findings": [], "criterion_assessments": [], "evidence_limits": [], "overall_assessment": "requirements_met"}, separators=(",", ":"))}]}}}]},
                        {"method": "turn/start", "occurrence": 5, "respond": {"result": {"turn": {"id": "turn-abort"}}}},
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
            transcript=reusable_transcript,
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
                        {"method": "turn/start", "occurrence": 2, "respond": {"result": {"turn": {"id": "turn-isolated-v3"}}}, "emit": [{"kind": "notification", "method": "turn/completed", "params": {"threadId": "thread-isolated", "turn": {"id": "turn-isolated-v3", "status": "completed", "items": [{"type": "agentMessage", "phase": "final_answer", "threadId": "thread-isolated", "turnId": "turn-isolated-v3", "status": "completed", "text": json.dumps({"summary": "structured isolated review", "findings": [], "criterion_assessments": [{"criterion_id": "C-isolated", "status": "met", "explanation": "the isolated candidate was checked", "evidence": [{"basis": "candidate", "description": "isolated worktree evidence", "path": "isolated.txt", "line_start": 1, "line_end": 1, "context_id": None}], "remaining_gap": None}], "evidence_limits": [], "overall_assessment": "requirements_met"}, separators=(",", ":"))}]}}}]},
                        {"method": "turn/start", "occurrence": 3, "respond": {"result": {"turn": {"id": "turn-isolated-transient"}}}, "emit": [{"kind": "notification", "method": "turn/completed", "params": {"threadId": "thread-isolated", "turn": {"id": "turn-isolated-transient", "status": "completed", "items": [{"type": "agentMessage", "phase": "final_answer", "threadId": "thread-isolated", "turnId": "turn-isolated-transient", "status": "completed", "text": json.dumps({"summary": "transient capture recovered", "findings": [], "criterion_assessments": [{"criterion_id": "C-transient", "status": "met", "explanation": "the durable terminal was reconciled", "evidence": [{"basis": "candidate", "description": "isolated worktree evidence", "path": "isolated.txt", "line_start": 1, "line_end": 1, "context_id": None}], "remaining_gap": None}], "evidence_limits": [], "overall_assessment": "requirements_met"}, separators=(",", ":"))}]}}}]},
                        {"method": "turn/start", "occurrence": 4, "respond": {"result": {"turn": {"id": "turn-isolated-expired"}}}, "emit": [{"kind": "notification", "method": "turn/completed", "params": {"threadId": "thread-isolated", "turn": {"id": "turn-isolated-expired", "status": "completed", "items": [{"type": "agentMessage", "phase": "final_answer", "threadId": "thread-isolated", "turnId": "turn-isolated-expired", "status": "completed", "text": json.dumps({"summary": "capture deadline case", "findings": [], "criterion_assessments": [{"criterion_id": "C-expired", "status": "met", "explanation": "the candidate was checked", "evidence": [{"basis": "candidate", "description": "isolated worktree evidence", "path": "isolated.txt", "line_start": 1, "line_end": 1, "context_id": None}], "remaining_gap": None}], "evidence_limits": [], "overall_assessment": "requirements_met"}, separators=(",", ":"))}]}}}]},
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
            missing_id = "01990000-0000-7000-8000-000000000001"
            for request, code, details in (
                ({"operation": "get_external_engagement", "engagement_id": missing_id},
                 "ENGAGEMENT_NOT_FOUND", {"required_action": "use_existing_engagement"}),
                ({"operation": "release_external_specialist", "engagement_id": engagement_id,
                  "specialist_run_id": missing_id, "reason": "unknown member",
                  "idempotency_key": "release-unknown"},
                 "SPECIALIST_NOT_MEMBER", {"specialist_run_id": missing_id}),
                ({"operation": "cancel_external_specialist_task", "engagement_id": engagement_id,
                  "task_id": missing_id, "reason": "unknown task", "idempotency_key": "cancel-unknown"},
                 "SPECIALIST_TASK_NOT_FOUND", {"task_id": missing_id}),
            ):
                error = call(binary, home, workspace, owner, request, expected_error=code)
                if error["details"] != details or error["retryable"]:
                    raise AssertionError(f"incorrect unknown-identity error: {error!r}")
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
            for overrides in (
                {"schema_version": 1},
                {"runtime_profile": profile},
                {"runtime_profile": None},
            ):
                rejected = json.loads(json.dumps(hire_request))
                rejected["agent_configuration"].update(overrides)
                call(
                    binary, home, workspace, owner, rejected, child,
                    expected_error="INVALID_ARGUMENT", invalid_request=True,
                )
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
            rejected["agent_configuration"]["selected_profile"] = "missing-profile"
            error = call(
                binary, home, workspace, owner, rejected, child,
                expected_error="PROFILE_NOT_FOUND",
            )
            if error["details"]["profile"] != "missing-profile":
                raise AssertionError(f"missing Profile rejected at the wrong boundary: {error!r}")
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
            database = state_root / "orchestration" / "orchestration.sqlite3"
            with sqlite3.connect(database) as connection:
                original_state = connection.execute(
                    "SELECT state FROM members WHERE specialist_run_id=?", (run_id,)
                ).fetchone()[0]
                connection.execute(
                    "UPDATE members SET state='provisioning' WHERE specialist_run_id=?", (run_id,)
                )
            try:
                for request in (
                    {"operation": "release_external_specialist", "engagement_id": engagement_id,
                     "specialist_run_id": run_id, "reason": "still provisioning",
                     "idempotency_key": "release-provisioning"},
                    {"operation": "close_external_engagement", "engagement_id": engagement_id,
                     "mode": "abort", "reason": "still provisioning",
                     "idempotency_key": "close-provisioning"},
                ):
                    error = call(binary, home, workspace, owner, request,
                                 expected_error="ENGAGEMENT_STATE_CONFLICT")
                    if not error["retryable"] or error["details"] != {
                        "specialist_run_id": run_id, "required_action": "retry_after_reconciliation"
                    }:
                        raise AssertionError(f"incorrect provisioning-lease error: {error!r}")
            finally:
                with sqlite3.connect(database) as connection:
                    connection.execute(
                        "UPDATE members SET state=? WHERE specialist_run_id=?", (original_state, run_id)
                    )
            error = call(binary, home, workspace, owner, {
                "operation": "close_external_engagement", "engagement_id": engagement_id,
                "mode": "complete", "reason": "unreleased member", "idempotency_key": "close-unreleased",
            }, expected_error="STATE_CONFLICT")
            if error["retryable"] or error["details"] != {"required_action": "inspect_engagement"}:
                raise AssertionError(f"incorrect close conflict: {error!r}")
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

            v3_request = {
                "schema": "dolgorae-external-specialist-facade/v3",
                "operation": "assign_external_specialist_task",
                "engagement_id": engagement_id,
                "specialist_run_id": run_id,
                "external_request_ref": {
                    "namespace": "dolgorae.e2e",
                    "kind": "completion-review",
                    "id": "v3-reusable",
                },
                "task": {
                    "purpose": "completion",
                    "brief": "재사용 Specialist로 완료를 검토하세요.\n`$HOME`은 데이터입니다.",
                    "contexts": [{
                        "id": "requirements",
                        "content": '첫 줄\r\n"인용된" 둘째 줄',
                        "provenance": "caller supplied requirements",
                    }],
                    "criteria": [{
                        "id": "C-reusable",
                        "statement": "승인된 문맥과 후보를 구분한다.",
                        "source_context_ids": ["requirements"],
                    }],
                    "expected_output": "structured_review_v3",
                },
                "execution_intent": "read_only",
                "deadline_seconds": 60,
                "idempotency_key": "task-v3",
            }
            v3_assigned = call(binary, home, workspace, owner, v3_request)
            v3_task_id = str(v3_assigned["task_id"])
            if call(binary, home, workspace, owner, v3_request)["task_id"] != v3_task_id:
                raise AssertionError("v3 task replay changed task identity")
            changed_v3 = json.loads(json.dumps(v3_request))
            changed_v3["task"]["contexts"][0]["content"] = "changed context"
            call(
                binary,
                home,
                workspace,
                owner,
                changed_v3,
                expected_error="IDEMPOTENCY_CONFLICT",
            )
            v3_waited = call(binary, home, workspace, owner, {
                "operation": "await_external_specialist_tasks",
                "engagement_id": engagement_id,
                "task_ids": [v3_task_id],
                "return_when": "all",
                "transport_wait_seconds": 10,
            }, structured_task_ids={v3_task_id})
            if v3_waited["tasks"][0]["state"] != "completed_not_delivered":
                raise AssertionError(f"v3 task did not become collectable: {v3_waited!r}")
            v3_collected = call(binary, home, workspace, owner, {
                "operation": "collect_external_specialist_results",
                "engagement_id": engagement_id,
                "after_sequence": cursor,
                "limit": 8,
            }, structured_task_ids={v3_task_id})
            v3_result = v3_collected["tasks"][0]["result"]
            if (
                v3_result["overall_assessment"] != "requirements_met"
                or v3_result["criterion_assessments"][0]["criterion_id"] != "C-reusable"
            ):
                raise AssertionError(
                    f"v3 report was not preserved through collection: {v3_result!r}"
                )
            cursor = int(v3_collected["next_after_sequence"])
            with sqlite3.connect(database) as connection:
                row = connection.execute(
                    "SELECT request_json FROM tasks WHERE task_id=?", (v3_task_id,)
                ).fetchone()
            accepted_request = json.loads(row[0])
            expected_request = {
                key: value for key, value in v3_request.items() if key != "idempotency_key"
            }
            if accepted_request != expected_request:
                raise AssertionError(
                    "v3 durable task did not preserve the complete accepted request"
                )
            transcript_text = reusable_transcript.read_text(encoding="utf-8")
            task_markers = (
                "재사용 Specialist로 완료를 검토하세요.",
                "C-reusable",
                "$HOME",
            )
            if any(marker not in transcript_text for marker in task_markers):
                raise AssertionError(
                    "v3 reusable task was not delivered as model-readable data"
                )
            reusable_messages = [
                json.loads(line)
                for line in transcript_text.splitlines()
                if line.strip()
            ]
            thread_starts = [
                message
                for message in reusable_messages
                if message.get("method") == "thread/start"
            ]
            if not thread_starts:
                raise AssertionError("v3 reusable task did not start a thread")
            developer_instructions = [
                message["params"]["developerInstructions"]
                for message in thread_starts
            ]
            role_instructions = hire_request["agent_configuration"]["instructions"]
            if any(
                not isinstance(instructions, str)
                or not instructions
                or role_instructions not in instructions
                for instructions in developer_instructions
            ):
                raise AssertionError(
                    "v3 reusable Role was absent from developerInstructions"
                )
            if any(
                marker in instructions
                for instructions in developer_instructions
                for marker in task_markers
            ):
                raise AssertionError(
                    "v3 reusable task bytes leaked into developerInstructions"
                )

            invalid_v3 = json.loads(json.dumps(v3_request))
            invalid_v3["external_request_ref"]["id"] = "v3-invalid"
            invalid_v3["idempotency_key"] = "task-v3-invalid"
            invalid_assigned = call(
                binary,
                home,
                workspace,
                owner,
                invalid_v3,
            )
            call(
                binary,
                home,
                workspace,
                owner,
                {
                    "operation": "await_external_specialist_tasks",
                    "engagement_id": engagement_id,
                    "task_ids": [str(invalid_assigned["task_id"])],
                    "return_when": "all",
                    "transport_wait_seconds": 10,
                },
                expected_error="REVIEW_OUTPUT_INVALID",
                v3_contract=True,
            )
            with sqlite3.connect(database) as connection:
                invalid_state = connection.execute(
                    "SELECT state,artifact_id,result_sha256,safe_error_code "
                    "FROM tasks ORDER BY created_at_ms DESC,task_id DESC LIMIT 1"
                ).fetchone()
            if invalid_state != ("failed", None, None, "REVIEW_OUTPUT_INVALID"):
                raise AssertionError(
                    f"invalid v3 output reached durable result storage: {invalid_state!r}"
                )

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
            isolated_root = (
                state_root
                / "orchestration"
                / "isolated"
                / engagement_id
                / isolated_run_id
            )
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

            isolated_v3_request = {
                "schema": "dolgorae-external-specialist-facade/v3",
                "operation": "assign_external_specialist_task",
                "engagement_id": engagement_id,
                "specialist_run_id": isolated_run_id,
                "external_request_ref": {
                    "namespace": "dolgorae.e2e",
                    "kind": "completion-review",
                    "id": "v3-isolated",
                },
                "task": {
                    "purpose": "completion",
                    "brief": "Review the isolated result.",
                    "contexts": [],
                    "criteria": [{
                        "id": "C-isolated",
                        "statement": "The isolated candidate was checked.",
                        "source_context_ids": [],
                    }],
                    "expected_output": "structured_review_v3",
                },
                "execution_intent": "isolated_write",
                "deadline_seconds": 60,
                "idempotency_key": "task-v3-isolated",
            }
            isolated_v3_assigned = call(
                binary, home, workspace, owner, isolated_v3_request
            )
            isolated_v3_task_id = str(isolated_v3_assigned["task_id"])
            isolated_v3_waited = call(
                binary,
                home,
                workspace,
                owner,
                {
                    "operation": "await_external_specialist_tasks",
                    "engagement_id": engagement_id,
                    "task_ids": [isolated_v3_task_id],
                    "return_when": "all",
                    "transport_wait_seconds": 10,
                },
                v3_contract=True,
                structured_task_ids={isolated_v3_task_id},
            )
            if isolated_v3_waited["tasks"][0]["state"] != "completed_not_delivered":
                raise AssertionError(
                    f"isolated v3 task did not complete: {isolated_v3_waited!r}"
                )
            isolated_v3_cursor = cursor
            isolated_v3_collected = call(
                binary,
                home,
                workspace,
                owner,
                {
                    "operation": "collect_external_specialist_results",
                    "engagement_id": engagement_id,
                    "after_sequence": isolated_v3_cursor,
                    "limit": 8,
                },
                v3_contract=True,
                structured_task_ids={isolated_v3_task_id},
            )
            if (
                len(isolated_v3_collected["tasks"]) != 1
                or isolated_v3_collected["tasks"][0]["task_id"]
                != isolated_v3_task_id
            ):
                raise AssertionError(
                    f"isolated v3 collection selected the wrong page: {isolated_v3_collected!r}"
                )
            isolated_v3_summary = isolated_v3_collected["tasks"][0]
            isolated_v3_result = isolated_v3_summary["result"]
            if isolated_v3_result["final_response"]["overall_assessment"] != "requirements_met":
                raise AssertionError(
                    f"isolated v3 report was not normalized: {isolated_v3_result!r}"
                )
            isolated_v3_patch = isolated_v3_result["isolated_change"]
            if isolated_v3_patch["format"] != "git_diff_binary_base64":
                raise AssertionError(
                    f"isolated v3 result used the wrong format: {isolated_v3_result!r}"
                )
            base64.b64decode(isolated_v3_patch["patch_base64"], validate=True)
            isolated_v3_redelivered = call(
                binary,
                home,
                workspace,
                owner,
                {
                    "operation": "collect_external_specialist_results",
                    "engagement_id": engagement_id,
                    "after_sequence": isolated_v3_cursor,
                    "limit": 8,
                },
                v3_contract=True,
                structured_task_ids={isolated_v3_task_id},
            )
            if isolated_v3_redelivered != isolated_v3_collected:
                raise AssertionError(
                    "isolated v3 redelivery changed the durable result or cursor"
                )
            cursor = int(isolated_v3_collected["next_after_sequence"])

            isolated_git_dir = pathlib.Path(
                subprocess.run(
                    [
                        "git",
                        "-C",
                        str(isolated_root),
                        "rev-parse",
                        "--absolute-git-dir",
                    ],
                    check=True,
                    capture_output=True,
                    text=True,
                ).stdout.strip()
            )
            isolated_index_lock = isolated_git_dir / "index.lock"
            isolated_index_lock.write_text("capture blocked\n", encoding="utf-8")
            transient_request = {
                "schema": "dolgorae-external-specialist-facade/v3",
                "operation": "assign_external_specialist_task",
                "engagement_id": engagement_id,
                "specialist_run_id": isolated_run_id,
                "external_request_ref": {
                    "namespace": "dolgorae.e2e",
                    "kind": "completion-review",
                    "id": "v3-isolated-transient",
                },
                "task": {
                    "purpose": "completion",
                    "brief": "Exercise transient isolated patch capture.",
                    "contexts": [],
                    "criteria": [{
                        "id": "C-transient",
                        "statement": "The durable terminal is reconciled after capture recovers.",
                        "source_context_ids": [],
                    }],
                    "expected_output": "structured_review_v3",
                },
                "execution_intent": "isolated_write",
                "deadline_seconds": 60,
                "idempotency_key": "task-v3-isolated-transient",
            }
            transient_assigned = call(
                binary, home, workspace, owner, transient_request
            )
            transient_task_id = str(transient_assigned["task_id"])
            if transient_assigned["state"] != "running":
                raise AssertionError(
                    "transient capture did not leave the accepted task running: "
                    f"{transient_assigned!r}"
                )
            transient_pending = call(
                binary,
                home,
                workspace,
                owner,
                {
                    "operation": "await_external_specialist_tasks",
                    "engagement_id": engagement_id,
                    "task_ids": [transient_task_id],
                    "return_when": "all",
                    "transport_wait_seconds": 1,
                },
                v3_contract=True,
                structured_task_ids={transient_task_id},
            )
            pending_summary = transient_pending["tasks"][0]
            if (
                pending_summary["state"] != "running"
                or pending_summary["safe_error_code"] != "OUTCOME_UNKNOWN"
                or transient_pending["pending"] != [transient_task_id]
            ):
                raise AssertionError(
                    "transient capture cause was not preserved while pending: "
                    f"{transient_pending!r}"
                )
            isolated_index_lock.unlink()
            transient_waited = call(
                binary,
                home,
                workspace,
                owner,
                {
                    "operation": "await_external_specialist_tasks",
                    "engagement_id": engagement_id,
                    "task_ids": [transient_task_id],
                    "return_when": "all",
                    "transport_wait_seconds": 10,
                },
                v3_contract=True,
                structured_task_ids={transient_task_id},
            )
            if (
                transient_waited["tasks"][0]["state"] != "completed_not_delivered"
                or transient_waited["tasks"][0]["safe_error_code"] is not None
            ):
                raise AssertionError(
                    f"transient capture did not recover: {transient_waited!r}"
                )
            transient_collected = call(
                binary,
                home,
                workspace,
                owner,
                {
                    "operation": "collect_external_specialist_results",
                    "engagement_id": engagement_id,
                    "after_sequence": cursor,
                    "limit": 8,
                },
                v3_contract=True,
                structured_task_ids={transient_task_id},
            )
            if (
                len(transient_collected["tasks"]) != 1
                or transient_collected["tasks"][0]["task_id"] != transient_task_id
                or transient_collected["tasks"][0]["state"] != "delivered"
                or transient_collected["tasks"][0]["safe_error_code"] is not None
            ):
                raise AssertionError(
                    f"recovered transient result was not delivered: {transient_collected!r}"
                )
            cursor = int(transient_collected["next_after_sequence"])

            isolated_index_lock.write_text("capture blocked\n", encoding="utf-8")
            expired_request = {
                "schema": "dolgorae-external-specialist-facade/v3",
                "operation": "assign_external_specialist_task",
                "engagement_id": engagement_id,
                "specialist_run_id": isolated_run_id,
                "external_request_ref": {
                    "namespace": "dolgorae.e2e",
                    "kind": "completion-review",
                    "id": "v3-isolated-expired",
                },
                "task": {
                    "purpose": "completion",
                    "brief": "Exercise isolated patch capture deadline expiry.",
                    "contexts": [],
                    "criteria": [{
                        "id": "C-expired",
                        "statement": "Deadline expiry is reported without a partial result.",
                        "source_context_ids": [],
                    }],
                    "expected_output": "structured_review_v3",
                },
                "execution_intent": "isolated_write",
                "deadline_seconds": 1,
                "idempotency_key": "task-v3-isolated-expired",
            }
            expired_assigned = call(binary, home, workspace, owner, expired_request)
            expired_task_id = str(expired_assigned["task_id"])
            if expired_assigned["state"] not in {"running", "expired"}:
                raise AssertionError(
                    f"deadline task used an invalid assignment state: {expired_assigned!r}"
                )
            if expired_assigned["state"] == "running":
                time.sleep(1.1)
            expired_waited = call(
                binary,
                home,
                workspace,
                owner,
                {
                    "operation": "await_external_specialist_tasks",
                    "engagement_id": engagement_id,
                    "task_ids": [expired_task_id],
                    "return_when": "all",
                    "transport_wait_seconds": 10,
                },
                v3_contract=True,
                structured_task_ids={expired_task_id},
            )
            isolated_index_lock.unlink()
            expired_summary = expired_waited["tasks"][0]
            if (
                expired_summary["state"] != "expired"
                or expired_summary["safe_error_code"] != "OPERATION_TIMEOUT"
                or expired_summary["result"] is not None
                or expired_summary["result_artifact_ref"] is not None
            ):
                raise AssertionError(
                    f"capture deadline did not fail closed: {expired_waited!r}"
                )
            isolated_release = call(binary, home, workspace, owner, {
                "operation": "release_external_specialist",
                "engagement_id": engagement_id,
                "specialist_run_id": isolated_run_id,
                "reason": "isolated E2E complete",
                "idempotency_key": "release-isolated",
            })
            if isolated_release["state"] != "released":
                raise AssertionError(f"isolated member was not released: {isolated_release!r}")
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
            active_error = call(
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
            if active_error["retryable"] or active_error["details"] != {"specialist_run_id": abort_run_id}:
                raise AssertionError(f"incorrect active-task error: {active_error!r}")
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

            database = state_root / "orchestration" / "orchestration.sqlite3"
            with sqlite3.connect(database) as connection:
                original_schema = connection.execute(
                    "SELECT value FROM metadata WHERE key='schema_version'"
                ).fetchone()[0]
                connection.execute("UPDATE metadata SET value='unsupported' WHERE key='schema_version'")
            try:
                error = call(binary, home, workspace, owner, {
                    "operation": "get_external_engagement", "engagement_id": engagement_id,
                }, expected_error="ORCHESTRATION_SCHEMA_UNSUPPORTED")
                if error["details"] != {"observed": "unsupported"} or error["retryable"]:
                    raise AssertionError(f"incorrect unsupported-schema error: {error!r}")
            finally:
                with sqlite3.connect(database) as connection:
                    connection.execute(
                        "UPDATE metadata SET value=? WHERE key='schema_version'", (original_schema,)
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
