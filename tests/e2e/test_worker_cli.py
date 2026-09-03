#!/usr/bin/env python3
"""Black-box validation of the hidden per-Run worker boundary."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import pathlib
import select
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import time
from typing import BinaryIO

import native_codex
from schema_support import assert_valid, validator

RUN_ID = "018f22e2-79b0-7cc3-a24c-78c48f28bbcf"
BOOT_UUID = subprocess.run(
    ["sysctl", "-n", "kern.bootsessionuuid"],
    check=True,
    capture_output=True,
    text=True,
).stdout.strip()
REPOSITORY = pathlib.Path(__file__).resolve().parents[2]
FAKE_APP_SERVER = REPOSITORY / "tools" / "fake_app_server"


def fresh_run_id() -> str:
    """A UUIDv7 run identity, so one worker socket never outlives its test."""
    milliseconds = time.time_ns() // 1_000_000
    raw = bytearray(milliseconds.to_bytes(6, "big") + os.urandom(10))
    raw[6] = 0x70 | (raw[6] & 0x0F)
    raw[8] = 0x80 | (raw[8] & 0x3F)
    value = raw.hex()
    return f"{value[:8]}-{value[8:12]}-{value[12:16]}-{value[16:20]}-{value[20:]}"


def private_directory(path: pathlib.Path) -> None:
    path.mkdir(mode=0o700, parents=True)
    path.chmod(0o700)


CONTROLLER_CAPABILITY_DOMAIN = b"dolgorae.controller-capability.v1\x00"
WORKSPACE_ID = "11" * 32


def jcs(document: object) -> bytes:
    """RFC 8785 canonical bytes for the ASCII-only fixtures used here."""
    return json.dumps(
        document, sort_keys=True, separators=(",", ":"), ensure_ascii=False
    ).encode("utf-8")


def jcs_sha256(document: object) -> str:
    return hashlib.sha256(jcs(document)).hexdigest()


def mint_controller(binary: pathlib.Path, output: pathlib.Path) -> dict[str, object]:
    """Mint a real Controller credential and return the binding a Run publishes.

    The capability itself never leaves the mode-0600 file; only its
    domain-separated digest reaches the Run record, exactly as the product
    does.
    """
    subprocess.run(
        [
            str(binary),
            "controller",
            "credential",
            "create",
            # `automation` pairs with the fixture's managed_agent control mode.
            "--kind",
            "automation",
            "--instance-id",
            f"worker-e2e-{output.stem}",
            "--output",
            str(output),
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    document = json.loads(output.read_text(encoding="utf-8"))
    encoded = str(document["capability"])
    capability = base64.urlsafe_b64decode(encoded + "=" * (-len(encoded) % 4))
    return {
        "identity": {
            "controller_id": document["controller_id"],
            "kind": document["kind"],
            "instance_id": document["instance_id"],
            "subject_id": document["subject_id"],
            "generation": 1,
        },
        "capability_sha256": hashlib.sha256(
            CONTROLLER_CAPABILITY_DOMAIN + capability
        ).hexdigest(),
    }


def run_manifest(run_id: str, controller: dict[str, object]) -> dict[str, object]:
    """The durable Run record the worker loads its Controller authority from."""
    instructions_text = "Answer briefly."
    instructions = {
        "schema": "dolgorae.instructions/v1",
        "common_prefix_version": 1,
        "mode_prefix_version": 1,
        "purpose_prefix_version": 1,
        "normalized_byte_length": len(instructions_text.encode("utf-8")),
        "normalized_sha256": hashlib.sha256(
            instructions_text.encode("utf-8")
        ).hexdigest(),
    }
    purpose = {"kind": "implementation", "external_label": None}
    executable_identity = {
        "resolved_path": "/usr/local/bin/codex",
        "device": 1,
        "inode": 2,
        "sha256": "8" * 64,
    }
    profile: dict[str, object] = {
        "schema_version": 1,
        "profile_name": "default",
        "canonical_codex_home": "/tmp/codex-home",
        "normalized_argv": ["/usr/local/bin/codex"],
        "launch_cwd_policy": "profile_state_directory_v1",
        "derived_launch_cwd": "/tmp/dolgorae/profiles/server",
        "sanitized_environment": {"LANG": "C"},
        "enabled_features": [],
        "disabled_features": [],
        "process_static_configuration": {},
        "initial_configuration_observation": {},
        "executable_identity": executable_identity,
        "codex_version": "0.147.0",
        "app_server_schema_sha256": "4" * 64,
        "compatibility_manifest_sha256": "a" * 64,
        "launch_contract_sha256": "0" * 64,
        "initial_server_key": "3" * 64,
    }
    profile["launch_contract_sha256"] = jcs_sha256(
        {
            "schema_version": profile["schema_version"],
            "canonical_codex_home": profile["canonical_codex_home"],
            "normalized_argv": profile["normalized_argv"],
            "launch_cwd_policy": profile["launch_cwd_policy"],
            "executable_identity": executable_identity,
            "launch_mode": "app_server_unix_socket_v1",
            "sanitized_environment": profile["sanitized_environment"],
            "process_static_configuration": profile["process_static_configuration"],
            "codex_version": profile["codex_version"],
            "app_server_schema_sha256": profile["app_server_schema_sha256"],
            "compatibility_manifest_sha256": profile["compatibility_manifest_sha256"],
            "enabled_features": profile["enabled_features"],
            "disabled_features": profile["disabled_features"],
        }
    )
    return {
        "schema_version": 1,
        "run_id": run_id,
        "workspace_id": WORKSPACE_ID,
        "canonical_workspace": "/tmp/workspace",
        "workspace_mode": "git",
        "start_baseline": {
            "head": None,
            "branch": None,
            "tracked_changes": [],
            "untracked_paths": [],
        },
        "created_at": "2026-08-22T12:34:56.123456Z",
        "initial_access": "read",
        "control_mode": "managed_agent",
        "execution_lane": "shared_readonly",
        "requested_assurance": "best_effort_personal_alpha",
        "achieved_assurance": "best_effort_personal_alpha",
        "profile": profile,
        "agent_configuration": {
            "schema_version": 1,
            "runtime_profile": "default",
            "runtime_profile_snapshot_sha256": jcs_sha256(profile),
            "model": "gpt-5.6",
            "default_effort": "medium",
            "purpose": purpose,
            "required_capabilities": ["reader"],
            "role_reference": None,
            "normalized_instructions": instructions_text,
            "instructions": instructions,
            "execution_lane": "shared_readonly",
            "required_assurance": "best_effort_personal_alpha",
            "native_subagent_policy": "enabled",
        },
        "profile_capability_snapshot": {
            "schema_version": 1,
            "profile_name": "default",
            "server_key": "3" * 64,
            "server_epoch": 1,
            "app_server_version": "0.147.0",
            "schema_sha256": "4" * 64,
            "capabilities": {"reader": "supported"},
        },
        "app_server": {
            "version": "0.147.0",
            "schema_status": "accepted",
            "actual_codex_home": "/tmp/codex-home",
        },
        "dolgorae": {
            "version": "0.1.0",
            "binary_sha256": "5" * 64,
            "ipc_protocol_version": 1,
        },
        "model": "gpt-5.6",
        "initial_reasoning_effort": "medium",
        "default_reasoning_effort": "medium",
        "instructions": instructions,
        "controller": controller,
        "purpose": purpose,
        "parent_ref": None,
        "required_capabilities": ["reader"],
        "thread_id": None,
        "fork_provenance": None,
        "write_continuation_provenance": None,
        "aggregate_binding": None,
        "audit": {
            "hash_scheme": "sha256-jcs-v1",
            "genesis_previous_hash": "0" * 64,
            "raw_payload_limit": 2 * 1024 * 1024,
            "represented_payload_limit": 3 * 1024 * 1024,
        },
        "compatibility": "accepted",
    }


def write_private(path: pathlib.Path, payload: bytes) -> None:
    path.write_bytes(payload)
    path.chmod(0o600)


def prepare(
    root: pathlib.Path,
    binary: pathlib.Path,
    run_id: str = RUN_ID,
    session: dict[str, object] | None = None,
) -> tuple[pathlib.Path, pathlib.Path]:
    """Publish one Run's durable state and return its bootstrap and credential.

    The Run record is real, because the worker resolves Controller authority
    from `manifest.json` and `controller.json` on every mutation.
    """
    ledger_root = root / "runs" / run_id
    runtime_runs = root / "runtime" / "runs"
    startup = root / "runtime" / "locks" / "startup"
    for directory in (
        root / "runs",
        ledger_root,
        ledger_root / "recovery",
        root / "runtime",
        root / "runtime" / "locks",
        runtime_runs,
        startup,
    ):
        if not directory.exists():
            private_directory(directory)
    audit = ledger_root / "audit.jsonl"
    audit.touch(mode=0o600)
    audit.chmod(0o600)
    credential = root / f"controller-{run_id[:8]}.json"
    controller = mint_controller(binary, credential)
    write_private(ledger_root / "manifest.json", jcs(run_manifest(run_id, controller)))
    write_private(ledger_root / "controller.json", jcs(controller))
    bootstrap = runtime_runs / f"{run_id}.bootstrap.json"
    document: dict[str, object] = {
        "schema_version": 1,
        "workspace_id": "11" * 32,
        "run_id": run_id,
        "run_generation": 1,
        "boot_uuid": BOOT_UUID,
        "executable_sha256": binary_sha256(binary),
        "executable_path_sha256": "33" * 32,
        "dolgorae_version": "0.1.0",
        "mutation_protocol_version": 1,
        "control_socket_epoch": 1,
        # A compatibility refusal has to name the profile it was rejected
        # against, so the Run carries the profile it is pinned to.
        "profile": "default",
        "state_root": str(root),
        "ledger_root": str(ledger_root),
        "runtime_record_path": str(runtime_runs / f"{run_id}.json"),
        "startup_lock_path": str(startup / f"{run_id}.lock"),
    }
    if session is not None:
        session = dict(session)
        session.setdefault("controller_id", controller["identity"]["controller_id"])
        session.setdefault("control_mode", "managed_agent")
        document["session"] = session
    bootstrap.write_text(
        json.dumps(document, separators=(",", ":")) + "\n", encoding="utf-8"
    )
    bootstrap.chmod(0o600)
    return bootstrap, credential


class WorkerProcess:
    def __init__(self, pid: int) -> None:
        self.pid = pid

    def wait(self, timeout: float) -> int:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            observed, status = os.waitpid(self.pid, os.WNOHANG)
            if observed == self.pid:
                return os.waitstatus_to_exitcode(status)
            time.sleep(0.01)
        raise TimeoutError(f"worker {self.pid} did not exit within {timeout} seconds")


def start(
    binary: pathlib.Path, bootstrap: pathlib.Path
) -> tuple[WorkerProcess, BinaryIO]:
    read_fd, write_fd = os.pipe()
    null_fd = os.open(os.devnull, os.O_RDWR)
    environment = os.environ.copy()
    environment["TMPDIR"] = str(bootstrap.parent / "changed-tmpdir")
    actions = [
        (os.POSIX_SPAWN_DUP2, null_fd, 0),
        (os.POSIX_SPAWN_DUP2, null_fd, 1),
        (os.POSIX_SPAWN_DUP2, null_fd, 2),
        (os.POSIX_SPAWN_DUP2, write_fd, 3),
    ]
    if read_fd != 3:
        actions.append((os.POSIX_SPAWN_CLOSE, read_fd))
    if write_fd != 3:
        actions.append((os.POSIX_SPAWN_CLOSE, write_fd))
    pid = os.posix_spawn(
        str(binary),
        [str(binary), "__worker", "--bootstrap", str(bootstrap)],
        environment,
        file_actions=actions,
        setsid=True,
    )
    os.close(write_fd)
    os.close(null_fd)
    return WorkerProcess(pid), os.fdopen(read_fd, "rb", buffering=0)


FROZEN_CONTROL_V1 = frozenset({"hello", "status", "shutdown"})

# The build every non-frozen control request in this file declares.  SPEC-004:
# an upgrade never silently mixes CLI and worker versions inside one run
# generation, so the worker refuses a request whose caller is not its own
# build.  It is republished by `handoff` from each worker's runtime record.
CALLER_BUILD: dict[str, object] | None = None


def handoff(channel: BinaryIO) -> dict[str, object]:
    readable, _, _ = select.select([channel], [], [], 10)
    if not readable:
        raise TimeoutError("worker did not emit the fd-3 startup frame within ten seconds")
    line = channel.readline(65538)
    if not line.endswith(b"\n") or len(line) > 65537:
        raise AssertionError(f"invalid fd-3 startup frame: {line[:100]!r}")
    frame = json.loads(line)
    record = frame.get("record")
    if isinstance(record, dict):
        # Every non-frozen request must declare the build that composed it,
        # and the worker refuses one that is not its own.  The record the
        # worker just published names that build, so the fixtures address the
        # worker they actually started rather than the binary that spawned it.
        global CALLER_BUILD  # noqa: PLW0603 - one published fact per worker
        CALLER_BUILD = {
            "version": record["dolgorae_version"],
            "mutation_protocol_version": record["mutation_protocol_version"],
            "binary_sha256": record["binary_sha256"],
        }
    return frame


def call(
    path: pathlib.Path,
    operation: str,
    identity: dict[str, object],
    timeout: float = 5,
    credential_fd: int | None = None,
    caller: dict[str, object] | None = None,
    **extra: object,
) -> dict[str, object]:
    """One control request, optionally carrying a Controller descriptor.

    The descriptor rides the frame's first byte, so the wire stays one
    newline-delimited JSON frame and a caller with no credential is
    byte-identical to the frozen control-v1 request it always was.
    """
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(timeout)
        client.connect(str(path))
        request: dict[str, object] = {
            "operation": operation,
            "expected": identity,
            **extra,
        }
        if operation not in FROZEN_CONTROL_V1:
            request["caller"] = CALLER_BUILD if caller is None else caller
        payload = json.dumps(request, separators=(",", ":")).encode("utf-8") + b"\n"
        if credential_fd is None:
            client.sendall(payload)
        else:
            socket.send_fds(client, [payload[:1]], [credential_fd])
            client.sendall(payload[1:])
        response = bytearray()
        while not response.endswith(b"\n"):
            chunk = client.recv(65536)
            if not chunk:
                raise AssertionError(f"worker closed before answering {operation}")
            response.extend(chunk)
        return json.loads(response)


def start_fake_app_server(
    root: pathlib.Path,
    scenario: str,
    codex_home: pathlib.Path,
    transcript: pathlib.Path | None = None,
) -> tuple[subprocess.Popen[bytes], pathlib.Path]:
    """Run the shared ADR-014 fake app-server and wait for its socket."""
    # AF_UNIX paths are ~104 bytes, so the socket lives beside /tmp rather than
    # inside the deeply nested test root.
    socket_root = pathlib.Path(f"/tmp/dg-fake-{os.getuid()}-{os.getpid()}")
    if not socket_root.exists():
        private_directory(socket_root)
    socket_path = socket_root / f"{root.name[-8:]}.sock"
    read_fd, write_fd = os.pipe()
    os.set_inheritable(write_fd, True)
    process = subprocess.Popen(
        [
            sys.executable,
            str(FAKE_APP_SERVER),
            "--socket",
            str(socket_path),
            "--scenario",
            str(FAKE_APP_SERVER / "scenarios" / scenario),
            "--bind",
            f"codex_home={codex_home}",
            "--ready-fd",
            str(write_fd),
            *(("--transcript", str(transcript)) if transcript is not None else ()),
        ],
        pass_fds=(write_fd,),
    )
    os.close(write_fd)
    with os.fdopen(read_fd, "rb") as ready:
        readable, _, _ = select.select([ready], [], [], 10)
        if not readable or not ready.readline():
            process.kill()
            raise TimeoutError("fake app-server did not bind its socket")
    return process, socket_path


def stop_fake_app_server(process: subprocess.Popen[bytes], socket_path: pathlib.Path) -> None:
    process.terminate()
    process.wait(timeout=10)
    socket_path.unlink(missing_ok=True)


def session_document(
    socket_path: pathlib.Path, codex_home: pathlib.Path, workspace: pathlib.Path
) -> dict[str, object]:
    return {
        "app_server_socket": str(socket_path),
        "canonical_codex_home": str(codex_home),
        "server_key": "44" * 32,
        "server_epoch": 1,
        "fixed_model": "gpt-5.6",
        "default_effort": "medium",
        "supported_efforts": ["low", "medium", "high"],
        "cwd": str(workspace),
        "developer_instructions": "Answer briefly.",
        "sandbox": "read-only",
        "approval_policy": "never",
        "artifact_root": str(workspace / "artifacts"),
        "attach": {"attach": "start"},
        "transport_timeout_seconds": 60,
    }


def validate(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(
        prefix="dolgorae-task004-validator-"
    ) as temporary:
        root = pathlib.Path(temporary)
        root.chmod(0o700)
        bootstrap, _credential = prepare(root, binary)
        process, channel = start(binary, bootstrap)
        bound = handoff(channel)
        ready = handoff(channel)
        channel.close()
        if bound["state"] != "bound" or ready["state"] != "ready":
            raise AssertionError(f"wrong startup order: {bound!r}, {ready!r}")
        if bound["record"] != ready["record"]:
            raise AssertionError("bound and ready described different worker identities")
        record = ready["record"]
        if not isinstance(record, dict):
            raise AssertionError("ready record was not an object")
        identity = record["identity"]
        if not isinstance(identity, dict):
            raise AssertionError("worker identity was not an object")
        if identity["pid"] != process.pid or identity["process_group_id"] != process.pid:
            raise AssertionError("worker did not retain its detached PID/PGID identity")
        socket_path = pathlib.Path(str(record["socket_path"]))
        if socket_path.parent != pathlib.Path(f"/tmp/dolgorae-{os.getuid()}/s"):
            raise AssertionError(f"worker socket followed TMPDIR: {socket_path}")
        if stat.S_IMODE(socket_path.stat().st_mode) != 0o600:
            raise AssertionError("worker socket mode is not 0600")
        for _ in range(2):
            status = call(socket_path, "status", identity)
            if status["result"] != "status" or status["active_turn"] is not None:
                raise AssertionError(f"reconnection status failed: {status!r}")
        foreign = dict(identity)
        foreign["run_id"] = "018f22e2-79b0-7cc3-a24c-78c48f28bbd0"
        rejected = call(socket_path, "status", foreign)
        expected_rejection = {
            "result": "rejected",
            "code": "DOLGORAE_PROTOCOL_MISMATCH",
        }
        if rejected != expected_rejection:
            raise AssertionError(f"cross-run request was not rejected: {rejected!r}")
        shutdown = call(socket_path, "shutdown", identity)
        if shutdown["result"] != "shutdown" or not shutdown["terminal_confirmed"]:
            raise AssertionError(f"worker shutdown failed: {shutdown!r}")
        if process.wait(timeout=5) != 0:
            raise AssertionError("worker did not exit cleanly")
        if socket_path.exists():
            raise AssertionError("worker left its verified socket behind")
        lock_path = root / "runtime" / "locks" / "startup" / f"{RUN_ID}.lock"
        if lock_path.stat().st_size != 8192 or any(lock_path.read_bytes()):
            raise AssertionError("worker did not clear its fixed startup owner slots")

        process, channel = start(binary, bootstrap)
        handoff(channel)
        handoff(channel)
        channel.close()
        os.kill(process.pid, 15)
        if process.wait(timeout=5) != 0:
            raise AssertionError("SIGTERM did not use the clean worker shutdown path")


def validate_session(binary: pathlib.Path) -> None:
    """Drive a whole read-only Run through the worker's own app-server session."""
    with tempfile.TemporaryDirectory(prefix="dolgorae-epic002-session-") as temporary:
        root = pathlib.Path(temporary)
        root.chmod(0o700)
        codex_home = root / "codex-home"
        workspace = root / "workspace"
        private_directory(codex_home)
        private_directory(workspace)
        workers: list[WorkerProcess] = []
        fake, socket_path = start_fake_app_server(
            root, "multi_turn_read_only.json", codex_home
        )
        try:
            run_id = fresh_run_id()
            session = session_document(socket_path, codex_home, workspace)
            session["approval_policy"] = "on-request"
            bootstrap, credential = prepare(root, binary, run_id, session)
            # A second, well-formed credential this Run was never bound to.
            stranger = root / "stranger.json"
            mint_controller(binary, stranger)
            process, channel = start(binary, bootstrap)
            handoff(channel)
            ready = handoff(channel)
            channel.close()
            workers.append(process)
            record = ready["record"]
            assert isinstance(record, dict)
            identity = record["identity"]
            assert isinstance(identity, dict)
            control = pathlib.Path(str(record["socket_path"]))
            owner_fd = os.open(str(credential), os.O_RDONLY)
            stranger_fd = os.open(str(stranger), os.O_RDONLY)

            idle = call(control, "status", identity)
            if idle["lifecycle"] != "idle" or idle["active_turn"] is not None:
                raise AssertionError(f"worker did not open an idle session: {idle!r}")

            # A direct socket caller that presents no descriptor, and one that
            # presents a credential this Run is not bound to, are both refused
            # before any Turn starts.
            for label, refused_fd in (("absent", None), ("foreign", stranger_fd)):
                refusal = call(
                    control,
                    "send",
                    identity,
                    timeout=30,
                    credential_fd=refused_fd,
                    request={
                        "message": "unauthorised question",
                        "idempotency_key": f"unauthorised-{label}",
                    },
                )
                if (
                    refusal["result"] != "failed"
                    or refusal["code"] != "CONTROLLER_MISMATCH"
                ):
                    raise AssertionError(
                        f"{label} credential was not refused: {refusal!r}"
                    )
            unauthorised = call(control, "status", identity)
            if unauthorised["active_turn"] is not None:
                raise AssertionError(
                    f"a refused mutation still started a Turn: {unauthorised!r}"
                )

            first = call(
                control,
                "send",
                identity,
                timeout=30,
                credential_fd=owner_fd,
                request={
                    "message": "first question",
                    "idempotency_key": "turn-one",
                },
            )
            if first["result"] != "terminal":
                raise AssertionError(f"send did not reach a terminal turn: {first!r}")
            terminal = first["terminal"]
            if terminal["thread_id"] != "thread-alpha" or terminal["turn_id"] != "turn-1":
                raise AssertionError(f"turn identity drifted: {terminal!r}")
            if terminal["final_response"] != {"kind": "inline", "text": "first answer"}:
                raise AssertionError(f"wrong final response: {terminal!r}")
            if terminal["usage"] is None:
                raise AssertionError("usage was not captured")

            replay = call(
                control,
                "send",
                identity,
                timeout=30,
                credential_fd=owner_fd,
                request={
                    "message": "first question",
                    "idempotency_key": "turn-one",
                },
            )
            if replay["result"] != "terminal" or replay["terminal"] != terminal:
                raise AssertionError(f"idempotent replay changed its answer: {replay!r}")

            waiting = call(
                control,
                "send",
                identity,
                timeout=30,
                credential_fd=owner_fd,
                request={
                    "message": "second question",
                    "idempotency_key": "turn-two",
                },
            )
            if waiting["result"] != "waiting_interaction":
                raise AssertionError(f"approval did not pause the turn: {waiting!r}")
            requests = waiting["requests"]
            if len(requests) != 1 or requests[0]["request_id"] != 7001:
                raise AssertionError(f"interaction was not surfaced: {waiting!r}")
            surfaced = json.dumps(requests[0])
            if "git" in surfaced or "status" in surfaced:
                raise AssertionError("interaction payload leaked through the control frame")
            if len(requests[0]["payload_sha256"]) != 64 or requests[0]["byte_length"] <= 0:
                raise AssertionError(f"interaction was not summarised: {requests[0]!r}")

            denied = call(
                control,
                "respond",
                identity,
                credential_fd=stranger_fd,
                request_id=7001,
                idempotency_key="approval-foreign",
                response={"decision": "accept_once"},
            )
            if denied["result"] != "failed" or denied["code"] != "CONTROLLER_MISMATCH":
                raise AssertionError(
                    f"a foreign credential answered an interaction: {denied!r}"
                )
            answered = call(
                control,
                "respond",
                identity,
                credential_fd=owner_fd,
                request_id=7001,
                idempotency_key="approval-owner",
                response={"decision": "accept_once"},
            )
            if answered["result"] != "responded":
                raise AssertionError(f"interaction response failed: {answered!r}")

            # SPEC-006: `wait` names the Turn it is rejoining.
            second = call(control, "wait", identity, timeout=30, turn_id="turn-2")
            if second["result"] != "terminal" or second["terminal"]["turn_id"] != "turn-2":
                raise AssertionError(f"wait did not resume the paused turn: {second!r}")
            if second["terminal"]["thread_id"] != "thread-alpha":
                raise AssertionError("the Run did not preserve one thread across turns")

            accepted = call(
                control,
                "submit",
                identity,
                timeout=30,
                credential_fd=owner_fd,
                request={"message": "third question", "idempotency_key": "turn-three"},
            )
            if accepted["result"] != "accepted" or accepted["accepted"]["turn_id"] != "turn-3":
                raise AssertionError(f"submit did not return on acceptance: {accepted!r}")

            # Observers must answer while that submitted turn is still running.
            running = call(control, "status", identity)
            if running["lifecycle"] != "running" or running["active_turn"] != "turn-3":
                raise AssertionError(f"status did not report the active turn: {running!r}")
            events = call(control, "events", identity, after=0, projection="operational", limit=8)
            if events["result"] != "events" or not str(events["next_cursor"]).isdigit():
                raise AssertionError(f"observer event page is malformed: {events!r}")

            interrupted = call(
                control, "interrupt", identity, timeout=30, credential_fd=owner_fd
            )
            if interrupted["result"] != "interrupted":
                raise AssertionError(f"interrupt failed: {interrupted!r}")
            final = call(control, "wait", identity, timeout=30, turn_id="turn-3")
            if final["result"] != "terminal" or final["terminal"]["status"] != "interrupted":
                raise AssertionError(f"interrupted turn did not settle: {final!r}")
            # A Turn this Run never had is refused rather than answered with
            # whichever Turn the Run happens to remember.
            absent = call(control, "wait", identity, timeout=30, turn_id="turn-absent")
            if absent["result"] != "failed" or absent["code"] != "TURN_NOT_FOUND":
                raise AssertionError(f"an absent turn was not refused: {absent!r}")
            # The settled Turn is what `run_status` reports as the last
            # terminal, which is where an exit-7 caller reads its response.
            # SPEC-007 freezes `status` byte-identically across builds, so the
            # wider answer rides the ordinary skew-checked request instead and
            # the frozen one stays exactly its v1 self.
            frozen_status = call(control, "status", identity)
            if "last_terminal" in frozen_status:
                raise AssertionError(f"the frozen status answer grew a member: {frozen_status!r}")
            settled_status = call(control, "run_status", identity)
            last_terminal = settled_status["last_terminal"]
            if not isinstance(last_terminal, dict) or last_terminal["turn_id"] != "turn-3":
                raise AssertionError(f"status lost the last terminal: {settled_status!r}")
            if last_terminal["status"] != "interrupted":
                raise AssertionError(f"status misreported the last terminal: {last_terminal!r}")

            # The Run is idle by now, so `close` needs no interrupt authority.
            closed = call(control, "close", identity, credential_fd=owner_fd, interrupt=False)
            if closed["result"] != "closed" or closed["thread_id"] != "thread-alpha":
                raise AssertionError(f"close did not report the owned thread: {closed!r}")
            refused = call(
                control,
                "send",
                identity,
                credential_fd=owner_fd,
                request={"message": "after close", "idempotency_key": "turn-four"},
            )
            if refused["result"] != "failed":
                raise AssertionError(f"a closed Run accepted a new turn: {refused!r}")
            os.close(owner_fd)
            os.close(stranger_fd)

            shutdown = call(control, "shutdown", identity)
            if shutdown["result"] != "shutdown":
                raise AssertionError(f"session worker shutdown failed: {shutdown!r}")
            if process.wait(timeout=10) != 0:
                raise AssertionError("session worker did not exit cleanly")
            ledger = root / "runs" / run_id / "audit.jsonl"
            kinds = [
                json.loads(line)["kind"]
                for line in ledger.read_text(encoding="utf-8").splitlines()
                if line
            ]
            for required in (
                "idempotency_reserved",
                "thread_bound",
                "turn_started",
                "turn_terminal",
                "interaction_opened",
                "interaction_resolved",
            ):
                if required not in kinds:
                    raise AssertionError(f"durable ledger is missing {required}: {kinds!r}")
        finally:
            for worker in workers:
                try:
                    if os.waitpid(worker.pid, os.WNOHANG) == (0, 0):
                        os.kill(worker.pid, 15)
                        worker.wait(timeout=10)
                except (ChildProcessError, ProcessLookupError):
                    pass
            stop_fake_app_server(fake, socket_path)


