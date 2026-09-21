#!/usr/bin/env python3
"""Opt-in TASK-047 live Primary bridge acceptance.

The campaign creates an isolated HOME, Codex home, Dolgorae state root, and Git
workspace. It copies only the account credential from the caller-selected
Codex home and does not write shared Profile, workspace, or orchestration state.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import sqlite3
import subprocess
import tempfile
from contextlib import ExitStack
from pathlib import Path
from typing import Any


OPT_IN = "DOLGORAE_RUN_LIVE_PRIMARY_BRIDGE"
PINNED_CODEX_VERSION = "codex-cli 0.153.4"
PROFILE = "task047-live"
POLICY = "task047-live"


def run(
    command: list[str], *, cwd: Path, env: dict[str, str], timeout: float = 900
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        cwd=cwd,
        env=env,
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )


def machine(
    binary: Path, args: list[str], *, cwd: Path, env: dict[str, str]
) -> dict[str, Any]:
    completed = run([str(binary), *args], cwd=cwd, env=env)
    try:
        envelope = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(
            f"{args!r} returned non-machine output: {completed.stdout!r} {completed.stderr!r}"
        ) from error
    if completed.returncode != 0 or envelope.get("ok") is not True:
        raise RuntimeError(
            f"{args!r} failed with exit {completed.returncode}: {envelope!r} {completed.stderr!r}"
        )
    return envelope["data"]


def write_json(path: Path, value: Any, mode: int = 0o600) -> None:
    path.write_text(
        json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )
    path.chmod(mode)


def checked_path(value: str) -> str:
    directories: list[str] = []
    for item in value.split(os.pathsep):
        path = Path(item)
        if path.is_absolute() and path.is_dir() and item not in directories:
            directories.append(item)
    if not directories:
        raise RuntimeError("the live Profile PATH has no existing absolute directories")
    return os.pathsep.join(directories)


def policy_input() -> dict[str, Any]:
    return {
        "schema_version": 2,
        "policy_name": POLICY,
        "revision": 1,
        "approval_policy": "user_approval_required",
        "max_active_specialists": 1,
        "roles": [
            {
                "role_ref": "probe",
                "role_source": {"scope": "project", "name": "probe"},
                "agent_configuration": {
                    "schema_version": 2,
                    "selected_profile": PROFILE,
                    "model": None,
                    "default_effort": "low",
                    "purpose": "review",
                    "purpose_label": None,
                    "required_capabilities": [],
                    "execution_lane": "shared_readonly",
                    "required_assurance": "best_effort_personal_alpha",
                    "native_subagent_policy": "enabled",
                },
                "max_active_instances": 1,
                "reuse_policy": "never",
                "allowed_access": ["read_only"],
                "activation_policy": "on_mail",
                "primary_may_request": True,
                "collaboration_source": False,
                "collaboration_target": False,
                "auto_approve_when_fully_delegated": False,
            }
        ],
    }


def tool_receipt(state_root: Path, run_id: str) -> dict[str, Any]:
    database = state_root / "orchestration" / "orchestration.sqlite3"
    connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
    try:
        rows = connection.execute(
            "SELECT session_id,source_run_id,source_turn_id,source_tool_call_id,"
            "idempotency_key,request_sha256,response_json "
            "FROM brokered_tool_results ORDER BY created_at_ms"
        ).fetchall()
    finally:
        connection.close()
    if len(rows) != 1:
        raise RuntimeError(f"expected exactly one durable Primary tool result, observed {len(rows)}")
    session_id, source_run_id, turn_id, call_id, key, request_sha256, response_json = rows[0]
    receipt = json.loads(response_json)
    if session_id != run_id or source_run_id != run_id:
        raise RuntimeError("the durable tool result is not bound to the Primary Run")
    if (
        receipt.get("schema_version") != "dolgorae.brokered-tool-result/v1"
        or receipt.get("outcome") != "ok"
        or set(receipt) != {"schema_version", "outcome", "value"}
    ):
        raise RuntimeError(f"unexpected durable Primary tool envelope: {receipt!r}")
    response = receipt["value"]
    if response.get("operation") != "list_specialists_result" or response.get("specialists") != []:
        raise RuntimeError(f"unexpected Primary tool response: {response!r}")
    return {
        "session_equals_primary_run": True,
        "source_turn_id_sha256": hashlib.sha256(turn_id.encode()).hexdigest(),
        "source_tool_call_id_sha256": hashlib.sha256(call_id.encode()).hexdigest(),
        "idempotency_key_sha256": hashlib.sha256(key.encode()).hexdigest(),
        "request_sha256": request_sha256,
        "operation": response["operation"],
        "specialist_count": len(response["specialists"]),
    }


def campaign(binary: Path, codex: Path, source_codex_home: Path) -> dict[str, Any]:
    version = run([str(codex), "--version"], cwd=Path.cwd(), env=dict(os.environ)).stdout.strip()
    if version != PINNED_CODEX_VERSION:
        raise RuntimeError(f"expected {PINNED_CODEX_VERSION!r}, observed {version!r}")
    with ExitStack() as cleanup:
        root = Path(
            cleanup.enter_context(
                tempfile.TemporaryDirectory(prefix="dolgorae-task047-live-")
            )
        )
        home = root / "home"
        codex_home = root / "codex-home"
        workspace = root / "workspace"
        home.mkdir(mode=0o700)
        codex_home.mkdir(mode=0o700)
        workspace.mkdir(mode=0o700)
        source_auth = source_codex_home / "auth.json"
        if not source_auth.is_file():
            raise RuntimeError("the selected Codex home has no auth.json")
        shutil.copyfile(source_auth, codex_home / "auth.json")
        (codex_home / "auth.json").chmod(0o600)
        env = {
            key: value
            for key, value in os.environ.items()
            if key in {"LANG", "LC_ALL", "PATH", "TMPDIR", OPT_IN}
        }
        env["HOME"] = str(home)
        subprocess.run(["git", "init", "--quiet", str(workspace)], check=True, env=env)
        machine(binary, ["init", str(workspace)], cwd=workspace, env=env)
        machine(
            binary,
            [
                "profile", "add", PROFILE,
                "--codex-home", str(codex_home),
                "--native-subagents", "enabled",
                "--env", f"PATH={checked_path(env.get('PATH', '/usr/bin:/bin'))}",
                "--env", f"LANG={env.get('LANG', 'en_US.UTF-8')}",
                "--env", f"LC_ALL={env.get('LC_ALL', 'en_US.UTF-8')}",
                "--", str(codex),
            ],
            cwd=workspace,
            env=env,
        )
        operator = root / "operator.json"
        machine(
            binary,
            ["operator", "credential", "initialize", "--output", str(operator)],
            cwd=workspace,
            env=env,
        )
        profile_state = machine(
            binary,
            ["profile", "server", "start", PROFILE],
            cwd=workspace,
            env=env,
        )["state"]
        server_key = str(profile_state["server_key"])
        cleanup.callback(
            run,
            [
                str(binary), "profile", "server", "stop", PROFILE,
                "--operator-file", str(operator),
                "--interrupt", "--confirm-server-key", server_key,
            ],
            cwd=workspace,
            env=env,
            timeout=90,
        )
        roles = workspace / ".dolgorae" / "roles"
        roles.mkdir(parents=True)
        write_json(
            roles / "probe.json",
            {
                "schema_version": 1,
                "name": "probe",
                "display_name": "TASK-047 Probe",
                "description": "Bounded role used only to compile the live bridge policy.",
                "instructions": "Do not act unless a later task explicitly provisions this role.",
            },
            mode=0o644,
        )
        policy = root / "policy.json"
        write_json(policy, policy_input())
        machine(
            binary,
            ["specialist", "policy", "add", POLICY, "--workspace", str(workspace), "--file", str(policy)],
            cwd=workspace,
            env=env,
        )
        controller = root / "controller.json"
        machine(
            binary,
            [
                "controller", "credential", "create",
                "--kind", "interactive-client",
                "--instance-id", "task047-live",
                "--orchestration-policy", POLICY,
                "--output", str(controller),
            ],
            cwd=workspace,
            env=env,
        )
        run_id: str | None = None
        try:
            started = machine(
                binary,
                [
                    "run", "--controller-file", str(controller), "start",
                    "--workspace", str(workspace),
                    "--profile", PROFILE,
                    "--control-mode", "direct-interactive",
                    "--execution-lane", "shared-readonly",
                    "--required-assurance", "best-effort-personal-alpha",
                    "--purpose", "implementation",
                    "--instructions", "For this acceptance turn, call dolgorae_orchestration exactly once with operation list_specialists, then report only whether the returned list is empty.",
                    "--idempotency-key", "task047-live-start",
                ],
                cwd=workspace,
                env=env,
            )
            run_id = str(started["run_id"])
            terminal = machine(
                binary,
                [
                    "run", "--controller-file", str(controller), "send", run_id,
                    "--workspace", str(workspace),
                    "--message", "Perform the required single list_specialists tool call now.",
                    "--idempotency-key", "task047-live-turn",
                    "--timeout", "12m",
                ],
                cwd=workspace,
                env=env,
            )
            if terminal.get("status") != "completed":
                raise RuntimeError(f"live Primary turn did not complete: {terminal!r}")
            state_root = home / ".dolgorae" / "workspaces" / str(started["workspace_id"])
            receipt = tool_receipt(state_root, run_id)
            return {
                "task": "TASK-047",
                "codex_version": version.removeprefix("codex-cli "),
                "isolated_home": True,
                "isolated_codex_home": True,
                "isolated_workspace": True,
                "shared_profile_mutated": False,
                "run_id_sha256": hashlib.sha256(run_id.encode()).hexdigest(),
                "turn_status": terminal["status"],
                "tool_receipt": receipt,
            }
        finally:
            if run_id is not None:
                run(
                    [
                        str(binary), "run", "--controller-file", str(controller),
                        "close", run_id, "--workspace", str(workspace), "--interrupt",
                    ],
                    cwd=workspace,
                    env=env,
                    timeout=90,
                )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--codex", type=Path, required=True)
    parser.add_argument("--codex-home", type=Path, required=True)
    args = parser.parse_args()
    if os.environ.get(OPT_IN) != "1":
        raise SystemExit(f"{OPT_IN}=1 is required")
    result = campaign(
        args.binary.expanduser().resolve(strict=True),
        args.codex.expanduser().resolve(strict=True),
        args.codex_home.expanduser().resolve(strict=True),
    )
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
