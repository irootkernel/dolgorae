#!/usr/bin/env python3
"""Offline admission and response-order checks for the bounded live probe."""

from __future__ import annotations

import json
import contextlib
import io
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from unittest.mock import patch

import run_live_instruction_discovery as live
from run_live_instruction_discovery import (
    CORE_PREFIX, OPT_IN, TARGET_MODEL, ProbeServer, inventory, observed_thread,
    qualify_inventory, selected_model, verify_core_prefix,
)
from test_codex_compatibility import BufferedServer, completed

RUNNER = Path(__file__).with_name("run_live_instruction_discovery.py")


class BufferedProbe(ProbeServer, BufferedServer):
    def __init__(self, frames: list[dict]) -> None:
        BufferedServer.__init__(self, frames)


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="dolgorae-discovery-admission-") as temporary:
        root = Path(temporary).resolve()
        invoked = root / "codex-invoked"
        codex = root / "codex"
        codex.write_text(
            "#!/usr/bin/env python3\nimport pathlib, sys\n"
            f"pathlib.Path({str(invoked)!r}).write_text(' '.join(sys.argv[1:]))\n"
            "print('codex-cli 0.159.3')\n",
            encoding="utf-8",
        )
        codex.chmod(0o700)
        auth = root / "selected-auth.json"
        auth.write_text('{"OPENAI_API_KEY":"offline-not-a-credential"}', encoding="utf-8")
        auth.chmod(0o600)
        environment = {key: value for key, value in os.environ.items() if key != OPT_IN}

        def invoke(selected: Path, expected: str = "codex-cli 0.159.3") -> subprocess.CompletedProcess[str]:
            return subprocess.run(
                [sys.executable, str(RUNNER), "--codex", str(codex),
                 "--expected-version", expected, "--auth-file", str(selected)],
                capture_output=True, text=True, env=environment, timeout=15,
            )

        absent_opt_in = invoke(auth)
        assert absent_opt_in.returncode == 2 and f"{OPT_IN}=1 is required" in absent_opt_in.stderr
        assert not invoked.exists(), "probe contacted Codex without explicit opt-in"
        with (patch.dict(os.environ, environment, clear=True),
              patch.object(sys, "argv", [str(RUNNER), "--codex", str(codex),
                                        "--expected-version", "codex-cli 0.159.3",
                                        "--auth-file", str(auth)]),
              patch.object(live, "read_selected_auth", side_effect=AssertionError("read credentials")),
              contextlib.redirect_stderr(io.StringIO())):
            try:
                live.main()
            except SystemExit as error:
                assert error.code == 2
            else:
                raise AssertionError("probe admitted missing opt-in")
        environment[OPT_IN] = "1"
        for selected in (root / "missing-auth", root):
            assert invoke(selected).returncode == 2
            assert not invoked.exists(), "probe contacted Codex before credential admission"
        link = root / "auth-link"
        link.symlink_to(auth)
        assert invoke(link).returncode == 2 and not invoked.exists()
        auth.chmod(0o644)
        assert invoke(auth).returncode == 2 and not invoked.exists()
        auth.chmod(0o600)
        auth.write_text('[]', encoding="utf-8")
        assert invoke(auth).returncode == 2 and not invoked.exists()
        auth.write_text('{"OPENAI_API_KEY":"offline-not-a-credential"}', encoding="utf-8")
        assert invoke(auth, "codex-cli 0.159.3-dev").returncode == 1 and not invoked.exists()
        wrong_pin = invoke(auth, "codex-cli 0.159.2")
        assert wrong_pin.returncode == 1 and json.loads(wrong_pin.stdout)["outcome"] == "unqualified"
        assert invoked.read_text() == "--version", "version refusal launched a live app-server"

        accepted = {"common_marker": "common", "role_marker": "role", "ambient_markers": []}
        for terminal_first in (True, False):
            response = {"id": 1, "result": {"turn": {"id": "turn"}}}
            notification = completed("thread", "turn", "completed")
            user_item = {"type": "userMessage", "id": "user", "clientId": None,
                         "content": [{"type": "text", "text": live.PROMPT}]}
            started = {"method": "item/started", "params": {
                "threadId": "thread", "turnId": "turn", "item": user_item,
            }}
            order = ([started, notification, response] if terminal_first
                     else [response, started, notification])
            server = BufferedProbe([
                completed("other-thread", "turn", "failed"),
                completed("thread", "other-turn", "failed"),
                *order,
                {"id": 2, "result": {"thread": {"turns": [{
                    "id": "turn", "status": "completed", "items": [user_item, {
                        "type": "agentMessage", "text": json.dumps(accepted),
                    }],
                }]}}},
            ])
            assert inventory(server, "thread") == accepted
            task = server.sent[0]["params"]
            assert task["approvalPolicy"] == "never" and task["sandboxPolicy"] == {
                "type": "readOnly", "networkAccess": False,
            }
            assert "common-unique-marker" not in json.dumps(task)

        for item_type, kind, safe_type in (
            ("commandExecution", "tool_execution", "commandExecution"),
            ("mcpToolCall", "tool_execution", "mcpToolCall"),
            ("plan", "unexpected_turn_item", "plan"),
            ("private provider kind /private/path", "unexpected_turn_item", "unknown"),
        ):
            for in_history in (False, True):
                item = {"type": item_type, "command": "private command /private/path"}
                frames = [{"id": 1, "result": {"turn": {"id": "turn"}}}]
                if in_history:
                    frames.extend([
                        completed("thread", "turn", "completed"),
                        {"id": 2, "result": {"thread": {"turns": [{
                            "id": "turn", "status": "completed", "items": [item],
                        }]}}},
                    ])
                else:
                    frames.append({"method": "item/started", "params": {
                        "threadId": "thread", "turnId": "turn", "item": item,
                    }})
                refused_item = BufferedProbe(frames)
                try:
                    inventory(refused_item, "thread")
                except live.ProbeCheckFailure as error:
                    assert error.kind == kind and error.observations == {"item_type": safe_type}
                    assert "private" not in json.dumps(error.observations)
                else:
                    raise AssertionError("probe admitted an item outside its inventory contract")

        refused = BufferedProbe([{
            "id": "approval", "method": "item/commandExecution/requestApproval", "params": {},
        }])
        try:
            inventory(refused, "thread")
        except RuntimeError:
            assert "error" in refused.sent[-1] and "result" not in refused.sent[-1]
        else:
            raise AssertionError("probe accepted a tool approval")

        budget = BufferedProbe([{"id": number, "result": {}} for number in (1, 2)])
        for _ in range(2):
            budget.request("turn/start", {})
        try:
            budget.request("turn/start", {})
        except live.ProbeCheckFailure as error:
            assert error.kind == "turn_budget_exhausted"
            assert len(budget.sent) == 2 and budget.model_turns_requested == 2
        else:
            raise AssertionError("probe dispatched beyond its two-Turn authorization")

        wrong_cwd = BufferedProbe([{"id": 1, "result": {
            "model": TARGET_MODEL, "thread": {"id": "thread", "cwd": str(root / "other")},
        }}])
        try:
            observed_thread(wrong_cwd, root, "common-unique-marker", 0)
        except RuntimeError:
            assert len(wrong_cwd.sent) == 1, "wrong cwd admission dispatched a Turn"
        else:
            raise AssertionError("probe accepted a different canonical workspace")
        params = wrong_cwd.sent[0]["params"]
        assert params["config"] == {"project_doc_max_bytes": 0}
        assert params["developerInstructions"] == "common-unique-marker"
        for ambient in ([], ["profile"], ["profile", "ancestor", "project"],
                        ["project", "ancestor", "profile"]):
            result = {"common_marker": "common", "role_marker": "role", "ambient_markers": ambient}
            for required in ([], ["profile", "ancestor", "project"]):
                try:
                    qualify_inventory(result, "common", "role", required)
                except RuntimeError:
                    assert set(ambient) != set(required)
                else:
                    assert set(ambient) == set(required)
        for result in (
            {"common_marker": "changed", "role_marker": "role", "ambient_markers": []},
            {"common_marker": "common", "role_marker": "changed", "ambient_markers": []},
            {"common_marker": "common", "role_marker": "role", "ambient_markers": ["profile"] * 3},
            {"common_marker": "common", "role_marker": "role",
             "ambient_markers": ["profile", "ancestor", "project", "unexpected"]},
        ):
            try:
                qualify_inventory(result, "common", "role", ["profile", "ancestor", "project"])
            except RuntimeError:
                pass
            else:
                raise AssertionError("probe qualified an incorrect source inventory")
        for response in ("not JSON", "[]", '{"common_marker":"common"}'):
            malformed = BufferedProbe([
                {"id": 1, "result": {"turn": {"id": "turn"}}},
                completed("thread", "turn", "completed"),
                {"id": 2, "result": {"thread": {"turns": [{
                    "id": "turn", "status": "completed", "items": [{
                        "type": "agentMessage", "text": response,
                    }],
                }]}}},
            ])
            try:
                inventory(malformed, "thread")
            except (ValueError, RuntimeError):
                pass
            else:
                raise AssertionError("probe accepted malformed provider output")
        for frame in (
            {"id": 1, "error": {"code": -1, "message": "offline error"}},
            completed("thread", "turn", "failed"),
        ):
            failed = BufferedProbe([
                *([{"id": 1, "result": {"turn": {"id": "turn"}}}] if "method" in frame else []),
                frame,
            ])
            try:
                inventory(failed, "thread")
            except RuntimeError:
                pass
            else:
                raise AssertionError("probe accepted an errored or failed Turn")
        for model, accepted_model in ((TARGET_MODEL, True), ("substituted-model", False)):
            models = BufferedProbe([{"id": 1, "result": {"data": [{
                "model": model, "supportedReasoningEfforts": [{"reasoningEffort": "low"}],
            }], "nextCursor": None}}])
            try:
                selected_model(models)
            except RuntimeError:
                assert not accepted_model
            else:
                assert accepted_model
        fixture = root / "instructions.rs"
        for version, body, valid in ((2, CORE_PREFIX, True), (3, CORE_PREFIX, False),
                                     (2, CORE_PREFIX + " changed", False)):
            fixture.write_text(f"pub const CORE_PREFIX_VERSION: u32 = {version};\n"
                               f"pub const CORE_PREFIX: &str = {json.dumps(body)};\n", encoding="utf-8")
            try:
                verify_core_prefix(fixture)
            except ValueError:
                assert not valid
            else:
                assert valid
        for malformed in (TypeError("private provider shape"), KeyError("private provider key"),
                          AttributeError("private provider attribute")):
            output = io.StringIO()
            with (patch.dict(os.environ, environment, clear=True),
                  patch.object(sys, "argv", [str(RUNNER), "--codex", str(codex),
                                            "--expected-version", "codex-cli 0.159.3",
                                            "--auth-file", str(auth)]),
                  patch.object(live, "campaign", side_effect=malformed),
                  contextlib.redirect_stdout(output)):
                assert live.main() == 1
            report = json.loads(output.getvalue())
            assert report["outcome"] == "unqualified"
            assert "private provider" not in output.getvalue()

        # Exercise the real campaign's failure reporting with an offline server,
        # including refusal before a Turn and a completed but mismatched inventory.
        class OfflineCampaign(BufferedProbe):
            def close(self) -> None:
                pass

        for failure in ("rpc_rejected", "thread_identity_mismatch", "inventory_mismatch",
                        "turn_not_completed"):
            def server_for_campaign(selected_codex: Path, profile: Path, env: dict) -> OfflineCampaign:
                assert selected_codex == codex and env["CODEX_HOME"] == str(profile)
                workspace = profile.parent / "ancestor" / "workspace"
                frames = [
                    {"id": 1, "result": {"codexHome": str(profile)}},
                    {"id": 2, "result": {"data": [{
                        "model": TARGET_MODEL, "supportedReasoningEfforts": [{
                            "reasoningEffort": "low",
                        }],
                    }], "nextCursor": None}},
                ]
                if failure == "rpc_rejected":
                    frames.append({"id": 3, "error": {
                        "code": -32602, "message": "private provider secret /private/path",
                    }})
                else:
                    observed_cwd = workspace if failure in {
                        "inventory_mismatch", "turn_not_completed",
                    } else profile
                    frames.append({"id": 3, "result": {
                        "model": TARGET_MODEL, "thread": {
                            "id": "thread", "cwd": str(observed_cwd),
                        },
                    }})
                if failure == "inventory_mismatch":
                    frames.extend([
                        {"id": 4, "result": {"turn": {"id": "turn"}}},
                        completed("thread", "turn", "completed"),
                        {"id": 5, "result": {"thread": {"turns": [{
                            "id": "turn", "status": "completed", "items": [{
                                "type": "agentMessage", "text": json.dumps({
                                    "common_marker": "private common body",
                                    "role_marker": "private Role body",
                                    "ambient_markers": ["private ambient body"],
                                }),
                            }],
                        }]}}},
                    ])
                elif failure == "turn_not_completed":
                    terminal = completed("thread", "turn", "failed")
                    terminal["params"]["turn"]["error"] = {
                        "message": "private provider body", "additionalDetails": "private path",
                        "codexErrorInfo": {"httpConnectionFailed": {"httpStatusCode": 401}},
                    }
                    frames.extend([{"id": 4, "result": {"turn": {"id": "turn"}}}, terminal])
                return OfflineCampaign(frames)

            output = io.StringIO()
            with (patch.dict(os.environ, environment, clear=True),
                  patch.object(sys, "argv", [str(RUNNER), "--codex", str(codex),
                                            "--expected-version", "codex-cli 0.159.3",
                                            "--auth-file", str(auth)]),
                  patch.object(live, "ProbeServer", side_effect=server_for_campaign),
                  contextlib.redirect_stdout(output)):
                assert live.main() == 1
            report = json.loads(output.getvalue())
            assert report["failed_phase"] == "baseline" and report["failure_kind"] == failure
            assert report["model_turns_requested"] == (1 if failure in {
                "inventory_mismatch", "turn_not_completed",
            } else 0)
            assert report["instruction_discovery_control"] == "unverified"
            assert "private" not in output.getvalue() and "thread\"" not in output.getvalue()
            if failure == "rpc_rejected":
                assert report["rpc_error_code"] == -32602
            elif failure == "thread_identity_mismatch":
                assert report["selected_model_matches"] is True
                assert report["canonical_workspace_cwd_matches"] is False
            elif failure == "inventory_mismatch":
                assert report["common_marker_matches"] is False and report["role_marker_matches"] is False
                assert report["expected_ambient_count"] == 3 and report["observed_ambient_count"] == 1
            else:
                assert report["codex_error_kind"] == "httpConnectionFailed"
                assert report["http_status_code"] == 401 and report["turn_status"] == "failed"
        for info in ("private arbitrary kind", {"private key": {}},
                     {"httpConnectionFailed": {"httpStatusCode": True}},
                     {"httpConnectionFailed": {"httpStatusCode": 9999}}):
            details = live.turn_failure_details({"error": {
                "message": "private body", "codexErrorInfo": info,
            }})
            assert "private" not in json.dumps(details) and "http_status_code" not in details
    print("Live instruction discovery admission, no-approval, cwd, and ordering checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
