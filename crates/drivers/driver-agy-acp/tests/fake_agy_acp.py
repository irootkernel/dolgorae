#!/usr/bin/env python3
"""Offline fake ACP v1 stdio server for driver-agy-acp integration tests."""

from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path


def log_record(log_path: str | None, record: dict) -> None:
    if not log_path:
        return
    with open(log_path, "a", encoding="utf-8") as fh:
        fh.write(json.dumps(record, sort_keys=True) + "\n")
        fh.flush()


def send_frame(frame: dict) -> None:
    sys.stdout.write(json.dumps(frame) + "\n")
    sys.stdout.flush()


def read_frame() -> dict | None:
    line = sys.stdin.readline()
    if not line:
        return None
    line = line.strip()
    if not line:
        return {}
    return json.loads(line)


def wait_for_response(expected_id: int | str, log_path: str | None) -> tuple[dict | None, bool]:
    """Read stdin until response matching expected_id arrives; track session/cancel."""
    cancelled = False
    while True:
        msg = read_frame()
        if msg is None:
            return (None, cancelled)
        if not msg:
            continue
        log_record(log_path, {"event": "stdin", "msg": msg})
        if msg.get("method") == "session/cancel":
            cancelled = True
            continue
        if msg.get("id") == expected_id and "method" not in msg:
            return (msg, cancelled)