def validate_foreign_thread(binary: pathlib.Path) -> None:
    """A shared app-server's other Threads must not cost this Run its outcome.

    The Run's own outcome survives, this connection never answers into the
    other Thread, and the observation is recorded in the Runtime Profile's
    durable diagnostic journal rather than in the Run's ledger.
    """
    with tempfile.TemporaryDirectory(prefix="dolgorae-epic002-foreign-") as temporary:
        enclosing = pathlib.Path(temporary)
        enclosing.chmod(0o700)
        # A faithful durable layout: the worker locates its Runtime Profile's
        # diagnostic journal from the Dolgorae home above the
        # workspace state root, so the state root has to sit where a real one
        # does.
        dolgorae_home = enclosing / "dolgorae-home"
        root = dolgorae_home / "workspaces" / WORKSPACE_ID
        private_directory(root)
        codex_home = enclosing / "codex-home"
        workspace = enclosing / "workspace"
        transcript = enclosing / "client-messages.jsonl"
        private_directory(codex_home)
        private_directory(workspace)
        workers: list[WorkerProcess] = []
        fake, socket_path = start_fake_app_server(
            root, "foreign_thread_request.json", codex_home, transcript
        )
        try:
            run_id = fresh_run_id()
            bootstrap, credential = prepare(
                root,
                binary,
                run_id,
                session_document(socket_path, codex_home, workspace),
            )
            process, channel = start(binary, bootstrap)
            handoff(channel)
            ready = handoff(channel)
            channel.close()
            workers.append(process)
            record = ready["record"]
            assert isinstance(record, dict)
            identity = record["identity"]
            assert isinstance(identity, dict)
            control = pathlib.Path(str(record["socket_path"]))
            owner_fd = os.open(str(credential), os.O_RDONLY)
            answered = call(
                control,
                "send",
                identity,
                timeout=30,
                credential_fd=owner_fd,
                request={"message": "own question", "idempotency_key": "own-turn"},
            )
            if answered["result"] != "terminal":
                raise AssertionError(
                    f"a foreign-thread request cost this Run its outcome: {answered!r}"
                )
            if answered["terminal"]["final_response"] != {
                "kind": "inline",
                "text": "own answer",
            }:
                raise AssertionError(f"wrong final response: {answered!r}")
            os.close(owner_fd)
            call(control, "shutdown", identity)
            process.wait(timeout=10)

            # Nothing was written back into the foreign Thread's pending
            # request. The transcript is proven live by this Run's own
            # requests appearing in it.
            sent = [
                json.loads(line)
                for line in transcript.read_text(encoding="utf-8").splitlines()
                if line.strip()
            ]
            methods = {message.get("method") for message in sent}
            if not {"initialize", "thread/start", "turn/start"} <= methods:
                raise AssertionError(f"the client transcript is not live: {methods!r}")
            answers = [
                message
                for message in sent
                if "method" not in message and message.get("id") == 9101
            ]
            if answers:
                raise AssertionError(
                    f"this connection answered a foreign Thread's request: {answers!r}"
                )

            # The observation is durable, in the Runtime Profile's journal
            # rather than the Run ledger, and carries no request payload.
            journal = (
                dolgorae_home / "profiles" / ("44" * 32) / "diagnostics.jsonl"
            )
            if not journal.exists():
                raise AssertionError("no profile diagnostic journal was written")
            records = [
                json.loads(line)
                for line in journal.read_text(encoding="utf-8").splitlines()
                if line.strip()
            ]
            ignored = [
                record
                for record in records
                if record.get("kind") == "foreign_thread_request_ignored"
            ]
            if len(ignored) != 1:
                raise AssertionError(f"expected one foreign observation: {records!r}")
            details = ignored[0]["details"]
            if (
                details["request_id"] != 9101
                or details["thread_id"] != "thread-foreign"
                or details["turn_id"] != "turn-foreign"
                or ignored[0]["projection"] != "operational"
            ):
                raise AssertionError(f"wrong foreign observation: {ignored[0]!r}")
            if "rm" in json.dumps(ignored[0]) or "command" in details:
                raise AssertionError(
                    f"the foreign request payload leaked: {ignored[0]!r}"
                )
            ledger = json.dumps(
                [
                    json.loads(line)
                    for line in (root / "runs" / run_id / "audit.jsonl")
                    .read_text(encoding="utf-8")
                    .splitlines()
                    if line.strip()
                ]
            )
            if "thread-foreign" in ledger:
                raise AssertionError(
                    "a foreign-thread diagnostic must never be a Run event"
                )
        finally:
            for worker in workers:
                try:
                    if os.waitpid(worker.pid, os.WNOHANG) == (0, 0):
                        os.kill(worker.pid, 15)
                        worker.wait(timeout=10)
                except (ChildProcessError, ProcessLookupError):
                    pass
            stop_fake_app_server(fake, socket_path)


