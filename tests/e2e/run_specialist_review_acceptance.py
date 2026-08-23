#!/usr/bin/env python3
"""Opt-in TASK-013 live acceptance runner for one-shot Specialist Review."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import selectors
import signal
import sqlite3
import subprocess
import sys
import time
import uuid
from pathlib import Path
from typing import Any

from jsonschema import Draft202012Validator, FormatChecker
from referencing import Registry, Resource

PINNED_CODEX_VERSION = "codex-cli 0.149.0"
LIVE_OPT_IN = "DOLGORAE_RUN_LIVE_SPECIALIST_REVIEW"
MAX_OUTPUT_BYTES = 1_048_576
ROOT = Path(__file__).resolve().parents[2]
PROTOCOL = ROOT / "docs" / "protocol"


def digest(data: bytes) -> str:
    return "sha256:" + hashlib.sha256(data).hexdigest()


def run(
    command: list[str],
    *,
    cwd: Path,
    env: dict[str, str],
    interrupt_after_seconds: float | None = None,
) -> subprocess.CompletedProcess[bytes]:
    process = subprocess.Popen(
        command,
        cwd=cwd,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert process.stdout is not None and process.stderr is not None
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ, "stdout")
    selector.register(process.stderr, selectors.EVENT_READ, "stderr")
    captured = {"stdout": bytearray(), "stderr": bytearray()}
    started = time.monotonic()
    normal_deadline = time.monotonic() + 660
    cleanup_deadline: float | None = None
    bounded_failure: str | None = None
    interrupted = False
    try:
        while selector.get_map():
            if process.poll() is not None and cleanup_deadline is None:
                cleanup_deadline = time.monotonic() + 60
            if (
                interrupt_after_seconds is not None
                and not interrupted
                and time.monotonic() - started >= interrupt_after_seconds
            ):
                process.send_signal(signal.SIGINT)
                interrupted = True
            now = time.monotonic()
            if bounded_failure is None and now >= normal_deadline:
                bounded_failure = "process exceeded the 660 second acceptance bound"
                cleanup_deadline = now + 60
                if process.poll() is None:
                    process.send_signal(signal.SIGINT)
            remaining = (cleanup_deadline or normal_deadline) - now
            if remaining <= 0:
                process.kill()
                process.wait()
                raise RuntimeError(
                    "process cleanup outcome is unknown after the 60 second SIGINT grace"
                )
            for key, _ in selector.select(min(remaining, 1.0)):
                chunk = os.read(key.fileobj.fileno(), 65_536)
                if not chunk:
                    selector.unregister(key.fileobj)
                    continue
                if bounded_failure is None:
                    captured[key.data].extend(chunk)
                    if sum(map(len, captured.values())) > MAX_OUTPUT_BYTES:
                        bounded_failure = (
                            "combined process output exceeds the 1 MiB acceptance bound"
                        )
                        cleanup_deadline = time.monotonic() + 60
                        if process.poll() is None:
                            process.send_signal(signal.SIGINT)
        completed = subprocess.CompletedProcess(
            command,
            process.wait(),
            bytes(captured["stdout"]),
            bytes(captured["stderr"]),
        )
        if bounded_failure is not None:
            raise ValueError(bounded_failure + "; child completed SIGINT cleanup")
        return completed
    finally:
        selector.close()
        process.stdout.close()
        process.stderr.close()


def workspace_fingerprint(workspace: Path) -> str:
    listed = subprocess.run(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
        cwd=workspace,
        check=True,
        capture_output=True,
    ).stdout
    ignored = subprocess.run(
        ["git", "ls-files", "--others", "--ignored", "--exclude-standard", "-z"],
        cwd=workspace,
        check=True,
        capture_output=True,
    ).stdout
    state = hashlib.sha256()
    for encoded in sorted(item for item in listed.split(b"\0") if item):
        path = workspace / os.fsdecode(encoded)
        state.update(len(encoded).to_bytes(8, "big"))
        state.update(encoded)
        if path.is_symlink():
            target = os.readlink(path).encode("utf-8", "surrogateescape")
            state.update(b"symlink\0" + len(target).to_bytes(8, "big") + target)
        elif path.is_file():
            state.update(b"file\0" + path.stat().st_size.to_bytes(8, "big"))
            with path.open("rb") as source:
                while chunk := source.read(65_536):
                    state.update(chunk)
        else:
            state.update(b"other\0")
    for encoded in sorted(item for item in ignored.split(b"\0") if item):
        metadata = os.lstat(workspace / os.fsdecode(encoded))
        state.update(b"ignored\0" + len(encoded).to_bytes(8, "big") + encoded)
        for value in (
            metadata.st_mode,
            metadata.st_size,
            metadata.st_ino,
            metadata.st_mtime_ns,
            metadata.st_ctime_ns,
        ):
            state.update(value.to_bytes(16, "big", signed=False))
    git_directories = subprocess.run(
        ["git", "rev-parse", "--path-format=absolute", "--git-dir", "--git-common-dir"],
        cwd=workspace,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.splitlines()
    for directory in sorted(set(git_directories)):
        root = Path(directory)
        state.update(b"git-metadata\0" + digest(str(root).encode()).encode())
        for path in sorted(root.rglob("*")):
            if path.is_symlink() or not path.is_file():
                continue
            metadata = path.stat()
            relative = os.fsencode(path.relative_to(root))
            state.update(len(relative).to_bytes(8, "big") + relative)
            for value in (
                metadata.st_mode,
                metadata.st_size,
                metadata.st_ino,
                metadata.st_mtime_ns,
                metadata.st_ctime_ns,
            ):
                state.update(value.to_bytes(16, "big", signed=False))
    return "sha256:" + state.hexdigest()


def parse_envelope(output: bytes) -> dict[str, Any]:
    if len(output) > MAX_OUTPUT_BYTES:
        raise ValueError("machine output exceeds the 1 MiB acceptance bound")
    value = json.loads(output)
    if not isinstance(value, dict) or value.get("schema_version") != 1:
        raise ValueError("machine output is not a v1 object envelope")
    return value


def validate_review_result(value: Any) -> None:
    registry = Registry()
    for path in sorted(PROTOCOL.glob("*.schema.json")):
        schema = json.loads(path.read_text(encoding="utf-8"))
        registry = registry.with_resource(schema["$id"], Resource.from_contents(schema))
    schema = json.loads(
        (PROTOCOL / "dolgorae-specialist-review-tool-v1.schema.json").read_text(encoding="utf-8")
    )
    errors = list(
        Draft202012Validator(
            schema, registry=registry, format_checker=FormatChecker()
        ).iter_errors(value)
    )
    if errors:
        raise ValueError("review result does not satisfy the checked protocol schema")


def observable_scan(payload: bytes, workspace: Path, canary: str) -> dict[str, bool]:
    text = payload.decode("utf-8", "replace")
    home = str(Path.home())
    return {
        "host_context_canary_absent": canary not in text,
        "credential_material_absent": not any(
            marker in text
            for marker in ("OPENAI_API_KEY", "Bearer ", "controller_credential", "credential.json")
        ),
        "private_endpoint_absent": not any(
            marker in text
            for marker in ("unix://", ".sock", str(workspace / ".dolgorae"), home + "/Library/Application Support/Dolgorae")
        ),
    }


def canary_absent_from_state(state_root: Path, canary: str) -> bool:
    if not state_root.exists():
        return True
    needle = canary.encode()
    inspected = 0
    for path in sorted(state_root.rglob("*")):
        if path.is_symlink() or not path.is_file():
            continue
        with path.open("rb") as handle:
            overlap = b""
            while chunk := handle.read(65_536):
                inspected += len(chunk)
                if inspected > 268_435_456:
                    raise ValueError("Dolgorae state canary scan exceeds the 256 MiB bound")
                if needle in overlap + chunk:
                    return False
                overlap = chunk[-max(len(needle) - 1, 0) :]
    return True


def reviewer_isolation_evidence(state_root: Path, reviewer_run_id: str) -> dict[str, Any]:
    parsed = uuid.UUID(reviewer_run_id)
    if parsed.version != 7:
        raise ValueError("reviewer Run identity is not UUIDv7")
    matches = list(state_root.glob(f"workspaces/*/runs/{reviewer_run_id}/state.json"))
    if len(matches) != 1:
        raise ValueError("exactly one Reviewer Run state record is required")
    state = json.loads(matches[0].read_text(encoding="utf-8"))
    manifest = json.loads(matches[0].with_name("manifest.json").read_text(encoding="utf-8"))
    reviewer_thread = state.get("thread_id")
    host_thread = os.environ.get("CODEX_THREAD_ID")
    if not isinstance(reviewer_thread, str) or not reviewer_thread or not host_thread:
        raise ValueError("host and Reviewer thread identities must both be observable")
    reviewer_digest = digest(reviewer_thread.encode())
    host_digest = digest(host_thread.encode())
    if reviewer_digest == host_digest:
        raise ValueError("Reviewer reused the host Codex thread")
    mcp_servers = (
        manifest.get("profile", {})
        .get("process_static_configuration", {})
        .get("mcp_servers", {})
    )
    if not isinstance(mcp_servers, dict) or any(
        name == "dolgorae_review" or "dolgorae_review" in json.dumps(configuration)
        for name, configuration in mcp_servers.items()
    ):
        raise ValueError("Reviewer manifest contains the recursive review adapter")
    return {
        "host_thread_ref_sha256": host_digest,
        "reviewer_thread_ref_sha256": reviewer_digest,
        "separate_codex_thread": True,
        "recursive_review_adapter_absent": True,
    }


def checked_environment(canary: str) -> dict[str, str]:
    allowed = {
        key: value
        for key, value in os.environ.items()
        if key in {"HOME", "LANG", "LC_ALL", "PATH", "TMPDIR"}
    }
    allowed["DOLGORAE_ACCEPTANCE_HOST_CONTEXT_CANARY"] = canary
    return allowed


def verify_codex(codex: Path, workspace: Path, env: dict[str, str]) -> str:
    observed = run([str(codex), "--version"], cwd=workspace, env=env)
    version = observed.stdout.decode("utf-8", "strict").strip()
    if observed.returncode != 0 or version != PINNED_CODEX_VERSION:
        raise ValueError(f"expected {PINNED_CODEX_VERSION!r}, observed {version!r}")
    return version


def execute_review(
    binary: Path,
    workspace: Path,
    profile: str,
    env: dict[str, str],
    *,
    interrupt_after_seconds: float | None = None,
) -> subprocess.CompletedProcess[bytes]:
    return run(
        [
            str(binary),
            "specialist",
            "review",
            "--workspace",
            str(workspace),
            "--profile",
            profile,
            "--scope",
            "working-tree",
            "--format",
            "json",
        ],
        cwd=workspace,
        env=env,
        interrupt_after_seconds=interrupt_after_seconds,
    )


def engagement_states(state_root: Path) -> dict[str, str]:
    states: dict[str, str] = {}
    for database in state_root.glob("workspaces/*/orchestration/orchestration.sqlite3"):
        connection = sqlite3.connect(f"file:{database}?mode=ro", uri=True)
        try:
            for engagement_id, state in connection.execute(
                "SELECT engagement_id, state FROM engagements"
            ):
                if engagement_id in states:
                    raise ValueError("duplicate engagement identity across workspace stores")
                states[engagement_id] = state
        finally:
            connection.close()
    return states


def success_evidence(
    binary: Path, workspace: Path, profile: str, codex: Path, canary: str
) -> dict[str, Any]:
    env = checked_environment(canary)
    version = verify_codex(codex, workspace, env)
    before = workspace_fingerprint(workspace)
    completed = execute_review(binary, workspace, profile, env)
    after = workspace_fingerprint(workspace)
    combined = completed.stdout + b"\n" + completed.stderr
    envelope = parse_envelope(completed.stdout)
    if completed.returncode != 0 or envelope.get("ok") is not True:
        raise ValueError("live Specialist Review did not succeed")
    if envelope.get("command") != "specialist.review":
        raise ValueError("unexpected machine command identity")
    data = envelope.get("data")
    if not isinstance(data, dict) or data.get("state") != "completed":
        raise ValueError("review result is not completed")
    validate_review_result(data)
    findings = data.get("findings")
    if not isinstance(findings, list):
        raise ValueError("review findings are not an array")
    state_root = Path(env["HOME"]) / "Library" / "Application Support" / "Dolgorae"
    isolation = reviewer_isolation_evidence(state_root, str(data.get("reviewer_run_id")))
    scan = observable_scan(combined, workspace, canary)
    scan["host_context_canary_absent_from_state"] = canary_absent_from_state(
        state_root, canary
    )
    if before != after or data.get("workspace_write_observed") is not False or not all(scan.values()):
        raise ValueError("review isolation or observable-output canary failed")
    return {
        "phase": "machine_cli_review",
        "codex_version": version,
        "carrier": "machine_cli",
        "profile": profile,
        "review_id": data.get("review_id"),
        "reviewer_run_id": data.get("reviewer_run_id"),
        "result_artifact_ref": data.get("result_artifact_ref"),
        "finding_count": len(findings),
        "findings": [
            {
                "severity": finding.get("severity"),
                "title": finding.get("title"),
                "path": finding.get("path"),
                "line_start": finding.get("line_start"),
                "confidence": finding.get("confidence"),
            }
            for finding in findings
            if isinstance(finding, dict)
        ],
        "workspace_fingerprint_before": before,
        "workspace_fingerprint_after": after,
        "workspace_write_observed": False,
        "reviewer_isolation": isolation,
        "observable_scan": scan,
        "machine_output_digest": digest(completed.stdout),
        "machine_stderr_digest": digest(completed.stderr),
    }


def failure_evidence(binary: Path, workspace: Path, codex: Path, canary: str) -> dict[str, Any]:
    env = checked_environment(canary)
    version = verify_codex(codex, workspace, env)
    before = workspace_fingerprint(workspace)
    completed = execute_review(binary, workspace, "__task013_missing_profile__", env)
    after = workspace_fingerprint(workspace)
    combined = completed.stdout + b"\n" + completed.stderr
    envelope = parse_envelope(completed.stdout)
    error = envelope.get("error")
    if completed.returncode == 0 or envelope.get("ok") is not False or not isinstance(error, dict):
        raise ValueError("missing-profile probe did not fail safely")
    if error.get("code") != "PROFILE_NOT_FOUND":
        raise ValueError("missing-profile probe returned the wrong failure code")
    scan = observable_scan(combined, workspace, canary)
    scan["host_context_canary_absent_from_state"] = canary_absent_from_state(
        Path(env["HOME"]) / "Library" / "Application Support" / "Dolgorae", canary
    )
    if before != after or error.get("retryable") is not False or not all(scan.values()):
        raise ValueError("safe-failure cleanup or observable-output canary failed")
    return {
        "phase": "safe_non_success",
        "codex_version": version,
        "carrier": "machine_cli",
        "failure_kind": "missing_profile_before_allocation",
        "error_code": error.get("code"),
        "retryable": error.get("retryable"),
        "workspace_fingerprint_before": before,
        "workspace_fingerprint_after": after,
        "observable_scan": scan,
        "machine_output_digest": digest(completed.stdout),
        "machine_stderr_digest": digest(completed.stderr),
    }


def cancellation_evidence(
    binary: Path, workspace: Path, profile: str, codex: Path, canary: str
) -> dict[str, Any]:
    env = checked_environment(canary)
    version = verify_codex(codex, workspace, env)
    state_root = Path(env["HOME"]) / "Library" / "Application Support" / "Dolgorae"
    before = workspace_fingerprint(workspace)
    engagements_before = engagement_states(state_root)
    completed = execute_review(
        binary, workspace, profile, env, interrupt_after_seconds=8.0
    )
    after = workspace_fingerprint(workspace)
    engagements_after = engagement_states(state_root)
    new_states = [
        state
        for engagement_id, state in engagements_after.items()
        if engagement_id not in engagements_before
    ]
    combined = completed.stdout + b"\n" + completed.stderr
    envelope = parse_envelope(completed.stdout)
    error = envelope.get("error")
    if completed.returncode == 0 or envelope.get("ok") is not False or not isinstance(error, dict):
        raise ValueError("live cancellation did not return a checked non-success")
    if error.get("code") != "REVIEW_CANCELLED":
        raise ValueError("live cancellation returned the wrong failure code")
    scan = observable_scan(combined, workspace, canary)
    scan["host_context_canary_absent_from_state"] = canary_absent_from_state(
        state_root, canary
    )
    if (
        before != after
        or not new_states
        or any(state not in {"closed", "interrupted_unknown"} for state in new_states)
        or error.get("retryable") is not False
        or not all(scan.values())
    ):
        raise ValueError("live cancellation did not clean up to a safe state")
    return {
        "phase": "live_cancellation",
        "codex_version": version,
        "carrier": "machine_cli",
        "error_code": error.get("code"),
        "retryable": error.get("retryable"),
        "new_engagement_count": len(new_states),
        "new_engagement_terminal_states": sorted(new_states),
        "workspace_fingerprint_before": before,
        "workspace_fingerprint_after": after,
        "observable_scan": scan,
        "machine_output_digest": digest(completed.stdout),
        "machine_stderr_digest": digest(completed.stderr),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--profile", default="reviewer")
    parser.add_argument("--codex", type=Path, required=True)
    parser.add_argument(
        "--phase", choices=("review", "safe-failure", "cancel"), required=True
    )
    args = parser.parse_args()
    if os.environ.get(LIVE_OPT_IN) != "1":
        print(f"{LIVE_OPT_IN}=1 is required for live acceptance", file=sys.stderr)
        return 2
    workspace = args.workspace.resolve(strict=True)
    if not args.binary.is_absolute() or not args.codex.is_absolute() or not args.workspace.is_absolute():
        print("binary, codex, and workspace must be absolute paths", file=sys.stderr)
        return 2
    canary = "task013-host-hidden-context-7fe3c879"
    try:
        if args.phase == "review":
            evidence = success_evidence(args.binary, workspace, args.profile, args.codex, canary)
        elif args.phase == "cancel":
            evidence = cancellation_evidence(
                args.binary, workspace, args.profile, args.codex, canary
            )
        else:
            evidence = failure_evidence(args.binary, workspace, args.codex, canary)
    except (OSError, subprocess.SubprocessError, ValueError, json.JSONDecodeError) as error:
        print(f"acceptance failed: {error}", file=sys.stderr)
        return 1
    json.dump(evidence, sys.stdout, sort_keys=True, separators=(",", ":"))
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
