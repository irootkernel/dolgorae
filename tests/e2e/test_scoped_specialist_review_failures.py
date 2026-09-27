#!/usr/bin/env python3
"""Failure, preservation, drift, tamper, and cancellation checks for TASK-015."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import shutil
import signal
import sqlite3
import subprocess
import tempfile
import time

import native_codex
from schema_support import assert_valid, validator


def machine(binary: pathlib.Path, home: pathlib.Path, arguments: list[str]) -> tuple[int, dict[str, object]]:
    environment = os.environ.copy()
    environment["HOME"] = str(home)
    completed = subprocess.run(
        [str(binary), *arguments], check=False, capture_output=True, text=True, env=environment
    )
    if completed.stderr:
        raise AssertionError(f"unexpected stderr for {arguments}: {completed.stderr!r}")
    return completed.returncode, json.loads(completed.stdout)


def machine_input(
    binary: pathlib.Path,
    home: pathlib.Path,
    arguments: list[str],
    request: dict[str, object],
    environment_overrides: dict[str, str] | None = None,
) -> tuple[int, dict[str, object]]:
    environment = os.environ.copy()
    environment["HOME"] = str(home)
    if environment_overrides:
        environment.update(environment_overrides)
    completed = subprocess.run(
        [str(binary), *arguments],
        input=json.dumps(request, separators=(",", ":")),
        check=False,
        capture_output=True,
        text=True,
        env=environment,
    )
    if completed.stderr:
        raise AssertionError(f"unexpected stderr for {arguments}: {completed.stderr!r}")
    return completed.returncode, json.loads(completed.stdout)


def start(binary: pathlib.Path, home: pathlib.Path, arguments: list[str]) -> subprocess.Popen[str]:
    environment = os.environ.copy()
    environment["HOME"] = str(home)
    return subprocess.Popen(
        [str(binary), *arguments],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=environment,
    )


def start_input(
    binary: pathlib.Path,
    home: pathlib.Path,
    arguments: list[str],
    request: dict[str, object],
) -> subprocess.Popen[str]:
    environment = os.environ.copy()
    environment["HOME"] = str(home)
    process = subprocess.Popen(
        [str(binary), *arguments],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=environment,
    )
    if process.stdin is None:
        raise AssertionError("review stdin pipe was not created")
    process.stdin.write(json.dumps(request, separators=(",", ":")))
    process.stdin.close()
    process.stdin = None
    return process


def git(repository: pathlib.Path, *arguments: str) -> str:
    return subprocess.run(
        ["git", "-C", str(repository), *arguments],
        check=True, capture_output=True, text=True,
    ).stdout.strip()


def wait_for(path_root: pathlib.Path, predicate, process: subprocess.Popen[str]) -> pathlib.Path:
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if process.poll() is not None:
            stdout, stderr = process.communicate()
            raise AssertionError(f"review exited before fault injection: {stdout!r} {stderr!r}")
        if path_root.exists():
            for candidate in path_root.iterdir():
                if predicate(candidate):
                    return candidate
        time.sleep(0.001)
    process.kill()
    raise AssertionError("timed out waiting for the review fault-injection boundary")


def finish(
    process: subprocess.Popen[str], timeout: int = 30
) -> tuple[int, dict[str, object]]:
    stdout, stderr = process.communicate(timeout=timeout)
    if stderr:
        raise AssertionError(f"unexpected review stderr: {stderr!r}")
    return process.returncode, json.loads(stdout)


def add_profile(
    binary: pathlib.Path,
    home: pathlib.Path,
    workspace: pathlib.Path,
    root: pathlib.Path,
    schema_source: pathlib.Path,
    name: str,
    scenario_name: str,
    terminal_status: str | None = None,
    recursive_adapter: bool = False,
    output_text: str | None = None,
    configuration: str | None = None,
    advertised_models: list[dict[str, object]] | None = None,
) -> pathlib.Path:
    codex_home = root / f"codex-home-{name}"
    bin_root = root / f"bin-{name}"
    codex_home.mkdir(mode=0o700)
    bin_root.mkdir(mode=0o700)
    if recursive_adapter or configuration is not None:
        config = codex_home / "config.toml"
        config.write_text(
            '[mcp_servers.dolgorae_review]\ncommand = "dolgorae"\n'
            if recursive_adapter else configuration,
            encoding="utf-8",
        )
        config.chmod(0o600)
    codex = bin_root / "codex"
    scenario_source = native_codex.scenario_path(scenario_name)
    scenario = root / f"scenario-{name}.json"
    scenario_value = json.loads(
        scenario_source.read_text(encoding="utf-8").replace(
            "thread-timeout", f"thread-{name}"
        )
    )
    if advertised_models is not None:
        for step in scenario_value["steps"]:
            if step.get("method") == "model/list":
                step["respond"]["result"]["data"] = advertised_models
    if terminal_status is not None:
        turn_start = next(
            step
            for step in scenario_value["steps"]
            if step.get("method") == "turn/start"
        )
        terminal = turn_start["emit"][0]["params"]["turn"]
        terminal["status"] = terminal_status
        terminal["items"] = []
    if output_text is not None:
        turn_start = next(
            step for step in scenario_value["steps"] if step.get("method") == "turn/start"
        )
        turn_start["emit"][0]["params"]["turn"]["items"][0]["text"] = output_text
    scenario.write_text(
        json.dumps(scenario_value, separators=(",", ":")), encoding="utf-8"
    )
    native_codex.create_native_codex(
        codex,
        scenario=scenario,
        codex_home=codex_home,
        schema_source=schema_source,
        transcript=root / f"transcript-{name}.jsonl",
    )
    code, envelope = machine(
        binary,
        home,
        [
            "profile", "add", name,
            "--codex-home", str(codex_home), "--native-subagents", "enabled",
            "--env", "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
            "--env", "LANG=en_US.UTF-8", "--env", "LC_ALL=en_US.UTF-8",
            "--", str(codex),
        ],
    )
    if code != 0:
        raise AssertionError(f"profile add failed: {envelope!r}")
    return codex


def review_args(workspace: pathlib.Path, profile: str, deadline: int = 600) -> list[str]:
    return [
        "specialist", "review", "--workspace", str(workspace), "--profile", profile,
        "--target-kind", "workspace", "--deadline-seconds", str(deadline), "--format", "json",
    ]


def v3_review_request() -> dict[str, object]:
    return {
        "schema": "dolgorae-specialist-review-request/v3",
        "operation": "review_target",
        "target": {"kind": "workspace"},
        "purpose": "completion",
        "brief": "Check the captured candidate.",
        "contexts": [],
        "criteria": [{
            "id": "C-failure-envelope",
            "statement": "The candidate is reviewed.",
            "source_context_ids": [],
        }],
        "expected_output": "structured_review_v3",
        "deadline_seconds": 60,
    }


def assert_failure(
    envelope: dict[str, object],
    code: str,
    settlement: str,
    details_schema,
    machine_schema,
    *,
    engagement_state: str | None = None,
    required_action: str | None = None,
) -> str:
    assert_valid(envelope, machine_schema, f"{code} Machine envelope")
    error = envelope["error"]  # type: ignore[index]
    if error["code"] != code:  # type: ignore[index]
        raise AssertionError(f"expected {code}, got {error!r}")
    details = error["details"]  # type: ignore[index]
    assert_valid(details, details_schema, f"{code} v2 details")
    if details["settlement_state"] != settlement:  # type: ignore[index]
        raise AssertionError(f"unexpected settlement state: {details!r}")
    if engagement_state is not None and details["engagement_state"] != engagement_state:  # type: ignore[index]
        raise AssertionError(f"unexpected engagement state: {details!r}")
    if required_action is not None and details["required_action"] != required_action:  # type: ignore[index]
        raise AssertionError(f"unexpected required action: {details!r}")
    if "cause_details" not in details:  # type: ignore[operator]
        raise AssertionError(f"failure lost its causal details: {details!r}")
    return str(details["capture_ref"])  # type: ignore[index]


def terminate_owned(root: pathlib.Path) -> None:
    listing = subprocess.run(["ps", "-axo", "pid=,command="], check=True, capture_output=True, text=True)
    own_pid = os.getpid()
    for line in listing.stdout.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        pid_text, _, command = stripped.partition(" ")
        if str(root) not in command:
            continue
        pid = int(pid_text)
        if pid != own_pid:
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass


def validate(binary: pathlib.Path, protocol_root: pathlib.Path) -> None:
    details_schema = validator(protocol_root, "dolgorae-specialist-review-tool-v2.schema.json")
    details_schema_v3 = validator(
        protocol_root,
        "dolgorae-specialist-review-tool-v3.schema.json",
        "#/$defs/error_details",
    )
    machine_schema = validator(protocol_root, "dolgorae-machine-v2.schema.json")
    diagnostic_schema = validator(protocol_root, "dolgorae-review-output-diagnostic-v1.schema.json")
    schema_source = native_codex.installed_codex()
    with tempfile.TemporaryDirectory(prefix="dolgorae-task015-failures-") as temporary:
        root = pathlib.Path(temporary)
        home = root / "home"
        workspace = root / "workspace"
        home.mkdir(mode=0o700)
        workspace.mkdir(mode=0o700)
        git(workspace, "init", "-b", "main")
        git(workspace, "config", "user.name", "Dolgorae E2E")
        git(workspace, "config", "user.email", "dolgorae@example.invalid")
        (workspace / "root.txt").write_text("root\n", encoding="utf-8")
        git(workspace, "add", "root.txt")
        git(workspace, "commit", "-m", "root")
        code, initialized = machine(binary, home, ["init", str(workspace)])
        if code != 0:
            raise AssertionError(f"init failed: {initialized!r}")
        workspace_id = str(initialized["data"]["workspace_id"])  # type: ignore[index]
        targets = home / ".dolgorae" / "workspaces" / workspace_id / "review-targets"
        try:
            drift_codex = add_profile(
                binary, home, workspace, root, schema_source, "drift-reviewer", "scoped_specialist_review.json"
            )
            drift = start(binary, home, review_args(workspace, "drift-reviewer"))
            wait_for(targets, lambda path: path.name.startswith(".verify-"), drift)
            old_codex = drift_codex.with_name("codex.old")
            drift_codex.rename(old_codex)
            shutil.copy2(schema_source, drift_codex)
            drift_codex.chmod(0o755)
            returncode, envelope = finish(drift)
            if returncode == 0:
                raise AssertionError("executable drift unexpectedly succeeded")
            capture_ref = assert_failure(
                envelope,
                "REVIEW_EXECUTABLE_DRIFT",
                "settled",
                details_schema,
                machine_schema,
            )
            if (targets / capture_ref / "source").exists():
                raise AssertionError("authoritative pre-launch drift retained source bytes")
            drift_codex.unlink()
            old_codex.rename(drift_codex)

            add_profile(
                binary,
                home,
                workspace,
                root,
                schema_source,
                "recursive-reviewer",
                "scoped_specialist_review.json",
                recursive_adapter=True,
            )

            preflight_args = [
                "specialist", "review", "--workspace", str(workspace),
                "--profile", "recursive-reviewer", "--request-stdin", "--format", "json",
            ]
            code, envelope = machine_input(
                binary,
                home,
                preflight_args,
                v3_review_request(),
            )
            if code == 0:
                raise AssertionError("recursive Reviewer adapter unexpectedly succeeded")
            assert_valid(envelope, machine_schema, "bare v3 preflight failure")
            preflight_error = envelope["error"]
            if (
                preflight_error["code"] != "REVIEW_PROFILE_UNAVAILABLE"
                or preflight_error["details"]
                != {"required_action": "remove_dolgorae_review_from_reviewer_profile"}
            ):
                raise AssertionError(
                    f"preflight Reviewer failure used the wrong envelope: {envelope!r}"
                )

            for name, model_names, efforts, configuration, check, expected, actual in (
                ("missing-default-model", ["gpt-5.6"], ["medium", "high"], None,
                 "model", ["gpt-5.6"], "gpt-6-sol"),
                ("missing-default-effort", ["gpt-6-sol"], ["medium", "low"], None,
                 "reasoning_effort", ["medium", "low"], "high"),
                ("numeric-model", ["gpt-5.6", "gpt-6-sol"], ["medium", "high"], 'model = 123\n',
                 "model", ["gpt-5.6", "gpt-6-sol"], 123),
            ):
                models = [{
                    "model": model,
                    "isDefault": index == 0,
                    "supportedReasoningEfforts": [{"reasoningEffort": effort} for effort in efforts],
                } for index, model in enumerate(model_names)]
                add_profile(
                    binary, home, workspace, root, schema_source, name,
                    "scoped_specialist_review_v3.json",
                    configuration=configuration, advertised_models=models,
                )
                code, envelope = machine_input(
                    binary, home,
                    [
                        "specialist", "review", "--workspace", str(workspace),
                        "--profile", name, "--request-stdin", "--format", "json",
                    ],
                    v3_review_request(),
                )
                assert_valid(envelope, machine_schema, f"{name} compatibility failure")
                error = envelope.get("error", {})
                if (
                    code == 0
                    or error.get("code") != "COMPATIBILITY_REJECTED"
                    or error.get("details") != {
                        "profile": name, "check": check, "expected": expected, "actual": actual,
                    }
                ):
                    raise AssertionError(f"Reviewer silently substituted an unavailable setting: {envelope!r}")
                messages = [
                    json.loads(line) for line in (root / f"transcript-{name}.jsonl")
                    .read_text(encoding="utf-8").splitlines()
                ]
                if any(message.get("method") in ("thread/start", "turn/start") for message in messages):
                    raise AssertionError("incompatible Reviewer settings reached thread or Turn creation")

            add_profile(
                binary,
                home,
                workspace,
                root,
                schema_source,
                "failed-v2-reviewer",
                "scoped_specialist_review.json",
                terminal_status="failed",
            )
            code, envelope = machine(
                binary, home, review_args(workspace, "failed-v2-reviewer")
            )
            if code == 0:
                raise AssertionError("failed v2 Reviewer Turn unexpectedly succeeded")
            assert_failure(
                envelope,
                "REVIEW_TASK_FAILED",
                "settled",
                details_schema,
                machine_schema,
            )

            add_profile(
                binary,
                home,
                workspace,
                root,
                schema_source,
                "failed-v1-reviewer",
                "scoped_specialist_review.json",
                terminal_status="failed",
            )
            code, envelope = machine(
                binary,
                home,
                [
                    "specialist", "review", "--workspace", str(workspace),
                    "--profile", "failed-v1-reviewer", "--scope", "working-tree",
                    "--format", "json",
                ],
            )
            if code != 7:
                raise AssertionError(
                    f"legacy failed Reviewer Turn changed exit class: {code} {envelope!r}"
                )
            assert_valid(envelope, machine_schema, "legacy Reviewer failure")
            if envelope["error"]["code"] != "TURN_FAILED":
                raise AssertionError(
                    f"legacy Reviewer failure changed error meaning: {envelope!r}"
                )

            add_profile(
                binary,
                home,
                workspace,
                root,
                schema_source,
                "failed-v3-reviewer",
                "scoped_specialist_review.json",
                terminal_status="failed",
            )
            code, envelope = machine_input(
                binary,
                home,
                [
                    "specialist", "review", "--workspace", str(workspace),
                    "--profile", "failed-v3-reviewer", "--request-stdin", "--format", "json",
                ],
                v3_review_request(),
            )
            if code == 0:
                raise AssertionError("failed v3 Reviewer Turn unexpectedly succeeded")
            assert_failure(
                envelope,
                "REVIEW_TASK_FAILED",
                "settled",
                details_schema_v3,
                machine_schema,
            )
            failed_details = envelope["error"]["details"]
            database = targets.parent / "orchestration" / "orchestration.sqlite3"
            with sqlite3.connect(database) as connection:
                failed_tasks = connection.execute(
                    "SELECT specialist_run_id FROM tasks WHERE engagement_id=?",
                    (failed_details["engagement_id"],),
                ).fetchall()
            if len(failed_tasks) != 1 or failed_details.get("execution") != {
                "model": "gpt-6-sol", "effort": "high",
                "codex_version": native_codex.PINNED_CODEX_VERSION,
                "run_id": failed_tasks[0][0],
            }:
                raise AssertionError("post-preparation v3 failure lost its allocated Reviewer identity")

            for name, category, path in (
                ("missing-nullable", "missing_field", "/criterion_assessments/0/remaining_gap"),
                ("unknown-key", "unknown_field", ""),
                ("oversized", "output_too_large", ""),
            ):
                provider_output = {
                    "summary": "private-provider-output-가",
                    "findings": [],
                    "criterion_assessments": [{
                        "criterion_id": "C-failure-envelope",
                        "status": "met",
                        "explanation": "The captured candidate was checked.",
                        "evidence": [{
                            "basis": "candidate", "description": "The captured file.",
                            "path": None, "line_start": None, "line_end": None, "context_id": None,
                        }],
                        "remaining_gap": None,
                    }],
                    "evidence_limits": [],
                    "overall_assessment": "requirements_met",
                }
                if name == "missing-nullable":
                    del provider_output["criterion_assessments"][0]["remaining_gap"]
                elif name == "unknown-key":
                    provider_output["private-unknown-key~/credential"] = "private-provider-value"
                else:
                    provider_output["summary"] = "private-provider-output" * 50_000
                raw_output = json.dumps(provider_output, ensure_ascii=False, indent=2) + "\n"
                profile = f"invalid-v3-{name}"
                add_profile(
                    binary, home, workspace, root, schema_source, profile,
                    "scoped_specialist_review_v3.json", output_text=raw_output,
                )
                code, envelope = machine_input(
                    binary, home,
                    [
                        "specialist", "review", "--workspace", str(workspace),
                        "--profile", profile, "--request-stdin", "--format", "json",
                    ],
                    v3_review_request(),
                )
                if code == 0:
                    raise AssertionError("invalid v3 output unexpectedly succeeded")
                capture_ref = assert_failure(
                    envelope, "REVIEW_OUTPUT_INVALID", "settled", details_schema_v3, machine_schema,
                )
                details = envelope["error"]["details"]
                diagnostic = details["cause_details"]["diagnostic"]
                assert_valid(diagnostic, diagnostic_schema, f"{name} persisted diagnostic")
                if (
                    diagnostic["category"] != category
                    or diagnostic["path"] != path
                    or diagnostic["output_bytes"] != len(raw_output.encode("utf-8"))
                    or diagnostic["output_sha256"] != "sha256:" + hashlib.sha256(raw_output.encode("utf-8")).hexdigest()
                    or diagnostic["unknown_top_level_key_count"] != int(name == "unknown-key")
                    or diagnostic["known_top_level_keys"] != ([] if name == "oversized" else [
                        "criterion_assessments", "evidence_limits", "findings", "overall_assessment", "summary"
                    ])
                ):
                    raise AssertionError(f"invalid v3 output lost its safe structural identity: {diagnostic!r}")
                wire = json.dumps(envelope, ensure_ascii=False)
                if any(canary in wire for canary in ("private-provider", "private-unknown-key")):
                    raise AssertionError("failure envelope exposed provider content or an arbitrary key")
                execution = diagnostic["execution"]
                expected_execution = {
                    "model": "gpt-6-sol", "effort": "high",
                    "codex_version": native_codex.PINNED_CODEX_VERSION,
                    "run_id": execution["run_id"],
                }
                if details.get("execution") != expected_execution or execution != {
                    **expected_execution,
                    "task_id": execution["task_id"], "turn_id": "turn-1",
                }:
                    raise AssertionError("failure envelope and diagnostic lost the actual Reviewer or fixture Turn identity")
                database = targets.parent / "orchestration" / "orchestration.sqlite3"
                with sqlite3.connect(database) as connection:
                    stored = connection.execute(
                        "SELECT specialist_run_id,state,safe_error_code,diagnostic_json FROM tasks "
                        "WHERE engagement_id=? AND task_id=?",
                        (details["engagement_id"], execution["task_id"]),
                    ).fetchone()
                if (
                    stored is None
                    or stored[:3] != (execution["run_id"], "failed", "REVIEW_OUTPUT_INVALID")
                    or json.loads(stored[3]) != diagnostic
                ):
                    raise AssertionError("terminal storage and first returned diagnostic disagree")
                messages = [
                    json.loads(line) for line in (root / f"transcript-{profile}.jsonl")
                    .read_text(encoding="utf-8").splitlines()
                ]
                if sum(message.get("method") == "turn/start" for message in messages) != 1:
                    raise AssertionError("invalid v3 output triggered an extra repair Turn")
                if (targets / capture_ref / "source").exists():
                    raise AssertionError("terminal invalid v3 output retained captured source bytes")

            slow_tree = workspace / "post-drift-window"
            slow_tree.mkdir()
            # Keep post-turn integrity verification observable without paying
            # for 2,000 fsyncs during the two pre-launch materializations.
            padding = "review fixture\n" * 8_192
            for index in range(16):
                (slow_tree / f"{index:04d}.txt").write_text(padding, encoding="utf-8")
            post_drift_codex = add_profile(
                binary,
                home,
                workspace,
                root,
                schema_source,
                "post-drift-reviewer",
                "scoped_specialist_review.json",
            )
            # Establish the fake server before timing the post-turn boundary;
            # cold profile startup is covered separately by profile lifecycle tests.
            code, envelope = machine(
                binary, home, ["profile", "server", "start", "post-drift-reviewer"]
            )
            if code != 0:
                raise AssertionError(f"post-turn fixture server start failed: {envelope!r}")
            post_drift = start(binary, home, review_args(workspace, "post-drift-reviewer"))
            database = targets.parent / "orchestration" / "orchestration.sqlite3"
            started = time.monotonic()
            # Capture, fresh reviewer bootstrap, and the Turn share this budget.
            # Measured successful preparation nearly exhausted 30 seconds;
            # retain a bounded wait with room for a loaded native-test host.
            deadline = started + 60
            observed_states = []
            while time.monotonic() < deadline:
                if database.is_file():
                    with sqlite3.connect(database) as connection:
                        state = connection.execute(
                            "SELECT state FROM engagements ORDER BY rowid DESC LIMIT 1"
                        ).fetchone()
                    if not observed_states or observed_states[-1][1] != state:
                        observed_states.append((round(time.monotonic() - started, 3), state))
                    if state is not None and state[0] == "result_ready":
                        os.kill(post_drift.pid, signal.SIGSTOP)
                        break
                if post_drift.poll() is not None:
                    stdout, stderr = post_drift.communicate()
                    raise AssertionError(
                        "post-turn drift review exited before fault injection: "
                        f"states={observed_states!r}, stdout={stdout!r}, stderr={stderr!r}"
                    )
                time.sleep(0.001)
            else:
                transcript = root / "transcript-post-drift-reviewer.jsonl"
                methods = (
                    [json.loads(line).get("method") for line in transcript.read_text().splitlines()]
                    if transcript.exists() else []
                )
                captures = [
                    sum(1 for _ in path.rglob("*")) for path in targets.iterdir()
                ]
                raise AssertionError(
                    "post-turn drift review never reached result_ready: "
                    f"states={observed_states!r}, methods={methods!r}, "
                    f"capture_entry_counts={captures!r}"
                )
            old_post_drift = post_drift_codex.with_name("codex.old")
            post_drift_codex.rename(old_post_drift)
            shutil.copy2(schema_source, post_drift_codex)
            post_drift_codex.chmod(0o755)
            os.kill(post_drift.pid, signal.SIGCONT)
            returncode, envelope = finish(post_drift)
            if returncode == 0:
                raise AssertionError("post-turn executable drift unexpectedly succeeded")
            capture_ref = assert_failure(
                envelope,
                "REVIEW_EXECUTABLE_DRIFT",
                "settled",
                details_schema,
                machine_schema,
            )
            if (targets / capture_ref / "source").exists():
                raise AssertionError("authoritative post-turn drift retained source bytes")
            shutil.rmtree(slow_tree)

            add_profile(
                binary, home, workspace, root, schema_source, "tamper-reviewer", "scoped_specialist_review.json"
            )
            tamper = start(binary, home, review_args(workspace, "tamper-reviewer"))
            capture_root = wait_for(
                targets,
                lambda path: not path.name.startswith(".") and (path / "source" / "current" / "root.txt").is_file(),
                tamper,
            )
            target_file = capture_root / "source" / "current" / "root.txt"
            target_file.chmod(0o644)
            target_file.write_text("tampered\n", encoding="utf-8")
            # Capture publication precedes cold Reviewer bootstrap and the
            # Turn. Leave room for both on a loaded native-test host.
            returncode, envelope = finish(tamper, timeout=60)
            if returncode == 0:
                raise AssertionError("capture tampering unexpectedly succeeded")
            capture_ref = assert_failure(
                envelope,
                "REVIEW_TARGET_MUTATED",
                "preserved",
                details_schema,
                machine_schema,
                engagement_state="result_ready",
                required_action="inspect_authority",
            )
            if not (targets / capture_ref / "source").exists():
                raise AssertionError("tampered capture recovery evidence was removed")

            add_profile(
                binary, home, workspace, root, schema_source, "timeout-reviewer", "scoped_specialist_review_timeout.json"
            )
            code, envelope = machine(binary, home, review_args(workspace, "timeout-reviewer", 1))
            if code == 0:
                raise AssertionError("unknown timeout unexpectedly succeeded")
            capture_ref = assert_failure(
                envelope,
                "REVIEW_TIMEOUT",
                "preserved",
                details_schema,
                machine_schema,
                engagement_state="interrupted_unknown",
                required_action="inspect_authority",
            )
            if not (targets / capture_ref / "source").exists():
                raise AssertionError("unknown timeout removed recovery bytes")

            add_profile(
                binary, home, workspace, root, schema_source, "cancel-reviewer", "scoped_specialist_review_timeout.json"
            )
            known_targets = set(targets.iterdir())
            cancelled = start(binary, home, review_args(workspace, "cancel-reviewer", 30))
            wait_for(
                targets,
                lambda path: path not in known_targets
                and not path.name.startswith(".")
                and (path / "source").is_dir(),
                cancelled,
            )
            database = targets.parent / "orchestration" / "orchestration.sqlite3"
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                if database.is_file():
                    with sqlite3.connect(database) as connection:
                        state = connection.execute(
                            "SELECT state FROM engagements ORDER BY rowid DESC LIMIT 1"
                        ).fetchone()
                    if state is not None and state[0] == "executing":
                        break
                if cancelled.poll() is not None:
                    raise AssertionError("cancellation review exited before task acceptance")
                time.sleep(0.01)
            else:
                raise AssertionError("cancellation review never reached executing authority")
            cancelled.send_signal(signal.SIGINT)
            returncode, envelope = finish(cancelled)
            if returncode == 0:
                raise AssertionError("explicit cancellation unexpectedly succeeded")
            capture_ref = assert_failure(
                envelope,
                "REVIEW_CANCELLED",
                "preserved",
                details_schema,
                machine_schema,
                engagement_state="interrupted_unknown",
                required_action="inspect_authority",
            )
            if not (targets / capture_ref / "source").exists():
                raise AssertionError("cancellation removed unknown recovery bytes")
            methods = [
                json.loads(line).get("method")
                for line in (root / "transcript-cancel-reviewer.jsonl")
                .read_text(encoding="utf-8").splitlines()
            ]
            if methods.count("turn/start") != 1 or methods.count("turn/interrupt") != 1:
                raise AssertionError("cancellation must interrupt the accepted Turn exactly once")

            add_profile(
                binary,
                home,
                workspace,
                root,
                schema_source,
                "cancel-v3-reviewer",
                "scoped_specialist_review_timeout.json",
            )
            known_targets = set(targets.iterdir())
            cancelled_v3 = start_input(
                binary,
                home,
                [
                    "specialist", "review", "--workspace", str(workspace),
                    "--profile", "cancel-v3-reviewer", "--request-stdin",
                    "--format", "json",
                ],
                v3_review_request(),
            )
            wait_for(
                targets,
                lambda path: path not in known_targets
                and not path.name.startswith(".")
                and (path / "source").is_dir(),
                cancelled_v3,
            )
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                if database.is_file():
                    with sqlite3.connect(database) as connection:
                        state = connection.execute(
                            "SELECT state FROM engagements ORDER BY rowid DESC LIMIT 1"
                        ).fetchone()
                    if state is not None and state[0] == "executing":
                        break
                if cancelled_v3.poll() is not None:
                    raise AssertionError(
                        "v3 cancellation review exited before task acceptance"
                    )
                time.sleep(0.01)
            else:
                raise AssertionError(
                    "v3 cancellation review never reached executing authority"
                )
            cancelled_v3.send_signal(signal.SIGINT)
            returncode, envelope = finish(cancelled_v3)
            if returncode == 0:
                raise AssertionError("explicit v3 cancellation unexpectedly succeeded")
            capture_ref = assert_failure(
                envelope,
                "REVIEW_CANCELLED",
                "preserved",
                details_schema_v3,
                machine_schema,
                engagement_state="interrupted_unknown",
                required_action="inspect_authority",
            )
            if not (targets / capture_ref / "source").exists():
                raise AssertionError("v3 cancellation removed unknown recovery bytes")
            v3_methods = [
                json.loads(line).get("method")
                for line in (root / "transcript-cancel-v3-reviewer.jsonl")
                .read_text(encoding="utf-8").splitlines()
            ]
            if v3_methods.count("turn/start") != 1 or v3_methods.count("turn/interrupt") != 1:
                raise AssertionError(
                    "v3 cancellation must interrupt the accepted Turn exactly once"
                )
        finally:
            terminate_owned(root)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--protocol-root", default="docs/protocol", type=pathlib.Path)
    arguments = parser.parse_args()
    validate(arguments.binary.resolve(), arguments.protocol_root.resolve())
    print("scoped specialist review failure tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