def validate_shutdown_interrupt(binary: pathlib.Path) -> None:
    """SIGTERM during an active Turn must interrupt it and prove the outcome.

    docs/specs/README.md: "If worker `SIGTERM` arrives during an active turn, it sends
    `turn/interrupt`, waits up to five seconds for a terminal event, fsyncs
    terminal evidence when observed, and records `outcome_unknown` on expiry
    before generation cleanup."  A worker that instead reported itself idle
    would leave a live Codex turn behind and nothing durable to recover from.
    """
    with tempfile.TemporaryDirectory(prefix="dolgorae-epic002-sigterm-") as temporary:
        root = pathlib.Path(temporary)
        root.chmod(0o700)
        codex_home = root / "codex-home"
        workspace = root / "workspace"
        transcript = root / "client-messages.jsonl"
        private_directory(codex_home)
        private_directory(workspace)
        workers: list[WorkerProcess] = []
        fake, socket_path = start_fake_app_server(
            root, "shutdown_interrupt.json", codex_home, transcript
        )
        try:
            run_id = fresh_run_id()
            bootstrap, credential = prepare(
                root,
                binary,
                run_id,
                session_document(socket_path, codex_home, workspace),
            )
            process, channel = start(binary, bootstrap)
            handoff(channel)
            ready = handoff(channel)
            channel.close()
            workers.append(process)
            record = ready["record"]
            assert isinstance(record, dict)
            identity = record["identity"]
            assert isinstance(identity, dict)
            control = pathlib.Path(str(record["socket_path"]))

            owner_fd = os.open(str(credential), os.O_RDONLY)
            accepted = call(
                control,
                "submit",
                identity,
                timeout=30,
                credential_fd=owner_fd,
                request={"message": "long question", "idempotency_key": "sigterm-turn"},
            )
            os.close(owner_fd)
            if accepted["result"] != "accepted":
                raise AssertionError(f"the turn was not accepted: {accepted!r}")
            running = call(control, "status", identity)
            if running["lifecycle"] != "running" or running["active_turn"] != "turn-1":
                raise AssertionError(f"the turn is not live before SIGTERM: {running!r}")

            os.kill(process.pid, signal.SIGTERM)
            if process.wait(timeout=20) != 0:
                raise AssertionError("worker did not exit cleanly after SIGTERM")

            sent = [
                json.loads(line)
                for line in transcript.read_text(encoding="utf-8").splitlines()
                if line.strip()
            ]
            interrupts = [
                message
                for message in sent
                if message.get("method") == "turn/interrupt"
            ]
            if len(interrupts) != 1:
                raise AssertionError(f"SIGTERM did not interrupt the live turn: {sent!r}")
            if interrupts[0].get("params", {}).get("turnId") != "turn-1":
                raise AssertionError(f"the interrupt named the wrong turn: {interrupts[0]!r}")

            ledger = root / "runs" / run_id / "audit.jsonl"
            records = [
                json.loads(line)
                for line in ledger.read_text(encoding="utf-8").splitlines()
                if line
            ]
            terminals = [
                record for record in records if record["kind"] == "turn_terminal"
            ]
            if len(terminals) != 1 or terminals[0]["payload"]["status"] != "interrupted":
                raise AssertionError(
                    f"the observed terminal was not fsynced before exit: {records!r}"
                )
            if any(record["kind"] == "outcome_unknown" for record in records):
                raise AssertionError(
                    f"an observed terminal was still recorded as unknown: {records!r}"
                )
        finally:
            for worker in workers:
                try:
                    if os.waitpid(worker.pid, os.WNOHANG) == (0, 0):
                        os.kill(worker.pid, 15)
                        worker.wait(timeout=10)
                except (ChildProcessError, ProcessLookupError):
                    pass
            stop_fake_app_server(fake, socket_path)


