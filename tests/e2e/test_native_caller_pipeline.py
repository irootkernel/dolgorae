#!/usr/bin/env python3
"""Offline admission, isolation and cleanup checks for the native caller driver."""
from __future__ import annotations

import contextlib
import argparse
import io
import json
import os
import subprocess
import tempfile
from pathlib import Path
from unittest.mock import patch

import run_native_caller_pipeline as probe


def main() -> int:
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

        def run(argv, *, env, **_kwargs):
            if argv == ["fixture-codex", "--version"]:
                return subprocess.CompletedProcess(argv, 0, "codex-cli 0.159.3\n", "")
            root = Path(argv[1])
            launches.append(argv)
            owned.append(root)
            assert env[probe.OPT_IN] == "1" and Path(env["HOME"]) == root / "home"
            assert (root / "fixture-owned").read_bytes() == b"native-caller-pipeline/v1\n"
            assert (root / "home" / "workspace").is_dir()
            assert not (root / "home" / ".git").exists()
            assert (root / "home" / "workspace" / "AGENTS.md").is_file()
            assert all((root / name / "AGENTS.md").is_file() for name in ("fixture-a", "fixture-b", "home"))
            if failure:
                return subprocess.CompletedProcess(argv, 1, "private-canary", "private-canary")
            (root / "cleanup-result").write_bytes(b"complete\n")
            fixtures[-1].posts = int(unexpected_post)
            return subprocess.CompletedProcess(argv, 0, json.dumps({"schema": "dolgorae.native-caller-pipeline/v1"}), "")

        with patch.dict(os.environ, {probe.OPT_IN: "1", "TMPDIR": str(parent),
                                    "OPENAI_API_KEY": "private-canary", "PRIVATE_FIXTURE_CANARY": "private-canary"}), \
             patch.object(probe, "executable_identity", return_value={"sha256": probe.CODEX_SHA256}), \
             patch("run_live_instruction_discovery.executable_identity", return_value={"sha256": probe.CODEX_SHA256}), \
             patch.object(probe, "checked_codex", side_effect=version), \
             patch.object(probe, "ScriptedResponses", Fixture), \
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
    print("native caller pipeline offline checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
