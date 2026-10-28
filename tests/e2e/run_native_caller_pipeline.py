#!/usr/bin/env python3
"""Opt-in native caller and read-only pool checks in fixture-owned state."""
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
CODEX_VERSION = "0.164.0"
CODEX_SHA256 = "d8990e3ee7acfbc26f91265b12a1a83f0fee40d0e0f8f1f40a37d4ec21f499cf"


def campaign(driver: Path, codex: Path, *, default_variant: str | None = None, readonly_pool: bool = False, pool_limit: int = 1) -> dict[str, object]:
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
        if readonly_pool:
            environment["DOLGORAE_NATIVE_READONLY_POOL"] = "1"
            private_file(scratch / "pool-limit", str(pool_limit).encode())
        checked_codex(codex, f"codex-cli {CODEX_VERSION}", environment)
        fixture = ScriptedResponses()
        if default_variant is not None:
            if default_variant not in DEFAULT_FEATURE_VARIANTS:
                raise ValueError("unknown default-feature variant")
            private_file(scratch / "default-feature-variant", default_variant.encode())
        for name in ("fixture-a", "fixture-b"):
            config=b'model = "gpt-5.6-luna"\nmodel_reasoning_effort = "low"\n'
            if readonly_pool and name == "fixture-b":
                config += b'model_context_window = 65536\n'
            config += fixture_config(fixture.base_url)
            if default_variant == "config-enable":
                config += b'\n[features]\nshell_snapshot = true\n'
            elif default_variant is None:
                config += b'\n[features]\nshell_snapshot = false\n'
            private_file(scratch / name / "config.toml", config)
            private_file(scratch / name / "AGENTS.md", b"Permitted isolated native Profile guidance.\n")
        private_file(scratch / "fixture-owned", b"native-caller-pipeline/v1\n")
        private_file(scratch / "home" / "AGENTS.md", b"Conflicting isolated ancestor instructions.\n")
        private_file(scratch / "home" / "workspace" / "AGENTS.md", b"Conflicting isolated project instructions.\n")
        if readonly_pool:
            import shlex
            command = "printf attempted > " + shlex.quote(str(scratch / "home" / "workspace" / "pool-write-denied"))
            private_file(scratch / "pool-command", command.encode())
            fixture.prepare(command)
            fixture.instruction_markers = {"common": "Follow the current task and the selected Role", "role": "POOL_EXPLICIT_ROLE", "profile": "Permitted isolated native Profile guidance.", "project": "Conflicting isolated project instructions.", "ancestor": "Conflicting isolated ancestor instructions."}
        result = subprocess.run([str(driver), str(scratch), str(codex)], env=environment,
                                capture_output=True, text=True, timeout=300)
        private_file(scratch / "driver.stdout", result.stdout.encode())
        private_file(scratch / "driver.stderr", result.stderr.encode())
        if result.returncode or fixture.failure or (not readonly_pool and fixture.posts):
            raise ValueError("native caller pipeline failed; no automatic retry")
        if readonly_pool:
            if (fixture.result is None or fixture.result[0] == 0 or fixture.posts != 2
                    or fixture.instruction_inputs_observed != 1
                    or fixture.ambient_seen["project"] or fixture.ambient_seen["ancestor"]
                    or (scratch / "home" / "workspace" / "pool-write-denied").exists()):
                raise ValueError("native pooled read-only or instruction suppression failed")
        observations = json.loads(result.stdout)
        if observations.get("schema") != "dolgorae.native-caller-pipeline/v1":
            raise ValueError("native caller pipeline observation shape invalid")
        if (scratch / "cleanup-result").read_bytes() != b"complete\n":
            raise ValueError("native caller pipeline cleanup incomplete")
        if default_variant is not None:
            require_default_feature_observation(observations.get("default_feature_startup"), default_variant)
        if readonly_pool:
            observations.update(native_write_exit=fixture.result[0], explicit_instruction_inputs=fixture.instruction_inputs_observed, ambient_markers_seen=fixture.ambient_seen)
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


DEFAULT_FEATURE_VARIANTS = ("default", "config-enable", "argv-enable", "ordered-overrides")


def require_default_feature_observation(value, variant: str) -> None:
    if not isinstance(value, dict) or value.get("variant") != variant or value.get("effective_shell_snapshot") is not False or value.get("quiescent_startup") is not True:
        raise ValueError("actual default-feature behavior unproved")
    launch=value.get("accepted_launch", {})
    if ("shell_snapshot" not in launch.get("disabled_features", [])
            or value.get("recomputed_launch_digest") != launch.get("launch_contract_sha256")
            or value.get("recomputed_server_key") != launch.get("server_key")
            or not isinstance(value.get("actual_pid"), int) or value["actual_pid"] <= 0
            or not isinstance(value.get("actual_command"), str) or not value["actual_command"]
            or not isinstance(value.get("run_launch_digest"), str)
            or value.get("shared_feature_observation", {}).get("enabled") is not False
            or value.get("dedicated_feature_observation", {}).get("enabled") is not False
            or value.get("shared_feature_observation", {}).get("name") != "shell_snapshot"
            or value.get("dedicated_feature_observation", {}).get("name") != "shell_snapshot"
            or value.get("dedicated_argv", [])[:len(launch.get("normalized_argv", []))] != launch.get("normalized_argv")):
        raise ValueError("actual argv/accepted launch binding unproved")
    argv=launch.get("normalized_argv", [])
    decisions=[a == "--enable" for a,b in zip(argv,argv[1:]) if a in ("--enable","--disable") and b == "shell_snapshot"]
    if not decisions or decisions[-1] is not False or "--disable shell_snapshot" not in value["actual_command"]:
        raise ValueError("actual final shell_snapshot disabling unproved")


def default_feature_campaign(driver: Path, codex: Path) -> dict[str, object]:
    if os.environ.get(OPT_IN) != "1":
        raise ValueError("explicit native caller pipeline opt-in required")
    observations=[campaign(driver,codex,default_variant=v) for v in DEFAULT_FEATURE_VARIANTS]
    return {"schema":"dolgorae.default-feature-startup/v1","variants":observations,"passed":True,
            "external_provider_requests":0,"native_model_turns":0,"existing_quiescence_rules_unchanged":True}


def main() -> int:
    if os.environ.get(OPT_IN) != "1":
        print("native caller pipeline requires explicit opt-in")
        return 2
    parser = argparse.ArgumentParser()
    parser.add_argument("--driver", type=Path, required=True)
    parser.add_argument("--codex", type=Path, required=True)
    parser.add_argument("--default-feature-startup-only", action="store_true")
    parser.add_argument("--readonly-pool", action="store_true")
    parser.add_argument("--pool-limit", type=int, choices=(1, 4), default=1)
    args = parser.parse_args()
    try:
        selected=default_feature_campaign if args.default_feature_startup_only else campaign
        kwargs = {"readonly_pool": True, "pool_limit": args.pool_limit} if args.readonly_pool else {}
        observations = selected(args.driver.resolve(strict=True), args.codex.resolve(strict=True), **kwargs)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
        print(json.dumps({"schema": "dolgorae.native-caller-pipeline-failure/v1", "passed": False}))
        return 1
    print(json.dumps(observations, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