def validate_streamed_history(binary: pathlib.Path) -> None:
    """Read a terminal Turn back from a Thread history larger than any message bound."""
    with tempfile.TemporaryDirectory(prefix="dolgorae-epic002-streamed-") as temporary:
        root = pathlib.Path(temporary)
        root.chmod(0o700)
        codex_home = root / "codex-home"
        workspace = root / "workspace"
        private_directory(codex_home)
        private_directory(workspace)
        workers: list[WorkerProcess] = []
        fake, socket_path = start_fake_app_server(
            root, "streamed_thread_read.json", codex_home
        )
        try:
            run_id = fresh_run_id()
            bootstrap, credential = prepare(
                root,
                binary,
                run_id,
                session_document(socket_path, codex_home, workspace),
            )
            process, channel = start(binary, bootstrap)
            handoff(channel)
            ready = handoff(channel)
            channel.close()
            workers.append(process)
            record = ready["record"]
            assert isinstance(record, dict)
            identity = record["identity"]
            assert isinstance(identity, dict)
            control = pathlib.Path(str(record["socket_path"]))
            owner_fd = os.open(str(credential), os.O_RDONLY)
            answered = call(
                control,
                "send",
                identity,
                timeout=120,
                credential_fd=owner_fd,
                request={"message": "large history", "idempotency_key": "streamed"},
            )
            if answered["result"] != "terminal":
                raise AssertionError(f"streamed history read failed: {answered!r}")
            response = answered["terminal"]["final_response"]
            if response["kind"] != "inline" or response["text"] != "t" * 4096:
                raise AssertionError(
                    "the wanted Turn was not recovered from the streamed history"
                )
            os.close(owner_fd)
            call(control, "shutdown", identity)
            process.wait(timeout=10)
        finally:
            for worker in workers:
                try:
                    if os.waitpid(worker.pid, os.WNOHANG) == (0, 0):
                        os.kill(worker.pid, 15)
                        worker.wait(timeout=10)
                except (ChildProcessError, ProcessLookupError):
                    pass
            stop_fake_app_server(fake, socket_path)


def machine(
    binary: pathlib.Path,
    *arguments: str,
    fds: tuple[int, ...] = (),
    stdin: str | None = None,
    environment: dict[str, str] | None = None,
) -> tuple[int, list[dict[str, object]]]:
    """Run the real argv and decode every machine object it printed.

    `run events` is the one family that emits more than one object, so the
    reader is line-oriented for every command rather than special-casing it.
    `stdin` is a real pipe, which is what makes a non-TTY stdin body a fact
    about the shipped CLI rather than about this harness. `environment`
    overlays the caller's own environment, which a case needs when it must own
    a `HOME`-scoped singleton — the operator credential — instead of sharing
    the one the rest of the suite already initialized.
    """
    completed = subprocess.run(
        [str(binary), *arguments],
        capture_output=True,
        text=True,
        check=False,
        pass_fds=fds,
        input="" if stdin is None else stdin,
        env=None if environment is None else {**os.environ, **environment},
    )
    if completed.stderr:
        raise AssertionError(f"machine mode wrote to stderr for {arguments}: {completed.stderr!r}")
    if completed.stdout and not completed.stdout.endswith("\n"):
        raise AssertionError(f"machine output lacks a final LF for {arguments}")
    objects = [
        json.loads(line) for line in completed.stdout.splitlines() if line.strip()
    ]
    return completed.returncode, objects


