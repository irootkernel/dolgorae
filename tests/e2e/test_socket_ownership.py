#!/usr/bin/env python3
"""Native macOS UDS ownership, readiness, singleton, and stale-node matrix."""
from __future__ import annotations

import argparse
import json
import os
import pathlib
import select
import signal
import socket
import stat
import subprocess
import tempfile

from schema_support import assert_valid, validator

REPOSITORY = pathlib.Path(__file__).resolve().parents[2]


def validate(binary: pathlib.Path) -> None:
    machine = validator(REPOSITORY / "docs/protocol", "dolgorae-machine-v2.schema.json")
    with tempfile.TemporaryDirectory(prefix="dg-socket-", dir="/private/tmp") as temporary:
        root = pathlib.Path(temporary)
        root.chmod(0o700)
        home, workspace, parent = (root / name for name in ("home", "workspace", "rpc"))
        for directory in (home, workspace, parent):
            directory.mkdir(mode=0o700)
        environment = dict(os.environ, HOME=str(home))
        subprocess.run([str(binary), "init", str(workspace), "--non-git"], env=environment, capture_output=True, check=True, timeout=30)

        def start(path: pathlib.Path, expected: str | None = None) -> tuple[subprocess.Popen, dict]:
            process = subprocess.Popen([str(binary), "serve", "--socket", str(path)], env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            assert process.stdout is not None
            if not select.select([process.stdout], [], [], 15)[0]:
                process.kill()
                process.communicate()
                raise AssertionError("gateway did not publish readiness within 15 seconds")
            envelope = json.loads(process.stdout.readline())
            assert_valid(envelope, machine, "gateway readiness")
            assert envelope["command"] == "serve", envelope
            if expected:
                rest, stderr = process.communicate(timeout=10)
                assert not rest, "failure emitted more than one readiness envelope"
                assert process.returncode != 0 and envelope["error"]["code"] == expected, (envelope, stderr)
            else:
                assert envelope["ok"] is True, envelope
            return process, envelope

        def stop(process: subprocess.Popen) -> None:
            process.send_signal(signal.SIGTERM)
            rest, stderr = process.communicate(timeout=7)
            assert process.returncode == 0, stderr
            assert not rest, "gateway wrote stdout after readiness"

        path = parent / "g.sock"
        parent.chmod(0o755)
        start(path, "RPC_SOCKET_UNSAFE")
        parent.chmod(0o700)
        alias = root / "alias"
        alias.symlink_to(parent, target_is_directory=True)
        start(alias / "g.sock", "RPC_SOCKET_UNSAFE")
        path.write_bytes(b"foreign-file")
        before = (path.stat().st_ino, path.read_bytes())
        start(path, "RPC_SOCKET_UNSAFE")
        assert before == (path.stat().st_ino, path.read_bytes())
        path.unlink()
        target = root / "target"
        target.write_bytes(b"foreign-target")
        path.symlink_to(target)
        start(path, "RPC_SOCKET_UNSAFE")
        assert path.is_symlink() and target.read_bytes() == b"foreign-target"
        path.unlink()
        with socket.socket(socket.AF_UNIX) as foreign:
            foreign.bind(str(path))
            path.chmod(0o600)
            inode = path.stat().st_ino
            start(path, "RPC_SOCKET_UNSAFE")
            assert path.stat().st_ino == inode
        path.unlink()  # The fixture owns this deliberately foreign test node.

        process, ready = start(path)
        try:
            assert stat.S_IMODE(path.stat().st_mode) == 0o600
            assert path.stat().st_uid == os.getuid()
            record_path = home / ".dolgorae/rpc/gateway.json"
            record = json.loads(record_path.read_text())
            assert record["pid"] == process.pid
            assert record["socket_inode"] == path.stat().st_ino
            assert record["server_instance_id"] == ready["data"]["server_instance_id"]
            assert stat.S_IMODE(record_path.stat().st_mode) == 0o600
            assert stat.S_IMODE((record_path.parent / "gateway.lock").stat().st_mode) == 0o600
            second = parent / "second.sock"
            _, collision = start(second, "RPC_SERVER_ALREADY_RUNNING")
            assert not second.exists()
            assert collision["error"]["details"]["server_instance_id"] == record["server_instance_id"]
            with socket.socket(socket.AF_UNIX) as client:
                client.connect(str(path))
        finally:
            stop(process)
        assert not path.exists()

        process, ready = start(path)
        process.kill()
        process.communicate(timeout=5)
        assert path.exists(), "crash fixture did not retain the owned socket"
        replacement, next_ready = start(path)
        assert next_ready["data"]["server_instance_id"] != ready["data"]["server_instance_id"]
        stop(replacement)
        assert not path.exists()

        process, _ = start(path)
        process.kill()
        process.communicate(timeout=5)
        path.unlink()  # Adversarial replacement within the isolated attack matrix.
        path.write_bytes(b"replacement-after-crash")
        before = (path.stat().st_ino, path.read_bytes())
        start(path, "RPC_SOCKET_UNSAFE")
        assert before == (path.stat().st_ino, path.read_bytes())

        # Invalid arguments still use and close an inherited readiness channel.
        reader, writer = os.pipe()
        process = subprocess.Popen([str(binary), "serve", "--ready-fd", str(writer)], env=environment, pass_fds=(writer,), stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        os.close(writer)
        with os.fdopen(reader, "rb") as pipe:
            assert select.select([pipe], [], [], 10)[0], "missing ready-fd failure"
            lines = pipe.read().splitlines()
        stdout, _ = process.communicate(timeout=5)
        assert not stdout and len(lines) == 1
        envelope = json.loads(lines[0])
        assert_valid(envelope, machine, "argument readiness failure")
        assert envelope["error"]["code"] == "INVALID_ARGUMENT"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    args = parser.parse_args()
    validate(args.binary.resolve())
    print("socket_ownership: macOS UDS attack matrix passed")


if __name__ == "__main__":
    main()
