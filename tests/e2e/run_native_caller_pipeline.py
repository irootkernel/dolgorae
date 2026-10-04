#!/usr/bin/env python3
"""Opt-in Dolgorae caller pipeline with fixture-owned state and no model Turns."""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from run_live_instruction_discovery import checked_codex, executable_identity, isolated_environment, private_file
from run_native_workspace_policy_fixture import ScriptedResponses, fixture_config

OPT_IN = "DOLGORAE_RUN_NATIVE_CALLER_PIPELINE"
CODEX_VERSION = "0.159.3"
CODEX_SHA256 = "4d210f7c5a18fd0386434df23b5bdbb8c0e7257d3e8a2b30b0769c8bbe99a878"


def campaign(driver: Path, codex: Path) -> dict[str, object]:
    if os.environ.get(OPT_IN) != "1":
        raise ValueError("explicit native caller pipeline opt-in required")
    if executable_identity(codex)["sha256"] != CODEX_SHA256:
        raise ValueError("native caller pipeline requires the exact qualified executable")
    scratch = Path(tempfile.mkdtemp(prefix="dcp-", dir=os.environ["TMPDIR"])).resolve()
    scratch.chmod(0o700)
    fixture: ScriptedResponses | None = None
    passed = False
    try:
        for name in ("home", "profile", "tmp", "config", "cache", "fixture-a", "fixture-b"):
            (scratch / name).mkdir(mode=0o700)
        (scratch / "home" / "workspace").mkdir(mode=0o700)
        environment = isolated_environment(scratch)
        environment[OPT_IN] = "1"
        checked_codex(codex, f"codex-cli {CODEX_VERSION}", environment)
        fixture = ScriptedResponses()
        for name in ("fixture-a", "fixture-b"):
            private_file(scratch / name / "config.toml", b'model = "gpt-5.6-luna"\nmodel_reasoning_effort = "low"\n' + fixture_config(fixture.base_url))
            private_file(scratch / name / "AGENTS.md", b"Permitted isolated native Profile guidance.\n")
        private_file(scratch / "fixture-owned", b"native-caller-pipeline/v1\n")
        private_file(scratch / "home" / "AGENTS.md", b"Conflicting isolated ancestor instructions.\n")
        private_file(scratch / "home" / "workspace" / "AGENTS.md", b"Conflicting isolated project instructions.\n")
        result = subprocess.run([str(driver), str(scratch), str(codex)], env=environment,
                                capture_output=True, text=True, timeout=300)
        private_file(scratch / "driver.stdout", result.stdout.encode())
        private_file(scratch / "driver.stderr", result.stderr.encode())
        if result.returncode or fixture.posts or fixture.failure:
            raise ValueError("native caller pipeline failed; no automatic retry")
        observations = json.loads(result.stdout)
        if observations.get("schema") != "dolgorae.native-caller-pipeline/v1":
            raise ValueError("native caller pipeline observation shape invalid")
        if (scratch / "cleanup-result").read_bytes() != b"complete\n":
            raise ValueError("native caller pipeline cleanup incomplete")
        observations.update(external_provider_requests=0, loopback_posts=fixture.posts,
                            real_account_or_credentials_used=False, executable_sha256=CODEX_SHA256,
                            codex_version=CODEX_VERSION, profile_capability_mutated=False)
        passed = True
        return observations
    finally:
        if fixture is not None:
            fixture.close()
        cleanup = scratch / "cleanup-result"
        if passed and cleanup.is_file() and cleanup.read_bytes() == b"complete\n":
            shutil.rmtree(scratch)
        else:
            # Retain failed fixture evidence and any unresolved managed runtime.
            print(json.dumps({"schema": "dolgorae.native-caller-pipeline-failure/v1",
                              "owned_fixture_retained": str(scratch),
                              "cleanup_complete": cleanup.is_file() and cleanup.read_bytes() == b"complete\n"}), file=sys.stderr)


def main() -> int:
    if os.environ.get(OPT_IN) != "1":
        print("native caller pipeline requires explicit opt-in")
        return 2
    parser = argparse.ArgumentParser()
    parser.add_argument("--driver", type=Path, required=True)
    parser.add_argument("--codex", type=Path, required=True)
    args = parser.parse_args()
    try:
        observations = campaign(args.driver.resolve(strict=True), args.codex.resolve(strict=True))
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
        print(json.dumps({"schema": "dolgorae.native-caller-pipeline-failure/v1", "passed": False}))
        return 1
    print(json.dumps(observations, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
