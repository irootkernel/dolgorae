#!/usr/bin/env python3
"""Opt-in EPIC-005 acceptance against the exact live Codex app-server."""

from __future__ import annotations

import argparse
import json
import os
import selectors
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

PINNED_VERSION = "codex-cli 0.149.0"
OPT_IN = "DOLGORAE_RUN_LIVE_ACCESS_SAFETY"
REQUIRED_REQUESTS = {
    "item/commandExecution/requestApproval",
    "item/fileChange/requestApproval",
}


class AppServer:
    def __init__(self, codex: Path, cwd: Path) -> None:
        self.process = subprocess.Popen(
            [str(codex), "app-server", "--listen", "stdio://"],
            cwd=cwd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            bufsize=0,
        )
        assert self.process.stdin is not None
        assert self.process.stdout is not None
        assert self.process.stderr is not None
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.process.stdout, selectors.EVENT_READ, "stdout")
        self.selector.register(self.process.stderr, selectors.EVENT_READ, "stderr")
        self.next_id = 1
        self.messages: list[dict[str, Any]] = []
        self.buffers = {"stdout": bytearray(), "stderr": bytearray()}

    def send(self, value: dict[str, Any]) -> None:
        assert self.process.stdin is not None
        self.process.stdin.write(
            (json.dumps(value, separators=(",", ":")) + "\n").encode()
        )
        self.process.stdin.flush()

    def request(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        request_id = self.next_id
        self.next_id += 1
        self.send({"id": request_id, "method": method, "params": params})
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            message = self.receive(deadline)
            if message.get("id") == request_id and "method" not in message:
                if "error" in message:
                    raise RuntimeError(f"{method} failed: {message['error']!r}")
                result = message.get("result")
                if not isinstance(result, dict):
                    raise RuntimeError(f"{method} returned a non-object result")
                return result
            self.handle_request(message)
        raise TimeoutError(f"timed out waiting for {method}")

    def receive(self, deadline: float) -> dict[str, Any]:
        while time.monotonic() < deadline:
            if b"\n" in self.buffers["stdout"]:
                encoded, _, remainder = self.buffers["stdout"].partition(b"\n")
                self.buffers["stdout"] = bytearray(remainder)
                value = json.loads(encoded)
                if not isinstance(value, dict):
                    raise RuntimeError("app-server emitted a non-object frame")
                self.messages.append(value)
                return value
            if self.process.poll() is not None:
                raise RuntimeError(
                    "app-server exited unexpectedly: "
                    + self.buffers["stderr"][-4096:].decode("utf-8", "replace")
                )
            timeout = max(0.0, min(1.0, deadline - time.monotonic()))
            for key, _ in self.selector.select(timeout):
                chunk = os.read(key.fileobj.fileno(), 65_536)
                if not chunk:
                    self.selector.unregister(key.fileobj)
                    continue
                if key.data == "stderr":
                    self.buffers["stderr"].extend(chunk)
                    continue
                self.buffers["stdout"].extend(chunk)
        raise TimeoutError("timed out waiting for an app-server frame")

    def handle_request(self, message: dict[str, Any]) -> None:
        method = message.get("method")
        if not isinstance(method, str) or "id" not in message:
            return
        if method in REQUIRED_REQUESTS:
            self.send({"id": message["id"], "result": {"decision": "accept"}})
            return
        if method == "item/tool/requestUserInput":
            questions = message.get("params", {}).get("questions", [])
            answers = {
                question["id"]: {"answers": ["continue"]}
                for question in questions
                if isinstance(question, dict) and isinstance(question.get("id"), str)
            }
            self.send({"id": message["id"], "result": {"answers": answers}})
            return
        self.send(
            {
                "id": message["id"],
                "error": {"code": -32601, "message": "unsupported by acceptance client"},
            }
        )

    def close(self) -> None:
        try:
            if self.process.stdin is not None:
                self.process.stdin.close()
            self.process.terminate()
            self.process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)
        finally:
            self.selector.close()


def exact_codex(candidate: Path) -> Path:
    codex = candidate.expanduser().resolve(strict=True)
    observed = subprocess.run(
        [str(codex), "--version"], check=True, capture_output=True, text=True
    ).stdout.strip()
    if observed != PINNED_VERSION:
        raise ValueError(f"expected {PINNED_VERSION!r}, observed {observed!r}")
    return codex


def schema_methods(codex: Path, root: Path) -> set[str]:
    bundle = root / "schema"
    subprocess.run(
        [str(codex), "app-server", "generate-json-schema", "--out", str(bundle)],
        check=True,
        capture_output=True,
    )
    text = "\n".join(
        path.read_text(encoding="utf-8") for path in sorted(bundle.rglob("*.json"))
    )
    return {method for method in REQUIRED_REQUESTS if method in text}


def default_model(server: AppServer) -> tuple[str, str]:
    cursor: str | None = None
    selected: tuple[str, str] | None = None
    while True:
        page = server.request("model/list", {"cursor": cursor, "limit": 100})
        for item in page.get("data", []):
            if not isinstance(item, dict) or item.get("isDefault") is not True:
                continue
            efforts = item.get("supportedReasoningEfforts", [])
            if not isinstance(item.get("model"), str) or not efforts:
                continue
            effort = efforts[0].get("reasoningEffort")
            if isinstance(effort, str):
                selected = (item["model"], effort)
        cursor = page.get("nextCursor")
        if cursor is None:
            break
        if not isinstance(cursor, str):
            raise RuntimeError("model/list returned an invalid cursor")
    if selected is None:
        raise RuntimeError("model/list returned no usable default model")
    return selected


