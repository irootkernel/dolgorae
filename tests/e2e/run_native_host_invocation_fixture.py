#!/usr/bin/env python3
"""Opt-in, account-free native-call and backend-fault probe of two MCP hosts.

Scripted numeric-loopback APIs drive the real hosts. The fault proxy exercises
the actual stdio backend, then resumes the exact native session in a fresh host
process. Pending-call replay after native host resume remains untested. This
does not advertise a production facade.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shlex
import signal
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


PRIVATE = "_dolgorae_host_binding"
TOOL = "dolgorae_identity_probe"
PINS = {
    "codex": ("codex-cli 0.160.0", "112fae7a5a1223e673c8a1791d32338f37df8b527ff1159bb8adac6c4dbf1b4b"),
    "claude": ("2.1.295 (Claude Code)", "0116ee2e0a513900b633d9951367f18747686478e2b462805b8c31609f047f70"),
}
POST_LIMIT = 8
BODY_LIMIT = 4 * 1024 * 1024


class ProbeFailure(Exception):
    pass


def private_file(path: Path, content: str) -> None:
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w") as stream:
        stream.write(content)


def host_environment(root: Path) -> dict[str, str]:
    # Deliberately do not inherit credential, provider, proxy, or account state.
    return {
        "PATH": "/usr/bin:/bin:/usr/sbin:/sbin:" + str(Path(sys.executable).parent),
        "HOME": str(root / "home"), "CODEX_HOME": str(root / "codex"),
        "CLAUDE_CONFIG_DIR": str(root / "claude"), "TMPDIR": str(root / "tmp"),
        "XDG_CONFIG_HOME": str(root / "config"), "XDG_CACHE_HOME": str(root / "cache"),
        "LANG": "en_US.UTF-8", "LC_ALL": "en_US.UTF-8",
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
        "DISABLE_TELEMETRY": "1", "DISABLE_ERROR_REPORTING": "1",
        "DISABLE_AUTOUPDATER": "1", "NO_PROXY": "127.0.0.1,localhost",
        "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null",
    }


def receipts_from(value: object) -> list[dict]:
    found = []
    if isinstance(value, dict):
        if value.get("schema") == "dolgorae.host-probe-receipt/v1":
            return [value]
        for child in value.values():
            found.extend(receipts_from(child))
    elif isinstance(value, list):
        for child in value:
            found.extend(receipts_from(child))
    elif isinstance(value, str):
        try:
            found.extend(receipts_from(json.loads(value)))
        except (ValueError, RecursionError):
            pass
    unique = {}
    for item in found:
        unique[item["external_request_ref"]] = item
    return list(unique.values())


def protected_native_history(root: Path, host: str, bindings: list[tuple[str]]) -> dict:
    journal_count = 0
    scanned = 0
    root_info = root.stat()
    if root_info.st_uid != os.getuid() or root_info.st_mode & 0o777 != 0o700:
        raise ProbeFailure("native_history_root_not_private")
    for category in ("home", "codex", "claude", "workspace", "config", "cache", "tmp"):
        base = root / category
        for path in [base, *base.rglob("*")]:
            if path.is_symlink():
                # Never follow native-created links to inspect another home.
                continue
            info = path.stat()
            if info.st_uid != os.getuid():
                raise ProbeFailure("native_history_permissions_not_private")
            if not path.is_file():
                continue
            if info.st_size > 32 * 1024 * 1024:
                raise ProbeFailure("native_history_scan_bound_exceeded")
            scanned += 1
            content = path.read_bytes()
            if not any(binding.encode() in content for (binding,) in bindings):
                continue
            allowed = host == "claude" and path.is_relative_to(root / "claude" / "projects") and path.suffix == ".jsonl"
            if not allowed or info.st_nlink != 1 or info.st_mode & 0o777 != 0o600:
                raise ProbeFailure("binding_outside_protected_native_hook_journal")
            for parent in path.parents:
                if parent == root:
                    break
                parent_info = parent.stat()
                if parent_info.st_uid != os.getuid() or parent_info.st_mode & 0o777 != 0o700:
                    raise ProbeFailure("binding_journal_parent_not_private")
            journal_count += 1
    return {"native_root_permissions_private": True, "native_files_scanned": scanned,
            "native_hook_journal_contains_binding": journal_count > 0,
            "protected_native_hook_journal_file_count": journal_count,
            "other_native_binding_locations": 0}


def native_tool(tools: object, namespace: str | None = None) -> tuple[str, str | None] | None:
    if isinstance(tools, list):
        for child in tools:
            found = native_tool(child, namespace)
            if found:
                return found
    elif isinstance(tools, dict):
        name = tools.get("name", "")
        if isinstance(name, str) and TOOL in name:
            return name, namespace
        next_namespace = name if tools.get("type") == "namespace" else namespace
        for key in ("tools", "functions", "function"):
            if key in tools:
                found = native_tool(tools[key], next_namespace)
                if found:
                    return found
    return None


class Fixture:
    def __init__(self, host: str, state: Path):
        self.host, self.state = host, state
        self.posts = 0
        self.message_posts = 0
        self.calls_sent = 0
        self.call_target = 2
        self.withheld_receipts = 0
        self.receipt: dict | None = None
        self.receipts: list[dict] = []
        self.failure: str | None = None
        self.lock = threading.Lock()
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                try:
                    length = int(self.headers.get("Content-Length", "0"))
                    if not 0 < length <= BODY_LIMIT:
                        raise ProbeFailure("request_body_outside_bound")
                    body = self.rfile.read(length)
                    with fixture.lock:
                        fixture.posts += 1
                        if fixture.posts > POST_LIMIT:
                            raise ProbeFailure("request_budget_exceeded")
                        data = json.loads(body)
                        fixture.check_private_absent(body)
                        if self.path.split("?")[0] == "/v1/messages/count_tokens":
                            encoded = b'{"input_tokens":1}'
                            self.send_response(200)
                            self.send_header("Content-Type", "application/json")
                            self.send_header("Content-Length", str(len(encoded)))
                            self.end_headers()
                            self.wfile.write(encoded)
                            return
                        expected = "/v1/responses" if host == "codex" else "/v1/messages"
                        if self.path.split("?")[0] != expected:
                            raise ProbeFailure("unexpected_fixture_endpoint")
                        events = fixture.respond(data)
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Connection", "close")
                    self.end_headers()
                    for event in events:
                        self.wfile.write(("event: " + event["type"] + "\ndata: " +
                                          json.dumps(event) + "\n\n").encode())
                    self.wfile.flush()
                except (ProbeFailure, ValueError, sqlite3.Error) as error:
                    fixture.failure = str(error) if isinstance(error, ProbeFailure) else "fixture_request_rejected"
                    self.send_error(400, "bounded fixture refused request")
                except (BrokenPipeError, ConnectionResetError):
                    fixture.failure = "fixture_connection_lost"

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def base_url(self) -> str:
        return f"http://127.0.0.1:{self.server.server_port}"

    def check_private_absent(self, body: bytes) -> None:
        if PRIVATE.encode() in body:
            raise ProbeFailure("private_binding_field_in_model_traffic")
        with sqlite3.connect(self.state / "invocations.sqlite") as db:
            bindings = db.execute("SELECT private_binding FROM calls").fetchall()
        if any(binding.encode() in body for (binding,) in bindings):
            raise ProbeFailure("private_binding_value_in_model_traffic")

    def respond(self, data: dict) -> list[dict]:
        self.message_posts += 1
        if self.calls_sent:
            self.receipts = receipts_from(data)
            if len(self.receipts) != self.calls_sent - self.withheld_receipts:
                raise ProbeFailure("native_receipt_not_returned")
            self.receipt = self.receipts[-1] if self.receipts else None
        tool = native_tool(data.get("tools", []))
        send_call = self.calls_sent < self.call_target
        if send_call and tool is None:
            raise ProbeFailure("native_mcp_tool_not_advertised")
        if send_call:
            self.calls_sent += 1
        if self.host == "codex":
            if send_call:
                item = {"type": "function_call", "id": f"fc_fixture_{self.calls_sent}", "call_id": f"call_fixture_{self.calls_sent}",
                        "name": tool[0], "arguments": json.dumps({"payload": "account-free-initial"})}
                if tool[1]:
                    item["namespace"] = tool[1]
            else:
                item = {"type": "message", "role": "assistant", "id": "msg_fixture",
                        "content": [{"type": "output_text", "text": "Fixture complete."}]}
            return [{"type": "response.created", "response": {"id": f"resp_fixture_{self.message_posts}"}},
                    {"type": "response.output_item.done", "item": item},
                    {"type": "response.completed", "response": {"id": f"resp_fixture_{self.message_posts}", "usage": {
                        "input_tokens": 0, "input_tokens_details": None, "output_tokens": 0,
                        "output_tokens_details": None, "total_tokens": 0}}}]
        block = ({"type": "tool_use", "id": f"toolu_fixture_{self.calls_sent}", "name": tool[0], "input": {}}
                 if send_call else {"type": "text", "text": ""})
        delta = ({"type": "input_json_delta", "partial_json": json.dumps({"payload": "account-free-initial"})}
                 if send_call else {"type": "text_delta", "text": "Fixture complete."})
        return [{"type": "message_start", "message": {"id": f"msg_fixture_{self.message_posts}", "type": "message",
                    "role": "assistant", "model": data.get("model", "claude-sonnet-4-6"),
                    "content": [], "stop_reason": None, "stop_sequence": None,
                    "usage": {"input_tokens": 1, "output_tokens": 0}}},
                {"type": "content_block_start", "index": 0, "content_block": block},
                {"type": "content_block_delta", "index": 0, "delta": delta},
                {"type": "content_block_stop", "index": 0},
                {"type": "message_delta", "delta": {"stop_reason": "tool_use" if send_call else "end_turn",
                    "stop_sequence": None}, "usage": {"output_tokens": 1}},
                {"type": "message_stop"}]

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=3)


HOOK_WRAPPER = '''import json,os,subprocess,sys
data=sys.stdin.buffer.read(1048577)
if len(data)>1048576: sys.exit(2)
event=json.loads(data)
source=event.get("mcp_server",{}).get("source")
summary={"native_event_fields_present":all(isinstance(event.get(k),str) and bool(event[k]) for k in ("session_id","tool_use_id","tool_name")),"mcp_server_provenance_present":isinstance(event.get("mcp_server"),dict),"mcp_server_source":source if isinstance(source,str) and len(source)<80 else None,"mcp_server_name":event.get("mcp_server",{}).get("name") if event.get("mcp_server",{}).get("name")=="dolgorae" else None}
fd=os.open(sys.argv[4],os.O_WRONLY|os.O_CREAT|os.O_TRUNC,0o600)
with os.fdopen(fd,"w") as output: json.dump(summary,output)
fd=os.open(sys.argv[5],os.O_WRONLY|os.O_CREAT|os.O_TRUNC,0o600)
with os.fdopen(fd,"w") as output: json.dump({"native_session":event.get("session_id"),"native_call":event.get("tool_use_id")},output)
result=subprocess.run([sys.argv[1],"host-hook",sys.argv[2],sys.argv[3]],input=data,capture_output=True,timeout=8)
sys.stdout.buffer.write(result.stdout)
sys.exit(result.returncode)
'''


def run_host(argv: list[str], root: Path, env: dict[str, str], phase: str, deadline: float,
             crash_marker: Path | None = None) -> Path:
    stdout_path, stderr_path = root / (phase + "-stdout.local"), root / (phase + "-stderr.local")
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise ProbeFailure("native_host_deadline_exceeded")
    with stdout_path.open("wb") as out, stderr_path.open("wb") as err:
        os.chmod(stdout_path, 0o600)
        os.chmod(stderr_path, 0o600)
        process = subprocess.Popen(argv, cwd=root / "workspace", env=env,
                                   stdin=subprocess.DEVNULL, stdout=out, stderr=err,
                                   start_new_session=True, umask=0o077)
        try:
            if crash_marker is not None:
                while not crash_marker.exists() and process.poll() is None and time.monotonic() < deadline:
                    time.sleep(0.025)
                if not crash_marker.exists():
                    raise ProbeFailure("native_crash_point_not_reached")
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=5)
                code = 0
            else:
                code = process.wait(timeout=remaining)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)
            raise ProbeFailure("native_host_deadline_exceeded") from None
        finally:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=5)
    if code:
        raise ProbeFailure("native_" + phase + "_process_failed")
    return stdout_path


def native_session(path: Path, host: str) -> str:
    identities = set()
    for line in path.read_text().splitlines():
        try:
            event = json.loads(line)
        except ValueError:
            continue
        value = (event.get("thread_id") if host == "codex" and event.get("type") == "thread.started"
                 else event.get("session_id") if host == "claude" and event.get("type") == "system"
                 and event.get("subtype") == "init" else None)
        if value:
            try:
                uuid.UUID(value)
            except (ValueError, AttributeError):
                raise ProbeFailure("native_session_identifier_invalid") from None
            identities.add(value)
    if len(identities) != 1:
        raise ProbeFailure("native_session_identifier_missing_or_ambiguous")
    return identities.pop()


def verify_lookup(prototype: Path, root: Path, route: str, expected: dict[str, dict], env: dict[str, str]) -> None:
    for reference, receipt in expected.items():
        result = subprocess.run([str(prototype), "lookup", str(root / "state"), route, reference],
                                env=env, capture_output=True, text=True, timeout=8)
        if result.returncode or json.loads(result.stdout) != receipt:
            raise ProbeFailure("receipt_lookup_changed_across_native_restart")


def qualify(host: str, executable: Path, prototype: Path, scratch: Path, crash_case: bool = False) -> dict:
    prefix = f"dolgorae-host-{host}-" + ("crash-" if crash_case else "")
    root = Path(tempfile.mkdtemp(prefix=prefix, dir=scratch)).resolve()
    for name in ("home", "codex", "claude", "tmp", "config", "cache", "workspace", "state"):
        (root / name).mkdir(mode=0o700)
    env = host_environment(root)
    expected_version, expected_sha = PINS[host]
    with executable.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    if digest != expected_sha:
        raise ProbeFailure("native_executable_checksum_mismatch")
    actual = subprocess.run([str(executable), "--version"], capture_output=True, text=True,
                            env=env, timeout=15, check=True).stdout.strip()
    if actual != expected_version:
        raise ProbeFailure("native_executable_version_mismatch")
    route = "codex-0.160.0" if host == "codex" else "claude-code-2.1.295"
    initialized = subprocess.run([str(prototype), "init", str(root / "state"), route],
                                 env=env, capture_output=True, timeout=10)
    if initialized.returncode:
        raise ProbeFailure("prototype_initialization_failed")
    wrapper = root / "hook.py"
    private_file(wrapper, HOOK_WRAPPER)
    summary = root / "hook-summary.json"
    native_key = root / "native-key.local"
    command = shlex.join([sys.executable, str(wrapper), str(prototype), str(root / "state"), route,
                         str(summary), str(native_key)])
    hooks = {"hooks": {"PreToolUse": [{"matcher": "^mcp__dolgorae__.*$", "hooks": [
        {"type": "command", "command": command, "timeout": 10}]}]}}
    private_file(root / "settings.json", json.dumps(hooks))
    proxy = Path(__file__).with_name("host_invocation_fault_proxy.py").resolve(strict=True)
    proxy_summary = root / "fault-summary.json"
    server_arguments = [str(proxy), str(prototype), str(root / "state"), route, str(proxy_summary)]
    if crash_case:
        server_arguments.append("crash-before-ack")
    fixture = Fixture(host, root / "state")
    if crash_case:
        fixture.call_target = 1
        fixture.withheld_receipts = 1
    try:
        if host == "codex":
            private_file(root / "codex" / "hooks.json", json.dumps(hooks))
            config = ('model = "gpt-5.4"\nmodel_provider = "bounded_fixture"\n'
                      'cli_auth_credentials_store = "file"\n[features]\nhooks = true\n'
                      '[model_providers.bounded_fixture]\nname = "Bounded local fixture"\n'
                      f'base_url = "{fixture.base_url}/v1"\nwire_api = "responses"\n'
                      'requires_openai_auth = false\nsupports_websockets = false\n'
                      'request_max_retries = 0\nstream_max_retries = 0\n'
                      '[mcp_servers.dolgorae]\n' + f'command = {json.dumps(sys.executable)}\n'
                      + 'args = ' + json.dumps(server_arguments) + '\n'
                      + f'[mcp_servers.dolgorae.tools.{TOOL}]\napproval_mode = "approve"\n')
            private_file(root / "codex" / "config.toml", config)
            argv = [str(executable), "--no-daemon", "--dangerously-bypass-hook-trust", "-a", "never", "exec",
                    "--skip-git-repo-check", "--sandbox", "read-only", "--json",
                    "--cd", str(root / "workspace"), "Execute the requested identity fixture tool calls."]
        else:
            env.update({"ANTHROPIC_BASE_URL": fixture.base_url,
                        "ANTHROPIC_API_KEY": "sk-ant-fixture-not-a-credential"})
            mcp = {"mcpServers": {"dolgorae": {"type": "stdio", "command": sys.executable, "args": server_arguments}}}
            private_file(root / "mcp.json", json.dumps(mcp))
            argv = [str(executable), "--restricted", "--setting-sources", "", "--settings", str(root / "settings.json"),
                    "--strict-mcp-config", "--mcp-config", str(root / "mcp.json"), "--tools", "",
                    "--permission-mode", "dontAsk", "--permission-prompts", "none", "--model", "claude-sonnet-4-6",
                    "-p", "--output-format", "stream-json", "--verbose", "Execute the identity fixture tool once."]
        # Both actual native executions share one bounded campaign deadline.
        deadline = time.monotonic() + 90
        initial_output = run_host(argv, root, env, "initial", deadline,
                                  root / "crash-ready.local" if crash_case else None)
        if fixture.failure or (fixture.receipt is None and not crash_case):
            raise ProbeFailure(fixture.failure or "native_initial_call_not_qualified")
        session = native_session(initial_output, host)
        if crash_case:
            trusted_key = json.loads(native_key.read_text())
            if trusted_key.get("native_session") != session:
                raise ProbeFailure("crash_native_hook_session_mismatch")
            lookup_argv = [str(prototype), "host-lookup", str(root / "state"), route,
                           trusted_key["native_session"], trusted_key["native_call"]]
            recovered = subprocess.run(lookup_argv, env=env, capture_output=True, text=True, timeout=8)
            if recovered.returncode:
                raise ProbeFailure("trusted_native_key_lookup_failed_after_crash")
            original = json.loads(recovered.stdout)
            if original.get("schema") != "dolgorae.host-probe-receipt/v1" or original.get("acceptances") != 1:
                raise ProbeFailure("trusted_native_key_lookup_receipt_invalid")
            original_receipts = {original["external_request_ref"]: original}
        else:
            original_receipts = {value["external_request_ref"]: value for value in fixture.receipts}
        if len(original_receipts) != (1 if crash_case else 2):
            raise ProbeFailure("initial_native_receipts_incomplete")
        verify_lookup(prototype, root, route, original_receipts, env)
        with fixture.lock:
            fixture.call_target = 2 if crash_case else 3
        if host == "codex":
            resume_argv = [str(executable), "--no-daemon", "--dangerously-bypass-hook-trust", "-a", "never",
                           "exec", "resume", "--skip-git-repo-check", "--json", session,
                           "Execute one additional identity fixture call with the requested payload."]
        else:
            resume_argv = argv[:-1] + ["--resume", session,
                                      "Execute one additional identity fixture call with the requested payload."]
        resumed_output = run_host(resume_argv, root, env, "resumed", deadline)
        if native_session(resumed_output, host) != session:
            raise ProbeFailure("native_resumed_session_identity_changed")
        expected_calls = 2 if crash_case else 3
        if fixture.failure or fixture.calls_sent != expected_calls:
            raise ProbeFailure(fixture.failure or "resumed_native_call_missing")
        verify_lookup(prototype, root, route, original_receipts, env)
        if crash_case:
            recovered_again = subprocess.run(lookup_argv, env=env, capture_output=True, text=True, timeout=8)
            if recovered_again.returncode or json.loads(recovered_again.stdout) != original:
                raise ProbeFailure("trusted_native_key_lookup_changed_after_resume")
        with sqlite3.connect(root / "state" / "invocations.sqlite") as db:
            rows = db.execute("SELECT status,receipt FROM calls").fetchall()
        if len(rows) != expected_calls or any(row[0] != "accepted" for row in rows):
            raise ProbeFailure("durable_receipt_mismatch")
        durable = {json.loads(row[1])["external_request_ref"]: json.loads(row[1]) for row in rows}
        observed = {value["external_request_ref"]: value for value in fixture.receipts}
        if crash_case:
            observed.update(original_receipts)
        if durable != observed:
            raise ProbeFailure("durable_receipt_mismatch")
        if fixture.receipt.get("acceptances") != 1:
            raise ProbeFailure("initial_acceptance_count_invalid")
        safe_hook = json.loads(summary.read_text())
        faults = json.loads(proxy_summary.read_text())
        if faults.get("failure") or faults.get("native_calls_observed") != expected_calls or faults.get("outer_stdio_process_starts") != 2:
            raise ProbeFailure("backend_fault_matrix_incomplete")
        faults["native_host_resume"] = "same_native_session_new_host_process"
        faults["old_receipt_lookup_after_host_exit_and_resume"] = "unchanged"
        if crash_case:
            faults["pending_native_delivery_recovery"] = "trusted_native_key_lookup_before_exact_session_resume"
            faults["recovery_uses_private_database_scraping"] = False
            faults["native_original_receipt_delivered_before_kill"] = False
        local_logs = b"".join(path.read_bytes() for path in root.glob("*-std*.local"))
        with sqlite3.connect(root / "state" / "invocations.sqlite") as db:
            private_values = db.execute("SELECT private_binding FROM calls").fetchall()
        exposed = PRIVATE.encode() in local_logs or any(value.encode() in local_logs for (value,) in private_values)
        if exposed:
            raise ProbeFailure("private_binding_exposed_in_native_runtime_output")
        history = protected_native_history(root, host, private_values)
        return {"host": host, "version": actual, "executable_sha256": digest,
                "initial_invocation": "passed", "durable_receipt_matches": True,
                "accepted_native_invocations": expected_calls, "private_binding_absent_from_model_traffic": True,
                "native_resumed_session_identity_matches": True,
                "private_binding_exposed_in_native_runtime_output": exposed,
                "protected_native_state": history,
                "loopback_posts": fixture.posts, "hook_observation": safe_hook,
                "fault_observations": faults, "full_matrix_qualified": False,
                "production_facade_support": "not_advertised"}
    finally:
        fixture.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("prototype", "codex", "claude", "scratch-root"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    if os.environ.get("DOLGORAE_RUN_NATIVE_HOST_INVOCATION_FIXTURE") != "1":
        parser.error("requires DOLGORAE_RUN_NATIVE_HOST_INVOCATION_FIXTURE=1")
    if sys.platform != "darwin" or platform.machine() != "arm64":
        parser.error("requires the selected native macOS ARM host")
    scratch = args.scratch_root.resolve(strict=True)
    if not scratch.is_dir() or not os.access(scratch, os.W_OK):
        parser.error("scratch-root must be an existing writable directory")
    prototype = args.prototype.resolve(strict=True)
    result = {"schema": "dolgorae.native-host-initial-invocation/v1", "provider_accounts_used": False,
              "full_matrix_qualified": False, "hosts": []}
    for host in ("codex", "claude"):
        try:
            executable = getattr(args, host).resolve(strict=True)
            host_result = qualify(host, executable, prototype, scratch)
            host_result["crash_recovery"] = qualify(host, executable, prototype, scratch, crash_case=True)
            host_result["combined_api_posts"] = host_result["loopback_posts"] + host_result["crash_recovery"]["loopback_posts"]
            result["hosts"].append(host_result)
        except (ProbeFailure, OSError, ValueError, subprocess.SubprocessError) as error:
            code = str(error) if isinstance(error, ProbeFailure) else "probe_preparation_failed"
            result["hosts"].append({"host": host, "initial_invocation": "failed", "reason": code})
    print(json.dumps(result, sort_keys=True))
    return 0 if all(host["initial_invocation"] == "passed" for host in result["hosts"]) else 1


if __name__ == "__main__":
    raise SystemExit(main())
