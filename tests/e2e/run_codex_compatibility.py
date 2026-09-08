#!/usr/bin/env python3
"""Opt-in black-box history, fork, interrupt, and restart compatibility checks."""

from __future__ import annotations

import argparse
import json
import os
import tempfile
import time
import uuid
from pathlib import Path

from run_access_safety_acceptance import (
    AppServer, PINNED_VERSION, TARGET_EFFORT, TARGET_MODEL, exact_codex, target_model,
)

OPT_IN = "DOLGORAE_RUN_LIVE_CODEX_COMPATIBILITY"
OBSERVATIONS = json.loads(
    (Path(__file__).resolve().parents[2] / "docs/protocol/codex-0.153.4-required-subset.json")
    .read_text(encoding="utf-8")
)["behavioral_observations"]


def initialize(server: AppServer) -> None:
    result = server.request("initialize", {
        "clientInfo": {"name": "dolgorae-compatibility", "version": "1"},
        "capabilities": {"experimentalApi": False, "optOutNotificationMethods": []},
    })
    if Path(result["codexHome"]).resolve() != Path(os.environ["CODEX_HOME"]).resolve():
        raise RuntimeError("initialize selected the wrong Codex home")
    server.send({"method": "initialized", "params": {}})
    target_model(server)


def expect_error(server: AppServer, method: str, params: dict) -> int:
    try:
        server.request(method, params)
    except RuntimeError:
        message = server.messages[-1]
        if "error" not in message:
            raise
        return message["error"]["code"]
    raise RuntimeError(f"{method} unexpectedly succeeded")


def start_turn(server: AppServer, thread_id: str, prompt: str) -> str:
    result = server.request("turn/start", {
        "threadId": thread_id,
        "input": [{"type": "text", "text": prompt}],
        "model": TARGET_MODEL,
        "effort": TARGET_EFFORT,
        "approvalPolicy": "on-request",
        "sandboxPolicy": {"type": "readOnly", "networkAccess": False},
    })
    return result["turn"]["id"]


def terminal(server: AppServer, thread_id: str, turn_id: str, expected: str) -> None:
    deadline = time.monotonic() + 180
    message_index = 0
    while time.monotonic() < deadline:
        # RPC response waits may already have received the terminal notification.
        if message_index == len(server.messages):
            message = server.receive(deadline)
            server.handle_request(message)
        message = server.messages[message_index]
        message_index += 1
        params = message.get("params", {})
        if (
            message.get("method") == "turn/completed"
            and params.get("threadId") == thread_id
            and params.get("turn", {}).get("id") == turn_id
        ):
            if params["turn"]["status"] != expected:
                raise RuntimeError(f"turn did not reach {expected}")
            return
    raise TimeoutError("turn completion was not observed")