def run_acceptance(codex: Path) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="dolgorae-live-access-") as temporary:
        root = Path(temporary)
        workspace = root / "workspace"
        workspace.mkdir(mode=0o700)
        os_temp = Path(tempfile.gettempdir()) / f"dolgorae-live-access-{os.getpid()}.txt"
        workspace_file = workspace / "writer.txt"
        server = AppServer(codex, workspace)
        try:
            initialized = server.request(
                "initialize",
                {
                    "clientInfo": {
                        "name": "dolgorae-live-acceptance",
                        "title": "Dolgorae live acceptance",
                        "version": "0.1.2",
                    },
                    "capabilities": {
                        "experimentalApi": False,
                        "optOutNotificationMethods": [],
                    },
                },
            )
            server.send({"method": "initialized", "params": {}})
            model, effort = default_model(server)
            thread = server.request(
                "thread/start",
                {
                    "cwd": str(workspace),
                    "model": model,
                    "sandbox": "read-only",
                    "approvalPolicy": "on-request",
                    "developerInstructions": (
                        "Follow the request exactly. Do not inspect unrelated files or use network."
                    ),
                },
            )
            thread_id = thread.get("thread", {}).get("id")
            if not isinstance(thread_id, str):
                raise RuntimeError("thread/start returned no thread identity")
            sandbox = {
                "type": "workspaceWrite",
                "writableRoots": [str(workspace)],
                "networkAccess": False,
                "excludeSlashTmp": False,
                "excludeTmpdirEnvVar": False,
            }

            def execute(prompt: str, policy: dict[str, Any]) -> None:
                turn = server.request(
                    "turn/start",
                    {
                        "threadId": thread_id,
                        "input": [{"type": "text", "text": prompt}],
                        "model": model,
                        "effort": effort,
                        "sandboxPolicy": policy,
                        "approvalPolicy": "on-request",
                    },
                )
                turn_id = turn.get("turn", {}).get("id")
                if not isinstance(turn_id, str):
                    raise RuntimeError("turn/start returned no turn identity")
                deadline = time.monotonic() + 660
                while time.monotonic() < deadline:
                    message = server.receive(deadline)
                    server.handle_request(message)
                    if message.get("method") != "turn/completed":
                        continue
                    observed = message.get("params", {}).get("turn", {})
                    if observed.get("id") != turn_id:
                        continue
                    if observed.get("status") != "completed":
                        raise RuntimeError(
                            f"turn did not complete successfully: {observed.get('status')!r}"
                        )
                    return
                raise TimeoutError("timed out waiting for turn completion")

            execute(
                "Perform exactly two writes, then answer done. Use apply_patch to create "
                f"{workspace_file} containing workspace-ok followed by a newline. Use a shell "
                f"command to create {os_temp} containing temp-ok followed by a newline. Do not "
                "read or change anything else.",
                sandbox,
            )
            if workspace_file.read_text(encoding="utf-8") != "workspace-ok\n":
                raise RuntimeError("workspace writer did not create the expected content")
            if os_temp.read_text(encoding="utf-8") != "temp-ok\n":
                raise RuntimeError("OS temporary-directory write did not create expected content")
            read_only = {"type": "readOnly", "networkAccess": False}
            execute(
                "Use only apply_patch to create approval-file.txt containing approved followed "
                "by a newline. Do not use a shell command. Request approval if required, then "
                "answer done.",
                read_only,
            )
            execute(
                "Use only a shell command to append command-approved followed by a newline to "
                f"{os_temp}. Do not use apply_patch. Request approval if required, then answer done.",
                read_only,
            )
            methods = {
                message["method"]
                for message in server.messages
                if isinstance(message.get("method"), str)
            }
            missing_live = REQUIRED_REQUESTS - methods
            if missing_live:
                raise RuntimeError(
                    f"live turns omitted interaction methods: {sorted(missing_live)!r}"
                )
            missing_schema = REQUIRED_REQUESTS - schema_methods(codex, root)
            if missing_schema:
                raise RuntimeError(
                    f"pinned live schema omitted interaction methods: {sorted(missing_schema)!r}"
                )
            return {
                "schema_version": 1,
                "codex_version": PINNED_VERSION,
                "codex_home": initialized.get("codexHome"),
                "model": model,
                "sandbox_policy": sandbox,
                "workspace_write": True,
                "os_temp_write": True,
                "live_request_methods": sorted(methods & REQUIRED_REQUESTS),
                "schema_interaction_methods": sorted(REQUIRED_REQUESTS),
            }
        finally:
            server.close()
            os_temp.unlink(missing_ok=True)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--codex", type=Path, required=True)
    args = parser.parse_args()
    if os.environ.get(OPT_IN) != "1":
        print(f"{OPT_IN}=1 is required for live acceptance", file=sys.stderr)
        return 2
    try:
        evidence = run_acceptance(exact_codex(args.codex))
    except (OSError, ValueError, RuntimeError, TimeoutError, subprocess.SubprocessError) as error:
        print(f"acceptance failed: {error}", file=sys.stderr)
        return 1
    json.dump(evidence, sys.stdout, sort_keys=True, separators=(",", ":"))
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
