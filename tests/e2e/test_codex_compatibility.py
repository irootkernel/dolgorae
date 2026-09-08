#!/usr/bin/env python3
"""Offline checks for RPC response and terminal notification ordering."""

from __future__ import annotations

import json

from run_codex_compatibility import AppServer, terminal


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
