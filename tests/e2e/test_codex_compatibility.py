#!/usr/bin/env python3
"""Offline checks for RPC response and terminal notification ordering."""

from __future__ import annotations

import json
import tempfile
from pathlib import Path

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


def main() -> int:
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
    print("Codex compatibility notification ordering checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
