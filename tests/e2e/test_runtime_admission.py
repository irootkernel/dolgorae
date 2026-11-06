#!/usr/bin/env python3
"""Separate generic Profile compatibility from qualified caller admission.

The Profile compatibility specification admits the selected release and a synthetic next patch;
caller-workspaces-v1 requires qualification before Run or review allocation.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile

import native_codex
from test_profile_cli import create_fake_codex, add_arguments, set_mode
from orphan_cleanup import cleanup_removed_root


@contextmanager
def fixture_root(binary: Path):
    root = None
    try:
        with tempfile.TemporaryDirectory(prefix="dolgorae-admission-") as temporary:
            root = Path(temporary).resolve()
            yield root
    finally:
        if root is not None:
            cleanup_removed_root(binary, root)


def validate(binary: Path) -> None:
    schema_source = native_codex.installed_codex()
    selected = native_codex.codex_version(schema_source)
    major, minor, patch = map(int, selected.split("."))
    for version in (selected, f"{major}.{minor}.{patch + 1}"):
        with fixture_root(binary) as root:
            home, workspace, profile_home, bin_root = (root / name for name in
                                                      ("home", "workspace", "profile", "bin"))
            for path in (home, workspace, profile_home, bin_root):
                path.mkdir(mode=0o700)
            environment = dict(os.environ, HOME=str(home))

            def call(*args: str, error: str | None = None, stdin: str | None = None) -> dict:
                result = subprocess.run([str(binary), *args], input=stdin, env=environment,
                                        capture_output=True, text=True, timeout=120)
                assert not result.stderr, result.stderr
                value = json.loads(result.stdout)
                if error:
                    assert result.returncode != 0 and value["error"]["code"] == error, value
                else:
                    assert result.returncode == 0, value
                return value

            subprocess.run(["git", "-C", str(workspace), "init", "-b", "main"],
                           check=True, capture_output=True, env=environment)
            workspace_id = call("init", str(workspace))["data"]["workspace_id"]
            operator, owner = root / "operator", root / "owner"
            call("operator", "credential", "initialize", "--output", str(operator))
            call("controller", "credential", "create", "--kind", "automation",
                 "--instance-id", "admission-fixture", "--output", str(owner))
            fake = bin_root / "codex"
            create_fake_codex(fake, schema_source, profile_home)
            set_mode(fake, version=version)
            call("profile", "add", "fixture", *add_arguments(profile_home, fake))
            doctor = call("profile", "doctor", "fixture")["data"]
            assert doctor["compatibility"] == ("tested" if tuple(map(int, version.split("."))) in native_codex.TESTED_CODEX_VERSIONS else "unverified")
            call("profile", "doctor", "fixture", "--launch-probe")
            key = doctor["server_key"]
            state_root = home / ".dolgorae/workspaces" / workspace_id
            try:
                started = call("profile", "server", "start", "fixture")["data"]
                assert started["state"]["server_key"] == key
                capabilities = call("runtime", "capabilities", "--profile", "fixture")["data"]
                assert capabilities["caller_workspace"]["integrated"] is False
                for name in ("managed_instructions", "modern_startup", "checked_candidate_reads"):
                    assert capabilities["caller_workspace"][name]["state"] == "unavailable"
                before_runs = sorted((state_root / "runs").glob("*"))

                def refuse(_: int) -> None:
                    call("run", "start", "--workspace", str(workspace), "--controller-file", str(owner),
                         "--profile", "fixture", "--control-mode", "managed-agent", "--purpose",
                         "implementation", "--access", "read-only", "--idempotency-key", "fresh-old",
                         error="WORKSPACE_STARTUP_UNSUPPORTED")

                with ThreadPoolExecutor(max_workers=2) as pool:
                    list(pool.map(refuse, range(2)))
                assert sorted((state_root / "runs").glob("*")) == before_runs
                assert not list((state_root / "idempotency/run-start").glob("*.json"))
                request = {"schema": "dolgorae-specialist-review-request/v4", "operation": "review_target",
                           "target": {"kind": "workspace", "revision": None}, "purpose": "completion",
                           "brief": "Check the caller candidate", "contexts": [{"id": "ctx", "content":
                           "Accepted requirement", "provenance": "caller"}], "criteria": [{"id": "criterion",
                           "statement": "Candidate meets the requirement", "source_context_ids": ["ctx"]}],
                           "expected_output": "structured_review_v4", "deadline_seconds": 60, "role": None}
                for one_shot in (False, True):
                    args = ["specialist", "review", "--workspace", str(workspace), "--profile", "fixture",
                            "--request-stdin", "--format", "json"]
                    if one_shot:
                        args += ["--request-ref", "01900000-0000-7000-8000-000000000077",
                                 "--recovery-controller-file", str(owner)]
                    call(*args, stdin=json.dumps(request), error="REVIEW_READING_BOUNDARY_UNSUPPORTED")
                assert sorted((state_root / "runs").glob("*")) == before_runs
                assert not list((state_root / "review-targets").glob("*"))
                for database in state_root.rglob("*.sqlite3"):
                    with sqlite3.connect(database) as connection:
                        for (table,) in connection.execute("SELECT name FROM sqlite_master WHERE type='table'"):
                            if table in ("one_shot_operations", "external_hire_operations", "members", "tasks"):
                                assert connection.execute(f'SELECT COUNT(*) FROM "{table}"').fetchone()[0] == 0
                # Seed a prior one-shot acceptance below admission, as the Rust
                # retained-state fixtures do. Retrying it must precede qualification.
                reference = "01900000-0000-7000-8000-000000000078"
                credential = json.loads(owner.read_text())
                identity = {name: credential[name] for name in
                            ("controller_id", "kind", "instance_id", "subject_id")}
                identity["generation"] = 1
                capability = base64.urlsafe_b64decode(credential["capability"] + "=")
                controller = {"identity": identity, "capability_sha256": hashlib.sha256(
                    b"dolgorae.controller-capability.v1\0" + capability).hexdigest()}
                def canonical(value):
                    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
                retained_request = {name: value for name, value in request.items() if value is not None}
                retained_request["target"] = {"kind": request["target"]["kind"]}
                prior = {"schema": "dolgorae-one-shot-operation/v1", "input": {
                    "request_ref": reference, "workspace_id": workspace_id, "request": retained_request,
                    "profile": "fixture", "recovery_controller": controller,
                    "carrier_root": str(state_root / "orchestration" / ("one-shot-" + reference)),
                    "temporary_server": False}, "global_profile_binding": None, "capture_ref": None,
                    "engagement_id": None, "terminal": None}
                prior["request_sha256"] = hashlib.sha256(canonical(retained_request)).hexdigest()
                prior["input_sha256"] = hashlib.sha256(canonical(prior["input"])).hexdigest()
                receipt = canonical(prior).decode()
                database = state_root / "orchestration/orchestration.sqlite3"
                with sqlite3.connect(database) as connection:
                    connection.execute("INSERT INTO one_shot_operations(request_ref,receipt_json,receipt_sha256,engagement_id) VALUES(?,?,?,NULL)",
                                       (reference, receipt, hashlib.sha256(receipt.encode()).hexdigest()))
                replay = call("specialist", "review", "--workspace", str(workspace), "--profile", "fixture",
                              "--request-stdin", "--format", "json", "--request-ref", reference,
                              "--recovery-controller-file", str(owner), stdin=json.dumps(request),
                              error="REVIEW_RECOVERY_BLOCKED")
                assert replay["error"]["details"]["reason"] == "already_started"
                changed = dict(request, brief="A conflicting request")
                call("specialist", "review", "--workspace", str(workspace), "--profile", "fixture",
                     "--request-stdin", "--format", "json", "--request-ref", reference,
                     "--recovery-controller-file", str(owner), stdin=json.dumps(changed),
                     error="IDEMPOTENCY_CONFLICT")
                with sqlite3.connect(database) as connection:
                    assert connection.execute("SELECT receipt_json FROM one_shot_operations WHERE request_ref=?", (reference,)).fetchone()[0] == receipt
                status = call("profile", "server", "status", "fixture")["data"]
                assert status["state"]["lifecycle"] == "ready"
                assert status["server_key"] == key
            finally:
                call("profile", "server", "stop", "fixture", "--operator-file", str(operator),
                     "--interrupt", "--confirm-server-key", key)
    print("Runtime admission separation passed: generic lifecycle, concurrent Run refusal, checked review, one-shot, capabilities")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    validate(parser.parse_args().binary.resolve(strict=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
