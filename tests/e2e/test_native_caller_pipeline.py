#!/usr/bin/env python3
"""Offline admission, isolation and cleanup checks for the native caller driver."""
from __future__ import annotations

import contextlib
import copy
import argparse
import io
import json
import os
import subprocess
import tempfile
import tomllib
from types import SimpleNamespace
from pathlib import Path
from unittest.mock import patch

import run_native_caller_pipeline as probe


def main() -> int:
    from native_specialist_responses import decoded_result
    original = {"schema": "dolgorae.consumer-control-response/v1", "ok": True,
                "data": {"mode": "native_only", "allowed_role_refs": []}}
    encoded = original
    for _ in range(3):
        encoded = json.dumps(encoded)
        assert decoded_result(encoded) == original
    for invalid in ([], "[]", "null", '"unparsed"', 17):
        try:
            decoded_result(invalid)
        except (ValueError, TypeError):
            pass
        else:
            raise AssertionError("native result decoder admitted a non-object")
    parser = argparse.ArgumentParser()
    parser.add_argument("--driver", type=Path, required=True)
    driver = parser.parse_args().driver.resolve(strict=True)
    with patch.dict(os.environ, {}, clear=True), patch.object(probe, "executable_identity") as identity:
        assert probe.main() == 2
        try:
            probe.campaign(Path("unused-driver"), Path("unused-codex"))
        except ValueError:
            pass
        else:
            raise AssertionError("native execution admitted without opt-in")
        identity.assert_not_called()
    with patch.dict(os.environ, {probe.OPT_IN: "1"}), patch.object(
        probe, "executable_identity", return_value={"sha256": "0" * 64}
    ), patch.object(probe.tempfile, "mkdtemp") as allocation:
        try:
            probe.campaign(Path("unused-driver"), Path("unused-codex"))
        except ValueError:
            pass
        else:
            raise AssertionError("unqualified executable admitted")
        allocation.assert_not_called()

    with patch.dict(os.environ, {probe.OPT_IN: "1"}), patch.object(probe, "executable_identity") as identity, patch.object(probe.tempfile, "mkdtemp") as allocation:
        try:
            probe.campaign(Path("unused-driver"), Path("unused-codex"), task094_session=True)
        except ValueError:
            pass
        else:
            raise AssertionError("session mode admitted without an ordinary artifact")
        identity.assert_not_called()
        allocation.assert_not_called()

    with tempfile.TemporaryDirectory(prefix="caller-pipeline-offline-", dir=os.environ["TMPDIR"]) as temporary:
        parent = Path(temporary)
        invalid_bootstrap = parent / "invalid-bootstrap.json"
        probe.private_file(invalid_bootstrap, b"{}\n")
        handoff = parent / "failure-handoff.json"
        environment = dict(os.environ)
        environment.pop(probe.OPT_IN, None)
        failed_worker = subprocess.run(
            ["/bin/sh", "-c", 'umask 077; exec "$1" __worker --bootstrap "$2" 3>"$3"',
             "fixture-worker", str(driver), str(invalid_bootstrap), str(handoff)],
            env=environment, capture_output=True, text=True, timeout=5,
        )
        assert failed_worker.returncode == 6
        assert json.loads(handoff.read_text()) == {"state": "failed", "code": "RUNTIME_PATH_COLLISION"}
        assert not failed_worker.stdout and not failed_worker.stderr
        assert handoff.stat().st_mode & 0o777 == 0o600
        fixtures = []
        launches = []
        owned = []
        checked_version = probe.checked_codex

        class Fixture:
            base_url = "http://127.0.0.1:12345/v1"
            posts = 0
            failure = None

            def __init__(self):
                self.closed = False
                fixtures.append(self)

            def close(self):
                self.closed = True

        class InteractionFixture(Fixture):
            failure_details = None
            issued_cases = {"permission", "mcp"}
            completed_cases = {"permission", "mcp"}
            mcp_decision = "accept"
            mcp_http = SimpleNamespace(server_port=12346)

        def version(codex, expected, environment):
            assert environment["HOME"] != os.environ.get("HOME")
            assert set(environment) <= {"PATH", "LANG", "LC_ALL", "TERM", "HOME", "CODEX_HOME",
                                        "TMPDIR", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", probe.OPT_IN}
            assert "OPENAI_API_KEY" not in environment and "PRIVATE_FIXTURE_CANARY" not in environment
            root = Path(environment["HOME"]).parent
            assert not list(root.rglob("auth.json"))
            checked_version(codex, expected, environment)

        failure = False
        unexpected_post = False
        sequential_verified = True

        def run(argv, *, env, **_kwargs):
            if argv == ["fixture-codex", "--version"]:
                return subprocess.CompletedProcess(argv, 0, f"codex-cli {probe.CODEX_VERSION}\n", "")
            root = Path(argv[1])
            launches.append(argv)
            owned.append(root)
            assert env[probe.OPT_IN] == "1" and Path(env["HOME"]) == root / "home"
            assert (root / "fixture-owned").read_bytes() == b"native-caller-pipeline/v1\n"
            assert (root / "home" / "workspace").is_dir()
            assert not (root / "home" / ".git").exists()
            assert (root / "home" / "workspace" / "AGENTS.md").is_file()
            assert all((root / name / "AGENTS.md").is_file() for name in ("fixture-a", "fixture-b", "home"))
            for name in ("fixture-a", "fixture-b"):
                features = tomllib.loads((root / name / "config.toml").read_text())["features"]
                if (root / "default-feature-variant").exists():
                    assert "plugins" not in features
                else:
                    assert features["plugins"] is False
                if (root / "interaction-cases").exists():
                    config = tomllib.loads((root / name / "config.toml").read_text())
                    assert config["mcp_servers"]["fixture"] == {"url":"http://127.0.0.1:12346/mcp", "default_tools_approval_mode":"approve"}
                    assert features["request_permissions_tool"] is True
                    assert features["tool_search_always_defer_mcp_tools"] is False
                variant_file = root / "default-feature-variant"
                if variant_file.exists() and variant_file.read_text() == "config-enable":
                    assert features["shell_snapshot"] is True
                elif variant_file.exists():
                    assert "shell_snapshot" not in features
                else:
                    assert features["shell_snapshot"] is False
            if failure:
                return subprocess.CompletedProcess(argv, 1, "private-canary", "private-canary")
            (root / "cleanup-result").write_bytes(b"complete\n")
            fixtures[-1].posts = 4 if (root / "interaction-cases").exists() else int(unexpected_post)
            value = {"schema": "dolgorae.native-caller-pipeline/v1"}
            if (root / "interaction-cases").exists():
                value["sequential_write_admissions_verified"] = sequential_verified
            if (root / "default-feature-variant").exists():
                launch = {"disabled_features":["shell_snapshot"], "normalized_argv":["codex","--disable","shell_snapshot"], "launch_contract_sha256":"digest", "server_key":"key"}
                value["default_feature_startup"] = {"variant":(root / "default-feature-variant").read_text(),
                    "effective_shell_snapshot":False, "quiescent_startup":True,
                    "quiescent_verified_result":True, "accepted_launch":launch,
                    "startup":{"path":"first_attempt","initial_error":None,"recovery":None,
                               "run_id":"original-run","thread_id":"original-thread"},
                    "recomputed_launch_digest":"digest", "recomputed_server_key":"key", "actual_pid":1,
                    "actual_command":"codex --disable shell_snapshot", "run_launch_digest":"run",
                    "shared_feature_observation":{"name":"shell_snapshot", "enabled":False},
                    "dedicated_feature_observation":{"name":"shell_snapshot", "enabled":False},
                    "shared_plugin_observation":{"name":"plugins", "enabled":True},
                    "dedicated_plugin_observation":{"name":"plugins", "enabled":True},
                    "dedicated_argv":launch["normalized_argv"]}
            return subprocess.CompletedProcess(argv, 0, json.dumps(value), "")

        with patch.dict(os.environ, {probe.OPT_IN: "1", "TMPDIR": str(parent),
                                    "OPENAI_API_KEY": "private-canary", "PRIVATE_FIXTURE_CANARY": "private-canary"}), \
             patch.object(probe, "executable_identity", return_value={"sha256": probe.CODEX_SHA256}), \
             patch("run_live_instruction_discovery.executable_identity", return_value={"sha256": probe.CODEX_SHA256}), \
             patch.object(probe, "checked_codex", side_effect=version), \
             patch.object(probe, "ScriptedResponses", Fixture), \
             patch.object(probe, "InteractionResponses", lambda _: InteractionFixture()), \
             patch.object(probe.subprocess, "run", side_effect=run):
            result = probe.campaign(Path("fixture-driver"), Path("fixture-codex"))
            assert result["external_provider_requests"] == result["loopback_posts"] == 0
            assert not result["real_account_or_credentials_used"]
            assert not owned[-1].exists() and fixtures[-1].closed
            unexpected_post = True
            with contextlib.redirect_stderr(io.StringIO()):
                try:
                    probe.campaign(Path("fixture-driver"), Path("fixture-codex"))
                except ValueError:
                    pass
                else:
                    raise AssertionError("provider activity accepted in a turnless campaign")
            assert owned[-1].is_dir() and fixtures[-1].closed
            unexpected_post = False
            failure = True
            report = io.StringIO()
            with contextlib.redirect_stderr(report):
                try:
                    probe.campaign(Path("fixture-driver"), Path("fixture-codex"))
                except ValueError as error:
                    assert "private-canary" not in str(error)
                else:
                    raise AssertionError("failed native pipeline accepted")
            assert owned[-1].is_dir() and fixtures[-1].closed
            assert (owned[-1] / "driver.stderr").read_text() == "private-canary"
            assert (owned[-1] / "driver.stderr").stat().st_mode & 0o777 == 0o600
            assert "private-canary" not in report.getvalue()
            assert json.loads(report.getvalue())["owned_fixture_retained"] == str(owned[-1])
            assert len(launches) == 3, "native campaign automatically retried"
            failure = False
            defaults = probe.default_feature_campaign(Path("fixture-driver"), Path("fixture-codex"))
            assert len(launches) == 7 and defaults["passed"]
            assert defaults["external_provider_requests"] is None
            assert defaults["other_network_metered"] is False
            assert defaults["scripted_model_provider_requests"] == 0
            assert [row["default_feature_startup"]["variant"] for row in defaults["variants"]] == list(probe.DEFAULT_FEATURE_VARIANTS)
            evidence = defaults["variants"][0]["default_feature_startup"]
            for key in ("shared_plugin_observation", "dedicated_plugin_observation"):
                bad = dict(evidence, **{key:{"name":"plugins", "enabled":False}})
                try:
                    probe.require_default_feature_observation(bad, "default")
                except ValueError:
                    pass
                else:
                    raise AssertionError("plugin-disabled input accepted as default qualification")
            recovered = copy.deepcopy(evidence)
            recovered["quiescent_startup"] = False
            recovered["startup"] = {"path":"recovered","run_id":"original-run","thread_id":"original-thread",
                "initial_error":{"code":"TRANSPORT_FAILURE","retryable":False,
                    "details":{"acceptance":"uncertain","stage":"correlate","run_id":"original-run","request_id":None}},
                "recovery":{"operation":"Recover","writer_state":"active","policy_access":"write","runs":1,
                    "manifest_unchanged":True,"worker_identity_unchanged":True,"dedicated_identity_unchanged":True,
                    "census_refusal":{"details":{"predicate":"dedicated_census_membership"}},
                    "request_sha256":"a"*64,"reservation_sha256":"b"*64,"manifest_sha256":"c"*64,
                    "run_generation":1,"receipt":{"run_id":"original-run","original_thread_id":"original-thread",
                        "accepted_intent_sha256":"b"*64,"run_generation":1},
                    "replay":{"exact_replay":True,"run_id":"original-run","thread_id":"original-thread"}}}
            probe.require_default_feature_observation(recovered, "default")
            invalid = []
            for path, value in [(("startup","initial_error"), None),
                    (("startup","recovery","policy_access"), "read"),
                    (("startup","recovery","census_refusal","details","predicate"), "dedicated_identity"),
                    (("startup","recovery","replay","run_id"), "replacement"),
                    (("startup","recovery","receipt","accepted_intent_sha256"), "d"*64),
                    (("startup","recovery","worker_identity_unchanged"), False),
                    (("quiescent_startup",), True)]:
                bad = copy.deepcopy(recovered)
                target = bad
                for key in path[:-1]:
                    target = target[key]
                target[path[-1]] = value
                invalid.append(bad)
            missing = copy.deepcopy(recovered)
            del missing["startup"]["recovery"]["receipt"]
            invalid.append(missing)
            for bad in invalid:
                try:
                    probe.require_default_feature_observation(bad, "default")
                except ValueError:
                    pass
                else:
                    raise AssertionError("unproved original-Run recovery accepted")
            interaction = probe.campaign(Path("fixture-driver"), Path("fixture-codex"), interactions=True)
            assert len(launches) == 8 and interaction["loopback_posts"] == 4
            assert fixtures[-1].closed and not owned[-1].exists()
            sequential_verified = False
            with contextlib.redirect_stderr(io.StringIO()):
                try:
                    probe.campaign(Path("fixture-driver"), Path("fixture-codex"), interactions=True)
                except ValueError:
                    pass
                else:
                    raise AssertionError("historical READ fallback helper accepted as sequential WRITE proof")
            assert fixtures[-1].closed and owned[-1].exists()
        plugin_root = parent / "source-observation"
        git = plugin_root / "fixture-a/.tmp/plugins/.git"
        (git / "refs/heads").mkdir(parents=True)
        (git / "config").write_text('[remote "origin"]\n\turl = https://github.com/openai/plugins.git\n')
        (git / "HEAD").write_text("ref: refs/heads/main\n")
        (git / "refs/heads/main").write_text("a"*40 + "\n")
        observed = probe.observed_plugin_sources(plugin_root)
        assert observed[0]["revision"] == "a"*40
        assert observed[0]["origin"] == "https://github.com/openai/plugins.git"
        assert observed[1]["repository_present"] is False and observed[1]["revision"] is None
        (git / "refs/heads/main").unlink()
        (git / "packed-refs").write_text("b"*40 + " refs/heads/main\n")
        assert probe.observed_plugin_sources(plugin_root)[0]["revision"] == "b"*40
    print("native caller pipeline offline checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
