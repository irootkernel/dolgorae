#!/usr/bin/env python3
"""Black-box validation of the hidden per-Run worker boundary."""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import select
import socket
import stat
import sys
import tempfile
import time
from typing import BinaryIO

RUN_ID = "018f22e2-79b0-7cc3-a24c-78c48f28bbcf"
BOOT_UUID = "2e349290-1744-4fc3-bb62-9cbf9f5859c0"


def private_directory(path: pathlib.Path) -> None:
    path.mkdir(mode=0o700, parents=True)
    path.chmod(0o700)


def prepare(root: pathlib.Path) -> pathlib.Path:
    ledger_root = root / "runs" / RUN_ID
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
        private_directory(directory)
    audit = ledger_root / "audit.jsonl"
    audit.touch(mode=0o600)
    audit.chmod(0o600)
    bootstrap = runtime_runs / f"{RUN_ID}.bootstrap.json"
    document = {
        "schema_version": 1,
        "workspace_id": "11" * 32,
        "run_id": RUN_ID,
        "run_generation": 1,
        "boot_uuid": BOOT_UUID,
        "executable_sha256": "22" * 32,
        "executable_path_sha256": "33" * 32,
        "dolgorae_version": "0.1.0",
        "mutation_protocol_version": 1,
        "control_socket_epoch": 1,
        "ledger_root": str(ledger_root),
        "runtime_record_path": str(runtime_runs / f"{RUN_ID}.json"),
        "startup_lock_path": str(startup / f"{RUN_ID}.lock"),
    }
    bootstrap.write_text(
        json.dumps(document, separators=(",", ":")) + "\n", encoding="utf-8"
    )
    bootstrap.chmod(0o600)
    return bootstrap


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


def handoff(channel: BinaryIO) -> dict[str, object]:
    readable, _, _ = select.select([channel], [], [], 10)
    if not readable:
        raise TimeoutError("worker did not emit the fd-3 startup frame within ten seconds")
    line = channel.readline(65538)
    if not line.endswith(b"\n") or len(line) > 65537:
        raise AssertionError(f"invalid fd-3 startup frame: {line[:100]!r}")
    return json.loads(line)


def call(
    path: pathlib.Path, operation: str, identity: dict[str, object]
) -> dict[str, object]:
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(5)
        client.connect(str(path))
        request = {"operation": operation, "expected": identity}
        client.sendall(
            json.dumps(request, separators=(",", ":")).encode("utf-8") + b"\n"
        )
        response = bytearray()
        while not response.endswith(b"\n"):
            response.extend(client.recv(65536))
        return json.loads(response)


def validate(binary: pathlib.Path) -> None:
    with tempfile.TemporaryDirectory(
        prefix="dolgorae-task004-validator-"
    ) as temporary:
        root = pathlib.Path(temporary)
        root.chmod(0o700)
        bootstrap = prepare(root)
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


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    arguments = parser.parse_args()
    validate(arguments.binary.resolve())
    print("Worker CLI validation passed: fd-3, replay, reconnect, identity, cleanup")
    return 0


if __name__ == "__main__":
    sys.exit(main())
