#!/usr/bin/env python3
"""Caller defaults, migration, retirement and generic Profile admission separation."""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import tempfile
from pathlib import Path

import native_codex
from test_runtime_admission import validate as validate_runtime_admission
from test_profile_cli import create_fake_codex, set_mode
from test_trusted_host_setup_cli import (validate as validate_trusted_host_setup,
                                         validate_member_recovery)
from run_native_integrated_acceptance import TURN_TIMEOUT, overwrite_fixture_file
from schema_support import assert_valid, validator

PROTOCOL = Path(__file__).resolve().parents[2] / "docs" / "protocol"


def validate(binary: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="dolgorae-cutover-", dir=os.environ["TMPDIR"]) as temporary:
        root = Path(temporary)
        home, workspace = root / "home", root / "workspace"
        home.mkdir(mode=0o700)
        workspace.mkdir(mode=0o700)
        environment = dict(os.environ, HOME=str(home))

        def call(*args: str, request: dict | None = None, error: str | None = None) -> dict:
            completed = subprocess.run([str(binary), *args], env=environment,
                                       input=json.dumps(request) if request is not None else None,
                                       capture_output=True, text=True, timeout=10)
            envelope = json.loads(completed.stdout)
            assert_valid(envelope, validator(PROTOCOL,
                         f"dolgorae-machine-v{envelope['schema_version']}.schema.json"), "cutover output")
            assert not completed.stderr
            if error:
                assert completed.returncode != 0 and envelope["error"]["code"] == error, envelope
                return envelope
            assert completed.returncode == 0 and envelope["ok"], envelope
            return envelope["data"]

        initialized = call("init", "--non-git", str(workspace))
        for relative in ("home/AGENTS.md", "roles/general/AGENTS.md", "roles/reviewer/AGENTS.md"):
            source = home / ".dolgorae" / relative
            assert source.is_file()
            overwrite_fixture_file(source, b"Bounded fixture instructions.\n")
            assert source.read_bytes() == b"Bounded fixture instructions.\n"
        controller = root / "probe-controller"
        call("controller", "credential", "create", "--kind", "automation",
             "--instance-id", "timeout-preflight", "--output", str(controller))
        send = ("run", "send", "018f1111-2222-7333-8444-555555555555", "--workspace", str(workspace),
                "--controller-file", str(controller), "--message", "duration preflight",
                "--idempotency-key", "missing-run")
        call(*send, "--timeout", "60", error="INVALID_ARGUMENT")
        call(*send, "--timeout", TURN_TIMEOUT, error="RUN_NOT_FOUND")
        retired = call("review-target", "capture", "--workspace", str(workspace),
                       "--kind", "workspace", "--backend-kind", "fixture",
                       "--backend-lifecycle-id", "retired", "--settlement-owner-file", str(root / "owner"),
                       error="SOURCE_ISOLATION_RETIRED")
        assert retired["schema_version"] == 4
        assert not (root / "owner").exists()
        state_root = home / ".dolgorae/workspaces" / initialized["workspace_id"]
        invalid = call("run", "start", "--workspace", str(workspace), "--profile", "missing",
                       "--control-mode", "managed-agent", "--purpose", "implementation",
                       "--idempotency-key", "invalid-role", "--role", "../escape", error="INVALID_ARGUMENT")
        assert invalid["schema_version"] == 4 and not list((state_root / "runs").iterdir())
        capabilities = call("runtime", "capabilities")
        assert capabilities["profile"] is None
        assert capabilities["caller_workspace"]["integrated"] is False
        for name in ("managed_instructions", "modern_startup", "checked_candidate_reads"):
            guarantee = capabilities["caller_workspace"][name]
            assert guarantee["state"] == "unverified" and guarantee["evidence_sha256"] is None

        profile_home = root / "profile-home"
        profile_home.mkdir(mode=0o700)
        fake = root / "codex"
        create_fake_codex(fake, native_codex.installed_codex(), profile_home)
        set_mode(fake, version="0.168.9")
        call("profile", "add", "historical-runtime", "--codex-home", str(profile_home),
             "--native-subagents", "enabled", "--env", "PATH=/usr/bin:/bin",
             "--env", "LANG=C", "--env", "LC_ALL=C", "--", str(fake))
        selected = call("runtime", "capabilities", "--profile", "historical-runtime")
        assert selected["profile"] == "historical-runtime"
        assert selected["caller_workspace"]["integrated"] is False
        for name in ("managed_instructions", "modern_startup", "checked_candidate_reads"):
            guarantee = selected["caller_workspace"][name]
            assert guarantee["state"] == "unavailable" and guarantee["evidence_sha256"] is None
        status = call("profile", "server", "status", "historical-runtime")
        assert status["lifecycle"] == "stopped"
        assert not list((state_root / "runs").iterdir())

        source = json.loads((PROTOCOL / "examples/role-source.valid.json").read_text())
        source["name"] = "audit"
        legacy_common = home / ".dolgorae/roles/audit.json"
        legacy_common.write_text(json.dumps(source))
        legacy_common.chmod(0o600)
        project = workspace / ".dolgorae/roles"
        project.mkdir(parents=True, mode=0o700)
        source["instructions"] = "Project-selected instructions.\n"
        legacy_project = project / "audit.json"
        legacy_project.write_text(json.dumps(source))
        legacy_project.chmod(0o600)
        original_common, original_project = legacy_common.read_bytes(), legacy_project.read_bytes()
        plan = call("instructions", "migrate", "--workspace", str(workspace))
        assert len(plan["roles"]) == 2 and all(role["name_collision"] for role in plan["roles"])
        selected = next(role for role in plan["roles"] if role["source"]["scope"] == "project")
        request = {"schema": "dolgorae-instruction-migration-request/v1", "roles": [{
            "source": selected["source"], "source_sha256": selected["source_sha256"],
            "destination_name": "project-audit", "publish": True}], "policies": []}
        missing_choice = json.loads(json.dumps(request))
        del missing_choice["roles"][0]["publish"]
        call("instructions", "migrate", "--workspace", str(workspace), "--request-stdin",
             request=missing_choice, error="INVALID_ARGUMENT")
        destination = home / ".dolgorae/roles/project-audit/AGENTS.md"
        assert not destination.exists()
        result = call("instructions", "migrate", "--workspace", str(workspace), "--request-stdin", request=request)
        assert result["schema"] == "dolgorae-instruction-migration-result/v1"
        assert destination.read_text() == source["instructions"]
        call("instructions", "migrate", "--workspace", str(workspace), "--request-stdin",
             request=request, error="INVALID_ARGUMENT")
        assert legacy_common.read_bytes() == original_common and legacy_project.read_bytes() == original_project
        policy = root / "legacy-policy.json"
        policy.write_bytes((PROTOCOL / "examples/specialist-policy-input-v2.valid.json").read_bytes())
        policy.chmod(0o600)
        call("specialist", "policy", "validate", "--workspace", str(workspace), "--file", str(policy),
             error="ROLE_MIGRATION_REQUIRED")
        old_request = json.loads((PROTOCOL / "examples/specialist-review-v3-request.valid.json").read_text())
        call("specialist", "review", "--workspace", str(workspace), "--profile", "missing",
             "--request-stdin", "--format", "json", request=old_request, error="SOURCE_ISOLATION_RETIRED")
        assert not list((state_root / "runs").iterdir()) and not (state_root / "review-targets").exists()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--trusted-host-setup-only", action="store_true")
    arguments = parser.parse_args()
    binary = arguments.binary.resolve(strict=True)
    if not arguments.trusted_host_setup_only:
        validate(binary)
        validate_runtime_admission(binary)
    validate_trusted_host_setup(binary)
    validate_member_recovery(binary)
    print("Trusted host setup CLI checks passed" if arguments.trusted_host_setup_only
          else "Caller cutover CLI checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
