#!/usr/bin/env python3
"""Account-free cold-process coverage of the private, explicit host setup."""
from __future__ import annotations

import argparse
import base64
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
import json
import os
import hashlib
from pathlib import Path
import re
import sqlite3
import subprocess
import tempfile


def validate_member_recovery(binary: Path) -> None:
    """Run the Rust owner/history corpus with the actual cold CLI boundary."""
    selector = "worker::tests::administrative_member_recovery_"
    cargo_target = Path(os.environ.get("CARGO_TARGET_DIR",
                                      str(Path(__file__).resolve().parents[2] / "target")))
    candidates = sorted((cargo_target / "debug/deps").glob("dolgorae-*"),
                        key=lambda path: path.stat().st_mtime, reverse=True)
    harness = None
    for candidate in candidates:
        if not candidate.is_file() or candidate.suffix or not os.access(candidate, os.X_OK):
            continue
        listing = subprocess.run([str(candidate), "--list", selector],
                                 capture_output=True, text=True, timeout=30)
        if listing.returncode == 0 and selector in listing.stdout:
            harness = candidate
            break
    assert harness is not None, "the preceding unit layer must build the member recovery corpus"
    print("Member recovery Rust harness SHA256=" + hashlib.sha256(harness.read_bytes()).hexdigest(), flush=True)
    environment = dict(os.environ, DOLGORAE_MEMBER_RECOVERY_TEST_BINARY=str(binary))
    # Publication fault injection is process-local; the cold corpus exercises
    # ordinary recovery, both owners and unchanged original execution counts.
    for name in ("joins_original_owners_and_history_only", "preserves_read_and_write_results"):
        result = subprocess.run([str(harness), "--exact", selector + name,
                                 "--test-threads=1", "--nocapture"],
                                env=environment, capture_output=True, text=True, timeout=180)
        if result.returncode == 0:
            print(result.stdout, end="", flush=True)
        assert result.returncode == 0 and "1 passed; 0 failed" in result.stdout, result.stdout + result.stderr


