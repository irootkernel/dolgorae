#!/usr/bin/env python3
"""Opt-in native caller and read-only pool checks in fixture-owned state."""
from __future__ import annotations

import argparse
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import hashlib
import os
import re
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


class InteractionResponses(ScriptedResponses):
    """Two current interaction Turns; responses stay on the owned loopback fixture."""
    def __init__(self, workspace: Path, *, max_mcp_requests: int | None = None, protected_canary: str | None = None):
        super().__init__()
        self.workspace = workspace
        self.issued_cases = set()
        self.completed_cases = set()
        self.mcp_reply = threading.Event()
        self.mcp_decision = None
        self.mcp_response = None
        self.mcp_requests = 0
        self.max_mcp_requests = max_mcp_requests
        owner = self
        class McpHandler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass
            def do_POST(self):
                owner.mcp_requests += 1
                if owner.max_mcp_requests is not None and owner.mcp_requests > owner.max_mcp_requests:
                    owner.failure = "mcp_request_budget_exceeded"
                    self.send_error(400); return
                size = int(self.headers.get("Content-Length", "0"))
                if not 0 < size <= 65536 or self.headers.get("Authorization"):
                    self.send_error(400); return
                request = json.loads(self.rfile.read(size))
                if "method" not in request:
                    owner.mcp_response = request
                    owner.mcp_decision = request.get("result", {}).get("action")
                    owner.mcp_reply.set()
                    self.send_response(202); self.end_headers(); return
                method = request["method"]
                if "id" not in request:
                    self.send_response(202); self.end_headers(); return
                if method == "initialize":
                    result = {"protocolVersion": request["params"]["protocolVersion"], "capabilities": {"tools": {}}, "serverInfo": {"name":"task079-fixture","version":"1"}}
                elif method == "tools/list":
                    result = {"tools":[{"name":"ask","description":"Request one protected input.","inputSchema":{"type":"object","properties":{}}}]}
                elif method == "tools/call":
                    self.send_response(200); self.send_header("Content-Type", "text/event-stream"); self.end_headers()
                    message = {"jsonrpc":"2.0","id":7901,"method":"elicitation/create","params":{"message":protected_canary or "Protected fixture input","requestedSchema":{"type":"object","properties":{"q":{"type":"string"}},"required":["q"]}}}
                    self.wfile.write(("event: message\ndata: " + json.dumps(message) + "\n\n").encode()); self.wfile.flush()
                    if not owner.mcp_reply.wait(90):
                        owner.failure = "mcp_reply_timeout"; return
                    result = {"content":[{"type":"text","text":"Input decision received."}]}
                    message = {"jsonrpc":"2.0","id":request["id"],"result":result}
                    self.wfile.write(("event: message\ndata: " + json.dumps(message) + "\n\n").encode()); self.wfile.flush(); self.close_connection=True; return
                else:
                    self.send_error(400); return
                body = json.dumps({"jsonrpc":"2.0","id":request["id"],"result":result}).encode()
                self.send_response(200); self.send_header("Content-Type","application/json"); self.send_header("Content-Length",str(len(body))); self.end_headers(); self.wfile.write(body)
        self.mcp_http = ThreadingHTTPServer(("127.0.0.1", 0), McpHandler)
        self.mcp_worker = threading.Thread(target=self.mcp_http.serve_forever, daemon=True)
        self.mcp_worker.start()

    def respond(self, request):
        from run_native_workspace_policy_fixture import code_call
        with self.lock:
            self.posts += 1
            if self.posts > 6 or not isinstance(request, dict):
                raise ValueError("interaction fixture request bound")
            users = [v for v in request.get("input", []) if v.get("type")=="message" and v.get("role")=="user"]
            text = json.dumps(users[-1]) if users else ""
            case = "permission" if "TASK079 permission" in text else "mcp" if "TASK079 mcp" in text else None
            if case is None:
                raise ValueError("interaction task correlation")
            if case not in self.issued_cases:
                self.issued_cases.add(case)
                name = "request_permissions" if case=="permission" else "mcp__fixture__ask"
                args = {"permissions":{"file_system":{"read":[str(self.workspace)]}},"reason":"Bounded fixture read permission"} if case=="permission" else {}
                if case == "permission":
                    item = code_call(request, "task079-"+case, [(case,name,args)])
                else:
                    # The driver checks this configured server's actual native inventory.
                    # Native deferred tools are callable without being in exec's static description.
                    item = {"type":"custom_tool_call", "call_id":"task079-mcp", "name":"exec", "namespace":"functions", "input":"text({label:'mcp',result:await tools.mcp__fixture__ask({})});"}
            else:
                self.completed_cases.add(case)
                item={"type":"message","role":"assistant","id":"task079-message-"+case,"content":[{"type":"output_text","text":"Done."}]}
            identity = "task079-response-"+str(self.posts)
            return [{"type":"response.created","response":{"id":identity}}, {"type":"response.output_item.done","item":item}, {"type":"response.completed","response":{"id":identity,"usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0}}}]

    def close(self):
        self.mcp_reply.set()
        self.mcp_http.shutdown(); self.mcp_http.server_close(); self.mcp_worker.join(timeout=5)
        super().close()


def campaign(driver: Path, codex: Path, *, binary: Path | None = None, default_variant: str | None = None, readonly_pool: bool = False, pool_limit: int = 1, interactions: bool = False, mcp_only: bool = False) -> dict[str, object]:
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
        if binary is not None:
            binary = binary.resolve(strict=True)
            environment["DOLGORAE_NATIVE_ORDINARY_BIN"] = str(binary)
        if readonly_pool:
            environment["DOLGORAE_NATIVE_READONLY_POOL"] = "1"
            private_file(scratch / "pool-limit", str(pool_limit).encode())
        checked_codex(codex, f"codex-cli {CODEX_VERSION}", environment)
        fixture = InteractionResponses(scratch / "home/workspace") if interactions else ScriptedResponses()
        if default_variant is not None:
            if default_variant not in DEFAULT_FEATURE_VARIANTS:
                raise ValueError("unknown default-feature variant")
            private_file(scratch / "default-feature-variant", default_variant.encode())
        for name in ("fixture-a", "fixture-b"):
            config=b'model = "gpt-5.6-luna"\nmodel_reasoning_effort = "low"\n'
            if readonly_pool and name == "fixture-b":
                config += b'model_context_window = 65536\n'
            config += fixture_config(fixture.base_url)
            if interactions:
                config += (f'\n[mcp_servers.fixture]\nurl = "http://127.0.0.1:{fixture.mcp_http.server_port}/mcp"\ndefault_tools_approval_mode = "approve"\n').encode()
            config += b'\n[features]\n'
            if default_variant is None:
                config += b'plugins = false\n'
            if default_variant == "config-enable":
                config += b'shell_snapshot = true\n'
            elif default_variant is None:
                config += b'shell_snapshot = false\n'
                if interactions:
                    config += b'request_permissions_tool = true\ntool_search_always_defer_mcp_tools = false\n'
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
        if interactions:
            private_file(scratch / "interaction-cases", b"mcp\n" if mcp_only else b"permission,mcp\n")
        result = subprocess.run([str(driver), str(scratch), str(codex)], env=environment,
                                capture_output=True, text=True, timeout=None if binary is not None else 300)
        private_file(scratch / "driver.stdout", result.stdout.encode())
        private_file(scratch / "driver.stderr", result.stderr.encode())
        if interactions:
            private_file(scratch / "interaction-diagnostic.json", json.dumps({"failure":fixture.failure,"details":fixture.failure_details,"issued_cases":sorted(fixture.issued_cases),"completed_cases":sorted(fixture.completed_cases),"mcp_decision":fixture.mcp_decision,"posts":fixture.posts}).encode())
        if result.returncode or fixture.failure or (not readonly_pool and not interactions and fixture.posts):
            raise ValueError("native caller pipeline failed; no automatic retry")
        if readonly_pool:
            if (fixture.result is None or fixture.result[0] == 0 or fixture.posts != 2
                    or fixture.instruction_inputs_observed != 1
                    or fixture.ambient_seen["project"] or fixture.ambient_seen["ancestor"]
                    or (scratch / "home" / "workspace" / "pool-write-denied").exists()):
                raise ValueError("native pooled read-only or instruction suppression failed")
        if interactions and (fixture.completed_cases != ({"mcp"} if mcp_only else {"permission", "mcp"}) or fixture.mcp_decision != "accept" or fixture.posts != (2 if mcp_only else 4)):
            raise ValueError("native interaction routing incomplete")
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
        if binary is not None:
            observations["ordinary_product"] = {"path": str(binary), "sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}
        if default_variant is not None:
            observations.update(external_provider_requests=None, other_network_metered=False,
                                scripted_model_provider_requests=fixture.posts,
                                plugin_sources=observed_plugin_sources(scratch))
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


def observed_plugin_sources(root: Path) -> list[dict[str, object]]:
    """Retain public source observations before cleanup, without invoking Git."""
    result = []
    for name in ("fixture-a", "fixture-b"):
        git = root / name / ".tmp/plugins/.git"
        row = {"profile": name, "repository_present": git.is_dir(), "revision": None}
        if git.is_dir():
            config = (git / "config").read_text() if (git / "config").is_file() else ""
            public_origin = "https://github.com/openai/plugins.git"
            row["origin"] = public_origin if f"url = {public_origin}" in config else "unrecognized"
            head = (git / "HEAD").read_text().strip() if (git / "HEAD").is_file() else ""
            if head.startswith("ref: "):
                ref = head[5:]
                if ref != "refs/heads/main":
                    raise ValueError("unexpected default plugin source reference")
                loose = git / ref
                head = loose.read_text().strip() if loose.is_file() else ""
                packed = git / "packed-refs"
                if not head and packed.is_file():
                    head = next((line.split()[0] for line in packed.read_text().splitlines()
                                 if line.endswith(" " + ref)), "")
            if re.fullmatch(r"[0-9a-f]{40}", head):
                row["revision"] = head
        result.append(row)
    return result


def require_default_startup_path(value: dict) -> None:
    startup = value.get("startup", {})
    path = startup.get("path")
    if (not {"path", "initial_error", "recovery", "run_id", "thread_id"} <= startup.keys()
            or not startup.get("run_id") or not startup.get("thread_id")):
        raise ValueError("original startup identity missing")
    if path == "first_attempt":
        if (startup.get("initial_error") is not None or startup.get("recovery") is not None
                or value.get("quiescent_startup") is not True):
            raise ValueError("first-attempt startup evidence conflicts")
        return
    if path != "recovered" or value.get("quiescent_startup") is not False:
        raise ValueError("startup path unproved")
    error, recovery = startup.get("initial_error"), startup.get("recovery")
    if not isinstance(error, dict) or not isinstance(recovery, dict):
        raise ValueError("original failure and recovery evidence required")
    details, replay = error.get("details", {}), recovery.get("replay", {})
    receipt = recovery.get("receipt", {})
    if (error.get("code") != "TRANSPORT_FAILURE" or error.get("retryable") is not False
            or details.get("acceptance") != "uncertain" or details.get("stage") != "correlate"
            or details.get("run_id") != startup["run_id"] or details.get("request_id") is not None
            or recovery.get("operation") != "Recover" or recovery.get("writer_state") != "active"
            or recovery.get("policy_access") != "write" or recovery.get("runs") != 1
            or any(recovery.get(key) is not True for key in
                   ("manifest_unchanged", "worker_identity_unchanged", "dedicated_identity_unchanged"))
            or recovery.get("census_refusal", {}).get("details", {}).get("predicate") not in
                   ("dedicated_census_membership", "worker_scope")
            or not all(re.fullmatch(r"[0-9a-f]{64}", recovery.get(key, "")) for key in
                       ("request_sha256", "reservation_sha256", "manifest_sha256"))
            or replay.get("exact_replay") is not True or replay.get("run_id") != startup["run_id"]
            or replay.get("thread_id") != startup["thread_id"]
            or receipt.get("run_id") != startup["run_id"]
            or receipt.get("original_thread_id") != startup["thread_id"]
            or receipt.get("accepted_intent_sha256") != recovery.get("reservation_sha256")
            or receipt.get("run_generation") != recovery.get("run_generation")):
        raise ValueError("original-Run writable recovery and replay unproved")


def require_default_feature_observation(value, variant: str) -> None:
    if not isinstance(value, dict) or value.get("variant") != variant or value.get("effective_shell_snapshot") is not False or value.get("quiescent_verified_result") is not True:
        raise ValueError("actual default-feature behavior unproved")
    require_default_startup_path(value)
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
    for key in ("shared_plugin_observation", "dedicated_plugin_observation"):
        observed = value.get(key, {})
        if observed.get("name") != "plugins" or observed.get("enabled") is not True:
            raise ValueError("actual default-enabled plugin input unproved")
    argv=launch.get("normalized_argv", [])
    decisions=[a == "--enable" for a,b in zip(argv,argv[1:]) if a in ("--enable","--disable") and b == "shell_snapshot"]
    if not decisions or decisions[-1] is not False or "--disable shell_snapshot" not in value["actual_command"]:
        raise ValueError("actual final shell_snapshot disabling unproved")


def default_feature_campaign(driver: Path, codex: Path, *, binary: Path | None = None) -> dict[str, object]:
    if os.environ.get(OPT_IN) != "1":
        raise ValueError("explicit native caller pipeline opt-in required")
    observations=[campaign(driver,codex,binary=binary,default_variant=v) for v in DEFAULT_FEATURE_VARIANTS]
    return {"schema":"dolgorae.default-feature-startup/v1","variants":observations,"passed":True,
            "external_provider_requests":None,"other_network_metered":False,
            "scripted_model_provider_requests":sum(row["loopback_posts"] for row in observations),
            "native_model_turns":0,"existing_quiescence_rules_unchanged":True}


def main() -> int:
    if os.environ.get(OPT_IN) != "1":
        print("native caller pipeline requires explicit opt-in")
        return 2
    parser = argparse.ArgumentParser()
    parser.add_argument("--driver", type=Path, required=True)
    parser.add_argument("--codex", type=Path, required=True)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--default-feature-startup-only", action="store_true")
    parser.add_argument("--readonly-pool", action="store_true")
    parser.add_argument("--interactions", action="store_true")
    parser.add_argument("--interactions-mcp-only", action="store_true")
    parser.add_argument("--pool-limit", type=int, choices=(1, 4), default=1)
    args = parser.parse_args()
    try:
        selected=default_feature_campaign if args.default_feature_startup_only else campaign
        kwargs = {"readonly_pool": True, "pool_limit": args.pool_limit} if args.readonly_pool else {}
        if args.interactions or args.interactions_mcp_only:
            kwargs = {"interactions": True, "mcp_only": args.interactions_mcp_only}
        if args.binary is not None:
            kwargs["binary"] = args.binary
        observations = selected(args.driver.resolve(strict=True), args.codex.resolve(strict=True), **kwargs)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
        print(json.dumps({"schema": "dolgorae.native-caller-pipeline-failure/v1", "passed": False}))
        return 1
    print(json.dumps(observations, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
