#!/usr/bin/env python3
"""Offline checks for Codex selection and RPC notification ordering."""

from __future__ import annotations

import json
import os
import tempfile
from pathlib import Path
from unittest.mock import patch

import native_codex

from run_access_safety_acceptance import exact_codex as checked_access_codex
from run_codex_compatibility import AppServer, checked_codex, terminal


class BufferedServer(AppServer):
    def __init__(self, frames: list[dict]) -> None:
        self.next_id = 1
        self.messages = []
        self.sent = []
        self.buffers = {"stdout": bytearray(
            b"".join(json.dumps(frame).encode() + b"\n" for frame in frames)
        )}

    def send(self, value: dict) -> None:
        self.sent.append(value)

    def receive(self, deadline: float) -> dict:
        if b"\n" not in self.buffers["stdout"]:
            raise AssertionError("waited for a notification that was already received")
        return super().receive(deadline)


def completed(thread: str, turn: str, status: str) -> dict:
    return {"method": "turn/completed", "params": {
        "threadId": thread, "turn": {"id": turn, "status": status},
    }}


def check_schema_producer_selection() -> None:
    with tempfile.TemporaryDirectory(prefix="dolgorae-schema-selection-", dir="/tmp") as directory:
        root = Path(directory).resolve()
        codex = root / "codex"
        override = root / "override"
        with patch.dict(os.environ, {"PATH": str(root)}, clear=True):
            for version, accepted in (
                ("0.157.9", False),
                ("0.158.0", True),
                ("0.166.0", True),
                ("0.158.0-dev", False),
                ("invalid", False),
            ):
                codex.write_text(f"#!/bin/sh\nprintf '%s\\n' 'codex-cli {version}'\n")
                codex.chmod(0o700)
                try:
                    selected = native_codex.installed_codex()
                except AssertionError:
                    assert not accepted, version
                else:
                    assert accepted and selected == codex, version
                    assert native_codex.codex_version(selected) == version
            codex.write_text("#!/bin/sh\nprintf '%s\\n' 'codex-cli 0.166.0'\n")
            override.write_text("#!/bin/sh\nprintf '%s\\n' 'codex-cli 0.167.0'\n")
            override.chmod(0o700)
            os.environ[native_codex.TEST_CODEX_ENV] = str(override)
            assert native_codex.installed_codex() == override
            for contents in (
                "#!/bin/sh\nprintf '%s\\n' 'codex-cli 0.157.9'\n",
                "#!/bin/sh\nprintf '%s\\n' 'codex-cli 0.167.0'\nexit 1\n",
            ):
                override.write_text(contents)
                try:
                    native_codex.installed_codex()
                except AssertionError:
                    pass
                else:
                    raise AssertionError("invalid override fell back to PATH")
            override.unlink()
            try:
                native_codex.installed_codex()
            except AssertionError:
                pass
            else:
                raise AssertionError("missing override fell back to PATH")
            try:
                checked_access_codex(codex)
            except ValueError:
                pass
            else:
                raise AssertionError("access-safety default baseline changed")
            os.environ["DOLGORAE_CODEX_PINNED_VERSION"] = "codex-cli 0.166.0"
            assert checked_access_codex(codex) == codex
            os.environ["DOLGORAE_CODEX_PINNED_VERSION"] = "codex-cli 0.165.0"
            try:
                checked_access_codex(codex)
            except ValueError:
                pass
            else:
                raise AssertionError("access-safety accepted the wrong qualification candidate")


def main() -> int:
    check_schema_producer_selection()
    with tempfile.TemporaryDirectory(prefix="dolgorae-codex-version-") as directory:
        codex = Path(directory) / "codex"
        for version, accepted in (
            ("codex-cli 0.156.9", False),
            ("codex-cli 0.157.0", False),
            ("codex-cli 0.157.1", False),
            ("codex-cli 0.158.0", True),
            ("codex-cli 0.158.1", True),
            ("codex-cli 0.158.0-dev", False),
        ):
            codex.write_text(f"#!/bin/sh\nprintf '%s\\n' '{version}'\n", encoding="utf-8")
            codex.chmod(0o700)
            try:
                checked, observed = checked_codex(codex)
            except ValueError:
                assert not accepted, version
            else:
                assert accepted and checked == codex.resolve() and observed == version, version
    for method, status in (("turn/start", "completed"), ("turn/interrupt", "interrupted")):
        for notification_first in (True, False):
            for actual in (status, "failed"):
                response = {"id": 1, "result": {}}
                notification = completed("thread", "turn", actual)
                ordered = [notification, response] if notification_first else [response, notification]
                server = BufferedServer([
                    completed("other-thread", "turn", status),
                    completed("thread", "other-turn", status),
                    *ordered,
                ])
                server.request(method, {"threadId": "thread", "turnId": "turn"})
                try:
                    terminal(server, "thread", "turn", status)
                except RuntimeError as error:
                    assert actual == "failed", error
                    assert str(error) == f"turn did not reach {status}"
                else:
                    assert actual == status, "accepted an unexpected terminal status"
                assert len(server.messages) == 4
    print("Codex schema selection and compatibility notification ordering checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