def validate(binary: Path) -> None:
    def host_image(path: Path, version: str) -> dict:
        body = f'#!/bin/sh\nprintf attempted > "$0.executed"\n# {version}\nexit 99\n'.encode()
        path.write_bytes(body)
        path.chmod(0o700)
        return {"path": str(path), "version": version, "sha256": hashlib.sha256(body).hexdigest()}

    with tempfile.TemporaryDirectory(prefix="dolgorae-host-setup-") as temporary:
        root = Path(temporary).resolve()
        home = root / "home"
        workspace = home / "workspace"
        private = home / "private"
        native = private / "native"
        for path in (home, workspace, private, native):
            path.mkdir(mode=0o700)
        environment = {key: os.environ[key] for key in ("PATH", "TMPDIR") if key in os.environ}
        environment.update(HOME=str(home), CODEX_HOME=str(native))

        def command(*args: str) -> dict:
            result = subprocess.run([str(binary), *args], env=environment,
                                    capture_output=True, text=True, timeout=60)
            assert result.returncode == 0 and not result.stderr, result
            return json.loads(result.stdout)

        workspace_ref = command("init", str(workspace), "--non-git")["data"]["workspace_id"]
        operator, controller = private / "operator.json", private / "controller.json"
        command("operator", "credential", "initialize", "--output", str(operator))
        command("controller", "credential", "create", "--kind", "automation",
                "--instance-id", "host-setup-fixture", "--output", str(controller))
        request = private / "request.json"
        owner = private / "owner.json"
        version = command("version", "--json")["version"].removeprefix("v")
        request.write_text(json.dumps({
            "route": "codex-0.160.0",
            "executable": host_image(private / "not-launched", "0.172.0"),
            "owner": {"kind": "external"}, "controller_generation": 1,
            "controller_file": str(controller), "native_host_root": str(native),
            "allowed_scopes": [{"kind": "workspace", "reference": workspace_ref},
                               {"kind": "external_engagement", "reference": None}],
            "tools": ["specialist"], "caller_scope": {
                "binding_ref": "fixture-binding", "podway_workspace_ref": "fixture-workspace",
                "podway_session_ref": "fixture-session", "podway_attempt_ref": "fixture-attempt",
                "delegated_placement_ref": "fixture-placement", "max_rounds": 4},
            "adapter_id": "dolgorae.external/1", "adapter_version": version}))
        request.chmod(0o600)

        def setup(operation: str, *, failure: bool | str = False, extra=()) -> None:
            with operator.open("rb") as operator_file, controller.open("rb") as controller_file:
                args = [str(binary), "__trusted-specialist-host", operation,
                        "--owner-config", str(owner), "--operator-fd", str(operator_file.fileno()),
                        "--controller-fd", str(controller_file.fileno())]
                if operation == "owner-config-create":
                    args += ["--workspace", str(workspace), "--request", str(request)]
                args += extra
                result = subprocess.run(args, pass_fds=(operator_file.fileno(), controller_file.fileno()),
                                        env=environment, capture_output=True, text=True, timeout=60)
            assert not result.stdout, result.stdout
            if failure:
                assert result.returncode == 4 and re.fullmatch(r"[A-Z_]{1,64}\n", result.stderr), result
                assert str(private) not in result.stderr
                if isinstance(failure, str):
                    assert result.stderr == failure + "\n", result
            else:
                assert result.returncode == 0 and not result.stderr, result

        database = private / "invocations.sqlite"
        state = home / ".dolgorae" / "workspaces" / workspace_ref
        birth = state / "orchestration" / "namespace-birth.json"
        assert not birth.exists() and not database.exists()
        setup("host-state-init", failure=True)
        assert not owner.exists() and not birth.exists() and not database.exists()
        for operation in ("owner-config-create", "namespace-setup", "host-state-init"):
            if operation == "host-state-init":
                for name in ("invocations.sqlite", "invocations.pending"):
                    for suffix in ("-journal", "-wal", "-shm"):
                        remnant = private / (name + suffix)
                        for payload in (b"", b"interrupted persistence"):
                            remnant.write_bytes(payload)
                            entries = sorted(path.name for path in private.iterdir())
                            setup("host-state-init", failure=True)
                            assert sorted(path.name for path in private.iterdir()) == entries
                            assert remnant.read_bytes() == payload
                            assert not database.exists()
                            assert not (private / "invocations.pending").exists()
                            remnant.unlink()  # Fixture cleanup after preservation assertions.
                # Resume the DB-committed/birth-unpublished namespace cut.
                birth_id = json.loads(birth.read_text())["namespace_birth_id"]
                birth.unlink()
                setup("namespace-setup")
                assert json.loads(birth.read_text())["namespace_birth_id"] == birth_id
            with ThreadPoolExecutor(max_workers=2) as pool:
                list(pool.map(setup, [operation, operation]))
            assert owner.exists()
            if operation == "owner-config-create":
                assert not birth.exists() and not database.exists()
            if operation == "namespace-setup":
                assert birth.exists() and not database.exists()
        before = database.read_bytes()
        with sqlite3.connect(database) as connection:
            assert connection.execute("SELECT COUNT(*) FROM native_calls").fetchone()[0] == 0
            generation = connection.execute("SELECT generation FROM metadata").fetchone()[0]
            assert 1 <= generation <= 9007199254740991
        for suffix in ("-journal", "-wal", "-shm"):
            remnant = private / ("invocations.pending" + suffix)
            remnant.write_bytes(b"interrupted staging")
            setup("host-state-init", failure=True)
            assert database.read_bytes() == before
            assert remnant.read_bytes() == b"interrupted staging"
            remnant.unlink()  # Fixture cleanup; the command must preserve it.
        setup("host-state-init")
        assert database.read_bytes() == before
        assert database.stat().st_mode & 0o777 == 0o600
        # Source preparation uses only explicit private inputs and projected identities.
        export = home / "preparation-output"
        export.mkdir(mode=0o700)
        reservation = json.loads((Path(__file__).resolve().parents[2] / "docs/protocol/examples/workspace-execution-start-reservation.valid.json").read_text())
        binding = reservation["accepted_intent"]["global_profile_binding"]
        binding["launch_snapshot"]["canonical_codex_home"] = str(native)
        binding["launch_snapshot"]["codex_version"] = "0.164.0"
        binding["launch_snapshot"]["executable_identity"]["sha256"] = "d8990e3ee7acfbc26f91265b12a1a83f0fee40d0e0f8f1f40a37d4ec21f499cf"
        canonical = lambda value: json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()
        binding["launch_snapshot_sha256"] = hashlib.sha256(canonical(binding["launch_snapshot"])).hexdigest()
        preparation = private / "preparation.json"
        preparation.write_bytes(canonical({"role_name": "reviewer", "role": {"kind": "inline", "value": "Inspect the complete evidence.\n"}, "dependencies": [], "selected_guidance": [], "profile_binding": binding,
            "admission": {"model": "gpt-5.6", "effort": "high", "purpose_kind": "review", "purpose_label": None, "required_capabilities": [], "execution_lane": "dedicated", "requested_access": "read_only", "required_assurance": "best_effort_personal_alpha", "native_subagent_policy": "disabled"}}))
        preparation.chmod(0o600)
        original_preparation = preparation.read_bytes()

        def prepare(operation, output=None, request_path=preparation, *, failure=False, competing=False):
            with operator.open("rb") as operator_file, controller.open("rb") as controller_file:
                args = [str(binary), "__trusted-specialist-host", operation, "--owner-config", str(owner), "--operator-fd", str(operator_file.fileno()), "--controller-fd", str(controller_file.fileno())]
                if output is not None:
                    args += ["--request", str(request_path), "--output", str(output)]
                result = subprocess.run(args, pass_fds=(operator_file.fileno(), controller_file.fileno()), env=environment, capture_output=True, text=True, timeout=60)
            assert not result.stdout, result
            assert result.returncode in ((0, 4) if competing else ((4,) if failure else (0,))), result
            assert not result.stderr if result.returncode == 0 else re.fullmatch(r"[A-Z_]{1,64}\n", result.stderr), result
            return result

        registry = private / "preparations"
        prepare("provisioning-prepare", export / "missing.json", failure=True)
        assert not registry.exists()
        prepare("preparation-init")
        prepare("preparation-init")
        with ThreadPoolExecutor(max_workers=4) as pool:
            list(pool.map(lambda n: prepare("provisioning-prepare", export / f"identity-{n}.json"), range(4)))
        identities = [(export / f"identity-{n}.json").read_bytes() for n in range(4)]
        assert identities.count(identities[0]) == 4
        identity = json.loads(identities[0])
        assert set(identity) == {"common", "role", "instruction_identity", "configuration"}
        assert str(private).encode() not in identities[0]
        lookup = export / "lookup.json"
        lookup.write_bytes(canonical({"kind": "provisioning", "sources": identity}))
        lookup.chmod(0o600)
        (export / "identity-0.json").unlink()  # Lost projection, original registration remains.
        prepare("preparation-project", export / "recovered.json", lookup)
        assert (export / "recovered.json").read_bytes() == identities[0]
        # Different source provenance for one identity has exactly one winner.
        role = home / ".dolgorae" / "roles" / "source-race"
        role.mkdir(mode=0o700)
        role_file = role / "AGENTS.md"
        role_file.write_text("Source race instructions.\n")
        role_file.chmod(0o600)
        selected = json.loads(preparation.read_text())
        selected["role"] = {"kind": "inline", "value": role_file.read_text()}
        inline_request, file_request = private / "inline.json", private / "file.json"
        inline_request.write_bytes(canonical(selected))
        selected["role"] = {"kind": "file", "value": ["roles", "source-race"]}
        file_request.write_bytes(canonical(selected))
        for path in (inline_request, file_request):
            path.chmod(0o600)
        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(lambda item: prepare("provisioning-prepare", export / f"race-{item[0]}.json", item[1], competing=True), enumerate((inline_request, file_request))))
        assert sorted(result.returncode for result in results) == [0, 4]
        assert [result.stderr for result in results if result.returncode == 4] == ["IDEMPOTENCY_CONFLICT\n"]
        original_records = {p.name: p.read_bytes() for p in registry.iterdir() if p.is_file()}
        preparation.write_bytes(b'{"role_name":"reviewer","role_name":"other"}')
        prepare("provisioning-prepare", export / "invalid.json", failure=True)
        assert {p.name: p.read_bytes() for p in registry.iterdir() if p.is_file()} == original_records
        assert not (export / "invalid.json").exists()
        with sqlite3.connect(state / "orchestration" / "orchestration.sqlite3") as connection:
            assert connection.execute("SELECT COUNT(*) FROM delegation_operations").fetchone()[0] == 0
        assert database.read_bytes() == before
        assert not (private / "not-launched.executed").exists()
        setup("unknown-operation", failure=True)
        # Private paths, FD values and request contents never appear in its output.
        for missing in ([], ["owner-config-create"], ["owner-config-create", "--operator-fd", "-1"]):
            result = subprocess.run([str(binary), "__trusted-specialist-host", *missing],
                                    env=environment, capture_output=True, text=True, timeout=60)
            assert result.returncode == 4 and not result.stdout
            assert re.fullmatch(r"[A-Z_]{1,64}\n", result.stderr)

        # Exercise the actual private CLI/stdio entrypoints after all unchanged
        # preparation-only assertions above. No native executable is launched.
        original_owner, original_request, original_controller = owner, request, controller
        for case, route in (("codex-v2", "codex-0.160.0"), ("claude-v2", "claude-code-2.1.295"),
                            ("codex-v1", "codex-0.160.0")):
            legacy = case.endswith("v1")
            floor, updated = ("2.1.310", "2.1.311") if route.startswith("claude") else ("0.172.0", "0.172.1")
            if case != "codex-v2":
                fixture_owner = home / (case + "-private")
                fixture_owner.mkdir(mode=0o700)
                (fixture_owner / "native").mkdir(mode=0o700)
                controller = fixture_owner / "controller.json"
                controller.write_bytes(original_controller.read_bytes())
                controller.chmod(0o600)
                draft = json.loads(original_request.read_text())
                draft.update(route=route, controller_file=str(controller), native_host_root=str(fixture_owner / "native"))
                draft["executable"] = host_image(fixture_owner / "not-launched", floor)
                request, owner = fixture_owner / "request.json", fixture_owner / "owner.json"
                if legacy:
                    # Retain the exact historical schema and attestation while the
                    # fixture path already holds replacement bytes.
                    draft["executable"].update(version="0.164.0", sha256="d8990e3ee7acfbc26f91265b12a1a83f0fee40d0e0f8f1f40a37d4ec21f499cf")
                    draft["adapter_version"] = "0.1.3"
                    credential = json.loads(controller.read_text())
                    identity = {k: credential[k] for k in ("controller_id", "kind", "instance_id", "subject_id")}
                    identity["generation"] = 1
                    capability = base64.urlsafe_b64decode(credential["capability"] + "=")
                    controller_binding = {"identity": identity, "capability_sha256": hashlib.sha256(
                        b"dolgorae.controller-capability.v1\0" + capability).hexdigest()}
                    owner.write_bytes(canonical({"schema_version": 1, "workspace_ref": workspace_ref,
                                                "canonical_workspace": str(workspace), "controller": controller_binding,
                                                "configuration": draft}))
                    owner.chmod(0o600)
                    (fixture_owner / "host.lock").touch(mode=0o600)
                request.write_text(json.dumps(draft))
                request.chmod(0o600)
                setup("owner-config-create")
                setup("namespace-setup")
                setup("host-state-init")
            if legacy:
                prepare("preparation-init")
                legacy_registry = owner.parent / "preparations" / "metadata.json"
                legacy_partition = legacy_registry.read_bytes()
                legacy_input = owner.parent / "legacy-selection.json"
                legacy_input.write_bytes(original_preparation)
                legacy_input.chmod(0o600)
                legacy_projection = export / "legacy-before.json"
                prepare("provisioning-prepare", legacy_projection, legacy_input)
                legacy_lookup = owner.parent / "legacy-lookup.json"
                legacy_lookup.write_bytes(canonical({"kind": "provisioning", "sources": json.loads(legacy_projection.read_bytes())}))
                legacy_lookup.chmod(0o600)
            public = {"schema": "dolgorae.external-specialist-request/v5", "operation": "open_external_engagement", "workspace_ref": workspace_ref, "idempotency_key": case + "-open", "expected_revision": None}
            event = {"hook_event_name": "PreToolUse", "session_id": "native-thread", "turn_id": "native-turn", "tool_use_id": "native-call", "tool_name": "mcp__dolgorae__specialist", "tool_input": public}
            if route.startswith("claude"):
                event["mcp_server"] = {"name": "dolgorae", "source": "dynamic"}
            def private_call(operation, payload=None, extra=(), *, error=None):
                process = subprocess.Popen([str(binary), "__trusted-specialist-host", operation, "--owner-config", str(owner), *extra], env=environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                pool = ThreadPoolExecutor(max_workers=3)
                def send():
                    with process.stdin:
                        process.stdin.write(payload or "")
                try:
                    sender = pool.submit(send)
                    output = pool.submit(process.stdout.read)
                    errors = pool.submit(process.stderr.read)
                    rc = process.wait(timeout=30)
                    sender.result(timeout=5)
                    stdout, stderr = output.result(timeout=5), errors.result(timeout=5)
                    if error is not None:
                        assert (rc, stdout, stderr) == (4, "", error + "\n"), (rc, stderr)
                        return stdout
                    assert rc == 0 and not stderr, (rc, stderr)
                    return stdout
                finally:
                    if process.poll() is None:
                        process.kill()
                    process.wait(timeout=10)
                    pool.shutdown(wait=True)
                    for stream in (process.stdin, process.stdout, process.stderr):
                        if stream is not None and not stream.closed:
                            stream.close()
            allowed = json.loads(private_call("host-hook", json.dumps(event)))
            assert allowed["hookSpecificOutput"]["permissionDecision"] == "allow"
            arguments = allowed["hookSpecificOutput"].get("updatedInput", public)
            private_cap = arguments.get("_dolgorae_host_binding")
            if private_cap:
                arguments = {**public, "_dolgorae_host_binding": private_cap}
            def frames(rpc_id, value=arguments, meta=None):
                params = {"name": "specialist", "arguments": value}
                params["_meta"] = {"progressToken": 0, **(meta or {})}
                records = [
                    {"jsonrpc": "2.0", "id": -9007199254740992, "method": "initialize", "params": {
                        "protocolVersion": "2025-03-26", "capabilities": {},
                        "clientInfo": {"name": "fixture", "version": "1"}, "_meta": {"progressToken": "setup"}}},
                    {"jsonrpc": "2.0", "method": "notifications/initialized", "params": {"_meta": {"progressToken": None}}},
                    {"jsonrpc": "2.0", "id": 1e30, "method": "ping", "params": {"_meta": {"progressToken": 1e30}}},
                    {"jsonrpc": "2.0", "id": 9007199254740994, "method": "tools/list", "params": {"_meta": {"trace": "ignored"}}},
                    {"jsonrpc": "2.0", "id": rpc_id, "method": "tools/call", "params": params},
                ]
                return "".join(json.dumps(record) + "\n" for record in records)
            meta = {"threadId": "native-thread", "callId": "native-call"} if route.startswith("codex") else None
            # Missing metadata/carrier is an effect-free refusal before admission.
            invalid = [json.loads(line) for line in private_call("host-mcp", frames("missing", public)).splitlines()]
            assert invalid[-1].get("error", {}).get("code") == -32602 or invalid[-1]["result"]["structuredContent"]["error"]["effect"] == "none"
            replies = [json.loads(line) for line in private_call("host-mcp", frames(9007199254740992, meta=meta)).splitlines()]
            assert len(replies) == 4 and [reply["id"] for reply in replies] == [-9007199254740992, 1e30, 9007199254740994, 9007199254740992]
            assert replies[0]["result"]["protocolVersion"] == "2025-06-18"
            result = replies[-1]["result"]["structuredContent"]
            assert result["schema"] == "dolgorae.external-specialist-response/v5", result
            assert set(result) == {"schema", "operation", "engagement", "receipt"}
            serialized = json.dumps(replies)
            if private_cap:
                assert private_cap not in serialized
            for secret in (str(owner), str(controller), "_dolgorae_host_binding", "group_specialists", "merge_results"):
                assert secret not in serialized
            replayed = [json.loads(line) for line in private_call("host-mcp", frames(991, meta=meta)).splitlines()]
            assert replayed[-1]["result"]["structuredContent"]["receipt"] == result["receipt"]
            padding = 16 * 1024 * 1024 - len(json.dumps(public).encode())
            padded = frames("raw-limit", meta=meta).replace('"arguments": {', '"arguments": {' + " " * padding, 1)
            bounded = [json.loads(line) for line in private_call("host-mcp", padded).splitlines()]
            assert bounded[-1]["result"]["structuredContent"]["receipt"] == result["receipt"]
            too_large = frames("raw-over", meta=meta).replace('"arguments": {', '"arguments": {' + " " * (padding + 1), 1)
            refused = [json.loads(line) for line in private_call("host-mcp", too_large).splitlines()]
            assert refused[-1]["error"]["code"] == -32602

            lookup = json.loads(private_call("host-lookup", extra=("--session", "native-thread", "--call", "native-call")))
            assert lookup["lookup"]["state"] == "accepted" and lookup["lookup"]["receipt"] == result["receipt"]
            # Closing stdin preserves the accepted engagement and its receipt.
            with sqlite3.connect(state / "orchestration" / "orchestration.sqlite3") as connection:
                retained = json.loads(connection.execute("SELECT receipt_json FROM delegation_operations WHERE operation_ref=?", (result["receipt"]["operation_ref"],)).fetchone()[0])
            assert retained == result["receipt"]
            # Close the response reader after initialize and before the only
            # mutating call. Its failed write occurs after real acceptance.
            lost_public = {**public, "idempotency_key": case + "-lost-ack"}
            lost_event = {**event, "tool_use_id": "lost-call", "tool_input": lost_public}
            lost_hook = json.loads(private_call("host-hook", json.dumps(lost_event)))
            lost_arguments = lost_hook["hookSpecificOutput"].get("updatedInput", lost_public)
            process = subprocess.Popen([str(binary), "__trusted-specialist-host", "host-mcp", "--owner-config", str(owner)], env=environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                initialize = {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "loss", "version": "1"}}}
                process.stdin.write(json.dumps(initialize) + "\n")
                process.stdin.flush()
                import select
                assert select.select([process.stdout], [], [], 10)[0], "initialize response timed out"
                assert json.loads(process.stdout.readline())["result"]["protocolVersion"] == "2025-06-18"
                process.stdout.close()
                params = {"name": "specialist", "arguments": lost_arguments}
                if route.startswith("codex"):
                    params["_meta"] = {"threadId": "native-thread", "callId": "lost-call"}
                process.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n" + json.dumps({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": params}) + "\n")
                process.stdin.close()
                assert process.wait(timeout=10) == 4
                failure = process.stderr.read()
                process.stderr.close()
                assert re.fullmatch(r"[A-Z_]{1,64}\n", failure) and str(owner) not in failure
            finally:
                if process.poll() is None:
                    process.kill()
                process.wait(timeout=10)
                for stream in (process.stdin, process.stdout, process.stderr):
                    if stream is not None and not stream.closed:
                        stream.close()

            # Refresh with both open engagements and a committed response lost on stdio.
            # An additional fixture row models loss before the accepted receipt exists.
            blocked_public = {**public, "idempotency_key": case + "-unsettled"}
            blocked_event = {**event, "tool_use_id": "unsettled-call", "tool_input": blocked_public}
            assert json.loads(private_call("host-hook", json.dumps(blocked_event)))["hookSpecificOutput"]["permissionDecision"] == "allow"
            database = owner.parent / "invocations.sqlite"
            with sqlite3.connect(database) as connection:
                assert connection.execute("UPDATE native_calls SET attempted=1 WHERE tool_use_ref='unsettled-call'").rowcount == 1
            old_bytes = owner.read_bytes()
            old = json.loads(old_bytes)
            old_digest = "sha256:" + hashlib.sha256(canonical(old)).hexdigest()
            image = owner.parent / "not-launched"
            refresh_request = owner.parent / "refresh.json"
            refresh_request.write_bytes(canonical(host_image(image, updated)))
            refresh_request.chmod(0o600)
            options = ("--request", str(refresh_request))
            blocked_image = database.read_bytes()
            if not legacy:
                private_call("host-lookup", extra=("--session", "native-thread", "--call", "native-call"),
                             error="RECOVERY_REQUIRED")
                assert owner.read_bytes() == old_bytes and database.read_bytes() == blocked_image
            setup("owner-executable-refresh", failure=True, extra=options)
            assert owner.read_bytes() == old_bytes and database.read_bytes() == blocked_image
            with sqlite3.connect(database) as connection:
                assert connection.execute("UPDATE native_calls SET attempted=0 WHERE tool_use_ref='unsettled-call'").rowcount == 1
            before_refresh = database.read_bytes()
            for name in ("owner.json.pending", "invocations.pending", "invocations.sqlite-journal", "consumer.sqlite-shm"):
                pending = owner.parent / name
                pending.write_bytes(b"retained crash fixture")
                setup("owner-executable-refresh", failure=True, extra=options)
                assert pending.read_bytes() == b"retained crash fixture" and owner.read_bytes() == old_bytes
                assert database.read_bytes() == before_refresh
                pending.unlink()
            setup("owner-executable-refresh", extra=options)
            refreshed = json.loads(owner.read_bytes())
            assert refreshed["schema_version"] == 2
            assert refreshed["migrated_from_v1_digest"] == (old_digest if legacy else None)
            assert refreshed["executable_binding"]["executable_epoch"] == (1 if legacy else 2)
            if not legacy:
                assert refreshed["owner_identity_digest"] == old["owner_identity_digest"]
            assert database.read_bytes() == before_refresh
            if legacy:
                prepare("preparation-init")
                prepare("preparation-project", export / "legacy-after.json", legacy_lookup)
                assert (export / "legacy-after.json").read_bytes() == legacy_projection.read_bytes()
                prepare("provisioning-prepare", export / "legacy-repeat.json", legacy_input)
                assert (export / "legacy-repeat.json").read_bytes() == legacy_projection.read_bytes()
                assert legacy_registry.read_bytes() == legacy_partition

            recovered = json.loads(private_call("host-lookup", extra=("--session", "native-thread", "--call", "lost-call")))
            assert recovered["lookup"]["state"] == "accepted"
            assert recovered["lookup"]["receipt"]["namespace"]["idempotency_key"] == lost_public["idempotency_key"]
            assert recovered["lookup"]["receipt"]["operation_ref"] != result["receipt"]["operation_ref"]
            preserved = [json.loads(line) for line in private_call("host-mcp", frames(992, meta=meta)).splitlines()]
            assert preserved[-1]["result"]["structuredContent"]["receipt"] == result["receipt"]
            if legacy:
                close = {"schema": public["schema"], "operation": "close_external_engagement",
                         "engagement_ref": result["engagement"]["engagement_ref"],
                         "expected_revision": result["engagement"]["revision"], "allow_interrupt": False}
                close_event = {**event, "tool_use_id": "close-call", "tool_input": close}
                allowed = json.loads(private_call("host-hook", json.dumps(close_event)))
                assert allowed["hookSpecificOutput"]["permissionDecision"] == "allow"
                closed = [json.loads(line) for line in private_call("host-mcp", frames(993, close, {"threadId": "native-thread", "callId": "close-call"})).splitlines()]
                close_result = closed[-1]["result"]["structuredContent"]
                assert close_result["engagement"]["state"] == "closed", closed[-1]
                with sqlite3.connect(database) as connection:
                    assert connection.execute("SELECT attempted FROM native_calls WHERE tool_use_ref='close-call'").fetchone() == (1,)
            before_downgrade = owner.read_bytes()
            before_db = database.read_bytes()
            downgrade_image = owner.parent / "downgrade-host"
            refresh_request.write_bytes(canonical(host_image(downgrade_image, floor)))
            setup("owner-executable-refresh", failure=True, extra=options)
            assert owner.read_bytes() == before_downgrade and database.read_bytes() == before_db
            downgrade = (*options, "--allow-downgrade")
            if legacy:
                # Retain a real close owner, then model cuts around its two
                # product commits while holding the original native attempt.
                product_db = state / "orchestration" / "orchestration.sqlite3"
                engagement = close["engagement_ref"]
                key = f"successor-state:{close['operation']}:{engagement}:{engagement}"
                with closing(sqlite3.connect(product_db)) as connection:
                    terminal, = connection.execute("SELECT value FROM metadata WHERE key=?", (key,)).fetchone()
                    terminal_engagement = connection.execute(
                        "SELECT state, revision FROM engagements WHERE engagement_id=?", (engagement,)
                    ).fetchone()
                record = json.loads(terminal)
                assert record["terminal"] and record["operation_ref"] == close_result["operation_ref"]

                def digest(value):
                    return "sha256:" + hashlib.sha256(canonical(value)).hexdigest()

                def state_record(**changes):
                    adjusted = {**record, **changes}
                    adjusted.pop("record_digest")
                    adjusted["record_digest"] = digest(adjusted)
                    return canonical(adjusted).decode()

                assert state_record() == terminal
                assert record["input_digest"] == digest({"allow_interrupt": False})

                def select_state(value, state_and_revision):
                    with closing(sqlite3.connect(product_db)) as connection, connection:
                        if value is None:
                            assert connection.execute("DELETE FROM metadata WHERE key=?", (key,)).rowcount == 1
                        else:
                            connection.execute("INSERT OR REPLACE INTO metadata(key, value) VALUES (?, ?)", (key, value))
                        assert connection.execute(
                            "UPDATE engagements SET state=?, revision=? WHERE engagement_id=?",
                            (*state_and_revision, engagement),
                        ).rowcount == 1

                def product_view():
                    with closing(sqlite3.connect(product_db.as_uri() + "?mode=ro", uri=True)) as connection:
                        return "\n".join(sorted(connection.iterdump())).encode()

                terminal_view = product_view()
                initial_revision = close["expected_revision"]
                for label, value, view in (
                    ("absent", None, ("active", initial_revision)),
                    ("nonterminal", state_record(terminal=False), ("closing", initial_revision + 1)),
                    ("mismatched request", state_record(input_digest=digest({"allow_interrupt": True})), terminal_engagement),
                ):
                    select_state(value, view)
                    if label == "nonterminal":
                        observed = json.loads(private_call("host-lookup", extra=("--session", "native-thread", "--call", "close-call")))
                        assert observed["engagement"]["state"] == "closing"
                        assert observed["operation_ref"] == record["operation_ref"]
                    before_product = product_view()
                    setup("owner-executable-refresh", failure="RECOVERY_REQUIRED", extra=downgrade)
                    assert (owner.read_bytes(), database.read_bytes(), product_view()) == (before_downgrade, before_db, before_product), label
                select_state(terminal, terminal_engagement)
                assert product_view() == terminal_view
                setup("owner-executable-refresh", failure=True, extra=(*downgrade, "--allow-downgrade"))
                assert (owner.read_bytes(), database.read_bytes(), product_view()) == (before_downgrade, before_db, terminal_view)
            setup("owner-executable-refresh", extra=downgrade)
            if legacy:
                assert product_view() == terminal_view
                print("codex-v1 refresh refused absent, nonterminal and mismatched State authority and accepted the exact terminal owner")
            downgraded = json.loads(owner.read_bytes())
            assert downgraded["owner_identity_digest"] == refreshed["owner_identity_digest"]
            assert downgraded["migrated_from_v1_digest"] == refreshed["migrated_from_v1_digest"]
            assert downgraded["executable_binding"]["executable_epoch"] == refreshed["executable_binding"]["executable_epoch"] + 1
            assert database.read_bytes() == before_db
            assert not image.with_suffix(".executed").exists()
            assert not downgrade_image.with_suffix(".executed").exists()
            print(f"{case} setup and refresh preserved the exact native SQLite bytes and lost-ack receipt")

        owner, request, controller = original_owner, original_request, original_controller


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    arguments = parser.parse_args()
    validate(arguments.binary.resolve(strict=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
