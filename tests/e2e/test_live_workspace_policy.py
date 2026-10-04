#!/usr/bin/env python3
"""Offline admission, evidence and budget checks for workspace enforcement."""

from __future__ import annotations

import hashlib
import json
import os
import shlex
import subprocess
import sys
import tempfile
from pathlib import Path
from unittest.mock import patch

import run_live_workspace_policy as probe
from run_live_instruction_discovery import ProbeCheckFailure, ProbeServer
from test_codex_compatibility import BufferedServer


class BufferedPolicy(probe.PolicyServer, BufferedServer):
    def __init__(self, frames: list[dict]) -> None:
        BufferedServer.__init__(self, frames)


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="dolgorae-policy-offline-") as temporary:
        root = Path(temporary).resolve()
        invoked = root / "invoked"
        executable = root / "codex"
        executable.write_text(
            "#!/usr/bin/env python3\nimport pathlib,sys\n"
            f"pathlib.Path({str(invoked)!r}).write_text(' '.join(sys.argv[1:]))\n"
            "print('codex-cli 0.159.3')\n"
        )
        executable.chmod(0o700)
        digest = hashlib.sha256(executable.read_bytes()).hexdigest()
        auth = root / "selected-auth.json"
        auth.write_text('{"fake": "credential-canary"}')
        auth.chmod(0o600)
        auth_before = auth.read_bytes(), auth.stat().st_mtime_ns
        refused = subprocess.run([
            sys.executable, probe.__file__, "--codex", str(executable),
            "--expected-version", "codex-cli 0.159.3", "--expected-sha256", digest,
            "--auth-file", str(auth),
        ], env={key: value for key, value in os.environ.items() if key != probe.OPT_IN},
            text=True, capture_output=True, timeout=15)
        assert refused.returncode == 2 and not invoked.exists()
        with patch.object(probe, "read_selected_auth", side_effect=AssertionError("read before pin")):
            try:
                probe.campaign(executable, "codex-cli 0.159.3", "0" * 64, auth)
            except probe.CampaignFailure as error:
                assert error.observations["failure_kind"] == "executable_digest_mismatch"
                assert error.observations["credential_access"] is False
            else:
                raise AssertionError("digest mismatch admitted")
        for params in ({"sandboxPolicy": {}}, {"approvalPolicy": "never"}, {"cwd": "/foreign"}):
            server = BufferedPolicy([])
            try:
                server.request("turn/start", {
                    "threadId": "thread", "input": [], "model": probe.TARGET_MODEL,
                    "effort": probe.TARGET_EFFORT, **params,
                })
            except ProbeCheckFailure as error:
                assert error.kind == "turn_override_or_unexpected_parameter"
            else:
                raise AssertionError("probe overrode thread policy")
            assert server.sent == []
        server = BufferedPolicy([])
        server.model_turns_requested = 5
        try:
            server.request("turn/start", {
                "threadId": "thread", "input": [], "model": probe.TARGET_MODEL,
                "effort": probe.TARGET_EFFORT,
            })
        except ProbeCheckFailure as error:
            assert error.kind == "turn_budget_exhausted"
        else:
            raise AssertionError("sixth Turn admitted")
        assert server.sent == [] and ProbeServer.MAX_MODEL_TURNS == 2
        server = BufferedPolicy([])
        try:
            server.request("command/exec", {})
        except ProbeCheckFailure as error:
            assert error.kind == "request_outside_policy_probe"
        else:
            raise AssertionError("unrelated execution RPC admitted")
        assert server.sent == []
        command = "printf %s bounded-marker >> policy-target"
        quoted = "printf '%s' 'bounded-marker' >> 'policy-target'"
        for observed in (command, quoted, f"/bin/zsh -lc {shlex.quote(quoted)}"):
            probe.check_command({"command": observed, "cwd": str(root)}, command, root)
        for actual in (command, quoted, 'printf "%s" "bounded-marker">>"policy-target"'):
            probe.check_command({"command": actual, "cwd": str(root)}, command, root)
            target = root / "policy-target"
            target.write_bytes(b"baseline")
            executed = subprocess.run(["/bin/sh", "-c", actual], cwd=root,
                                      env={"PATH": os.defpath, "HOME": str(root)},
                                      capture_output=True, timeout=5)
            assert executed.returncode == 0 and target.read_bytes() == b"baselinebounded-marker"
        for actual in (command + "; pwd", command + "\ncat auth.json",
                       "printf %s bounded-marker '>>' policy-target",
                       "printf %s $(cat auth.json) >> policy-target",
                       "printf %s bounded-marker >> other-target",
                       "printf\n%s bounded-marker >> policy-target"):
            try:
                probe.check_command({"command": actual, "cwd": str(root)}, command, root)
            except ProbeCheckFailure as error:
                assert error.kind == "unexpected_command"
                assert error.observations["command_matches"] is False
                assert actual not in json.dumps(error.observations)
            else:
                raise AssertionError("command change or expansion admitted")
        for item in ({"type": "fileChange"},
                     {"type": "commandExecution", "command": "cat auth.json", "cwd": str(root)},
                     {"type": "commandExecution", "command": command, "cwd": "/foreign"}):
            try:
                probe.check_item(item, command, root)
            except ProbeCheckFailure:
                pass
            else:
                raise AssertionError("unrelated tool or command admitted")
        try:
            probe.check_command({"command": "private-command-canary", "cwd": "/private-path-canary",
                                 "source": "private-source-canary", "pluginId": "private-plugin-canary"},
                                command, root)
        except ProbeCheckFailure as error:
            assert error.observations["cwd_matches"] is False
            assert error.observations["execution_source_matches"] is False
            assert error.observations["plugin_attribution_present"] is True
            assert "canary" not in json.dumps(error.observations)
        else:
            raise AssertionError("unexpected command provenance accepted")
        for source in ("agent", "unifiedExecStartup", "unifiedExecInteraction", "userShell"):
            try:
                probe.check_command({"command": command, "cwd": str(root), "source": source}, command, root)
            except ProbeCheckFailure as error:
                assert source in {"unifiedExecInteraction", "userShell"}
                assert error.observations["command_source"] == source
            else:
                assert source in {"agent", "unifiedExecStartup"}

        servers: list[FakeServer] = []
        calls: list[tuple[str, dict]] = []

        class FakeServer:
            omit_command = False
            falsify_denial = False
            fail_start = False
            late_completion = False
            change_read_only_target = False
            fail_resume = False
            replace_resume_thread = False
            bad_resume_history = False
            command_source = "agent"

            def __init__(self, codex: Path, cwd: Path, env: dict[str, str]) -> None:
                assert codex == executable and env["CODEX_HOME"] == str(cwd)
                assert env["HOME"] != os.environ.get("HOME")
                assert (cwd / "auth.json").read_bytes() == auth_before[0]
                assert "OPENAI_API_KEY" not in env
                self.profile = cwd
                self.workspace = cwd.parent / "workspace"
                self.messages: list[dict] = []
                self.history = servers[0].history if servers else []
                self.write = True
                self.closed = False
                self.pending: dict | None = None
                servers.append(self)

            def send(self, value: dict) -> None:
                assert value["method"] == "initialized"

            def close(self) -> None:
                assert self.profile.is_dir(), "temporary home removed before server exit"
                self.closed = True

            def receive(self, deadline: float) -> dict:
                assert self.pending is not None
                frame, self.pending = self.pending, None
                self.messages.append(frame)
                return frame

            def handle_request(self, frame: dict) -> None:
                assert "id" not in frame

            def request(self, method: str, params: dict) -> dict:
                self.failed_operation = method
                calls.append((method, params))
                if method == "initialize":
                    return {"codexHome": str(self.profile)}
                if method == "thread/start":
                    if self.fail_start:
                        raise RuntimeError("private-runtime-canary")
                    return {"thread": {"id": "thread", "turns": []},
                            "cwd": str(self.workspace), "model": probe.TARGET_MODEL,
                            "approvalPolicy": "never",
                            "sandbox": {"type": "workspaceWrite", "writableRoots": []}}
                if method == "thread/settings/update":
                    self.write = params["sandboxPolicy"]["type"] == "workspaceWrite"
                    self.messages.append({"method": "thread/settings/updated", "params": {
                        "threadId": "thread", "threadSettings": {
                            "cwd": str(self.workspace), "approvalPolicy": "never",
                            "sandboxPolicy": params["sandboxPolicy"],
                        },
                    }})
                    return {}
                if method == "turn/start":
                    assert set(params) == {"threadId", "input", "model", "effort"}
                    self.model_turns_requested += 1
                    assert self.model_turns_requested <= 5
                    command = params["input"][0]["text"].split("directory: ")[1].split("\n")[0]
                    marker = command.split()[2]
                    if self.write or self.change_read_only_target:
                        with (self.workspace / "policy-target").open("ab") as target:
                            target.write(marker.encode())
                    item = {"type": "commandExecution", "command": command,
                            "source": self.command_source,
                            "cwd": str(self.workspace), "status": "completed" if self.write else "failed",
                            "exitCode": 0 if self.write or self.falsify_denial else 1,
                            "aggregatedOutput": "" if self.write else "Operation not permitted"}
                    turn = {"id": f"turn-{self.model_turns_requested}", "status": "completed",
                            "items": [] if self.omit_command else [item]}
                    self.history.append(turn)
                    completed = {"method": "turn/completed", "params": {"threadId": "thread", "turn": turn}}
                    if self.late_completion:
                        self.pending = completed
                    else:
                        self.messages.append(completed)
                    return {"turn": {"id": turn["id"]}}
                if method == "thread/read":
                    return {"thread": {"id": "thread", "turns": (
                        [] if self.bad_resume_history and len(servers) == 2 else self.history
                    )}}
                assert method == "thread/resume" and params == {"threadId": "thread"}
                assert servers[0].closed and self.model_turns_requested == 4
                if self.fail_resume:
                    raise ProbeCheckFailure("rpc_rejected", {"rpc_error_code": -32600})
                self.write = False
                return {"thread": {"id": "other" if self.replace_resume_thread else "thread"}, "cwd": str(self.workspace),
                        "model": probe.TARGET_MODEL, "approvalPolicy": "never",
                        "sandbox": {"type": "readOnly", "networkAccess": False}}

        for late, source in ((False, "agent"), (True, "agent"), (False, "unifiedExecStartup"),
                             (True, "unifiedExecStartup")):
            servers.clear()
            calls.clear()
            FakeServer.late_completion = late
            FakeServer.command_source = source
            with patch.object(probe, "PolicyServer", FakeServer):
                result = probe.campaign(executable, "codex-cli 0.159.3", digest, auth)
            assert result["model_turns_requested"] == 5
            assert result["writable_effects_observed"] == 2 and result["read_only_denials_observed"] == 3
            assert result["post_restart_enforcement"] == "read_only_denial_observed"
            assert result["profile_capability_mutated"] is False
            assert len(servers) == 2 and all(server.closed for server in servers)
            assert sum(method == "thread/start" for method, _ in calls) == 1
            assert sum(method == "thread/settings/update" for method, _ in calls) == 3
            assert "credential-canary" not in json.dumps(result)
        FakeServer.late_completion = False

        for attribute, failure, turns in (
            ("omit_command", "command_attempt_missing_or_ambiguous", 1),
            ("falsify_denial", "read_only_denial_unverified", 2),
            ("change_read_only_target", "workspace_effect_mismatch", 2),
            ("fail_start", "runtime_failure", 0),
            ("fail_resume", "rpc_rejected", 4),
            ("replace_resume_thread", "restart_thread_identity_mismatch", 4),
            ("bad_resume_history", "restart_history_mismatch", 4),
        ):
            servers.clear()
            calls.clear()
            setattr(FakeServer, attribute, True)
            try:
                with patch.object(probe, "PolicyServer", FakeServer):
                    probe.campaign(executable, "codex-cli 0.159.3", digest, auth)
            except probe.CampaignFailure as error:
                assert error.observations["failure_kind"] == failure, error.observations
                assert error.observations["model_turns_requested"] == turns
                assert all(server.closed for server in servers)
                assert sum(method == "turn/start" for method, _ in calls) == turns
                assert sum(method == "thread/start" for method, _ in calls) == 1
                assert "private-runtime-canary" not in json.dumps(error.observations)
            else:
                raise AssertionError("unverified effect or startup failure was accepted")
            finally:
                setattr(FakeServer, attribute, False)
        assert auth_before == (auth.read_bytes(), auth.stat().st_mtime_ns)
        assert invoked.read_text() == "--version"
    print("workspace policy probe offline checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
