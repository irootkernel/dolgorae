#!/usr/bin/env python3
"""Offline checks of the opt-in boundary and bounded turnless probe."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from unittest.mock import patch

import run_live_workspace_startup as probe
from run_live_instruction_discovery import ProbeCheckFailure
from test_codex_compatibility import BufferedServer


class BufferedTurnless(probe.TurnlessServer, BufferedServer):
    def __init__(self, frames: list[dict]) -> None:
        BufferedServer.__init__(self, frames)


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="dolgorae-turnless-offline-") as temporary:
        root = Path(temporary)
        invoked = root / "invoked"
        executable = root / "codex"
        executable.write_text(
            "#!/usr/bin/env python3\nimport pathlib,sys\n"
            f"pathlib.Path({str(invoked)!r}).write_text(' '.join(sys.argv[1:]))\n"
            "print('codex-cli 0.159.3')\n"
        )
        executable.chmod(0o700)
        environment = {key: value for key, value in os.environ.items() if key != probe.OPT_IN}
        refusal = subprocess.run([
            sys.executable, str(Path(probe.__file__)), "--codex", str(executable),
            "--expected-version", "codex-cli 0.159.3",
        ], env=environment, text=True, capture_output=True, timeout=15)
        assert refusal.returncode == 2 and not invoked.exists()
        for method in ("turn/start", "command/exec", "model/list"):
            server = BufferedTurnless([])
            try:
                server.request(method, {})
            except ProbeCheckFailure as error:
                assert error.kind == "request_outside_turnless_probe"
            else:
                raise AssertionError("probe admitted an effectful or unrelated request")
            assert server.sent == []
        workspace = root / "workspace"
        for early in (True, False):
            notification = {"method": "thread/settings/updated", "params": {
                "threadId": "selected-thread", "threadSettings": {
                    "cwd": str(workspace), "approvalPolicy": "never",
                    "sandboxPolicy": {"type": "readOnly", "networkAccess": False},
                },
            }}
            response = {"id": 1, "result": {}}
            frames = [notification, response] if early else [response, notification]
            server = BufferedTurnless(frames)
            probe.update(server, "selected-thread", workspace, False)
            assert server.sent[0]["method"] == "thread/settings/update"
        for roots in ([], [str(workspace)]):
            probe.check_policy({"type": "workspaceWrite", "writableRoots": roots,
                                "networkAccess": False}, True, workspace)
        for policy in ({"type": "workspaceWrite", "writableRoots": ["/foreign"]},
                       {"type": "workspaceWrite", "writableRoots": [str(workspace), "/foreign"]},
                       {"type": "workspaceWrite", "writableRoots": [str(workspace)],
                        "networkAccess": True}, {"type": "dangerFullAccess"}):
            try:
                probe.check_policy(policy, True, workspace)
            except ProbeCheckFailure:
                pass
            else:
                raise AssertionError("unsafe or incomplete policy metadata was accepted")

        class FakeServer:
            def __init__(self, codex: Path, cwd: Path, env: dict[str, str]) -> None:
                assert codex == executable and env["CODEX_HOME"] == str(cwd)
                assert env["HOME"] != os.environ.get("HOME")
                assert not (cwd / "auth.json").exists()
                self.messages: list[dict] = []
                self.profile = cwd
                self.workspace: str | None = None
                self.closed = False
                servers.append(self)

            def send(self, value: dict) -> None:
                assert value["method"] == "initialized"

            def close(self) -> None:
                self.closed = True

            def request(self, method: str, params: dict) -> dict:
                self.failed_operation = method
                calls.append(method)
                if method == "initialize":
                    return {"codexHome": str(self.profile)}
                if method == "thread/start":
                    self.workspace = params["cwd"]
                    return {"thread": {"id": "thread", "turns": []},
                            "cwd": self.workspace, "model": probe.TARGET_MODEL,
                            "sandbox": {"type": "workspaceWrite", "writableRoots": [self.workspace],
                                        "networkAccess": False}}
                if method == "thread/settings/update":
                    assert params["threadId"] == "thread"
                    self.messages.append({"method": "thread/settings/updated", "params": {
                        "threadId": "thread", "threadSettings": {
                            "cwd": self.workspace, "approvalPolicy": "never",
                            "sandboxPolicy": params["sandboxPolicy"],
                        },
                    }})
                    return {}
                assert method == "thread/resume" and params["threadId"] == "thread"
                assert servers[0].closed
                raise ProbeCheckFailure("rpc_rejected", {"rpc_error_code": -32600})

        servers: list[FakeServer] = []
        calls: list[str] = []
        with patch.object(probe, "TurnlessServer", FakeServer):
            result = probe.campaign(executable, "codex-cli 0.159.3")
        assert result["outcome"] == "metadata_observed"
        assert result["effective_policy_enforcement"] == "unverified"
        assert result["restart"] == "resume_rpc_rejected"
        assert result["rpc_error_code"] == -32600
        assert result["model_turns_requested"] == 0 and result["credential_access"] is False
        assert calls.count("thread/start") == 1 and calls.count("thread/settings/update") == 3
        assert all(server.closed for server in servers)
        assert invoked.read_text() == "--version"
        json.dumps(result)

        class ImplicitWorkspace(FakeServer):
            def request(self, method: str, params: dict) -> dict:
                reply = super().request(method, params)
                if method == "thread/start":
                    reply["sandbox"]["writableRoots"] = []
                elif method == "thread/settings/update" and params["sandboxPolicy"]["type"] == "workspaceWrite":
                    self.messages[-1]["params"]["threadSettings"]["sandboxPolicy"]["writableRoots"] = []
                return reply

        with patch.object(probe, "TurnlessServer", ImplicitWorkspace):
            implicit = probe.campaign(executable, "codex-cli 0.159.3")
        assert implicit["outcome"] == "metadata_observed"
        assert implicit["effective_policy_enforcement"] == "unverified"

        class MismatchedPolicy(FakeServer):
            def request(self, method: str, params: dict) -> dict:
                reply = super().request(method, params)
                if method == "thread/start":
                    reply["sandbox"]["writableRoots"] = ["private-path-canary"]
                return reply

        with patch.object(probe, "TurnlessServer", MismatchedPolicy):
            try:
                probe.campaign(executable, "codex-cli 0.159.3")
            except probe.CampaignFailure as error:
                assert error.observations["failed_phase"] == "turnless_start"
                assert error.observations["failed_operation"] == "thread/start"
                assert error.observations["policy_type"] == "workspaceWrite"
                assert error.observations["workspace_roots_match"] is False
                assert error.observations["writable_roots_count"] == 1
                assert "private-path-canary" not in json.dumps(error.observations)
            else:
                raise AssertionError("mismatched policy was accepted")
        assert all(server.closed for server in servers)
    print("turnless startup probe offline checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