def binary_sha256(binary: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with binary.open("rb") as handle:
        for chunk in iter(lambda: handle.read(65536), b""):
            digest.update(chunk)
    return digest.hexdigest()


def binary_version(binary: pathlib.Path) -> str:
    completed = subprocess.run(
        [str(binary), "version", "--json"], capture_output=True, text=True, check=True
    )
    version = str(json.loads(completed.stdout)["version"])
    return version.removeprefix("v")


def publish_run_for_cli(
    binary: pathlib.Path,
    state_root: pathlib.Path,
    workspace_id: str,
    run_id: str,
    session: dict[str, object] | None,
    *,
    build_skew: bool,
) -> tuple[pathlib.Path, pathlib.Path]:
    """Publish one Run where the real `dolgorae run` adapter looks for it.

    `prepare` publishes under a private root the test owns; the argv path
    resolves its own state root from HOME and the workspace digest, so the Run
    has to exist there instead.  `build_skew` fabricates a worker from another
    Dolgorae build, which is the only way to observe the ordinary-request
    version check the argv path performs.
    """
    ledger_root = state_root / "runs" / run_id
    runtime_runs = state_root / "runtime" / "runs"
    startup = state_root / "runtime" / "locks" / "startup"
    for directory in (
        state_root,
        state_root / "runs",
        ledger_root,
        ledger_root / "recovery",
        state_root / "runtime",
        state_root / "runtime" / "locks",
        runtime_runs,
        startup,
    ):
        if not directory.exists():
            directory.mkdir(mode=0o700, parents=True)
        directory.chmod(0o700)
    audit = ledger_root / "audit.jsonl"
    audit.touch(mode=0o600)
    audit.chmod(0o600)
    credential = state_root / f"controller-{run_id}.json"
    controller = mint_controller(binary, credential)
    manifest = run_manifest(run_id, controller)
    manifest["workspace_id"] = workspace_id
    write_private(ledger_root / "manifest.json", jcs(manifest))
    write_private(ledger_root / "controller.json", jcs(controller))
    bootstrap = runtime_runs / f"{run_id}.bootstrap.json"
    document: dict[str, object] = {
        "schema_version": 1,
        "workspace_id": workspace_id,
        "run_id": run_id,
        "run_generation": 1,
        "boot_uuid": BOOT_UUID,
        # Startup always binds the actual executable. A skew fixture changes
        # only the advertised build version after that identity proof.
        "executable_sha256": binary_sha256(binary),
        "executable_path_sha256": "33" * 32,
        "dolgorae_version": "0.0.0-other" if build_skew else binary_version(binary),
        "mutation_protocol_version": 1,
        "control_socket_epoch": 1,
        "profile": "default",
        "state_root": str(state_root),
        "ledger_root": str(ledger_root),
        "runtime_record_path": str(runtime_runs / f"{run_id}.json"),
        "startup_lock_path": str(startup / f"{run_id}.lock"),
    }
    if session is not None:
        session = dict(session)
        session.setdefault("controller_id", controller["identity"]["controller_id"])
        session.setdefault("control_mode", "managed_agent")
        document["session"] = session
    bootstrap.write_text(
        json.dumps(document, separators=(",", ":")) + "\n", encoding="utf-8"
    )
    bootstrap.chmod(0o600)
    return bootstrap, credential


def validate_run_cli(binary: pathlib.Path) -> None:
    """Drive the Machine CLI's own `run` verbs against a live worker.

    Nothing here speaks the control socket: every request goes through the real
    argv, so the runtime-record discovery, the SCM_RIGHTS credential send, and
    the published envelope shapes are all executed rather than assumed.
    """
    machine_schema = validator(pathlib.Path("docs/protocol").resolve(), "dolgorae-machine-v1.schema.json")
    home = pathlib.Path(os.environ["HOME"])
    state_home = home / ".dolgorae" / "workspaces"
    with tempfile.TemporaryDirectory(prefix="dolgorae-epic002-runcli-") as temporary:
        root = pathlib.Path(temporary)
        root.chmod(0o700)
        codex_home = root / "codex-home"
        workspace = root / "workspace"
        private_directory(codex_home)
        private_directory(workspace)
        status, objects = machine(binary, "init", str(workspace), "--non-git")
        if status != 0 or len(objects) != 1:
            raise AssertionError(f"init did not publish one workspace: {status} {objects!r}")
        assert_valid(objects[0], machine_schema, "init envelope")
        workspace_id = str(objects[0]["data"]["workspace_id"])  # type: ignore[index]
        state_root = state_home / workspace_id

        workers: list[WorkerProcess] = []
        transcript = root / "transcript.jsonl"
        fake, socket_path = start_fake_app_server(
            root, "multi_turn_read_only.json", codex_home, transcript
        )
        try:
            run_id = fresh_run_id()
            session = session_document(socket_path, codex_home, workspace)
            session["approval_policy"] = "on-request"
            bootstrap, credential = publish_run_for_cli(
                binary,
                state_root,
                workspace_id,
                run_id,
                session,
                build_skew=False,
            )
            stranger = root / "stranger.json"
            mint_controller(binary, stranger)
            process, channel = start(binary, bootstrap)
            handoff(channel)
            handoff(channel)
            channel.close()
            workers.append(process)

            owned = ["--workspace", str(workspace)]
            controlled = ["run", "--controller-file", str(credential)]
            observed = ["run"]

            def call(
                argv: list[str],
                *,
                expect: int = 0,
                fds: tuple[int, ...] = (),
                stdin: str | None = None,
            ) -> list[dict[str, object]]:
                status, objects = machine(binary, *argv, fds=fds, stdin=stdin)
                if status != expect:
                    raise AssertionError(
                        f"unexpected exit {status} (wanted {expect}) for {argv}: {objects!r}"
                    )
                if not objects:
                    raise AssertionError(f"no machine object for {argv}")
                for instance in objects:
                    assert_valid(instance, machine_schema, f"envelope for {argv}")
                return objects

            def data(argv: list[str], **kwargs: object) -> dict[str, object]:
                objects = call(argv, **kwargs)  # type: ignore[arg-type]
                if len(objects) != 1:
                    raise AssertionError(f"{argv} emitted {len(objects)} objects")
                if objects[0]["ok"] is not True:
                    raise AssertionError(f"{argv} failed: {objects[0]!r}")
                return dict(objects[0]["data"])  # type: ignore[arg-type]

            run = data([*observed, "status", run_id, *owned])
            if run["run_id"] != run_id or run["execution_lane"] != "shared_readonly":
                raise AssertionError(f"run status did not describe the Run: {run!r}")
            if run["profile"] != "default" or run["model"] != "gpt-5.6":
                raise AssertionError(f"run status lost its pinned identity: {run!r}")
            # No Turn has run, so the Run reports the durable projection it
            # actually has rather than the worker's momentary control state.
            if run["thread_id"] is not None or run["active_turn_id"] is not None:
                raise AssertionError(f"an unstarted Run claimed a Thread: {run!r}")

            turn = data(
                [
                    *controlled,
                    "send",
                    run_id,
                    *owned,
                    "--message",
                    "first question",
                    "--idempotency-key",
                    "cli-turn-one",
                ]
            )
            if turn["turn_id"] != "turn-1" or turn["status"] != "completed":
                raise AssertionError(f"run send did not answer with its terminal turn: {turn!r}")
            if turn["final_response"] != {"kind": "inline", "text": "first answer"}:
                raise AssertionError(f"run send lost the final response: {turn!r}")
            if turn["workspace_changes"]["attribution"] != "unverified":  # type: ignore[index]
                raise AssertionError(f"a read-only turn claimed measured changes: {turn!r}")

            # docs/specs/README.md: "`send` and `submit` accept exactly one text source:
            # `--message` or stdin ... If `--message` is absent, stdin is
            # required and MUST NOT be a TTY."  This turn names no message at
            # all, so starting one at all proves the piped bytes were read, and
            # the fixture's transcript proves they are what app-server received.
            waiting = data(
                [
                    *controlled,
                    "send",
                    run_id,
                    *owned,
                    "--idempotency-key",
                    "cli-turn-two",
                ],
                stdin="second question",
            )
            if waiting["status"] != "waiting_interaction" or waiting["turn_id"] != "turn-2":
                raise AssertionError(f"the approval did not pause the turn: {waiting!r}")
            sent = [
                json.loads(line)
                for line in transcript.read_text(encoding="utf-8").splitlines()
                if line.strip()
            ]
            piped = [
                message
                for message in sent
                if message.get("method") == "turn/start"
                and "second question" in json.dumps(message.get("params"))
            ]
            if len(piped) != 1:
                raise AssertionError(f"the piped message never reached turn/start: {sent!r}")

            pending = data([*observed, "pending", run_id, *owned])
            if len(pending["items"]) != 1:  # type: ignore[arg-type]
                raise AssertionError(f"the pending interaction was not projected: {pending!r}")
            request_id = pending["items"][0]["request_id"]  # type: ignore[index]

            answer = root / "approval.json"
            answer.write_text(json.dumps({"decision": "accept_once"}), encoding="utf-8")
            answer_fd = os.open(str(answer), os.O_RDONLY)
            os.set_inheritable(answer_fd, True)
            try:
                resumed = data(
                    [
                        *controlled,
                        "respond",
                        run_id,
                        *owned,
                        "--request-id",
                        str(request_id),
                        "--idempotency-key",
                        "cli-approval-one",
                        "--response-fd",
                        str(answer_fd),
                    ],
                    fds=(answer_fd,),
                )
            finally:
                os.close(answer_fd)
            if resumed["run_id"] != run_id:
                raise AssertionError(f"run respond did not answer with the Run: {resumed!r}")

            # docs/specs/README.md: "`respond` accepts a JSON body only from exactly one
            # protected inherited `--response-fd` or non-TTY stdin".  The body
            # below travels on a pipe with no descriptor named at all, so
            # reaching the Run's interaction table proves stdin was read.
            piped = call(
                [
                    *controlled,
                    "respond",
                    run_id,
                    *owned,
                    "--request-id",
                    fresh_run_id(),
                    "--idempotency-key",
                    "cli-approval-stdin",
                ],
                expect=3,
                stdin=json.dumps({"decision": "accept_once"}),
            )
            error = piped[0]["error"]  # type: ignore[index]
            if error["code"] != "INTERACTION_NOT_FOUND":  # type: ignore[index]
                raise AssertionError(f"a stdin response body was not accepted: {piped!r}")

            # SPEC-006 argv: `run wait <run-id> <turn-id>`.
            settled = data([*observed, "wait", run_id, "turn-2", *owned])
            if settled["turn_id"] != "turn-2" or settled["status"] != "completed":
                raise AssertionError(f"run wait did not resume the paused turn: {settled!r}")

            accepted = data(
                [
                    *controlled,
                    "submit",
                    run_id,
                    *owned,
                    "--message",
                    "third question",
                    "--idempotency-key",
                    "cli-turn-three",
                ]
            )
            if accepted["turn_id"] != "turn-3" or accepted["status"] != "accepted":
                raise AssertionError(f"run submit did not return on acceptance: {accepted!r}")

            # SPEC-006: "A caller-supplied timeout returns the current
            # nonterminal state without interrupting the worker", at exit 0.
            still_running = data(
                [*observed, "wait", run_id, "turn-3", *owned, "--timeout", "500ms"]
            )
            if still_running["turn_id"] != "turn-3" or still_running["status"] != "running":
                raise AssertionError(f"a caller timeout did not report the live turn: {still_running!r}")
            live = data([*observed, "status", run_id, *owned])
            if live["active_turn_id"] != "turn-3" or live["state"] != "running":
                raise AssertionError(f"a caller timeout disturbed the worker: {live!r}")

            # SPEC-006: one object per durable record through the head captured
            # at command start, then exactly one `end` frame.  This slice writes
            # no client-event audit records yet, so the stream is legitimately
            # the terminator alone; the shape of both frames is still checked.
            pages = call([*observed, "events", run_id, *owned, "--after", "0"])
            tail = pages[-1]["data"]
            if not isinstance(tail, dict) or tail.get("kind") != "end":
                raise AssertionError(f"run events did not terminate with an end frame: {pages[-1]!r}")
            if not str(tail["cursor"]).isdigit():
                raise AssertionError(f"the end frame carried no ledger cursor: {tail!r}")
            for page in pages[:-1]:
                record = page["data"]
                if not isinstance(record, dict) or record.get("kind") != "event":
                    raise AssertionError(f"run events emitted a non-delivery object: {page!r}")
                if "record" not in record:
                    raise AssertionError(f"an event delivery carried no record: {page!r}")

            interrupted = data([*controlled, "interrupt", run_id, *owned])
            if interrupted["run_id"] != run_id:
                raise AssertionError(f"run interrupt did not answer with the Run: {interrupted!r}")
            # SPEC-006 error table: `run wait` on an interrupted terminal is
            # `TURN_INTERRUPTED`, exit 7, so a Master classifies the outcome by
            # exit class instead of parsing a success envelope.
            settled = call([*observed, "wait", run_id, "turn-3", *owned], expect=7)
            error = settled[0]["error"]  # type: ignore[index]
            if error["code"] != "TURN_INTERRUPTED":  # type: ignore[index]
                raise AssertionError(f"the interrupted turn did not exit 7: {settled!r}")
            if error["details"] != {  # type: ignore[index]
                "run_id": run_id,
                "turn_id": "turn-3",
                "status": "interrupted",
            }:
                raise AssertionError(f"the exit-7 refusal omitted its details: {error!r}")
            # The exit-7 envelope is intentionally minimal, so the response,
            # usage, and cursor are read from `run status.data.last_terminal`.
            after = data([*observed, "status", run_id, *owned])
            last_terminal = after["last_terminal"]
            if not isinstance(last_terminal, dict):
                raise AssertionError(f"run status published no last terminal: {after!r}")
            if last_terminal["turn_id"] != "turn-3" or last_terminal["status"] != "interrupted":
                raise AssertionError(f"run status lost the settled turn: {last_terminal!r}")
            if last_terminal["run_id"] != run_id or last_terminal["effort"] != "medium":
                raise AssertionError(f"the last terminal misreports its turn: {last_terminal!r}")
            # A Turn this Run never had is absent, not the newest one.
            missing = call([*observed, "wait", run_id, "turn-absent", *owned], expect=3)
            error = missing[0]["error"]  # type: ignore[index]
            if error["code"] != "TURN_NOT_FOUND":  # type: ignore[index]
                raise AssertionError(f"an absent turn was not refused: {missing!r}")

            # A well-formed credential this Run was never bound to is refused
            # with the contract's own details, not an empty object.
            refused = call(
                [
                    "run",
                    "--controller-file",
                    str(stranger),
                    "send",
                    run_id,
                    *owned,
                    "--message",
                    "unauthorised",
                    "--idempotency-key",
                    "cli-turn-stranger",
                ],
                expect=4,
            )
            error = refused[0]["error"]  # type: ignore[index]
            if error["code"] != "CONTROLLER_MISMATCH":  # type: ignore[index]
                raise AssertionError(f"a foreign credential was not refused: {refused!r}")
            if error["details"] != {"run_id": run_id, "operation": "run.send"}:  # type: ignore[index]
                raise AssertionError(f"the refusal omitted its checked details: {error!r}")

            # docs/specs/README.md: "Empty text is rejected."  From either source, and as a
            # refusal about the argument rather than about the Run.
            for argv, body in (
                ([*controlled, "send", run_id, *owned, "--message", "",
                  "--idempotency-key", "cli-turn-empty-argv"], None),
                ([*controlled, "send", run_id, *owned,
                  "--idempotency-key", "cli-turn-empty-stdin"], ""),
            ):
                empty = call(argv, expect=2, stdin=body)
                error = empty[0]["error"]  # type: ignore[index]
                if error["code"] != "INVALID_ARGUMENT":  # type: ignore[index]
                    raise AssertionError(f"empty turn text was accepted: {empty!r}")

            # docs/specs/README.md: "`SHARED_RUN_WRITE_FORBIDDEN` ... a `shared_readonly`
            # run requested write; a lineage-linked write continuation is
            # required."  Exit class 4, with the lane and the required action
            # the error contract fixes.
            for verb in ("send", "submit"):
                forbidden = call(
                    [
                        *controlled,
                        verb,
                        run_id,
                        *owned,
                        "--write",
                        "--message",
                        "write please",
                        "--idempotency-key",
                        f"cli-turn-write-{verb}",
                    ],
                    expect=4,
                )
                error = forbidden[0]["error"]  # type: ignore[index]
                if error["code"] != "SHARED_RUN_WRITE_FORBIDDEN":  # type: ignore[index]
                    raise AssertionError(f"run {verb} --write was accepted: {forbidden!r}")
                if error["details"] != {  # type: ignore[index]
                    "run_id": run_id,
                    "execution_lane": "shared_readonly",
                    "reason": "a shared_readonly run requested write",
                    "required_action": "create_dedicated_write_continuation",
                }:
                    raise AssertionError(f"the write refusal lost its contract: {error!r}")

            # SPEC-005 gives `--timeout` to `send` and `wait` alone: `submit`
            # returns on acceptance and has no nonterminal wait to bound.
            timed_submit = call(
                [
                    *controlled,
                    "submit",
                    run_id,
                    *owned,
                    "--message",
                    "bounded?",
                    "--idempotency-key",
                    "cli-turn-submit-timeout",
                    "--timeout",
                    "1s",
                ],
                expect=2,
            )
            error = timed_submit[0]["error"]  # type: ignore[index]
            if error["code"] != "INVALID_ARGUMENT":  # type: ignore[index]
                raise AssertionError(f"submit accepted a caller timeout: {timed_submit!r}")

            # docs/specs/README.md: "`<duration>` is a positive base-10 integer followed
            # immediately by `ms`, `s`, `m`, or `h`.  Fractions, compound
            # durations, zero, negative values, and values greater than 24
            # hours are rejected with `INVALID_ARGUMENT`."
            for duration in ("25h", "+5s", "-5s", "0s", "1.5s", "1h30m", "5"):
                refused_duration = call(
                    [*observed, "wait", run_id, "turn-2", *owned, "--timeout", duration],
                    expect=2,
                )
                error = refused_duration[0]["error"]  # type: ignore[index]
                if error["code"] != "INVALID_ARGUMENT":  # type: ignore[index]
                    raise AssertionError(f"--timeout {duration} was accepted: {refused_duration!r}")
            # The upper bound itself is inside the grammar, not outside it.
            bounded_wait = data(
                [*observed, "wait", run_id, "turn-2", *owned, "--timeout", "24h"]
            )
            if bounded_wait["turn_id"] != "turn-2" or bounded_wait["status"] != "completed":
                raise AssertionError(f"a 24h bound was not accepted: {bounded_wait!r}")

            closed = data([*controlled, "close", run_id, *owned])
            if closed["run_id"] != run_id:
                raise AssertionError(f"run close did not answer with the Run: {closed!r}")
            # The lifecycle verbs answer with a Run read from durable state, so
            # the Thread the Turns bound has to be visible in it.
            if closed["thread_id"] != "thread-alpha":
                raise AssertionError(f"run close lost the Run's Thread: {closed!r}")
            if closed["run_generation"] != 1 or closed["state"] not in {"idle", "closed"}:
                raise AssertionError(f"run close reported an unreal Run state: {closed!r}")

            # SPEC-004: an ordinary request must not cross a build upgrade,
            # while frozen control v1 still reaches the same worker.
            skewed_id = fresh_run_id()
            # No app-server session: the version check happens in the CLI before
            # the control socket is touched, and the shared fake already owns one
            # scripted conversation.
            skewed_bootstrap, skewed_credential = publish_run_for_cli(
                binary,
                state_root,
                workspace_id,
                skewed_id,
                None,
                build_skew=True,
            )
            skewed_process, skewed_channel = start(binary, skewed_bootstrap)
            handoff(skewed_channel)
            handoff(skewed_channel)
            skewed_channel.close()
            workers.append(skewed_process)
            for argv in (
                [*observed, "wait", skewed_id, "turn-1", *owned],
                [*observed, "events", skewed_id, *owned, "--after", "0"],
                [
                    "run",
                    "--controller-file",
                    str(skewed_credential),
                    "interrupt",
                    skewed_id,
                    *owned,
                ],
            ):
                mismatched = call(argv, expect=5)
                error = mismatched[0]["error"]  # type: ignore[index]
                if error["code"] != "DOLGORAE_PROTOCOL_MISMATCH":  # type: ignore[index]
                    raise AssertionError(f"{argv} crossed a build upgrade: {mismatched!r}")
                if error["details"]["control_v1_available"] is not True:  # type: ignore[index]
                    raise AssertionError(f"{argv} hid the frozen control surface: {error!r}")
            # Frozen control v1 stays reachable across exactly that skew.
            across = data([*observed, "status", skewed_id, *owned])
            if across["run_id"] != skewed_id:
                raise AssertionError(f"frozen control v1 stopped crossing a build upgrade: {across!r}")

            # SPEC-006: `--after` accepts the canonical unsigned decimal
            # without leading zeroes, and a noncanonical or beyond-head cursor
            # is `EVENT_CURSOR_INVALID` — never a bare argument complaint.
            head = str(data([*observed, "status", run_id, *owned])["event_cursor"])
            for cursor in ("007", "999999"):
                refused_cursor = call(
                    [*observed, "events", run_id, *owned, "--after", cursor], expect=2
                )
                error = refused_cursor[0]["error"]  # type: ignore[index]
                if error["code"] != "EVENT_CURSOR_INVALID":  # type: ignore[index]
                    raise AssertionError(f"--after {cursor} was not refused as a cursor: {refused_cursor!r}")
                if error["details"]["run_id"] != run_id:  # type: ignore[index]
                    raise AssertionError(f"the cursor refusal lost its Run: {error!r}")
                if not str(error["details"]["head_cursor"]).isdigit():  # type: ignore[index]
                    raise AssertionError(f"the cursor refusal named no head: {error!r}")
            del head

            # docs/specs/README.md: projection-only `status` and `events` read the fsynced
            # projection directly and MUST NOT fail because the identity
            # verdict is unverifiable.  The Run's worker is stopped here, so
            # every fact below comes from durable state alone.
            os.kill(workers[0].pid, 15)
            workers[0].wait(timeout=10)
            offline = data([*observed, "status", run_id, *owned])
            if offline["identity_verdict"] not in {"Unverifiable", "Absent"}:
                raise AssertionError(f"a stopped worker was still claimed verified: {offline!r}")
            # The worker's control state died with it; `last_terminal` has to
            # come back from the Run's own ledger or it was never durable.
            durable = offline["last_terminal"]
            if not isinstance(durable, dict):
                raise AssertionError(f"the settled turn did not survive its worker: {offline!r}")
            if durable["turn_id"] != "turn-3" or durable["status"] != "interrupted":
                raise AssertionError(f"the durable terminal misreports its turn: {durable!r}")
            if durable["run_id"] != run_id or durable["effort"] != "medium":
                raise AssertionError(f"the durable terminal lost its identity: {durable!r}")
            offline_events = call([*observed, "events", run_id, *owned, "--after", "0"])
            tail = offline_events[-1]["data"]
            if not isinstance(tail, dict) or tail.get("kind") != "end":
                raise AssertionError(f"projection-only events did not terminate: {offline_events!r}")
        finally:
            for worker in workers:
                try:
                    if os.waitpid(worker.pid, os.WNOHANG) == (0, 0):
                        os.kill(worker.pid, 15)
                        worker.wait(timeout=10)
                except (ChildProcessError, ProcessLookupError):
                    pass
            stop_fake_app_server(fake, socket_path)


def validate_absent_run_status(binary: pathlib.Path) -> None:
    """An absent valid Run identity is a not-found result, not a path failure."""
    with tempfile.TemporaryDirectory(prefix="dolgorae-run-not-found-") as temporary:
        root = pathlib.Path(temporary)
        root.chmod(0o700)
        home = root / "home"
        workspace = root / "workspace"
        private_directory(home)
        private_directory(workspace)
        environment = {"HOME": str(home)}

        init_status, init_objects = machine(
            binary, "init", str(workspace), "--non-git", environment=environment
        )
        if init_status != 0 or len(init_objects) != 1:
            raise AssertionError(
                f"init did not publish one workspace: {init_status} {init_objects!r}"
            )

        run_id = fresh_run_id()
        query_status, query_objects = machine(
            binary,
            "run",
            "status",
            run_id,
            "--workspace",
            str(workspace),
            environment=environment,
        )
        if query_status != 3 or len(query_objects) != 1:
            raise AssertionError(
                f"absent run status returned the wrong exit or envelope: "
                f"{query_status} {query_objects!r}"
            )
        error = query_objects[0].get("error")
        if not isinstance(error, dict) or error.get("code") != "RUN_NOT_FOUND":
            raise AssertionError(f"absent run status was misclassified: {query_objects!r}")


def worker_pid(state_root: pathlib.Path, run_id: str) -> int | None:
    """The PID a Run's own runtime record published, for cleanup only.

    `run start` detaches its worker, so nothing this test spawned owns it. The
    record is read to reap that process, never to decide an assertion.
    """
    record = state_root / "runtime" / "runs" / f"{run_id}.json"
    if not record.is_file():
        return None
    document = json.loads(record.read_text(encoding="utf-8"))
    identity = document.get("identity")
    return int(identity["pid"]) if isinstance(identity, dict) else None


def validate_run_start_model_resolution(binary: pathlib.Path) -> None:
    """Drive `dolgorae run ... start` end to end against the independent fake.

    Nothing here reaches a network, an account, or the installed Codex's
    app-server. The Runtime Profile launches a compiled native fixture image
    named `codex`, and that image serves the shared ADR-014 fake on exactly the
    socket the launch contract named. So every `model/list` page a Run resolves
    against comes from the fixture: the catalogue is three pages, the middle one
    empty, and the wanted model appears only on the last, which is what makes
    the walk, its cursor chaining, and the pinned Codex 0.149 item shapes
    provable without a real app-server answering a real account.

    The installed Codex 0.149.0 is used for exactly one thing: generating the
    app-server JSON Schema bundle, whose digest the profile contract pins and
    whose contents are not checked into the tree. That is setup — it answers no
    protocol call, and the case fails closed when it is absent.

    Run membership is proved in the same pass. Lazy `run start` and `run fork
    --fresh` register live members without publishing workers; `profile server
    stop` refuses them until their threadless close releases both memberships.
    """
    machine_schema = validator(REPOSITORY / "docs" / "protocol", "dolgorae-machine-v1.schema.json")
    schema_source = native_codex.installed_codex()
    scenario = native_codex.scenario_path("run_start_model_list.json")
    with tempfile.TemporaryDirectory(prefix="dolgorae-epic002-runstart-") as temporary:
        root = pathlib.Path(temporary)
        root.chmod(0o700)
        codex_home = root / "codex-home"
        workspace = root / "workspace"
        bin_root = root / "bin"
        # The operator credential is a `HOME`-scoped singleton and this case
        # has to initialize one, so it owns its own account home rather than
        # racing the rest of the suite for the shared one.
        home = root / "home"
        for directory in (codex_home, workspace, bin_root, home):
            private_directory(directory)
        account = {"HOME": str(home)}
        state_home = home / ".dolgorae" / "workspaces"
        status, objects = machine(binary, "init", str(workspace), "--non-git", environment=account)
        if status != 0 or len(objects) != 1:
            raise AssertionError(f"init did not publish one workspace: {status} {objects!r}")
        workspace_id = str(objects[0]["data"]["workspace_id"])  # type: ignore[index]
        state_root = state_home / workspace_id
        owned = ["--workspace", str(workspace)]

        def call(
            argv: list[str], *, expect: int = 0, checked: bool = True
        ) -> list[dict[str, object]]:
            status, objects = machine(binary, *argv, environment=account)
            if status != expect:
                raise AssertionError(
                    f"unexpected exit {status} (wanted {expect}) for {argv}: {objects!r}"
                )
            if not objects:
                raise AssertionError(f"no machine object for {argv}")
            for instance in objects:
                if checked:
                    assert_valid(instance, machine_schema, f"envelope for {argv}")
            return objects

        # `checked=False` is only ever passed for a *successful* profile-family
        # envelope. The published machine schema already describes the full
        # profile view this slice has not finished publishing, so validating
        # those successes here would assert a contract no current command meets
        # — which is what test_profile_cli.py's own schema checks stay off for
        # too. Every failure envelope, and every Run object, is checked.
        def data(argv: list[str], *, checked: bool = True) -> dict[str, object]:
            objects = call(argv, checked=checked)
            if len(objects) != 1 or objects[0]["ok"] is not True:
                raise AssertionError(f"{argv} failed: {objects!r}")
            return dict(objects[0]["data"])  # type: ignore[arg-type]

        def failure(argv: list[str], *, expect: int) -> dict[str, object]:
            objects = call(argv, expect=expect)
            if len(objects) != 1 or objects[0]["ok"] is not False:
                raise AssertionError(f"{argv} unexpectedly succeeded: {objects!r}")
            return dict(objects[0]["error"])  # type: ignore[arg-type]

        def membership_records() -> int:
            verified = data(
                ["profile", "membership", "verify", "default", *owned], checked=False
            )
            return int(verified["records"])

        codex = bin_root / "codex"
        transcript = root / "app-server-transcript.jsonl"
        native_codex.create_native_codex(
            codex,
            scenario=scenario,
            codex_home=codex_home,
            schema_source=schema_source,
            transcript=transcript,
        )
        data(
            [
                "profile",
                "add",
                "default",
                *owned,
                "--codex-home",
                str(codex_home),
                "--native-subagents",
                "enabled",
                "--env",
                "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
                "--env",
                "LANG=en_US.UTF-8",
                "--env",
                "LC_ALL=en_US.UTF-8",
                "--",
                str(codex),
            ],
            checked=False,
        )
        # Minted before anything is spawned so the cleanup below can always
        # reach the singleton, including when a case fails halfway through it.
        operator = root / "operator"
        data(["operator", "credential", "initialize", "--output", str(operator)])

        started: list[str] = []
        server_key: str | None = None
        try:
            # The singleton is started explicitly so the probe's own walk is
            # attributable: everything the Run resolves later is a second and
            # third walk over the same paginated catalogue.
            state = data(["profile", "server", "start", "default", *owned], checked=False)[
                "state"
            ]
            if not isinstance(state, dict):
                raise AssertionError(f"profile server start published no state: {state!r}")
            server_key = str(state["server_key"])
            if state["models"] != ["gpt-5.4", "gpt-5.6"]:
                raise AssertionError(
                    f"the probe did not collect the catalogue across its pages: {state!r}"
                )
            # `gpt-5.6` is announced on the last page only, behind a page with
            # no data at all: a walk that stopped at the first empty page, or at
            # the first page, could not have found it.
            if state["default_model"] != "gpt-5.6":
                raise AssertionError(f"the paginated default model was lost: {state!r}")
            if state["capabilities"]["model_list"] != "supported":  # type: ignore[index]
                raise AssertionError(f"model_list was not proven by the walk: {state!r}")
            if membership_records() != 1:
                raise AssertionError("a fresh server published more than its own start record")

            credential = root / "controller.json"
            data(
                [
                    "controller",
                    "credential",
                    "create",
                    # `automation` is the kind a managed_agent Run binds.
                    "--kind",
                    "automation",
                    "--instance-id",
                    "runstart-e2e",
                    "--output",
                    str(credential),
                ]
            )
            start = [
                "run",
                "--controller-file",
                str(credential),
                "start",
                *owned,
                "--profile",
                "default",
                "--control-mode",
                "managed-agent",
                "--execution-lane",
                "shared-readonly",
                "--required-assurance",
                "best-effort-personal-alpha",
                "--purpose",
                "implementation",
            ]

            accepted = data(
                [
                    *start,
                    "--instructions",
                    "Answer briefly, and only once.",
                    "--idempotency-key",
                    "runstart-accepted",
                ]
            )
            started.append(str(accepted["run_id"]))
            # No `--model` and no `--effort` were given, so both come from the
            # fixture's pinned shapes: the one `isDefault` item in the whole
            # catalogue, and the first `reasoningEffort` that item advertises
            # once the duplicate `medium` is folded away.
            if accepted["model"] != "gpt-5.6" or accepted["effort"] != "medium":
                raise AssertionError(f"model resolution ignored the walk: {accepted!r}")
            if accepted["profile"] != "default" or accepted["identity_verdict"] != "Absent":
                raise AssertionError(f"the started Run lost its identity: {accepted!r}")
            if accepted["execution_lane"] != "shared_readonly" or accepted["thread_id"] is not None:
                raise AssertionError(f"a freshly started Run claimed a Thread: {accepted!r}")
            if membership_records() != 2:
                raise AssertionError("the started Run was not registered as a member")

            forked = data(
                [
                    "run",
                    "--controller-file",
                    str(credential),
                    "fork",
                    "--from",
                    started[0],
                    *owned,
                    "--fresh",
                    "--idempotency-key",
                    "runstart-fresh-fork",
                ]
            )
            started.append(str(forked["run_id"]))
            if (
                forked["thread_id"] is not None
                or forked["state"] != "idle"
                or forked["lineage"]["mode"] != "fresh"  # type: ignore[index]
                or forked["lineage"]["source_run_id"] != started[0]  # type: ignore[index]
            ):
                raise AssertionError(f"fresh fork copied source history: {forked!r}")
            if membership_records() != 3:
                raise AssertionError("the fresh fork was not registered as a member")

            runtime_record = state_root / "runtime" / "runs" / f"{started[0]}.json"
            for verb in ("resume", "recover", "reconcile"):
                refused = failure(
                    ["run", "--controller-file", str(credential), verb, started[0], *owned],
                    expect=4,
                )
                if refused["code"] != "RUN_STATE_CONFLICT":
                    raise AssertionError(f"threadless {verb} used the wrong refusal: {refused!r}")
                if runtime_record.exists():
                    raise AssertionError(f"threadless {verb} spawned a worker before refusal")

            terminal = data(
                [
                    "run",
                    "--controller-file",
                    str(credential),
                    "send",
                    started[0],
                    *owned,
                    "--message",
                    "establish history",
                    "--idempotency-key",
                    "runstart-history-turn",
                ]
            )
            if terminal["turn_id"] != "turn-1" or terminal["status"] != "completed":
                raise AssertionError(f"source history did not become terminal: {terminal!r}")

            closed = data(
                ["run", "--controller-file", str(credential), "close", started[0], *owned]
            )
            if closed["thread_id"] != "thread-alpha" or closed["state"] != "closed":
                raise AssertionError(f"terminal source did not close cleanly: {closed!r}")

            history = data(
                [
                    "run",
                    "--controller-file",
                    str(credential),
                    "fork",
                    "--from",
                    started[0],
                    *owned,
                    "--idempotency-key",
                    "runstart-history-fork",
                ]
            )
            started.append(str(history["run_id"]))
            lineage = history["lineage"]
            if (
                history["thread_id"] is not None
                or lineage["mode"] != "history_copy"  # type: ignore[index]
                or lineage["source_run_id"] != started[0]  # type: ignore[index]
                or lineage["source_thread_id"] != "thread-alpha"  # type: ignore[index]
                or lineage["source_turn_id"] != "turn-1"  # type: ignore[index]
                or lineage["last_confirmed_boundary"] != "completed"  # type: ignore[index]
            ):
                raise AssertionError(f"history-copy fork lost its boundary: {history!r}")
            fork_turn = data(
                [
                    "run",
                    "--controller-file",
                    str(credential),
                    "send",
                    started[2],
                    *owned,
                    "--message",
                    "continue copied history",
                    "--idempotency-key",
                    "runstart-history-fork-turn",
                ]
            )
            if fork_turn["turn_id"] != "turn-2" or fork_turn["status"] != "completed":
                raise AssertionError(f"history-copy fork did not run from its copied thread: {fork_turn!r}")
            history_closed = data(
                ["run", "--controller-file", str(credential), "close", started[2], *owned]
            )
            if history_closed["state"] != "closed" or history_closed["thread_id"] != "thread-beta":
                raise AssertionError(f"history fork close lost its Thread: {history_closed!r}")

            next_credential = root / "next-controller.json"
            data(
                [
                    "controller",
                    "credential",
                    "create",
                    "--kind",
                    "automation",
                    "--instance-id",
                    "runstart-e2e",
                    "--output",
                    str(next_credential),
                ]
            )
            continuation = data(
                [
                    "run",
                    "--controller-file",
                    str(credential),
                    "create-write-continuation",
                    "--from",
                    started[0],
                    "--from-turn",
                    "turn-1",
                    "--reason",
                    "shared-readonly-source",
                    "--purpose",
                    "implementation",
                    "--idempotency-key",
                    "runstart-write-continuation",
                    "--new-controller-file",
                    str(next_credential),
                    *owned,
                ]
            )
            started.append(str(continuation["run_id"]))
            continuation_lineage = continuation["lineage"]
            if (
                continuation["execution_lane"] != "dedicated"
                or continuation["thread_id"] is not None
                or continuation_lineage["source_run_id"] != started[0]  # type: ignore[index]
                or continuation_lineage["source_turn_id"] != "turn-1"  # type: ignore[index]
                or continuation_lineage["creation_reason"] != "shared_readonly_source"  # type: ignore[index]
            ):
                raise AssertionError(f"write continuation lost its lineage: {continuation!r}")

            destination = data(
                [
                    "run",
                    "--controller-file",
                    str(next_credential),
                    "start",
                    *owned,
                    "--profile",
                    "default",
                    "--control-mode",
                    "managed-agent",
                    "--execution-lane",
                    "dedicated",
                    "--required-assurance",
                    "best-effort-personal-alpha",
                    "--purpose",
                    "implementation",
                    "--instructions",
                    "Receive writer authority.",
                    "--idempotency-key",
                    "runstart-writer-destination",
                ]
            )
            started.append(str(destination["run_id"]))

            write_turn = data(
                [
                    "run",
                    "--controller-file",
                    str(next_credential),
                    "send",
                    started[3],
                    *owned,
                    "--write",
                    "--message",
                    "own the writer",
                    "--idempotency-key",
                    "runstart-writer-source-turn",
                ]
            )
            if write_turn["status"] != "completed":
                raise AssertionError(f"write continuation did not complete: {write_turn!r}")
            writer_status = data(["workspace", "writer", "status", *owned])
            if (
                writer_status["authority_state"] != "active"
                or writer_status["writer_run_id"] != started[3]
                or writer_status["writer_generation"] != 1
            ):
                raise AssertionError(f"writer acquisition was not durable: {writer_status!r}")

            destination_turn = data(
                [
                    "run",
                    "--controller-file",
                    str(next_credential),
                    "send",
                    started[4],
                    *owned,
                    "--message",
                    "prepare for handoff",
                    "--idempotency-key",
                    "runstart-writer-destination-turn",
                ]
            )
            if destination_turn["status"] != "completed":
                raise AssertionError(f"writer destination did not become idle: {destination_turn!r}")

            prepare_args = [
                "workspace",
                "writer",
                "handoff-prepare",
                *owned,
                "--from",
                started[3],
                "--to",
                started[4],
                "--expected-generation",
                "1",
                "--controller-file",
                str(next_credential),
            ]
            cancelled_prepare = data(prepare_args)
            cancelled = data(
                [
                    "workspace",
                    "writer",
                    "handoff-cancel",
                    *owned,
                    "--handoff-id",
                    str(cancelled_prepare["handoff_id"]),
                    "--controller-file",
                    str(next_credential),
                ]
            )
            if cancelled["status"] != "cancelled":
                raise AssertionError(f"writer handoff did not cancel: {cancelled!r}")

            prepared = data(prepare_args)
            committed = data(
                [
                    "workspace",
                    "writer",
                    "handoff-commit",
                    *owned,
                    "--handoff-id",
                    str(prepared["handoff_id"]),
                    "--expected-generation",
                    "1",
                    "--controller-file",
                    str(next_credential),
                ]
            )
            if committed["status"] != "committed":
                raise AssertionError(f"writer handoff did not commit: {committed!r}")
            writer_status = data(["workspace", "writer", "status", *owned])
            if (
                writer_status["authority_state"] != "active"
                or writer_status["writer_run_id"] != started[4]
                or writer_status["writer_generation"] != 2
            ):
                raise AssertionError(f"writer handoff was not durable: {writer_status!r}")
            data(
                [
                    "run",
                    "--controller-file",
                    str(next_credential),
                    "release-write",
                    started[4],
                    *owned,
                ]
            )

            stop = [
                "profile",
                "server",
                "stop",
                "default",
                *owned,
                "--operator-file",
                str(operator),
            ]
            gated = failure(stop, expect=4)
            if gated["code"] != "PROFILE_MEMBERSHIP_INCOMPLETE":
                raise AssertionError(f"a live member did not gate the stop: {gated!r}")
            if "3 live run member(s)" not in str(gated["details"]["reason"]):  # type: ignore[index]
                raise AssertionError(f"the stop gate did not count its members: {gated!r}")

            fork_closed = data(
                ["run", "--controller-file", str(credential), "close", started[1], *owned]
            )
            if fork_closed["state"] != "closed" or fork_closed["thread_id"] is not None:
                raise AssertionError(f"threadless fork close was not durable: {fork_closed!r}")
            continuation_closed = data(
                [
                    "run",
                    "--controller-file",
                    str(next_credential),
                    "close",
                    started[3],
                    *owned,
                ]
            )
            if continuation_closed["state"] != "closed" or continuation_closed["thread_id"] != "thread-alpha":
                raise AssertionError(f"write continuation close lost its Thread: {continuation_closed!r}")
            destination_closed = data(
                [
                    "run",
                    "--controller-file",
                    str(next_credential),
                    "close",
                    started[4],
                    *owned,
                ]
            )
            if destination_closed["state"] != "closed":
                raise AssertionError(f"writer destination did not close: {destination_closed!r}")
            if membership_records() != 11:
                raise AssertionError("a successful close did not release the membership")
            # The same stop, unchanged, now that the member is gone.
            if data(stop, checked=False)["stopped"] is not True:
                raise AssertionError("the released membership still gated the stop")

            # The fixture's own record of what the client sent it. Reading it
            # after the server is stopped is what makes the walk a fact rather
            # than an inference from the answers: these requests reached the
            # fake, so no installed app-server answered a single one of them.
            sent = [
                json.loads(line)
                for line in transcript.read_text(encoding="utf-8").splitlines()
                if line.strip()
            ]
            pages = [message for message in sent if message.get("method") == "model/list"]
            # Ten resolution walks each exhaust the same three-page catalogue.
            if [message["params"] for message in pages] != [
                {"cursor": None, "limit": 100},
                {"cursor": "page-2", "limit": 100},
                {"cursor": "page-3", "limit": 100},
            ] * 10:
                raise AssertionError(f"model/list was not walked as pinned: {pages!r}")
            if sum(1 for message in sent if message.get("method") == "initialize") != 14:
                raise AssertionError(f"the fixture served unexpected connections: {sent!r}")
            if sum(1 for message in sent if message.get("method") == "thread/fork") != 1:
                raise AssertionError(f"history-copy fork never reached thread/fork: {sent!r}")
            if sum(1 for message in sent if message.get("method") == "account/read") != 1:
                raise AssertionError(f"the probe's account read is not once-only: {sent!r}")
            if sum(1 for message in sent if message.get("method") == "thread/read") != 1:
                raise AssertionError(f"the absent-thread probe did not run once: {sent!r}")
        finally:
            for run_id in started:
                pid = worker_pid(state_root, run_id)
                if pid is None:
                    continue
                try:
                    os.kill(pid, 15)
                except ProcessLookupError:
                    pass
            # The singleton and its log drainer outlive this process, so they
            # are reaped here rather than left behind by a case that failed
            # before it reached its own stop. Already stopped is a no-op.
            if server_key is not None:
                machine(
                    binary,
                    "profile",
                    "server",
                    "stop",
                    "default",
                    *owned,
                    "--operator-file",
                    str(operator),
                    "--interrupt",
                    "--confirm-server-key",
                    server_key,
                    environment=account,
                )


def validate_fixture() -> None:
    """The shared fake must be trustworthy before anything is proved with it."""
    duplicate = subprocess.run(
        [
            sys.executable,
            "-c",
            "import sys;sys.path.insert(0, sys.argv[1]);import jsonlite;"
            "jsonlite.loads(sys.argv[2])",
            str(FAKE_APP_SERVER),
            '{"a":1,"a":2}',
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    if duplicate.returncode == 0 or "duplicate member" not in duplicate.stderr:
        raise AssertionError(
            "the fixture's reader accepted a duplicate member: "
            f"{duplicate.returncode} {duplicate.stderr!r}"
        )
    with tempfile.TemporaryDirectory(prefix="dolgorae-fixture-") as temporary:
        malformed = pathlib.Path(temporary) / "broken.json"
        malformed.write_text(
            json.dumps({"schema_version": 1, "name": "broken"}), encoding="utf-8"
        )
        refused = subprocess.run(
            [
                sys.executable,
                str(FAKE_APP_SERVER),
                "--socket",
                str(pathlib.Path(temporary) / "unused.sock"),
                "--scenario",
                str(malformed),
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        if refused.returncode == 0 or "steps" not in refused.stderr:
            raise AssertionError(
                f"the fixture started on an unvalidated scenario: {refused.stderr!r}"
            )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument("--only", default=None)
    arguments = parser.parse_args()
    binary = arguments.binary.resolve()
    stages = {
        "fixture": lambda _binary: validate_fixture(),
        "identity": validate,
        "session": validate_session,
        "foreign": validate_foreign_thread,
        "sigterm": validate_shutdown_interrupt,
        "streamed": validate_streamed_history,
        "runcli": validate_run_cli,
        "runnotfound": validate_absent_run_status,
        "runstart": validate_run_start_model_resolution,
    }
    if arguments.only:
        stages[arguments.only](binary)
        print(f"Worker CLI validation passed: {arguments.only}")
        return 0
    validate_fixture()
    validate(binary)
    validate_session(binary)
    validate_foreign_thread(binary)
    validate_shutdown_interrupt(binary)
    validate_streamed_history(binary)
    validate_run_cli(binary)
    validate_absent_run_status(binary)
    validate_run_start_model_resolution(binary)
    print(
        "Worker CLI validation passed: independent fixture, fd-3, replay, reconnect, "
        "identity, cleanup, multi-turn session, foreign-thread isolation, "
        "SIGTERM turn interruption, streamed history, machine-CLI run verbs, "
        "absent-Run classification, "
        "turn text sources, write refusal, the caller duration grammar, and "
        "argv `run start` model resolution against the faked app-server"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