def run(codex: Path) -> dict:
    with tempfile.TemporaryDirectory(prefix="dolgorae-live-history-") as temporary:
        workspace = Path(temporary)
        server = AppServer(codex, workspace)
        try:
            initialize(server)
            if expect_error(server, "thread/read", {
                "threadId": str(uuid.uuid4()), "includeTurns": True,
            }) != OBSERVATIONS["absent_thread_read_error_code"]:
                raise RuntimeError("absent thread error changed")
            started = server.request("thread/start", {
                "cwd": str(workspace), "model": TARGET_MODEL,
                "sandbox": "read-only", "approvalPolicy": "on-request",
            })
            if started["model"] != TARGET_MODEL:
                raise RuntimeError("thread/start substituted the target model")
            thread_id = started["thread"]["id"]
            first = start_turn(server, thread_id, "Reply exactly compatibility-ok. Do not use tools.")
            terminal(server, thread_id, first, "completed")
            history = server.request("thread/read", {"threadId": thread_id, "includeTurns": True})
            if not any(t["id"] == first and t["status"] == "completed" for t in history["thread"]["turns"]):
                raise RuntimeError("completed turn is absent from persisted history")
            id_offset = server.last_frame.find(b'"id"')
            result_offset = server.last_frame.find(b'"result"')
            if not 0 <= id_offset < min(OBSERVATIONS["thread_read_response_id_observed_before_byte"], result_offset):
                raise RuntimeError("thread/read no longer supplies an early response ID")
            forked = server.request("thread/fork", {"threadId": thread_id, "lastTurnId": first})
            if forked["thread"]["id"] == thread_id:
                raise RuntimeError("fork reused the source thread identity")

            interrupted = start_turn(server, thread_id, "Run sleep 30 in the shell, then reply done.")
            deadline = time.monotonic() + 180
            while True:
                message = server.receive(deadline)
                server.handle_request(message)
                params = message.get("params", {})
                if (
                    message.get("method") == "item/started"
                    and params.get("turnId") == interrupted
                    and params.get("item", {}).get("type") == "commandExecution"
                ):
                    break
                if message.get("method") == "turn/completed" and params.get("turn", {}).get("id") == interrupted:
                    raise RuntimeError("interrupt probe completed without starting the sleep command")
            server.request("turn/interrupt", {"threadId": thread_id, "turnId": interrupted})
            terminal(server, thread_id, interrupted, "interrupted")
            if OBSERVATIONS["codex_interrupted_last_turn_id"] != "accepted":
                raise RuntimeError("manifest does not describe the checked interrupted-fork behavior")
            interrupted_fork = server.request("thread/fork", {"threadId": thread_id, "lastTurnId": interrupted})
            if interrupted_fork["thread"]["id"] == thread_id:
                raise RuntimeError("interrupted fork reused the source thread identity")
            implicit_fork = server.request("thread/fork", {"threadId": thread_id})
            if implicit_fork["thread"]["id"] == thread_id:
                raise RuntimeError("implicit interrupted-head fork reused the source thread identity")

            pending_path = workspace / "pending.txt"
            pending = start_turn(server, thread_id,
                f"Use only a shell command to write pending to {pending_path}. "
                "Request approval if necessary. Do not use apply_patch.")
            deadline = time.monotonic() + 180
            while True:
                message = server.receive(deadline)
                if message.get("method") == "item/commandExecution/requestApproval":
                    if type(message.get("id")) is not int:
                        raise RuntimeError("approval request ID is not an integer")
                    params = message["params"]
                    if params["threadId"] != thread_id or params["turnId"] != pending:
                        raise RuntimeError("approval request is not correlated")
                    break
                if message.get("method") == "turn/completed":
                    raise RuntimeError("pending-approval turn completed without an approval request")
                server.handle_request(message)
            server.close()
            server = AppServer(codex, workspace)
            initialize(server)
            resumed = server.request("thread/resume", {"threadId": thread_id, "model": TARGET_MODEL})
            if resumed["thread"]["id"] != thread_id:
                raise RuntimeError("resume changed thread identity")
            history = server.request("thread/read", {"threadId": thread_id, "includeTurns": True})
            turns = history["thread"]["turns"]
            if not any(t["id"] == pending and t["status"] == "interrupted" for t in turns):
                raise RuntimeError("unanswered approval did not become interrupted after restart")
            if pending_path.exists():
                raise RuntimeError("unanswered approval executed its command")
            last = start_turn(server, thread_id, "Reply exactly resumed-ok. Do not use tools.")
            terminal(server, thread_id, last, "completed")
            if any(m.get("method") == "item/commandExecution/requestApproval" for m in server.messages):
                raise RuntimeError("restart replayed the unanswered approval")
            return {
                "schema_version": 1, "codex_version": PINNED_VERSION,
                "model": TARGET_MODEL, "effort": TARGET_EFFORT,
                "absent_thread_error": -32600, "early_response_id": True,
                "completed_history": True, "completed_fork": True,
                "codex_interrupted_fork": "accepted", "interrupt": True,
                "omitted_last_turn_id_on_interrupted_head": "accepted",
                "pending_approval_restart": "interrupted", "resume": True,
                "automatic_replay": False,
            }
        finally:
            server.close()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--codex", type=Path, required=True)
    args = parser.parse_args()
    if os.environ.get(OPT_IN) != "1":
        raise SystemExit(f"{OPT_IN}=1 is required")
    if not os.environ.get("CODEX_HOME"):
        raise SystemExit("an explicit prepared CODEX_HOME is required")
    print(json.dumps(run(exact_codex(args.codex)), sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
