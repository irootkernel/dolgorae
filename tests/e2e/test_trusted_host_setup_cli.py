#!/usr/bin/env python3
"""Account-free cold-process coverage of the private, explicit host setup."""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
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
    candidates = sorted((binary.parent / "deps").glob("dolgorae-*"),
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
            "executable": {"path": str(private / "not-launched"), "version": "0.164.0",
                           "sha256": "d8990e3ee7acfbc26f91265b12a1a83f0fee40d0e0f8f1f40a37d4ec21f499cf"},
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

        def setup(operation: str, *, failure: bool = False) -> None:
            with operator.open("rb") as operator_file, controller.open("rb") as controller_file:
                args = [str(binary), "__trusted-specialist-host", operation,
                        "--owner-config", str(owner), "--operator-fd", str(operator_file.fileno()),
                        "--controller-fd", str(controller_file.fileno())]
                if operation == "owner-config-create":
                    args += ["--workspace", str(workspace), "--request", str(request)]
                result = subprocess.run(args, pass_fds=(operator_file.fileno(), controller_file.fileno()),
                                        env=environment, capture_output=True, text=True, timeout=60)
            assert not result.stdout, result.stdout
            if failure:
                assert result.returncode == 4 and re.fullmatch(r"[A-Z_]{1,64}\n", result.stderr), result
                assert str(private) not in result.stderr
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
        assert not (private / "not-launched").exists()
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
        for route in ("codex-0.160.0", "claude-code-2.1.295"):
            if route.startswith("claude"):
                claude = home / "claude-private"
                claude.mkdir(mode=0o700)
                (claude / "native").mkdir(mode=0o700)
                controller = claude / "controller.json"
                controller.write_bytes(original_controller.read_bytes())
                controller.chmod(0o600)
                draft = json.loads(original_request.read_text())
                draft.update(route=route, controller_file=str(controller), native_host_root=str(claude / "native"))
                draft["executable"].update(path=str(claude / "not-launched"), version="2.1.310", sha256="ffb64331bc0ee1b69efb7c2c428bd3ab516bc31b75bf60886b356c97c1c25ffd")
                request, owner = claude / "request.json", claude / "owner.json"
                request.write_text(json.dumps(draft))
                request.chmod(0o600)
                setup("owner-config-create")
                setup("namespace-setup")
                setup("host-state-init")
            public = {"schema": "dolgorae.external-specialist-request/v5", "operation": "open_external_engagement", "workspace_ref": workspace_ref, "idempotency_key": route + "-open", "expected_revision": None}
            event = {"hook_event_name": "PreToolUse", "session_id": "native-thread", "turn_id": "native-turn", "tool_use_id": "native-call", "tool_name": "mcp__dolgorae__specialist", "tool_input": public}
            if route.startswith("claude"):
                event["mcp_server"] = {"name": "dolgorae", "source": "dynamic"}
            def private_call(operation, payload=None, extra=()):
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
            lost_public = {**public, "idempotency_key": route + "-lost-ack"}
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
            recovered = json.loads(private_call("host-lookup", extra=("--session", "native-thread", "--call", "lost-call")))
            assert recovered["lookup"]["state"] == "accepted"
            assert recovered["lookup"]["receipt"]["namespace"]["idempotency_key"] == lost_public["idempotency_key"]
            assert recovered["lookup"]["receipt"]["operation_ref"] != result["receipt"]["operation_ref"]

        owner, request, controller = original_owner, original_request, original_controller


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    arguments = parser.parse_args()
    validate(arguments.binary.resolve(strict=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
