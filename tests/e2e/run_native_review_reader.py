#!/usr/bin/env python3
"""Opt-in two-Turn native reader/history fixture using only loopback responses.

Rust owns target admission, candidate bytes, receipts, Turn correlation and
verdict checks. This harness supplies calls and checks correlated native returns.
It never fabricates a successful candidate read or enables production support.
"""
from __future__ import annotations

import argparse
import base64
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

from run_live_instruction_discovery import (
    ProbeCheckFailure, checked_codex, executable_identity, isolated_environment, private_file,
)
from run_native_workspace_policy_fixture import ScriptedResponses, code_call, code_results, fixture_config, terminal_output

OPT_IN = "DOLGORAE_RUN_NATIVE_REVIEW_READER"
CODEX_VERSION = "0.167.2"
CODEX_SHA256 = "97bc82fd46f117cf10efb95e6ff0c81dc4784f5797bcdc2cb75093ba09e4def0"


class ReaderResponses(ScriptedResponses):
    """At most two exact calls and four POSTs, with no retry or account."""

    def respond(self, request: Any) -> list[dict[str, Any]]:
        with self.lock:
            self.posts += 1
            if self.failure is not None or self.posts > 4:
                raise ProbeCheckFailure("fixture_request_outside_budget")
            if not isinstance(request, dict) or not isinstance(request.get("input"), list):
                raise ProbeCheckFailure("fixture_input_invalid")
            inputs = request["input"]
            step = (self.posts + 1) // 2
            call_id = f"bounded-review-call-{step}"
            if self.posts % 2:
                users = [item for item in inputs if isinstance(item, dict)
                         and item.get("type") == "message" and item.get("role") == "user"]
                if len([user for user in users if any(isinstance(part, dict) and part.get("type") == "input_text"
                           and f"fixture-step-{step}:" in part.get("text", "") for part in user.get("content", []))]) != 1:
                    raise ProbeCheckFailure("fixture_task_mismatch")
                if step == 1:
                    item = code_call(request, call_id, [(call_id, "dolgorae_candidate_read", {
                                "side": "after", "path": "file.txt", "byte_offset": 0,
                                "byte_length": 8})])
                else:
                    item = code_call(request, call_id, [(call_id, "exec_command", {
                                "cmd": "cat file.txt", "yield_time_ms": 1000})])
            else:
                output = code_results(request, call_id, [call_id])[call_id]
                if step == 1:
                    if not isinstance(output, str) or len(output) > 65536:
                        raise ProbeCheckFailure("fixture_candidate_return_missing")
                    result = json.loads(output)
                    if (base64.b64decode(result.get("content", ""), validate=True) != b"indexed\n"
                            or result.get("receipt", {}).get("side") != "after"
                            or result["receipt"].get("byte_length") != 8):
                        raise ProbeCheckFailure("fixture_index_candidate_mismatch")
                else:
                    code, text = terminal_output(output)
                    if code != 0 or text.strip() != "worktree":
                        raise ProbeCheckFailure("fixture_native_worktree_read_unverified")
                self.steps = step
                item = {"type": "message", "role": "assistant", "id": f"review-message-{step}",
                        "content": [{"type": "output_text", "text": "Done."}]}
            identity = f"review-response-{self.posts}"
            return [{"type": "response.created", "response": {"id": identity}},
                    {"type": "response.output_item.done", "item": item},
                    {"type": "response.completed", "response": {"id": identity, "usage": {
                        "input_tokens": 0, "input_tokens_details": None, "output_tokens": 0,
                        "output_tokens_details": None, "total_tokens": 0}}}]


def campaign(driver: Path, codex: Path) -> dict[str, object]:
    if os.environ.get(OPT_IN) != "1":
        raise ValueError("explicit native review-reader opt-in required")
    if executable_identity(codex)["sha256"] != CODEX_SHA256:
        raise ValueError("native reader fixture requires the exact pinned executable")
    root = Path(tempfile.mkdtemp(prefix="drr-", dir=os.environ["TMPDIR"])).resolve()
    root.chmod(0o700)
    fixture: ReaderResponses | None = None
    child: subprocess.Popen[str] | None = None
    passed = False
    try:
        for name in ("home", "profile", "tmp", "config", "cache", "workspace", "state"):
            (root / name).mkdir(mode=0o700)
        environment = isolated_environment(root)
        environment[OPT_IN] = "1"
        checked_codex(codex, f"codex-cli {CODEX_VERSION}", environment)
        environment.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null")
        def git(*args: str) -> None:
            subprocess.run(["/usr/bin/git", "-C", str(root / "workspace"), *args],
                           env=environment, check=True, capture_output=True, timeout=10)
        git("init", "-q")
        git("config", "user.name", "Native reader fixture")
        git("config", "user.email", "fixture@example.invalid")
        target = root / "workspace" / "file.txt"
        target.write_bytes(b"committed\n")
        git("add", "file.txt")
        git("commit", "-qm", "fixture")
        target.write_bytes(b"indexed\n")
        git("add", "file.txt")
        target.write_bytes(b"worktree\n")
        fixture = ReaderResponses()
        private_file(root / "profile" / "config.toml", b'model = "gpt-5.6-luna"\nmodel_reasoning_effort = "low"\n' + fixture_config(fixture.base_url))
        private_file(root / "fixture-owned", b"native-review-reader/v1\n")
        child = subprocess.Popen([str(driver), str(root), str(codex)], env=environment,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 text=True, start_new_session=True)
        stdout, stderr = child.communicate(timeout=240)
        private_file(root / "driver.stdout", stdout.encode())
        private_file(root / "driver.stderr", stderr.encode())
        if child.returncode or fixture.failure or fixture.posts != 4 or fixture.steps != 2:
            raise ValueError("native reader fixture failed; no automatic retry")
        result = json.loads(stdout)
        if result.get("schema") != "dolgorae.native-review-reader/v1" or result.get("passed") is not True:
            raise ValueError("native reader observations invalid")
        result.update(codex_version=CODEX_VERSION, executable_sha256=CODEX_SHA256,
                      external_provider_requests=0, loopback_posts=4,
                      real_account_or_credentials_used=False)
        passed = True
        return result
    finally:
        if child is not None:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            child.wait(timeout=5)
        if fixture is not None:
            fixture.close()
        if passed:
            shutil.rmtree(root)
        else:
            print(json.dumps({"schema": "dolgorae.native-review-reader-failure/v1",
                              "owned_fixture_retained": str(root)}), file=sys.stderr)


def main() -> int:
    if os.environ.get(OPT_IN) != "1":
        print("native reader fixture requires explicit opt-in")
        return 2
    parser = argparse.ArgumentParser()
    parser.add_argument("--driver", type=Path, required=True)
    parser.add_argument("--codex", type=Path, required=True)
    args = parser.parse_args()
    try:
        result = campaign(args.driver.resolve(strict=True), args.codex.resolve(strict=True))
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
        print(json.dumps({"schema": "dolgorae.native-review-reader-failure/v1", "passed": False}))
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