def main() -> int:
    if len(sys.argv) >= 2 and sys.argv[1] == "--version":
        if os.environ.get("FAKE_AGY_PROBE_FAIL") == "1":
            sys.stderr.write("broken agy binary\n")
            return 1
        print("agy 1.5.18")
        return 0

    log_path = os.environ.get("FAKE_AGY_LOG")
    scenario = os.environ.get("FAKE_AGY_SCENARIO", "basic")
    advertise_resume = os.environ.get("FAKE_AGY_RESUME", "1") == "1"
    advertise_close = os.environ.get("FAKE_AGY_CLOSE", "1") == "1"
    modes_env = os.environ.get("FAKE_AGY_MODES")
    if modes_env is not None:
        modes_payload = json.loads(modes_env)
    else:
        modes_payload = {
            "currentModeId": "default",
            "availableModes": [
                {
                    "id": "default",
                    "name": "Default",
                    "description": "Ask before edits and commands, per your permission settings.",
                },
                {
                    "id": "accept-edits",
                    "name": "Accept edits",
                    "description": "Approve file edits in the workspace without asking.",
                },
                {
                    "id": "auto",
                    "name": "Auto",
                    "description": "Approve edits and safe commands; ask before irreversible actions.",
                },
                {
                    "id": "plan",
                    "name": "Plan",
                    "description": "Plan carefully before executing a task.",
                },
            ],
        }
    workspace = os.environ.get("FAKE_AGY_WORKSPACE", "/tmp")
    grandchild_pid_file = os.environ.get("FAKE_AGY_GRANDCHILD_PID_FILE")

    log_record(log_path, {"event": "argv", "argv": sys.argv[1:]})

    grandchild_proc: subprocess.Popen[bytes] | None = None
    if grandchild_pid_file:
        # Spawn a grandchild sleeper in the SAME process group (no setsid).
        grandchild_proc = subprocess.Popen(
            ["/bin/sleep", "60"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        Path(grandchild_pid_file).write_text(str(grandchild_proc.pid), encoding="utf-8")

    if scenario == "ignore_term":
        signal.signal(signal.SIGTERM, signal.SIG_IGN)

    session_id = "acp-sess-001"
    next_srv_id = 9000

    while True:
        msg = read_frame()
        if msg is None:
            break
        if not msg:
            continue
        log_record(log_path, {"event": "stdin", "msg": msg})

        method = msg.get("method")
        req_id = msg.get("id")
        params = msg.get("params") or {}

        if method == "initialize":
            session_caps: dict[str, object] = {}
            if advertise_resume:
                session_caps["resume"] = {}
            if advertise_close:
                session_caps["close"] = {}
            send_frame(
                {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "protocolVersion": 1,
                        "agentInfo": {"name": "fake-agy", "version": "1.5.18"},
                        "agentCapabilities": {
                            "loadSession": True,
                            "sessionCapabilities": session_caps,
                            "_meta": {
                                "antigravity": {"systemPrompt": {"append": True}}
                            },
                        },
                    },
                }
            )
            continue

        if method == "session/new":
            # Emit interleaved session/update notifications BEFORE responding!
            send_frame(
                {
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": session_id,
                        "update": {
                            "sessionUpdate": "available_commands_update",
                            "availableCommands": [{"name": "help"}],
                        },
                    },
                }
            )
            send_frame(
                {
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": session_id,
                        "update": {
                            "sessionUpdate": "usage_update",
                            "inputTokens": 5,
                            "outputTokens": 0,
                        },
                    },
                }
            )
            send_frame(
                {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {"sessionId": session_id, "modes": modes_payload},
                }
            )
            continue

        if method == "session/resume":
            session_id = params.get("sessionId") or session_id
            send_frame(
                {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {"sessionId": session_id, "modes": modes_payload},
                }
            )
            continue

        if method in ("session/set_model", "session/set_config_option", "session/set_mode"):
            send_frame({"jsonrpc": "2.0", "id": req_id, "result": {}})
            continue

        if method == "session/close":
            send_frame({"jsonrpc": "2.0", "id": req_id, "result": {}})
            continue

        if method == "session/prompt":
            if scenario == "basic":
                send_frame(
                    {
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": "Hello, "},
                            },
                        },
                    }
                )
                send_frame(
                    {
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "tool_call",
                                "toolCallId": "tc-1",
                                "title": "Run tests",
                                "kind": "execute",
                                "status": "in_progress",
                                "rawInput": {"command": ["cargo", "test"]},
                                "locations": [{"path": "src/lib.rs"}],
                            },
                        },
                    }
                )
                send_frame(
                    {
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "tool_call_update",
                                "toolCallId": "tc-1",
                                "title": "Run tests",
                                "kind": "execute",
                                "status": "completed",
                                "rawInput": {"command": ["cargo", "test"]},
                                "rawOutput": {"exitCode": 0},
                                "locations": [{"path": "src/lib.rs"}],
                            },
                        },
                    }
                )
                send_frame(
                    {
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": "world!"},
                            },
                        },
                    }
                )
                send_frame(
                    {
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "usage_update",
                                "inputTokens": 120,
                                "outputTokens": 45,
                                "cachedInputTokens": 10,
                                "reasoningTokens": 5,
                                "cost": {"amount": 0.0042, "currency": "USD"},
                            },
                        },
                    }
                )
                send_frame(
                    {
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": {"stopReason": "end_turn"},
                    }
                )
                continue

            if scenario == "interactive_permissions":
                # 1st permission: execute command (expect caller to allow)
                perm1_id = next_srv_id
                next_srv_id += 1
                send_frame(
                    {
                        "jsonrpc": "2.0",
                        "id": perm1_id,
                        "method": "session/request_permission",
                        "params": {
                            "sessionId": session_id,
                            "toolCall": {
                                "toolCallId": "tc-exec-1",
                                "title": "Run ls",
                                "kind": "execute",
                                "rawInput": {"command": ["ls", "-la"], "cwd": workspace},
                            },
                            "options": [
                                {"optionId": "acp_opt_allow_once_77", "name": "Allow once", "kind": "allow_once"},
                                {"optionId": "acp_opt_allow_always_77", "name": "Always allow", "kind": "allow_always"},
                                {"optionId": "acp_opt_reject_once_77", "name": "Reject once", "kind": "reject_once"},
                                {"optionId": "acp_opt_reject_always_77", "name": "Reject always", "kind": "reject_always"},
                            ],
                        },
                    }
                )
                resp1, _ = wait_for_response(perm1_id, log_path)
                log_record(log_path, {"event": "perm_reply", "perm": 1, "resp": resp1})

                # 2nd permission: edit file (expect caller to deny)
                perm2_id = next_srv_id
                next_srv_id += 1
                send_frame(
                    {
                        "jsonrpc": "2.0",
                        "id": perm2_id,
                        "method": "session/request_permission",
                        "params": {
                            "sessionId": session_id,
                            "toolCall": {
                                "toolCallId": "tc-edit-2",
                                "title": "Edit lib.rs",
                                "kind": "edit",
                                "locations": [{"path": f"{workspace}/src/lib.rs"}],
                            },
                            "options": [
                                {"optionId": "acp_opt_allow_88", "name": "Allow", "kind": "allow_once"},
                                {"optionId": "acp_opt_reject_88", "name": "Reject", "kind": "reject_once"},
                            ],
                        },
                    }
                )
                resp2, _ = wait_for_response(perm2_id, log_path)
                log_record(log_path, {"event": "perm_reply", "perm": 2, "resp": resp2})

                send_frame(
                    {
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": "interactive-done"},
                            },
                        },
                    }
                )
                send_frame(
                    {"jsonrpc": "2.0", "id": req_id, "result": {"stopReason": "end_turn"}}
                )
                continue

            if scenario == "policy_matrix":
                requests = [
                    (
                        "edit_in",
                        {
                            "toolCallId": "tc-edit-in",
                            "title": "Edit inside workspace",
                            "kind": "edit",
                            "locations": [{"path": f"{workspace}/src/lib.rs"}],
                        },
                    ),
                    (
                        "edit_out",
                        {
                            "toolCallId": "tc-edit-out",
                            "title": "Edit outside workspace",
                            "kind": "edit",
                            "locations": [{"path": "/etc/passwd"}],
                        },
                    ),
                    (
                        "exec_in",
                        {
                            "toolCallId": "tc-exec-in",
                            "title": "Execute command in workspace",
                            "kind": "execute",
                            "rawInput": {"command": ["echo", "hi"], "cwd": workspace},
                        },
                    ),
                    (
                        "perm_other",
                        {
                            "toolCallId": "tc-fetch",
                            "title": "Fetch external URL",
                            "kind": "fetch",
                        },
                    ),
                ]
                for tag, tool_call in requests:
                    pid = next_srv_id
                    next_srv_id += 1
                    send_frame(
                        {
                            "jsonrpc": "2.0",
                            "id": pid,
                            "method": "session/request_permission",
                            "params": {
                                "sessionId": session_id,
                                "toolCall": tool_call,
                                "options": [
                                    {"optionId": f"allow_{tag}", "name": "Allow", "kind": "allow_once"},
                                    {"optionId": f"reject_{tag}", "name": "Reject", "kind": "reject_once"},
                                ],
                            },
                        }
                    )
                    resp, _ = wait_for_response(pid, log_path)
                    log_record(log_path, {"event": "perm_reply", "tag": tag, "resp": resp})

                send_frame(
                    {
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": "matrix-complete"},
                            },
                        },
                    }
                )
                send_frame(
                    {"jsonrpc": "2.0", "id": req_id, "result": {"stopReason": "end_turn"}}
                )
                continue

            if scenario == "readonly_interactive":
                # First: edit (must be hard-denied automatically!)
                pid1 = next_srv_id
                next_srv_id += 1
                send_frame(
                    {
                        "jsonrpc": "2.0",
                        "id": pid1,
                        "method": "session/request_permission",
                        "params": {
                            "sessionId": session_id,
                            "toolCall": {
                                "toolCallId": "tc-ro-edit",
                                "title": "Write file",
                                "kind": "edit",
                                "locations": [{"path": f"{workspace}/src/lib.rs"}],
                            },
                            "options": [
                                {"optionId": "allow_ro_edit", "name": "Allow", "kind": "allow_once"},
                                {"optionId": "reject_ro_edit", "name": "Reject", "kind": "reject_once"},
                            ],
                        },
                    }
                )
                resp1, _ = wait_for_response(pid1, log_path)
                log_record(log_path, {"event": "perm_reply", "tag": "ro_edit", "resp": resp1})

                # Second: execute (under ReadOnly+Interactive, escalates to human!)
                pid2 = next_srv_id
                next_srv_id += 1
                send_frame(
                    {
                        "jsonrpc": "2.0",
                        "id": pid2,
                        "method": "session/request_permission",
                        "params": {
                            "sessionId": session_id,
                            "toolCall": {
                                "toolCallId": "tc-ro-exec",
                                "title": "Run git status",
                                "kind": "execute",
                                "rawInput": {"command": ["git", "status"], "cwd": workspace},
                            },
                            "options": [
                                {"optionId": "allow_ro_exec", "name": "Allow", "kind": "allow_once"},
                                {"optionId": "reject_ro_exec", "name": "Reject", "kind": "reject_once"},
                            ],
                        },
                    }
                )
                resp2, _ = wait_for_response(pid2, log_path)
                log_record(log_path, {"event": "perm_reply", "tag": "ro_exec", "resp": resp2})

                send_frame(
                    {
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": "readonly-ok"},
                            },
                        },
                    }
                )
                send_frame(
                    {"jsonrpc": "2.0", "id": req_id, "result": {"stopReason": "end_turn"}}
                )
                continue

            if scenario == "cancel_pending":
                pid = next_srv_id
                next_srv_id += 1
                send_frame(
                    {
                        "jsonrpc": "2.0",
                        "id": pid,
                        "method": "session/request_permission",
                        "params": {
                            "sessionId": session_id,
                            "toolCall": {
                                "toolCallId": "tc-cancel",
                                "title": "Run slow command",
                                "kind": "execute",
                                "rawInput": {"command": ["sleep", "30"], "cwd": workspace},
                            },
                            "options": [
                                {"optionId": "allow_cancel", "name": "Allow", "kind": "allow_once"},
                                {"optionId": "reject_cancel", "name": "Reject", "kind": "reject_once"},
                            ],
                        },
                    }
                )
                resp, saw_cancel = wait_for_response(pid, log_path)
                log_record(
                    log_path,
                    {"event": "cancel_reply", "saw_cancel": saw_cancel, "resp": resp},
                )
                send_frame(
                    {"jsonrpc": "2.0", "id": req_id, "result": {"stopReason": "cancelled"}}
                )
                continue

            if scenario == "unknown_and_crash":
                send_frame(
                    {
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "future_vendor_widget",
                                "widgetId": "w-42",
                            },
                        },
                    }
                )
                sys.stderr.write("fatal simulated crash\n")
                sys.stderr.flush()
                return 42

            if scenario == "ignore_term":
                # Keep running until SIGKILL arrives.
                time.sleep(30)
                return 0

            if scenario == "readonly_switch_mode":
                pid = next_srv_id
                next_srv_id += 1
                send_frame(
                    {
                        "jsonrpc": "2.0",
                        "id": pid,
                        "method": "session/request_permission",
                        "params": {
                            "sessionId": session_id,
                            "toolCall": {
                                "toolCallId": "tc-switch-mode",
                                "title": "switch_mode",
                                "kind": "switch_mode",
                                "rawInput": {"modeId": "auto"},
                            },
                            "options": [
                                {"optionId": "allow_mode_switch", "name": "Allow", "kind": "allow_once"},
                                {"optionId": "reject_mode_switch", "name": "Reject", "kind": "reject_once"},
                            ],
                        },
                    }
                )
                resp, _ = wait_for_response(pid, log_path)
                log_record(log_path, {"event": "perm_reply", "tag": "switch_mode", "resp": resp})
                send_frame(
                    {"jsonrpc": "2.0", "id": req_id, "result": {"stopReason": "end_turn"}}
                )
                continue

            if scenario == "scoped_allow_always_only":
                pid = next_srv_id
                next_srv_id += 1
                send_frame(
                    {
                        "jsonrpc": "2.0",
                        "id": pid,
                        "method": "session/request_permission",
                        "params": {
                            "sessionId": session_id,
                            "toolCall": {
                                "toolCallId": "tc-edit-always-only",
                                "title": "Edit inside workspace",
                                "kind": "edit",
                                "locations": [{"path": f"{workspace}/src/lib.rs"}],
                            },
                            "options": [
                                {"optionId": "acp_perm_forever", "name": "Allow always", "kind": "allow_always"},
                                {"optionId": "acp_reject_once", "name": "Reject once", "kind": "reject_once"},
                            ],
                        },
                    }
                )
                resp, _ = wait_for_response(pid, log_path)
                log_record(log_path, {"event": "perm_reply", "tag": "always_only", "resp": resp})
                send_frame(
                    {"jsonrpc": "2.0", "id": req_id, "result": {"stopReason": "end_turn"}}
                )
                continue

            send_frame(
                {"jsonrpc": "2.0", "id": req_id, "result": {"stopReason": "end_turn"}}
            )

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
