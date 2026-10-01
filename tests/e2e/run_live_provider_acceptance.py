#!/usr/bin/env python3
"""Opt-in TASK-026 assembled-provider acceptance against pinned live Codex.

The campaign copies only the selected Codex account credential into disposable
homes.  It starts the production gateway, drives it with the generated Go
public-v1 client, and exercises one actual hierarchy under each approval mode.
Only bounded, identifier-free evidence is printed.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import select
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
from contextlib import ExitStack, contextmanager
from pathlib import Path
from typing import Any

from run_live_primary_bridge import (
    checked_path,
    machine as unsafe_machine,
    run,
    write_json,
)


OPT_IN = "DOLGORAE_RUN_LIVE_PROVIDER_ACCEPTANCE"
PINNED_CODEX_VERSION = "codex-cli 0.158.0"
ROOT = Path(__file__).resolve().parents[2]
CLIENT_SOURCE = ROOT / "tests/e2e/private_boundary_client"
RESULT_PAGE_BYTES = 65_536
LARGE_RESULT_MINIMUM = RESULT_PAGE_BYTES + 1
MAXIMUM_RESULT_BYTES = 33_554_432
MAXIMUM_TOOL_RESULT_ROWS = 128
MAXIMUM_TOOL_RESULT_ENVELOPE_BYTES = 1_048_576
CAMPAIGN_TEMP_ROOT = Path("/private/tmp")
GATEWAY_READY_TIMEOUT_SECONDS = 15
GATEWAY_READY_MAXIMUM_BYTES = 65_536


@contextmanager
def campaign_root(suffix: str):
    root = Path(tempfile.mkdtemp(prefix=f"d26-{suffix}-", dir=CAMPAIGN_TEMP_ROOT))

    def remove_credentials():
        for credential in (
            root / "codex-home/auth.json",
            root / "operator.json",
            root / "home/.dolgorae/controller-carriers/task026" / suffix / "controller.json",
        ):
            credential.unlink(missing_ok=True)

    try:
        yield root
    except BaseException:
        remove_credentials()
        print(f"live provider diagnostic root preserved: {root}", file=sys.stderr)
        raise
    else:
        remove_credentials()
        shutil.rmtree(root)


def machine(
    binary: Path, args: list[str], *, cwd: Path, env: dict[str, str]
) -> dict[str, Any]:
    try:
        return unsafe_machine(binary, args, cwd=cwd, env=env)
    except Exception:
        operation = args[0] if args else "unknown"
        raise RuntimeError(f"Dolgorae machine operation {operation} failed") from None


def policy_input(profile: str, policy: str, approval: str) -> dict[str, Any]:
    return {
        "schema_version": 2,
        "policy_name": policy,
        "revision": 1,
        "approval_policy": approval,
        "max_active_specialists": 1,
        "roles": [
            {
                "role_ref": "provider-acceptance",
                "role_source": {"scope": "project", "name": "provider-acceptance"},
                "agent_configuration": {
                    "schema_version": 2,
                    "selected_profile": profile,
                    "model": None,
                    "default_effort": "low",
                    "purpose": "research",
                    "purpose_label": None,
                    "required_capabilities": [],
                    "execution_lane": "shared_readonly",
                    "required_assurance": "best_effort_personal_alpha",
                    "native_subagent_policy": "enabled",
                },
                "max_active_instances": 1,
                "reuse_policy": "never",
                "allowed_access": ["read_only"],
                "activation_policy": "keep_resident",
                "primary_may_request": True,
                "collaboration_source": False,
                "collaboration_target": False,
                "auto_approve_when_fully_delegated": approval == "fully_delegated",
            }
        ],
    }


def client(
    executable: Path,
    operation: str,
    common: list[str],
    extra: list[str],
    *,
    cwd: Path,
    env: dict[str, str],
    timeout: float = 1_200,
) -> dict[str, Any]:
    try:
        completed = run(
            [str(executable), operation, *common, *extra],
            cwd=cwd,
            env=env,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        raise RuntimeError(f"generated client {operation} timed out") from None
    if completed.returncode:
        raise RuntimeError(
            f"generated client {operation} failed with exit {completed.returncode}"
        )
    try:
        return json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(
            f"generated client {operation} returned invalid JSON"
        ) from error


def read_gateway_ready_line(
    process: subprocess.Popen[str], timeout: float = GATEWAY_READY_TIMEOUT_SECONDS
) -> str:
    assert process.stdout is not None
    deadline = time.monotonic() + timeout
    buffered = bytearray()
    while b"\n" not in buffered:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise RuntimeError("gateway readiness timed out")
        readable, _, _ = select.select([process.stdout], [], [], remaining)
        if not readable:
            raise RuntimeError("gateway readiness timed out")
        chunk = os.read(process.stdout.fileno(), 4096)
        if not chunk:
            raise RuntimeError("gateway exited before readiness")
        buffered.extend(chunk)
        if len(buffered) > GATEWAY_READY_MAXIMUM_BYTES:
            raise RuntimeError("gateway readiness exceeded the size bound")
    line, _, remainder = buffered.partition(b"\n")
    if remainder:
        raise RuntimeError("gateway wrote unexpected stdout after readiness")
    try:
        return line.decode("utf-8")
    except UnicodeDecodeError as error:
        raise RuntimeError("gateway readiness was not UTF-8") from error


def gateway(
    binary: Path, socket: Path, env: dict[str, str], log: Path
) -> subprocess.Popen[str]:
    log_handle = log.open("w", encoding="utf-8")
    process = subprocess.Popen(
        [str(binary), "serve", "--socket", str(socket)],
        cwd=socket.parent,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=log_handle,
        text=True,
    )
    log_handle.close()
    try:
        ready = read_gateway_ready_line(process)
        envelope = json.loads(ready)
        if envelope.get("ok") is not True:
            raise RuntimeError("gateway failed readiness")
    except Exception:
        if process.poll() is None:
            process.kill()
        process.wait(timeout=5)
        raise
    return process


def stop_gateway(process: subprocess.Popen[str]) -> None:
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
    if process.returncode not in (0, -15):
        raise RuntimeError(f"gateway exited with {process.returncode}")


def stop_profile_server(
    binary: Path,
    profile: str,
    operator: Path,
    server_key: str,
    *,
    workspace: Path,
    env: dict[str, str],
) -> None:
    try:
        completed = run(
            [
                str(binary),
                "profile",
                "server",
                "stop",
                profile,
                "--operator-file",
                str(operator),
                "--interrupt",
                "--confirm-server-key",
                server_key,
            ],
            cwd=workspace,
            env=env,
            timeout=90,
        )
        if completed.returncode:
            try:
                code = json.loads(completed.stdout).get("error", {}).get("code", "UNKNOWN")
            except json.JSONDecodeError:
                code = "NON_MACHINE_OUTPUT"
            raise RuntimeError(f"Profile Server cleanup failed ({code})")
    except RuntimeError:
        raise
    except Exception as error:
        raise RuntimeError(f"Profile Server cleanup failed ({type(error).__name__})") from error


def tool_results(state_root: Path) -> list[dict[str, Any]]:
    database = state_root / "orchestration/orchestration.sqlite3"
    connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
    try:
        rows = connection.execute(
            "SELECT response_json FROM brokered_tool_results ORDER BY created_at_ms,source_tool_call_id"
        )
        decoded: list[dict[str, Any]] = []
        for index, (raw,) in enumerate(rows):
            if index >= MAXIMUM_TOOL_RESULT_ROWS:
                raise RuntimeError("live Primary produced too many tool results")
            if len(raw.encode("utf-8")) > MAXIMUM_TOOL_RESULT_ENVELOPE_BYTES:
                raise RuntimeError("live Primary tool result exceeded the envelope bound")
            envelope = json.loads(raw)
            if envelope.get("schema_version") != "dolgorae.brokered-tool-result/v1":
                raise RuntimeError("unexpected brokered tool result schema")
            if envelope.get("outcome") != "ok":
                raise RuntimeError("live Primary tool call returned a non-ok outcome")
            decoded.append(envelope["value"])
        return decoded
    finally:
        connection.close()


def assert_primary_consumed_result(results: list[dict[str, Any]], minimum: int) -> dict[str, Any]:
    operations = [result.get("operation") for result in results]
    required = {
        "request_specialist_result",
        "await_specialist_operations_result",
        "list_specialists_result",
        "assign_specialist_task_result",
        "await_specialist_tasks_result",
        "collect_specialist_results_result",
        "read_specialist_result_result",
    }
    missing = required - set(operations)
    if missing:
        raise RuntimeError(f"live Primary omitted required tool operations: {sorted(missing)!r}")
    pages = [result for result in results if result.get("operation") == "read_specialist_result_result"]
    if not pages:
        raise RuntimeError("live Primary did not read the Specialist result")
    unique_pages: dict[int, dict[str, Any]] = {}
    for page in pages:
        previous = unique_pages.get(page["offset"])
        if previous is not None and previous != page:
            raise RuntimeError("live Primary returned conflicting repeated result pages")
        unique_pages[page["offset"]] = page
    pages = sorted(unique_pages.values(), key=lambda page: page["offset"])
    expected_offset = 0
    task_id = pages[0]["task_id"]
    length = pages[0]["length"]
    digest = pages[0]["sha256"]
    if length > MAXIMUM_RESULT_BYTES:
        raise RuntimeError("live Primary result exceeded the artifact bound")
    downloaded = bytearray()
    for page in pages:
        if (
            page["task_id"] != task_id
            or page["length"] != length
            or page["sha256"] != digest
            or page["offset"] != expected_offset
        ):
            raise RuntimeError("live Primary result pages are not one contiguous immutable result")
        content = page["content"].encode("utf-8")
        if len(content) > RESULT_PAGE_BYTES:
            raise RuntimeError("live Primary result page exceeded the requested bound")
        if len(downloaded) + len(content) > min(length, MAXIMUM_RESULT_BYTES):
            raise RuntimeError("live Primary result content exceeded its declared bound")
        downloaded.extend(content)
        expected_offset += len(content)
    if expected_offset != length or pages[-1]["truncated"]:
        raise RuntimeError("live Primary did not consume the complete Specialist result")
    if length < minimum:
        raise RuntimeError(f"Specialist result was {length} bytes; required at least {minimum}")
    if minimum > RESULT_PAGE_BYTES and len(pages) < 2:
        raise RuntimeError("above-bound Specialist result did not require multiple pages")
    if hashlib.sha256(downloaded).hexdigest() != digest:
        raise RuntimeError("live Primary result page content failed digest verification")
    return {"bytes": length, "sha256": digest, "pages": len(pages)}


def send_and_wait(
    client_binary: Path,
    common: list[str],
    key: str,
    message: str,
    *,
    workspace: Path,
    env: dict[str, str],
) -> None:
    client(
        client_binary,
        "submit",
        common,
        ["--idempotency-key", key, "--message", message],
        cwd=workspace,
        env=env,
    )
    client(
        client_binary,
        "wait",
        common,
        ["--states", "idle", "--timeout", "15m"],
        cwd=workspace,
        env=env,
    )


def campaign(
    binary: Path,
    codex: Path,
    source_codex_home: Path,
    client_binary: Path,
    approval: str,
) -> dict[str, Any]:
    suffix = "user" if approval == "user_approval_required" else "delegated"
    with ExitStack() as cleanup:
        root = Path(
            cleanup.enter_context(campaign_root(suffix))
        ).resolve(strict=True)
        home = root / "home"
        codex_home = root / "codex-home"
        workspace = root / "workspace"
        for path in (home, codex_home, workspace, root / "tmp", root / "config", root / "cache", root / "socket"):
            path.mkdir(mode=0o700)
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
        env.update(
            HOME=str(home),
            TMPDIR=str(root / "tmp"),
            XDG_CONFIG_HOME=str(root / "config"),
            XDG_CACHE_HOME=str(root / "cache"),
        )
        subprocess.run(["git", "init", "--quiet", str(workspace)], check=True, env=env)
        initialized = machine(binary, ["init", str(workspace)], cwd=workspace, env=env)
        profile = f"task026-{suffix}"
        policy = f"task026-{suffix}"
        machine(
            binary,
            [
                "profile", "add", profile,
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
        machine(binary, ["operator", "credential", "initialize", "--output", str(operator)], cwd=workspace, env=env)
        profile_state = machine(binary, ["profile", "server", "start", profile], cwd=workspace, env=env)["state"]
        server_key = str(profile_state["server_key"])
        cleanup.callback(
            stop_profile_server,
            binary,
            profile,
            operator,
            server_key,
            workspace=workspace,
            env=env,
        )
        role_directory = workspace / ".dolgorae/roles"
        role_directory.mkdir(parents=True)
        write_json(
            role_directory / "provider-acceptance.json",
            {
                "schema_version": 1,
                "name": "provider-acceptance",
                "display_name": "Provider acceptance Specialist",
                "description": "Returns exact bounded data for the live provider acceptance campaign.",
                "instructions": "Follow the accepted task exactly. Return only the requested final content and do not call tools or modify files.",
            },
            mode=0o644,
        )
        policy_path = root / "policy.json"
        write_json(policy_path, policy_input(profile, policy, approval))
        machine(binary, ["specialist", "policy", "add", policy, "--workspace", str(workspace), "--file", str(policy_path)], cwd=workspace, env=env)
        controller_root = home / ".dolgorae/controller-carriers"
        controller_root.mkdir(parents=True, mode=0o700)
        controller_client_directory = controller_root / "task026"
        controller_client_directory.mkdir(mode=0o700)
        controller_directory = controller_client_directory / suffix
        controller_directory.mkdir(mode=0o700)
        controller = controller_directory / "controller.json"
        created = machine(
            binary,
            [
                "controller", "credential", "create",
                "--kind", "interactive-client",
                "--instance-id", f"task026-{suffix}",
                "--orchestration-policy", policy,
                "--output", str(controller),
            ],
            cwd=workspace,
            env=env,
        )
        controller_id = str(created["controller"]["controller_id"])
        socket = root / "socket/dolgorae.sock"
        server = gateway(binary, socket, env, root / "gateway.log")
        cleanup.callback(stop_gateway, server)
        common = [
            "--socket", str(socket), "--workspace", str(workspace),
            "--workspace-id", str(initialized["workspace_id"]),
            "--controller", str(controller), "--controller-id", controller_id,
        ]
        started = client(
            client_binary,
            "start",
            common,
            [
                "--profile", profile,
                "--idempotency-key", f"task026-{suffix}-start",
                "--instructions",
                "You are the TASK-026 live provider acceptance Primary. Use the Run-scoped dolgorae_orchestration tool exactly as each user turn directs. Never invent identifiers or use private files, sockets, credentials, or databases.",
            ],
            cwd=workspace,
            env=env,
        )
        run_id = str(started["run_id"])
        common.extend(["--run-id", run_id])
        request_prompt = (
            "Call dolgorae_orchestration exactly once with operation request_specialist, "
            "role_ref provider-acceptance, objective Prepare to return the acceptance payload, "
            "expected_output containing Exact requested payload only, requested_access read_only, "
            "and deadline_seconds 900. Do not call another tool in this turn."
        )
        send_and_wait(client_binary, common, f"task026-{suffix}-request", request_prompt, workspace=workspace, env=env)
        if approval == "user_approval_required":
            client(
                client_binary,
                "approve",
                common,
                ["--idempotency-key", f"task026-{suffix}-approval"],
                cwd=workspace,
                env=env,
            )
        operation_wait_prompt = (
            "Use the operation_id from the prior request_specialist result. Call "
            "await_specialist_operations with that one ID, return_when all, and "
            "transport_wait_seconds 60. You must call await_specialist_operations exactly once. "
            "Do not call another tool in this turn."
        )
        send_and_wait(
            client_binary,
            common,
            f"task026-{suffix}-await-operation",
            operation_wait_prompt,
            workspace=workspace,
            env=env,
        )
        list_prompt = (
            "Call list_specialists exactly once. Confirm one provider-acceptance member is ready. "
            "Do not call another tool and do not assign work in this turn."
        )
        send_and_wait(
            client_binary,
            common,
            f"task026-{suffix}-list-specialists",
            list_prompt,
            workspace=workspace,
            env=env,
        )
        if approval == "fully_delegated":
            task_objective = (
                "Return exactly one JSON object with key payload. Its string value must contain at "
                "least 70000 uppercase ASCII A characters. The exact count does not matter. Keep "
                "emitting A characters until the minimum is exceeded; do not abbreviate, explain, "
                "add other prose, or use Markdown."
            )
            minimum = LARGE_RESULT_MINIMUM
        else:
            task_objective = "Return exactly the plain UTF-8 text USER_APPROVAL_PATH_OK and nothing else."
            minimum = 1
        task_prompt = (
            "Using the ready provider-acceptance run_id returned by list_specialists, call "
            "assign_specialist_task with that run_id target, objective "
            f"{task_objective!r}, context_refs empty, expected_output containing Exact payload, "
            "execution_intent read_only, blocking true, and deadline_seconds 900. Then call "
            "await_specialist_tasks for the returned task_id with return_when all and "
            "transport_wait_seconds 60 until terminal. Call collect_specialist_results with "
            "after_sequence 0 and limit 64. Stop after collection; do not read the result in this "
            "turn and do not use private paths or ask the user for identifiers."
        )
        send_and_wait(client_binary, common, f"task026-{suffix}-task", task_prompt, workspace=workspace, env=env)
        read_prompt = (
            "Use the task_id from the prior assign or collect result. Call read_specialist_result "
            "for that task_id beginning at offset 0 with limit 65536. If truncated is true, call "
            "read_specialist_result again with offset advanced by the exact UTF-8 byte length of "
            "the prior content and the same limit. Continue until truncated is false. You must call "
            "read_specialist_result at least once and consume the complete immutable result. Do not "
            "call any other tool, use private paths, or ask the user for identifiers."
        )
        send_and_wait(
            client_binary,
            common,
            f"task026-{suffix}-read-result",
            read_prompt,
            workspace=workspace,
            env=env,
        )
        state_root = home / ".dolgorae/workspaces" / str(initialized["workspace_id"])
        consumed = assert_primary_consumed_result(tool_results(state_root), minimum)
        observed = client(
            client_binary,
            "observe",
            common,
            ["--minimum-result-bytes", str(minimum)],
            cwd=workspace,
            env=env,
        )
        if observed["sha256"] != consumed["sha256"] or observed["result_bytes"] != consumed["bytes"]:
            raise RuntimeError("Primary and public client did not consume the same immutable result")
        client(client_binary, "close", common, [], cwd=workspace, env=env)
        stop_gateway(server)
        return {
            "approval_policy": approval,
            "actual_primary": True,
            "actual_specialist": True,
            "generated_public_client": True,
            "method_count": observed["method_count"],
            "primary_result_pages": consumed["pages"],
            "result_bytes": consumed["bytes"],
            "result_sha256": consumed["sha256"],
            "isolated_home": True,
            "isolated_codex_home": True,
            "shared_profile_mutated": False,
        }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--codex", type=Path, required=True)
    parser.add_argument("--codex-home", type=Path, required=True)
    args = parser.parse_args()
    if os.environ.get(OPT_IN) != "1":
        raise SystemExit(f"{OPT_IN}=1 is required")
    binary = args.binary.expanduser().resolve(strict=True)
    codex = args.codex.expanduser().resolve(strict=True)
    source_codex_home = args.codex_home.expanduser().resolve(strict=True)
    version = run([str(codex), "--version"], cwd=ROOT, env=dict(os.environ)).stdout.strip()
    if version != PINNED_CODEX_VERSION:
        raise RuntimeError(f"expected {PINNED_CODEX_VERSION!r}, observed {version!r}")
    with tempfile.TemporaryDirectory(prefix="dolgorae-task026-client-") as build_root:
        client_binary = Path(build_root) / "private-boundary-client"
        built = run(
            ["go", "build", "-o", str(client_binary), "."],
            cwd=CLIENT_SOURCE,
            env=dict(os.environ, GOTOOLCHAIN="local"),
            timeout=300,
        )
        if built.returncode:
            raise RuntimeError("generated client build failed")
        campaigns = [
            campaign(binary, codex, source_codex_home, client_binary, "user_approval_required"),
            campaign(binary, codex, source_codex_home, client_binary, "fully_delegated"),
        ]
    print(
        json.dumps(
            {
                "task": "TASK-026",
                "codex_version": version.removeprefix("codex-cli "),
                "campaigns": campaigns,
            },
            sort_keys=True,
            separators=(",", ":"),
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
