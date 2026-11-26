#!/usr/bin/env python3
"""Offline fake `codex` executable for `driver-codex-app` integration tests.

Deliberately omits `"jsonrpc": "2.0"` from all stdout frames to match real
Codex 0.172.0 `app-server --stdio` behavior.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time


def log_event(record: dict) -> None:
    log_path = os.environ.get("FAKE_CODEX_LOG")
    if not log_path:
        return
    with open(log_path, "a", encoding="utf-8") as fh:
        fh.write(json.dumps(record) + "\n")


def emit(frame: dict) -> None:
    # Intentionally omit "jsonrpc" to verify tolerant parsing.
    sys.stdout.write(json.dumps(frame) + "\n")
    sys.stdout.flush()


def read_reply_for(expected_id: int) -> dict:
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        msg = json.loads(line)
        log_event({"dir": "in", "msg": msg})
        if msg.get("id") == expected_id and "method" not in msg:
            return msg
    return {}


def main() -> int:
    args = sys.argv[1:]
    if args == ["--version"]:
        sys.stdout.write("codex-cli 0.172.0-fake\n")
        sys.stdout.flush()
        return 0

    log_event({"dir": "argv", "argv": sys.argv})

    if args != ["app-server", "--stdio"]:
        sys.stderr.write(f"unexpected args: {args}\n")
        return 2

    thread_id = "thr-fake-001"
    turn_counter = 0
    workspace = "/tmp"
    grandchild: subprocess.Popen | None = None

    for raw in sys.stdin:
        raw = raw.strip()
        if not raw:
            continue
        msg = json.loads(raw)
        log_event({"dir": "in", "msg": msg})

        method = msg.get("method")
        req_id = msg.get("id")
        params = msg.get("params") or {}

        if method == "initialize":
            emit(
                {
                    "id": req_id,
                    "result": {
                        "userAgent": "codex/0.172.0-fake",
                        "codexHome": "/tmp/fake-codex",
                        "platformFamily": "unix",
                        "platformOs": "macos",
                    },
                }
            )
            continue

        if method == "initialized":
            continue

        if method == "thread/start":
            workspace = params.get("cwd", workspace)
            emit(
                {
                    "id": req_id,
                    "result": {
                        "thread": {"id": thread_id},
                        "cwd": workspace,
                        "sandbox": {"type": "workspaceWrite"},
                        "approvalPolicy": params.get("approvalPolicy", "on-request"),
                    },
                }
            )
            continue

        if method == "thread/resume":
            thread_id = params.get("threadId", thread_id)
            workspace = params.get("cwd", workspace)
            emit(
                {
                    "id": req_id,
                    "result": {
                        "thread": {"id": thread_id},
                        "cwd": workspace,
                    },
                }
            )
            continue

        if method == "turn/interrupt":
            target_turn = params.get("turnId", "turn-1")
            emit({"id": req_id, "result": {"interrupted": True}})
            emit(
                {
                    "method": "turn/completed",
                    "params": {
                        "threadId": thread_id,
                        "turn": {"id": target_turn, "status": "interrupted"},
                    },
                }
            )
            continue

        if method == "turn/start":
            turn_counter += 1
            turn_id = f"turn-{turn_counter}"
            inputs = params.get("input") or []
            prompt = inputs[0].get("text", "") if inputs else ""

            emit({"id": req_id, "result": {"turn": {"id": turn_id}}})
            emit(
                {
                    "method": "turn/started",
                    "params": {"threadId": thread_id, "turn": {"id": turn_id}},
                }
            )

            if prompt == "basic_stream":
                emit(
                    {
                        "method": "item/started",
                        "params": {
                            "turnId": turn_id,
                            "item": {
                                "id": "cmd-1",
                                "type": "commandExecution",
                                "command": ["echo", "hello"],
                            },
                        },
                    }
                )
                emit(
                    {
                        "method": "item/commandExecution/outputDelta",
                        "params": {
                            "turnId": turn_id,
                            "itemId": "cmd-1",
                            "delta": "hello\n",
                        },
                    }
                )
                emit(
                    {
                        "method": "item/completed",
                        "params": {
                            "turnId": turn_id,
                            "item": {
                                "id": "cmd-1",
                                "type": "commandExecution",
                                "command": ["echo", "hello"],
                                "exitCode": 0,
                                "aggregatedOutput": "hello\n",
                            },
                        },
                    }
                )
                emit(
                    {
                        "method": "item/completed",
                        "params": {
                            "turnId": turn_id,
                            "item": {
                                "id": "fc-1",
                                "type": "fileChange",
                                "changes": [{"path": "src/lib.rs", "kind": "modify"}],
                            },
                        },
                    }
                )
                emit(
                    {
                        "method": "item/agentMessage/delta",
                        "params": {"turnId": turn_id, "itemId": "msg-1", "delta": "{\"ok\":"},
                    }
                )
                emit(
                    {
                        "method": "item/agentMessage/delta",
                        "params": {"turnId": turn_id, "itemId": "msg-1", "delta": "true}"},
                    }
                )
                emit(
                    {
                        "method": "item/completed",
                        "params": {
                            "turnId": turn_id,
                            "item": {
                                "id": "msg-1",
                                "type": "agentMessage",
                                "text": "{\"ok\":true}",
                            },
                        },
                    }
                )
                emit(
                    {
                        "method": "thread/tokenUsage/updated",
                        "params": {
                            "turnId": turn_id,
                            "tokenUsage": {
                                "total": {
                                    "inputTokens": 120,
                                    "outputTokens": 30,
                                    "cachedInputTokens": 10,
                                    "reasoningOutputTokens": 5,
                                }
                            },
                        },
                    }
                )
                emit(
                    {
                        "method": "vendor/futureMetric",
                        "params": {"turnId": turn_id, "score": 99},
                    }
                )
                emit(
                    {
                        "method": "turn/completed",
                        "params": {"turn": {"id": turn_id, "status": "completed"}},
                    }
                )
                continue

            if prompt == "interactive_all":
                # 1. Command approval
                emit(
                    {
                        "id": 101,
                        "method": "item/commandExecution/requestApproval",
                        "params": {
                            "turnId": turn_id,
                            "command": ["cargo", "check"],
                            "cwd": workspace,
                            "reason": "Run check",
                        },
                    }
                )
                read_reply_for(101)

                # 2. File change approval (real Codex 0.172.0 schema: item/started carries changes, requestApproval has only itemId + grantRoot)
                emit(
                    {
                        "method": "item/started",
                        "params": {
                            "turnId": turn_id,
                            "item": {
                                "id": "fc-item-102",
                                "type": "fileChange",
                                "changes": [
                                    {
                                        "path": "src/main.rs",
                                        "kind": {"type": "update", "move_path": "src/app.rs"},
                                        "diff": "@@ -1 +1 @@",
                                    }
                                ],
                            },
                        },
                    }
                )
                emit(
                    {
                        "id": 102,
                        "method": "item/fileChange/requestApproval",
                        "params": {
                            "threadId": thread_id,
                            "turnId": turn_id,
                            "itemId": "fc-item-102",
                            "startedAtMs": 1700000000000,
                            "reason": "Update main",
                            "grantRoot": workspace,
                        },
                    }
                )
                read_reply_for(102)

                # 3. Permissions request matching Codex 0.172.0 schema (native grant stays driver-side)
                emit(
                    {
                        "id": 103,
                        "method": "item/permissions/requestApproval",
                        "params": {
                            "turnId": turn_id,
                            "reason": "Need network and fs read",
                            "permissions": {
                                "fileSystem": {"read": [workspace], "write": []},
                                "network": {"enabled": True},
                                "opaqueNativeToken": "SECRET_NATIVE_GRANT_PROFILE",
                            },
                        },
                    }
                )
                read_reply_for(103)

                # 4. Secret user question
                emit(
                    {
                        "id": 104,
                        "method": "item/tool/requestUserInput",
                        "params": {
                            "turnId": turn_id,
                            "questions": [
                                {
                                    "id": "q-secret",
                                    "header": "API Key",
                                    "question": "Enter token",
                                    "isSecret": True,
                                    "isOther": True,
                                    "options": [{"label": "use-env", "description": "Read env"}],
                                }
                            ],
                        },
                    }
                )
                read_reply_for(104)

                # 5. MCP elicitation
                emit(
                    {
                        "id": 105,
                        "method": "mcpServer/elicitation/request",
                        "params": {
                            "turnId": turn_id,
                            "serverName": "vault-mcp",
                            "message": "Confirm lease",
                            "requestedSchema": {"type": "object"},
                        },
                    }
                )
                read_reply_for(105)

                emit(
                    {
                        "method": "turn/completed",
                        "params": {"turn": {"id": turn_id, "status": "completed"}},
                    }
                )
                continue

            if prompt == "auto_scoped_mix":
                # Emit item/started for real Codex 0.172.0 fileChange schema (PatchChangeKind object + cached by itemId)
                for item_id, change_obj in [
                    ("fc-201", {"path": f"{workspace}/inside.rs", "kind": {"type": "add"}}),
                    ("fc-202", {"path": "../escape.rs", "kind": {"type": "update"}}),
                    ("fc-209", {"path": f"{workspace}/inside_bad_root.rs", "kind": {"type": "update"}}),
                    (
                        "fc-210",
                        {
                            "path": f"{workspace}/a.rs",
                            "kind": {"type": "update", "move_path": "/etc/escaped.rs"},
                        },
                    ),
                ]:
                    emit(
                        {
                            "method": "item/started",
                            "params": {
                                "turnId": turn_id,
                                "item": {
                                    "id": item_id,
                                    "type": "fileChange",
                                    "changes": [change_obj],
                                },
                            },
                        }
                    )
                requests = [
                    (
                        201,
                        "item/fileChange/requestApproval",
                        {
                            "threadId": thread_id,
                            "turnId": turn_id,
                            "itemId": "fc-201",
                            "startedAtMs": 1700000000001,
                            "grantRoot": workspace,
                        },
                    ),
                    (
                        202,
                        "item/fileChange/requestApproval",
                        {
                            "threadId": thread_id,
                            "turnId": turn_id,
                            "itemId": "fc-202",
                            "startedAtMs": 1700000000002,
                        },
                    ),
                    (
                        203,
                        "item/commandExecution/requestApproval",
                        {
                            "turnId": turn_id,
                            "command": ["ls"],
                            "cwd": workspace,
                        },
                    ),
                    (
                        204,
                        "item/commandExecution/requestApproval",
                        {
                            "turnId": turn_id,
                            "command": ["cat", "/etc/hosts"],
                            "cwd": "/etc",
                        },
                    ),
                    (
                        205,
                        "item/permissions/requestApproval",
                        {
                            "turnId": turn_id,
                            "permissions": {"network": {"enabled": True}},
                        },
                    ),
                    (
                        206,
                        "mcpServer/elicitation/request",
                        {
                            "turnId": turn_id,
                            "serverName": "srv",
                            "message": "confirm",
                        },
                    ),
                    (
                        207,
                        "item/tool/requestUserInput",
                        {
                            "turnId": turn_id,
                            "questions": [{"id": "q1", "question": "continue?"}],
                        },
                    ),
                    (
                        208,
                        "item/commandExecution/requestApproval",
                        {
                            "turnId": turn_id,
                            "command": ["curl", "https://api.example.com"],
                            "cwd": workspace,
                            "reason": "Outbound network request",
                            "networkApprovalContext": {
                                "host": "api.example.com",
                                "protocol": "https",
                            },
                        },
                    ),
                    (
                        209,
                        "item/fileChange/requestApproval",
                        {
                            "threadId": thread_id,
                            "turnId": turn_id,
                            "itemId": "fc-209",
                            "startedAtMs": 1700000000009,
                            "grantRoot": "/etc",
                        },
                    ),
                    (
                        210,
                        "item/fileChange/requestApproval",
                        {
                            "threadId": thread_id,
                            "turnId": turn_id,
                            "itemId": "fc-210",
                            "startedAtMs": 1700000000010,
                        },
                    ),
                ]
                for rid, rmethod, rparams in requests:
                    emit({"id": rid, "method": rmethod, "params": rparams})
                    read_reply_for(rid)
                emit(
                    {
                        "method": "turn/completed",
                        "params": {"turn": {"id": turn_id, "status": "completed"}},
                    }
                )
                continue

            if prompt == "readonly_file":
                emit(
                    {
                        "method": "item/started",
                        "params": {
                            "turnId": turn_id,
                            "item": {
                                "id": "fc-301",
                                "type": "fileChange",
                                "changes": [{"path": f"{workspace}/ok.rs", "kind": "modify"}],
                            },
                        },
                    }
                )
                emit(
                    {
                        "id": 301,
                        "method": "item/fileChange/requestApproval",
                        "params": {
                            "threadId": thread_id,
                            "turnId": turn_id,
                            "itemId": "fc-301",
                            "startedAtMs": 1700000000301,
                        },
                    }
                )
                read_reply_for(301)
                # Also verify ReadOnly hard-denies filesystem write permission escalation
                emit(
                    {
                        "id": 302,
                        "method": "item/permissions/requestApproval",
                        "params": {
                            "turnId": turn_id,
                            "reason": "Request write permission",
                            "permissions": {
                                "fileSystem": {"read": [workspace], "write": [workspace]},
                                "network": {"enabled": False},
                            },
                        },
                    }
                )
                read_reply_for(302)
                # Also verify ReadOnly hard-denies fileSystem.entries write permission escalation
                emit(
                    {
                        "id": 304,
                        "method": "item/permissions/requestApproval",
                        "params": {
                            "turnId": turn_id,
                            "reason": "Request write entry permission",
                            "permissions": {
                                "fileSystem": {
                                    "entries": [{"path": workspace, "access": "write"}]
                                },
                                "network": {"enabled": False},
                            },
                        },
                    }
                )
                read_reply_for(304)
                # Also verify unknown server request receives immediate JSON-RPC error without hanging
                emit(
                    {
                        "id": 303,
                        "method": "vendor/unknownReverseRequest",
                        "params": {"turnId": turn_id, "detail": "future"},
                    }
                )
                read_reply_for(303)
                emit(
                    {
                        "method": "turn/completed",
                        "params": {"turn": {"id": turn_id, "status": "completed"}},
                    }
                )
                continue

            if prompt == "server_resolved_race":
                emit(
                    {
                        "id": 901,
                        "method": "item/commandExecution/requestApproval",
                        "params": {
                            "turnId": turn_id,
                            "command": ["echo", "race"],
                            "cwd": workspace,
                        },
                    }
                )
                time.sleep(0.05)
                emit(
                    {
                        "method": "serverRequest/resolved",
                        "params": {"requestId": 901, "turnId": turn_id},
                    }
                )
                emit(
                    {
                        "method": "turn/completed",
                        "params": {"turn": {"id": turn_id, "status": "completed"}},
                    }
                )
                continue

            if prompt == "interrupt_me":
                # Keep reading stdin until turn/interrupt arrives.
                continue

            if prompt == "crash_now":
                sys.stderr.write("fatal: simulated codex crash\n")
                sys.stderr.flush()
                emit(
                    {
                        "method": "error",
                        "params": {
                            "turnId": turn_id,
                            "error": {"message": "simulated upstream failure"},
                            "willRetry": False,
                        },
                    }
                )
                return 42

            if prompt == "spawn_grandchild":
                grandchild = subprocess.Popen(
                    [sys.executable, "-c", "import time; time.sleep(60)"],
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )
                emit(
                    {
                        "method": "item/completed",
                        "params": {
                            "turnId": turn_id,
                            "item": {
                                "id": "gc-msg",
                                "type": "agentMessage",
                                "text": f"GRANDCHILD_PID={grandchild.pid}",
                            },
                        },
                    }
                )
                emit(
                    {
                        "method": "turn/completed",
                        "params": {"turn": {"id": turn_id, "status": "completed"}},
                    }
                )
                continue

            emit(
                {
                    "method": "turn/completed",
                    "params": {"turn": {"id": turn_id, "status": "completed"}},
                }
            )

    if grandchild is not None:
        grandchild.terminate()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
