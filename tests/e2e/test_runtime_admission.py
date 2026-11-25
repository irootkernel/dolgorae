#!/usr/bin/env python3
"""Separate generic Profile compatibility from qualified caller admission.

Generic Profile compatibility admits synthetic versions below the qualified creation
minimum; caller-workspaces-v1 refuses them before Run or review allocation.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import fcntl
import json
import os
from pathlib import Path
import selectors
import sqlite3
import stat
import subprocess
import tempfile
import time

import accepted_legacy_fixture
import native_codex
from test_profile_cli import add_arguments, append_membership, create_fake_codex, set_mode, write_bound_manifest
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
    # These are offline boundary fixtures, not qualification of old runtimes.
    for version in ("0.169.0", "0.171.9", "0.172.0-rc.1"):
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
            assert doctor["compatibility"] == "unverified"
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
    validate_rollover(binary, schema_source)


def validate_rollover(binary: Path, schema_source: Path) -> None:
    with fixture_root(binary) as root:
        home, workspace, profile_home, bin_root, fault_bin = (
            root / name for name in ("home", "workspace", "profile", "bin", "fault-bin")
        )
        for path in (home, workspace, profile_home, bin_root, fault_bin):
            path.mkdir(mode=0o700)
        env = dict(os.environ, HOME=str(home))

        def call(*args: str, stdin: str | None = None, error: str | None = None) -> dict:
            completed = subprocess.run([str(binary), *args], input=stdin, env=env,
                                       capture_output=True, text=True, timeout=120)
            assert not completed.stderr, completed.stderr
            result = json.loads(completed.stdout)
            if error:
                assert completed.returncode and result["error"]["code"] == error, result
                return result["error"]
            assert completed.returncode == 0, result
            return result["data"]

        subprocess.run(["git", "-C", str(workspace), "init", "-b", "main"],
                       check=True, capture_output=True, env=env)
        workspace_id = call("init", str(workspace))["workspace_id"]
        operator, owner = root / "operator", root / "owner"
        configuration = profile_home / "config.toml"
        configuration.write_text('model = "gpt-5.6"\nmodel_reasoning_effort = "medium"\n')
        configuration.chmod(0o600)
        call("operator", "credential", "initialize", "--output", str(operator))
        call("controller", "credential", "create", "--kind", "automation", "--instance-id",
             "rollover-fixture", "--output", str(owner))
        fake, fault = bin_root / "codex", fault_bin / "codex"
        # Synthetic versions use the configured schema source without taking its verified version.
        for name, executable, version in (("fixture", fake, "0.172.1"),
                                          ("fault", fault, "0.174.1")):
            create_fake_codex(executable, schema_source, profile_home)
            set_mode(executable, version=version)
            call("profile", "add", name, *add_arguments(profile_home, executable))

        dol = home / ".dolgorae"
        home_key = hashlib.sha256(b"dolgorae-home-v1\0" + os.fsencode(profile_home)).hexdigest()
        active_path = dol / "homes" / home_key / "active.json"
        migration_path = active_path.with_name("migration.json")
        run_root = dol / "workspaces" / workspace_id / "runs"

        def read(path: Path) -> dict:
            return json.loads(path.read_text())

        def state_path(key: str) -> Path:
            return dol / "profiles" / key / "state.json"

        def ready(version: str) -> dict:
            active = read(active_path)
            state = read(state_path(active["server_key"]))
            assert active["lifecycle"] == state["lifecycle"] == "ready", (active, state)
            assert active["server_epoch"] == state["server_epoch"], (active, state)
            assert state["snapshot"]["codex_version"] == version, state
            return active

        def committed(old: dict, version: str) -> dict:
            active = ready(version)
            record = read(migration_path)
            assert record["phase"] == "committed", record
            assert record["old_server_key"] == old["server_key"], record
            assert record["new_server_key"] == active["server_key"] != old["server_key"], record
            assert record["old_epoch"] == old["server_epoch"], record
            assert record["new_epoch"] == active["server_epoch"], record
            assert not state_path(old["server_key"]).exists()
            return active

        def start_run(profile: str, key: str, error: str) -> dict:
            return call("run", "start", "--workspace", str(workspace), "--controller-file",
                        str(owner), "--profile", profile, "--control-mode", "managed-agent",
                        "--purpose", "implementation", "--access", "read-only", "--model",
                        "fixture-missing", "--idempotency-key", key, error=error)

        def typed_fixture(*args: str, error: str | None = None) -> dict:
            completed = accepted_legacy_fixture.prepare(binary, home, list(args), env=env, timeout=120)
            assert not completed.stderr, completed.stderr
            result = json.loads(completed.stdout)
            if error:
                assert completed.returncode and result["error"]["code"] == error, result
                return result["error"]
            assert completed.returncode == 0, result
            return result["data"]

        def specialist_args(carrier: str, key: str, model: str = "fixture-missing") -> list[str]:
            return ["typed-start", carrier, "--workspace", str(workspace), "--profile", "fixture",
                    "--control-mode", "managed-agent", "--execution-lane", "dedicated",
                    "--required-assurance", "best-effort-personal-alpha", "--purpose", "review",
                    "--model", model, "--instructions", "Fixture", "--controller-file", str(owner),
                    "--idempotency-key", key]

        def specialist_start(carrier: str, key: str, error: str) -> dict:
            return typed_fixture(*specialist_args(carrier, key), error=error)

        @contextmanager
        def startup_barrier(command: list[str], environment: dict, gate: Path, stdin: str = ""):
            with subprocess.Popen(command, env=environment, stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) as child:
                child.stdin.write(stdin)
                child.stdin.close()
                child.stdin = None
                try:
                    deadline = time.monotonic() + 90
                    while not gate.with_suffix(".entered").exists():
                        assert child.poll() is None, child.communicate()
                        assert time.monotonic() < deadline, "fake app-server did not reach startup barrier"
                        time.sleep(0.025)
                    yield child
                finally:
                    gate.with_suffix(".release").touch()
                    try:
                        child.communicate(timeout=120)
                    except subprocess.TimeoutExpired:
                        child.kill()
                        child.communicate()
                        raise

        typed_fixture("rollover-fixture", "former-manifest", "--profile", "fixture")
        former = ready("0.172.1")
        start_run("fixture", "manifest-only-rollover", "COMPATIBILITY_REJECTED")
        first = ready("0.172.1")
        assert committed(former, "0.172.1") == first
        record = read(migration_path)
        for field in ("version", "executable_sha256", "schema_sha256"):
            assert record["old_" + field] == record["new_" + field], record
        assert record["old_manifest_sha256"] != record["new_manifest_sha256"], record

        set_mode(fake, version="0.172.2")
        updated = call("profile", "doctor", "fixture")["server_key"]
        call("profile", "doctor", "fixture", "--launch-probe", error="PROFILE_LAUNCH_CONFLICT")
        call("profile", "server", "start", "fixture", "--confirm-server-key", updated,
             error="PROFILE_LAUNCH_CONFLICT")
        typed_fixture("rollover-fixture", "retained-qualified", "--profile", "fixture",
                      error="PROFILE_LAUNCH_CONFLICT")
        typed_fixture("rollover-fixture", "retained-successor", "--profile", "fixture",
                      error="PROFILE_LAUNCH_CONFLICT")
        assert ready("0.172.1") == first and read(migration_path) == record

        profile_root = state_path(first["server_key"]).parent
        member = "01900000-0000-7000-8000-000000000080"
        member_workspace = "c" * 64
        member_root = dol / "workspaces" / member_workspace / "runs" / member
        append_membership(profile_root, "run_registered", member_workspace, member, first["server_epoch"])
        write_bound_manifest(home, profile_root, member_workspace, member)
        specialist_start("external", "live-member-fence", "PROFILE_SERVER_BUSY")
        (member_root / "manifest.json").unlink()
        member_root.rmdir()
        specialist_start("brokered", "orphan-member-fence", "PROFILE_SERVER_BUSY")
        assert ready("0.172.1") == first and read(migration_path) == record
        append_membership(profile_root, "run_closed", member_workspace, member, first["server_epoch"])
        member_root.parent.rmdir()
        member_root.parent.parent.rmdir()

        rejected = start_run("fixture", "new-caller-rollover", "COMPATIBILITY_REJECTED")
        assert rejected["details"]["check"] == "model", rejected
        second = committed(first, "0.172.2")
        assert not list(run_root.glob("*"))

        set_mode(fake, version="0.172.1")
        for refused in (
            start_run("fixture", "qualified-downgrade", "PROFILE_MIGRATION_REQUIRED"),
            call("profile", "server", "start", "fixture", error="PROFILE_MIGRATION_REQUIRED"),
            typed_fixture("rollover-fixture", "retained-legacy", "--profile", "fixture",
                          error="PROFILE_MIGRATION_REQUIRED"),
        ):
            assert refused["details"]["drift_fields"] == ["codex_version"], refused
            assert refused["details"]["previous_server_key"] == second["server_key"], refused
        assert ready("0.172.2") == second

        # An explicit Operator stop clears the conflict for the pre-policy binding.
        set_mode(fake, version="0.172.2")
        call("profile", "server", "stop", "fixture", "--operator-file", str(operator),
             "--confirm-server-key", second["server_key"])
        set_mode(fake, version="0.172.1")
        underlying = typed_fixture("rollover-fixture", "retained-legacy", "--profile", "fixture")
        resumed = ready("0.172.1")
        assert resumed["server_key"] == underlying["server_key"] != first["server_key"]

        set_mode(fake, version="0.173.0")
        frames = [
            ("initialize", {"protocolVersion": "2026-07-28"}),
            ("tools/list", {}),
            ("tools/call", {"name": "dolgorae_review", "arguments": {}}),
        ]
        request = "".join(json.dumps({"jsonrpc": "2.0", "id": index, "method": method,
                                      "params": params}) + "\n"
                          for index, (method, params) in enumerate(frames, 1))
        mcp = subprocess.run([str(binary), "__specialist-review-mcp", "--workspace", str(workspace),
                              "--profile", "fixture"], input=request, env=env,
                             capture_output=True, text=True, timeout=120)
        assert mcp.returncode == 0 and not mcp.stderr, (mcp.stdout, mcp.stderr)
        initialized, advertised, unavailable = map(json.loads, mcp.stdout.splitlines())
        assert initialized["result"]["protocolVersion"] == "2026-07-28", initialized
        assert advertised["result"]["tools"] == [], advertised
        assert unavailable["error"]["data"]["code"] == "MCP_REVIEW_UNAVAILABLE", unavailable
        third = committed(resumed, "0.173.0")
        current = third
        for carrier, version, reference in (
            ("external", "0.173.1", "01900000-0000-7000-8000-000000000090"),
            ("brokered", "0.173.2", "01900000-0000-7000-8000-000000000094"),
        ):
            gate = root / (carrier + "-gate")
            set_mode(fake, version=version, startup_gate=gate)
            key = carrier + "-fixture"
            args = [*specialist_args(carrier, key, "gpt-5.6"), "--fixture-start-ref", reference]
            reservation = (run_root.parent / "idempotency" / "run-start"
                           / (hashlib.sha256(key.encode()).hexdigest() + ".json"))
            command, fixture_env, fixture_request = accepted_legacy_fixture.command(home, args, env)
            try:
                with startup_barrier(command, fixture_env, gate) as fresh:
                    assert read(migration_path)["phase"] == "applying"
                    assert not reservation.exists()
                    with reservation.with_suffix(".lock").open("rb") as lock:
                        try:
                            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                        except BlockingIOError:
                            pass
                        else:
                            raise AssertionError("fresh Specialist start released allocation during readiness")
                    gate.with_suffix(".release").touch()
                    stdout, stderr = fresh.communicate(timeout=120)
                assert fresh.returncode and not stderr, (stdout, stderr)
                refusal = json.loads(stdout)["error"]
                assert refusal["code"] == "INVALID_ARGUMENT", refusal
                assert refusal["details"]["argument"] == "agent_configuration", refusal
            finally:
                fixture_request.unlink()

            accepted = reservation.read_bytes()
            assert json.loads(accepted)["run_id"] == reference
            assert not (run_root / reference).exists()
            fresh_generation = committed(current, version)
            launch = read(state_path(fresh_generation["server_key"]))["snapshot"]["sanitized_environment"]
            assert launch.get("DOLGORAE_SUCCESSOR_PROFILE") == ("fixture" if carrier == "brokered" else None)
            typed_fixture(*args, error="INVALID_ARGUMENT")
            assert reservation.read_bytes() == accepted

            # Restore an older incumbent without changing the retained target binding.
            set_mode(fault, version="0.168.9")
            older_key = call("profile", "doctor", "fault")["server_key"]
            call("profile", "server", "start", "fault", "--confirm-server-key", older_key,
                 error="PROFILE_LAUNCH_CONFLICT")
            downgrade = call("profile", "server", "start", "fault", error="PROFILE_MIGRATION_REQUIRED")
            assert downgrade["details"] == {"profile": "fault", "new_server_key": older_key,
                    "previous_server_key": fresh_generation["server_key"], "drift_fields": ["codex_version"],
                    "required_action": "profile_server_migrate_with_operator"}, downgrade
            migrated = call("profile", "server", "migrate", "fault", "--operator-file", str(operator),
                            "--confirm-old-server-key", fresh_generation["server_key"],
                            "--confirm-new-server-key", older_key)
            assert migrated["migrated"] and migrated["server_key"] == older_key, migrated
            current = committed(fresh_generation, "0.168.9")
            migration = migration_path.read_bytes()

            def retained(_: int) -> None:
                typed_fixture(*args, error="PROFILE_LAUNCH_CONFLICT")

            with ThreadPoolExecutor(max_workers=2) as pool:
                list(pool.map(retained, range(2)))
            assert ready("0.168.9") == current and migration_path.read_bytes() == migration
            assert reservation.read_bytes() == accepted and not (run_root / reference).exists()
            print(f"Typed {carrier} rollover passed: held allocation at fresh readiness and retained retry refusal")

        request = json.dumps({"schema": "dolgorae-specialist-review-request/v4",
                              "operation": "review_target", "target": {"kind": "head"},
                              "purpose": "completion", "brief": "Review the empty fixture",
                              "contexts": [{"id": "ctx", "content": "Fixture", "provenance": "caller"}],
                              "criteria": [{"id": "criterion", "statement": "Check the fixture",
                                            "source_context_ids": ["ctx"]}],
                              "expected_output": "structured_review_v4", "deadline_seconds": 60})

        def review_args(reference: str, profile: str) -> list[str]:
            return ["specialist", "review", "--workspace", str(workspace), "--profile", profile,
                    "--request-stdin", "--format", "json", "--request-ref", reference,
                    "--recovery-controller-file", str(owner)]

        def review_ownership(key: str, reference: str) -> dict:
            claims = [read(path) for path in (state_path(key).parent / "review-owners").glob("*.json")]
            return next(claim for claim in claims if claim["owner"]["request_ref"] == reference)

        @contextmanager
        def paused_rollover(profile: str, reference: str):
            args = ["rollover-fixture", "concurrent-review", "--workspace", str(workspace),
                    "--profile", profile, "--request-ref", reference]
            command, fixture_env, fixture_request = accepted_legacy_fixture.command(home, args, env)
            try:
                with subprocess.Popen(command, env=fixture_env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                      stderr=subprocess.PIPE, text=True) as loser:
                    try:
                        with selectors.DefaultSelector() as wait:
                            wait.register(loser.stdout, selectors.EVENT_READ)
                            assert wait.select(timeout=90), "losing review did not finish rollover PREPARE"
                            prepared = json.loads(loser.stdout.readline())
                        assert prepared["phase"] == "rollover_prepared", prepared
                        yield loser, prepared
                    finally:
                        if loser.poll() is None:
                            loser.kill()
                            loser.communicate(timeout=10)
            finally:
                fixture_request.unlink()

        set_mode(fake, version="0.173.3")
        alias = "fixture-removed"
        call("profile", "add", alias, *add_arguments(profile_home, fake))
        reference = "01900000-0000-7000-8000-000000000081"
        loser_ref = "01900000-0000-7000-8000-000000000086"
        removed_ref = "01900000-0000-7000-8000-000000000087"
        before_migration = migration_path.read_bytes()
        with (paused_rollover("fixture", loser_ref) as (loser, prepared),
              paused_rollover(alias, removed_ref) as (removed, alias_prepared)):
            assert alias_prepared == prepared
            assert prepared["incumbent_key"] == current["server_key"], prepared
            assert prepared["requested_key"] != prepared["incumbent_key"], prepared
            assert read(active_path) == current and migration_path.read_bytes() == before_migration
            assert not state_path(prepared["requested_key"]).exists()

            # The product CLI commits the replacement before it rejects the empty Git target.
            review_failure = call(*review_args(reference, "fixture"), stdin=request,
                                  error="REVIEW_TARGET_UNAVAILABLE")
            fourth = committed(current, "0.173.3")
            assert fourth["server_key"] == prepared["requested_key"], fourth
            winner_path = state_path(fourth["server_key"])
            winner_bytes = winner_path.read_bytes()
            winner = json.loads(winner_bytes)
            migration = migration_path.read_bytes()
            claim = review_ownership(fourth["server_key"], reference)
            assert claim["phase"] == "ready" and claim["state"] == winner, claim
            socket = Path(winner["socket_path"])
            identity = (winner["socket_device"], winner["socket_inode"])
            metadata = socket.lstat()
            assert stat.S_ISSOCK(metadata.st_mode) and (metadata.st_dev, metadata.st_ino) == identity

            history = dol / "profile-bindings.json"
            removed_binding = alias + ":" + fourth["server_key"]
            assert removed_binding in read(history)["bindings"]
            call("profile", "remove", alias)
            assert removed_binding not in read(history)["bindings"]
            claims = winner_path.parent / "review-owners"
            ownership_before = {path.name: path.read_bytes() for path in claims.glob("*.json")}
            stdout, stderr = removed.communicate(input="1", timeout=30)
            assert removed.returncode and not stderr, (stdout, stderr)
            rejected = json.loads(stdout)["error"]
            assert rejected["code"] == "PROFILE_NOT_FOUND" and rejected["details"]["profile"] == alias, rejected
            assert {path.name: path.read_bytes() for path in claims.glob("*.json")} == ownership_before

            stdout, stderr = loser.communicate(input="1", timeout=30)
            assert loser.returncode == 0 and not stderr, (stdout, stderr)
            joined = json.loads(stdout)["data"]
            assert joined["state"] == winner, joined
            assert joined["retirement"]["status"] == "preexisting", joined
            assert joined["retirement"]["epoch_id"] == winner["epoch_id"], joined
            claim = review_ownership(fourth["server_key"], loser_ref)
            assert claim["phase"] == "preexisting" and claim["state"] == winner, claim
            assert claim["launch_token"] is None and claim["owner"]["workspace_id"] == workspace_id
            assert read(active_path) == fourth and winner_path.read_bytes() == winner_bytes
            assert migration_path.read_bytes() == migration
            assert not state_path(prepared["incumbent_key"]).exists()
            metadata = socket.lstat()
            assert stat.S_ISSOCK(metadata.st_mode) and (metadata.st_dev, metadata.st_ino) == identity
            os.kill(winner["pid"], 0)
        print("Concurrent removed alias refused: exact winner intact without stale ownership")
        print("Concurrent review rollover passed: independent commit, exact Preexisting attach, no retirement")

        # A separate executable lets the failed candidate and rollback source have
        # independent startup outcomes in the same canonical Codex home.
        fault_gate = root / "fault-gate"
        set_mode(fault, version="0.174.1", startup_gate=fault_gate, startup_fail=True)
        fault_ref = "01900000-0000-7000-8000-000000000082"

        def reserved() -> None:
            record = read(migration_path)
            assert record["phase"] == "applying", record
            assert record["old_server_key"] == fourth["server_key"], record
            claim = review_ownership(record["new_server_key"], fault_ref)
            assert claim["phase"] == "reserved" and claim["launch_token"] and claim["state"] is None, claim
            call("profile", "server", "start", "fixture", error="PROFILE_SERVER_BUSY")

        with startup_barrier([str(binary), *review_args(fault_ref, "fault")], env,
                             fault_gate, request) as child:
            reserved()
            fault_gate.with_suffix(".release").touch()
            stdout, stderr = child.communicate(timeout=120)
            assert child.returncode and not stderr, (stdout, stderr)
            failed = json.loads(stdout)["error"]
        assert failed["code"] == "TRANSPORT_FAILURE", failed
        rolled_back = ready("0.173.3")
        record = read(migration_path)
        assert record["phase"] == "rolled_back" and record["old_server_key"] == rolled_back["server_key"], record
        assert rolled_back["server_key"] == fourth["server_key"] and rolled_back["server_epoch"] > fourth["server_epoch"]
        assert not state_path(record["new_server_key"]).exists()
        assert all(read(path)["owner"]["request_ref"] != fault_ref for path in
                   (state_path(rolled_back["server_key"]).parent / "review-owners").glob("*.json"))

        set_mode(fault, version="0.174.1")
        temp_ref = "01900000-0000-7000-8000-000000000083"
        call(*review_args(temp_ref, "fault"), "--temporary-server", stdin=request, error=review_failure["code"])
        record = read(migration_path)
        assert record["phase"] == "committed" and record["old_server_key"] == rolled_back["server_key"], record
        claim = review_ownership(record["new_server_key"], temp_ref)
        assert claim["phase"] == "retired" and claim["server_epoch"] == record["new_epoch"], claim
        assert not active_path.exists() and not state_path(record["new_server_key"]).exists()
        assert not list(run_root.glob("*"))

        call("profile", "server", "start", "fixture")
        stopped = ready("0.173.3")
        set_mode(fake, version="0.173.3", startup_fail=True)
        set_mode(fault, version="0.174.1", startup_fail=True)
        blocked_ref = "01900000-0000-7000-8000-000000000085"
        call(*review_args(blocked_ref, "fault"), stdin=request, error="TRANSPORT_FAILURE")
        blocked = read(migration_path)
        assert blocked["phase"] == "migration_blocked" and blocked["old_server_key"] == stopped["server_key"], blocked
        assert not active_path.exists() and not state_path(blocked["new_server_key"]).exists()
        call("profile", "server", "start", "fixture", error="PROFILE_SERVER_BUSY")
        assert read(migration_path) == blocked
        call("profile", "state", "reset", "fixture", "--operator-file", str(operator),
             "--confirm-server-key", stopped["server_key"], "--require-server-absence")
        repaired = read(migration_path)
        assert repaired["migration_id"] == blocked["migration_id"], repaired
        assert repaired["phase"] == "rolled_back" and repaired["failure"] == "OPERATOR_STATE_RESET", repaired

        # Crash the isolated CLI during its first probe, after detached process
        # registration but before it has committed review ownership as ready.
        set_mode(fake, version="0.173.3")
        call("profile", "server", "start", "fixture")
        crash_ref = "01900000-0000-7000-8000-000000000084"
        crash_gate = root / "crash-gate"
        set_mode(fault, version="0.174.1", probe_gate=crash_gate)
        with startup_barrier([str(binary), *review_args(crash_ref, "fault"), "--temporary-server"],
                             env, crash_gate, request) as child:
            record = read(migration_path)
            claim = review_ownership(record["new_server_key"], crash_ref)
            assert record["phase"] == "applying" and claim["phase"] == "reserved", (record, claim)
            child.kill()
            child.wait(timeout=10)
        observe_args = ["--workspace", str(workspace), "--request-ref", crash_ref, "--format", "json"]
        observed = call("specialist", "review-inspect", *observe_args)
        assert observed["outcome"] == "unknown", observed
        assert observed["server"]["status"] == "recovery_blocked", observed
        assert observed["server"]["reason"] == "launch_identity_incomplete", observed
        refused = call("specialist", "review-recover", *observe_args, "--action", "cleanup",
                       "--recovery-controller-file", str(owner), error="REVIEW_RECOVERY_BLOCKED")
        assert refused["details"]["reason"] == "cleanup_blocked", refused
        assert read(migration_path) == record
        assert review_ownership(record["new_server_key"], crash_ref) == claim
    print("Automatic rollover passed: CLI and typed Run, MCP setup, CLI review ownership, manifest, membership, downgrade, rollback and retirement")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    validate(parser.parse_args().binary.resolve(strict=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
