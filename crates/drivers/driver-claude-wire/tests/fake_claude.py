#!/usr/bin/env python3
"""Offline fake Claude Code stream-json stdio server for driver-claude-wire tests."""

from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
import time
from pathlib import Path


def emit(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def append_stdin_log(log_path: Path, raw_line: str) -> None:
    with log_path.open("a", encoding="utf-8") as fh:
        fh.write(raw_line + "\n")
        fh.flush()


def find_flag_value(argv: list[str], flag: str) -> str | None:
    for idx, arg in enumerate(argv):
        if arg == flag and idx + 1 < len(argv):
            return argv[idx + 1]
    return None


def main() -> int:
    argv = sys.argv[1:]
    if "--version" in argv:
        if os.environ.get("FAKE_CLAUDE_FAIL_VERSION") == "1":
            sys.stderr.write("version check failed\n")
            return 1
        sys.stdout.write("2.1.332 (Fake Claude Code)\n")
        sys.stdout.flush()
        return 0

    cwd = Path.cwd()
    mode_path = cwd / ".fake_claude_mode.json"
    mode = {}
    if mode_path.is_file():
        mode = json.loads(mode_path.read_text(encoding="utf-8"))

    init_mode = mode.get("init_mode", "on_start")
    session_id = (
        mode.get("session_id")
        or find_flag_value(argv, "--resume")
        or find_flag_value(argv, "--session-id")
        or "sess-fake-default"
    )
    fail_init = bool(mode.get("fail_init", False))

    mcp_path_str = find_flag_value(argv, "--mcp-config")
    mcp_info: dict = {}
    if mcp_path_str:
        mcp_file = Path(mcp_path_str)
        mcp_dir = mcp_file.parent
        file_mode = stat.S_IMODE(mcp_file.stat().st_mode) if mcp_file.exists() else None
        dir_mode = stat.S_IMODE(mcp_dir.stat().st_mode) if mcp_dir.exists() else None
        mcp_json = json.loads(mcp_file.read_text(encoding="utf-8")) if mcp_file.exists() else None
        mcp_info = {
            "mcp_config_path": str(mcp_file),
            "mcp_dir_path": str(mcp_dir),
            "mcp_file_mode": file_mode,
            "mcp_dir_mode": dir_mode,
            "mcp_config_json": mcp_json,
        }

    startup = {
        "argv": argv,
        "pid": os.getpid(),
        "pgid": os.getpgrp(),
        "session_id": session_id,
        **mcp_info,
    }
    (cwd / ".fake_claude_startup.json").write_text(
        json.dumps(startup, indent=2), encoding="utf-8"
    )

    stdin_log = cwd / ".fake_claude_stdin.jsonl"
    init_emitted = False

    for raw in sys.stdin:
        line = raw.strip()
        if not line:
            continue
        append_stdin_log(stdin_log, line)
        msg = json.loads(line)
        msg_type = msg.get("type")

        if msg_type == "control_request":
            req_id = msg.get("request_id", "")
            req = msg.get("request", {})
            subtype = req.get("subtype")
            if subtype == "initialize":
                if fail_init:
                    emit(
                        {
                            "type": "control_response",
                            "response": {
                                "subtype": "error",
                                "request_id": req_id,
                                "error": "forced init failure",
                            },
                        }
                    )
                    continue
                if init_mode == "on_start":
                    emit({"type": "system", "subtype": "init", "session_id": session_id})
                    init_emitted = True
                    emit(
                        {
                            "type": "control_response",
                            "response": {
                                "subtype": "success",
                                "request_id": req_id,
                                "response": {"capabilities": {}},
                            },
                        }
                    )
                elif init_mode == "in_control_response":
                    emit(
                        {
                            "type": "control_response",
                            "response": {
                                "subtype": "success",
                                "request_id": req_id,
                                "response": {"session_id": session_id},
                            },
                        }
                    )
                else:
                    # init_mode == "on_first_turn"
                    emit(
                        {
                            "type": "control_response",
                            "response": {
                                "subtype": "success",
                                "request_id": req_id,
                                "response": {"capabilities": {}},
                            },
                        }
                    )
            elif subtype == "interrupt":
                emit(
                    {
                        "type": "control_response",
                        "response": {
                            "subtype": "success",
                            "request_id": req_id,
                            "response": {"still_queued": []},
                        },
                    }
                )
            continue

        if msg_type == "user":
            if not init_emitted and init_mode == "on_first_turn":
                emit({"type": "system", "subtype": "init", "session_id": session_id})
                init_emitted = True

            content = msg.get("message", {}).get("content", "")
            # Replay user turn and send keep_alive like real Claude Code
            emit(
                {
                    "type": "user",
                    "message": {"role": "user", "content": content},
                    "session_id": session_id,
                }
            )
            emit({"type": "keep_alive"})

            if content.startswith("SCENARIO:stream_full"):
                emit(
                    {
                        "type": "stream_event",
                        "event": {"type": "content_block_start", "index": 0},
                        "session_id": session_id,
                    }
                )
                emit(
                    {
                        "type": "stream_event",
                        "event": {
                            "type": "content_block_delta",
                            "index": 0,
                            "delta": {"type": "text_delta", "text": "Hello, "},
                        },
                        "session_id": session_id,
                    }
                )
                emit(
                    {
                        "type": "stream_event",
                        "event": {
                            "type": "content_block_delta",
                            "index": 0,
                            "delta": {"type": "text_delta", "text": "world!"},
                        },
                        "session_id": session_id,
                    }
                )
                emit(
                    {
                        "type": "assistant",
                        "message": {
                            "role": "assistant",
                            "content": [
                                {"type": "text", "text": "Hello, world!"},
                                {
                                    "type": "tool_use",
                                    "id": "toolu_bash_1",
                                    "name": "Bash",
                                    "input": {"command": "cargo test", "description": "run tests"},
                                },
                                {
                                    "type": "tool_use",
                                    "id": "toolu_edit_1",
                                    "name": "Edit",
                                    "input": {"file_path": "src/lib.rs"},
                                },
                            ],
                        },
                        "session_id": session_id,
                    }
                )
                emit(
                    {
                        "type": "user",
                        "message": {
                            "role": "user",
                            "content": [
                                {
                                    "type": "tool_result",
                                    "tool_use_id": "toolu_bash_1",
                                    "content": "ok 42 tests",
                                    "is_error": False,
                                },
                                {
                                    "type": "tool_result",
                                    "tool_use_id": "toolu_edit_1",
                                    "content": "permission denied",
                                    "is_error": True,
                                },
                            ],
                        },
                        "session_id": session_id,
                    }
                )
                emit({"type": "vendor_future_event", "version": 99})
                emit(
                    {
                        "type": "result",
                        "subtype": "success",
                        "is_error": False,
                        "result": "Hello, world!",
                        "structured_output": {"ok": True, "score": 99},
                        "usage": {
                            "input_tokens": 120,
                            "output_tokens": 45,
                            "cache_read_input_tokens": 15,
                        },
                        "total_cost_usd": 0.0042,
                        "session_id": session_id,
                    }
                )
            elif content.startswith("SCENARIO:readonly_review"):
                emit(
                    {
                        "type": "stream_event",
                        "event": {
                            "type": "content_block_delta",
                            "index": 0,
                            "delta": {"type": "text_delta", "text": "Review complete: LGTM"},
                        },
                        "session_id": session_id,
                    }
                )
                emit(
                    {
                        "type": "result",
                        "subtype": "success",
                        "is_error": False,
                        "result": "Review complete: LGTM",
                        "session_id": session_id,
                    }
                )
            elif content.startswith("SCENARIO:can_use_tool:"):
                rest = content[len("SCENARIO:can_use_tool:") :]
                tool_name, payload_raw = rest.split(":", 1)
                tool_input = json.loads(payload_raw)
                native_id = "native-perm-1"
                emit(
                    {
                        "type": "control_request",
                        "request_id": native_id,
                        "request": {
                            "subtype": "can_use_tool",
                            "tool_name": tool_name,
                            "input": tool_input,
                            "tool_use_id": "toolu_perm_1",
                            "decision_reason": f"approve {tool_name}",
                        },
                    }
                )
                # Wait for control_response matching native_id
                perm_resp = None
                for resp_raw in sys.stdin:
                    resp_line = resp_raw.strip()
                    if not resp_line:
                        continue
                    append_stdin_log(stdin_log, resp_line)
                    resp_msg = json.loads(resp_line)
                    if resp_msg.get("type") == "control_response":
                        perm_resp = resp_msg
                        break
                (cwd / ".fake_claude_last_perm_response.json").write_text(
                    json.dumps(perm_resp, indent=2), encoding="utf-8"
                )
                emit(
                    {
                        "type": "result",
                        "subtype": "success",
                        "is_error": False,
                        "result": json.dumps(perm_resp),
                        "session_id": session_id,
                    }
                )
            elif content.startswith("SCENARIO:unknown_control_request"):
                native_id = "native-unk-1"
                emit(
                    {
                        "type": "control_request",
                        "request_id": native_id,
                        "request": {
                            "subtype": "future_hook_probe",
                            "detail": "check",
                        },
                    }
                )
                unk_resp = None
                for resp_raw in sys.stdin:
                    resp_line = resp_raw.strip()
                    if not resp_line:
                        continue
                    append_stdin_log(stdin_log, resp_line)
                    resp_msg = json.loads(resp_line)
                    if resp_msg.get("type") == "control_response":
                        unk_resp = resp_msg
                        break
                (cwd / ".fake_claude_last_perm_response.json").write_text(
                    json.dumps(unk_resp, indent=2), encoding="utf-8"
                )
                emit(
                    {
                        "type": "result",
                        "subtype": "success",
                        "is_error": False,
                        "result": "unknown control handled",
                        "session_id": session_id,
                    }
                )
            elif content.startswith("SCENARIO:cancel_request"):
                native_id = "native-cancel-1"
                emit(
                    {
                        "type": "control_request",
                        "request_id": native_id,
                        "request": {
                            "subtype": "can_use_tool",
                            "tool_name": "Bash",
                            "input": {"command": "echo hi"},
                            "tool_use_id": "toolu_cancel_1",
                        },
                    }
                )
                time.sleep(0.05)
                emit({"type": "control_cancel_request", "request_id": native_id})
                emit(
                    {
                        "type": "result",
                        "subtype": "success",
                        "is_error": False,
                        "result": "request cancelled by provider",
                        "session_id": session_id,
                    }
                )
            elif content.startswith("SCENARIO:interrupt_wait"):
                emit(
                    {
                        "type": "stream_event",
                        "event": {
                            "type": "content_block_delta",
                            "index": 0,
                            "delta": {"type": "text_delta", "text": "working..."},
                        },
                        "session_id": session_id,
                    }
                )
                for next_raw in sys.stdin:
                    next_line = next_raw.strip()
                    if not next_line:
                        continue
                    append_stdin_log(stdin_log, next_line)
                    next_msg = json.loads(next_line)
                    if (
                        next_msg.get("type") == "control_request"
                        and next_msg.get("request", {}).get("subtype") == "interrupt"
                    ):
                        emit(
                            {
                                "type": "control_response",
                                "response": {
                                    "subtype": "success",
                                    "request_id": next_msg.get("request_id", ""),
                                    "response": {"still_queued": []},
                                },
                            }
                        )
                        emit(
                            {
                                "type": "result",
                                "subtype": "interrupted",
                                "is_error": True,
                                "error": "interrupted by host",
                                "session_id": session_id,
                            }
                        )
                        break
            elif content.startswith("SCENARIO:crash_mid_turn"):
                emit(
                    {
                        "type": "stream_event",
                        "event": {
                            "type": "content_block_delta",
                            "index": 0,
                            "delta": {"type": "text_delta", "text": "about to crash"},
                        },
                        "session_id": session_id,
                    }
                )
                sys.stderr.write("fatal crash boom\n")
                sys.stderr.flush()
                return 42
            elif content.startswith("SCENARIO:spawn_grandchild"):
                gc = subprocess.Popen(
                    [sys.executable, "-c", "import time; time.sleep(60)"],
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )
                (cwd / ".fake_claude_grandchild.pid").write_text(
                    str(gc.pid), encoding="utf-8"
                )
                emit(
                    {
                        "type": "result",
                        "subtype": "success",
                        "is_error": False,
                        "result": f"spawned:{gc.pid}",
                        "session_id": session_id,
                    }
                )
            else:
                emit(
                    {
                        "type": "result",
                        "subtype": "success",
                        "is_error": False,
                        "result": f"echo:{content}",
                        "session_id": session_id,
                    }
                )

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
