#!/usr/bin/env python3
"""Opt-in turnless startup metadata probe; no account, credentials or model Turns.

Observed policy metadata is not empirical enforcement qualification. This probe
only tests whether the selected runtime offers a same-thread settings boundary
and whether its turnless binding survives an App Server restart.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

from run_access_safety_acceptance import TARGET_MODEL
from run_live_instruction_discovery import (
    ProbeCheckFailure, ProbeServer, checked_codex, executable_identity, isolated_environment,
    private_file,
)

OPT_IN = "DOLGORAE_RUN_LIVE_WORKSPACE_STARTUP"


class CampaignFailure(RuntimeError):
    def __init__(self, phase: str, operation: str, cause: Exception) -> None:
        super().__init__("turnless startup probe failed")
        self.observations = {"failed_phase": phase, "failed_operation": operation}
        if isinstance(cause, ProbeCheckFailure):
            self.observations["failure_kind"] = cause.kind
            self.observations.update(cause.observations)
        else:
            self.observations["failure_kind"] = (
                "timeout" if isinstance(cause, TimeoutError) else "runtime_failure"
            )


class TurnlessServer(ProbeServer):
    def request(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        if method not in {"initialize", "thread/start", "thread/settings/update", "thread/resume"}:
            raise ProbeCheckFailure("request_outside_turnless_probe")
        return super().request(method, params)


def initialize(server: TurnlessServer, profile: Path) -> None:
    result = server.request("initialize", {
        "clientInfo": {"name": "dolgorae-turnless-startup", "version": "1"},
        "capabilities": {"experimentalApi": True, "optOutNotificationMethods": []},
    })
    if result.get("codexHome") != str(profile):
        raise ProbeCheckFailure("profile_home_mismatch")
    server.send({"method": "initialized", "params": {}})


def check_policy(value: Any, write: bool, workspace: Path) -> None:
    observed = value if isinstance(value, dict) else {}
    kind = observed.get("type")
    roots = observed.get("writableRoots")
    # Codex 0.159.3 SandboxPolicy::get_writable_roots_with_cwd always adds cwd.
    # writableRoots contains additional roots; [] and [cwd] are equivalent here.
    # https://github.com/openai/codex/blob/rust-v0.159.3/codex-rs/protocol/src/protocol.rs
    workspace_roots_match = roots == [] or roots == [str(workspace)]
    details = {
        "expected_access": "writable" if write else "read-only",
        "policy_type": kind if isinstance(kind, str) and kind in {
            "readOnly", "workspaceWrite", "dangerFullAccess", "externalSandbox",
        } else "unknown",
        "policy_object_present": isinstance(value, dict),
        "network_disabled": observed.get("networkAccess", False) is False,
        "workspace_roots_match": workspace_roots_match,
        "writable_roots_count": min(len(roots), 100) if isinstance(roots, list) else None,
        "slash_tmp_excluded": observed.get("excludeSlashTmp", False) is True,
        "tmpdir_excluded": observed.get("excludeTmpdirEnvVar", False) is True,
    }
    if not isinstance(value, dict) or value.get("networkAccess", False) is not False:
        raise ProbeCheckFailure("policy_metadata_mismatch", details)
    if not write:
        if value.get("type") != "readOnly":
            raise ProbeCheckFailure("policy_metadata_mismatch", details)
        return
    if (value.get("type") != "workspaceWrite"
            or not workspace_roots_match
            or value.get("excludeSlashTmp", False) is not False
            or value.get("excludeTmpdirEnvVar", False) is not False):
        raise ProbeCheckFailure("policy_metadata_mismatch", details)


def update(server: TurnlessServer, thread_id: str, workspace: Path, write: bool,
           *, approval_policy: str = "never") -> None:
    policy = ({"type": "workspaceWrite", "writableRoots": [str(workspace)],
               "networkAccess": False, "excludeSlashTmp": False,
               "excludeTmpdirEnvVar": False} if write
              else {"type": "readOnly", "networkAccess": False})
    # Empty responses and sandbox labels cannot establish even the metadata
    # boundary. Require the runtime's matching settings notification.
    before = len(server.messages)
    server.request("thread/settings/update", {
        "threadId": thread_id, "sandboxPolicy": policy, "approvalPolicy": approval_policy,
    })
    notifications = [frame for frame in server.messages[before:]
                     if frame.get("method") == "thread/settings/updated"
                     and frame.get("params", {}).get("threadId") == thread_id]
    deadline = time.monotonic() + 5
    while not notifications:
        message = server.receive(deadline)
        if (message.get("method") == "thread/settings/updated"
                and message.get("params", {}).get("threadId") == thread_id):
            notifications.append(message)
        else:
            server.handle_request(message)
    if len(notifications) != 1:
        raise ProbeCheckFailure("settings_notification_missing_or_ambiguous")
    settings = notifications[0]["params"].get("threadSettings", {})
    if settings.get("cwd") != str(workspace) or settings.get("approvalPolicy") != approval_policy:
        raise ProbeCheckFailure("settings_identity_mismatch")
    check_policy(settings.get("sandboxPolicy"), write, workspace)


def campaign(codex: Path, expected: str) -> dict[str, object]:
    with tempfile.TemporaryDirectory(prefix="dolgorae-turnless-probe-") as temporary:
        root = Path(temporary).resolve()
        for name in ("home", "profile", "tmp", "config", "cache", "workspace"):
            (root / name).mkdir(mode=0o700)
        environment = isolated_environment(root)
        private_file(root / "profile" / "config.toml", b'cli_auth_credentials_store = "file"\n')
        selected = checked_codex(codex, expected, environment)
        workspace = root / "workspace"
        def start_server() -> TurnlessServer:
            if executable_identity(codex) != {key: selected[key] for key in ("metadata", "sha256")}:
                raise ProbeCheckFailure("executable_changed")
            return TurnlessServer(codex, root / "profile", environment)
        server = start_server()
        phase = "initialize"
        try:
            initialize(server, root / "profile")
            phase = "turnless_start"
            started = server.request("thread/start", {
                "cwd": str(workspace), "model": TARGET_MODEL,
                "sandbox": "workspace-write", "approvalPolicy": "never",
                "developerInstructions": "No task has been submitted. Instructions do not grant access.",
                "config": {"sandbox_workspace_write": {
                    "writable_roots": [str(workspace)], "network_access": False,
                    "exclude_slash_tmp": False, "exclude_tmpdir_env_var": False,
                }},
            })
            thread = started.get("thread", {})
            thread_id = thread.get("id")
            if (not isinstance(thread_id, str) or not thread_id
                    or started.get("cwd") != str(workspace)
                    or started.get("model") != TARGET_MODEL or thread.get("turns") != []):
                raise ProbeCheckFailure("turnless_thread_identity_mismatch")
            check_policy(started.get("sandbox"), True, workspace)
            for phase, write in (("release_read_only", False), ("reacquire_writable", True),
                                 ("final_release_read_only", False)):
                update(server, thread_id, workspace, write)
        except (OSError, ValueError, RuntimeError, TimeoutError, subprocess.SubprocessError) as error:
            raise CampaignFailure(phase, getattr(server, "failed_operation", "initialize"), error) from error
        finally:
            server.close()
        resumed = start_server()
        phase = "restart_initialize"
        restart_observations: dict[str, object] = {}
        try:
            initialize(resumed, root / "profile")
            phase = "resume_original_thread"
            try:
                reply = resumed.request("thread/resume", {"threadId": thread_id})
            except ProbeCheckFailure as error:
                if error.kind != "rpc_rejected":
                    raise
                # A generic RPC refusal does not prove the thread was lost.
                restart = "resume_rpc_rejected"
                restart_observations = error.observations
            else:
                if reply.get("thread", {}).get("id") != thread_id:
                    raise ProbeCheckFailure("restart_replaced_thread")
                check_policy(reply.get("sandbox"), False, workspace)
                restart = "same_thread_read_only_metadata"
        except (OSError, ValueError, RuntimeError, TimeoutError, subprocess.SubprocessError) as error:
            raise CampaignFailure(phase, getattr(resumed, "failed_operation", "initialize"), error) from error
        finally:
            resumed.close()
        return {
            "schema": "dolgorae.turnless-startup-probe/v1",
            "outcome": "metadata_observed", "codex_version": selected["version"],
            "codex_sha256": selected["sha256"], "model_turns_requested": 0,
            "credential_access": False, "profile_capability_mutated": False,
            "same_thread_settings_updates": 3, "restart": restart,
            "effective_policy_enforcement": "unverified",
            **restart_observations,
        }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--codex", type=Path, required=True)
    parser.add_argument("--expected-version", required=True)
    args = parser.parse_args()
    if os.environ.get(OPT_IN) != "1":
        parser.error(f"{OPT_IN}=1 is required")
    try:
        result = campaign(args.codex.expanduser().resolve(strict=True), args.expected_version)
    except (OSError, ValueError, RuntimeError, TimeoutError, subprocess.SubprocessError) as error:
        result = {"schema": "dolgorae.turnless-startup-probe/v1",
                  "outcome": "unqualified", "model_turns_requested": 0,
                  "credential_access": False, "effective_policy_enforcement": "unverified"}
        if isinstance(error, ProbeCheckFailure):
            result["failure_kind"] = error.kind
            result.update(error.observations)
        elif isinstance(error, CampaignFailure):
            result.update(error.observations)
        print(json.dumps(result, sort_keys=True))
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
