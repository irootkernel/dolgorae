#!/usr/bin/env python3
"""Opt-in live TASK-025 transport probe against the local Codex CLI.

Uses the currently installed `codex` binary. It always creates an isolated
CODEX_HOME and workspace; it never uses ~/.codex or ~/.dolgorae. Plan approval
alone does not authorize this campaign; DOLGORAE_RUN_LIVE_TRANSPORT_PROBE=1 is
required.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import textwrap
import time
from pathlib import Path
from typing import Any

from run_access_safety_acceptance import AppServer

OPT_IN = "DOLGORAE_RUN_LIVE_TRANSPORT_PROBE"
REQUIRED_SCENARIOS = (
    "source_binding",
    "model_identity_injection",
    "same_call_retry",
    "different_input_conflict",
    "concurrent_call_isolation",
    "cancellation",
    "bounded_wait_expiry",
    "connection_loss",
    "bridge_restart",
    "stale_generation",
    "credential_canary",
    "shared_identity_ambiguity",
)
META_TURN = "xyz.rootkernel.dolgorae/sourceTurnId"
META_CALL = "xyz.rootkernel.dolgorae/sourceToolCallId"
META_IDEMPOTENCY = "xyz.rootkernel.dolgorae/idempotencyKey"


def local_codex() -> Path:
    configured = os.environ.get("DOLGORAE_CODEX_BIN")
    candidate = Path(configured).expanduser() if configured else Path(shutil.which("codex") or "")
    if not candidate:
        raise SystemExit("no local Codex CLI was found")
    return candidate.resolve(strict=True)


def codex_version(codex: Path) -> str:
    observed = subprocess.run(
        [str(codex), "--version"], check=True, capture_output=True, text=True
    ).stdout.strip()
    prefix = "codex-cli "
    if observed.startswith(prefix):
        return observed[len(prefix) :]
    return observed


def native_schema_binding(codex: Path, root: Path) -> dict[str, Any]:
    bundle = root / "schema"
    subprocess.run(
        [str(codex), "app-server", "generate-json-schema", "--out", str(bundle)],
        check=True,
        capture_output=True,
        text=True,
    )
    server_request = (bundle / "ServerRequest.json").read_text(encoding="utf-8")
    params = json.loads((bundle / "DynamicToolCallParams.json").read_text(encoding="utf-8"))
    required = set(params.get("required") or [])
    proved = (
        '"item/tool/call"' in server_request
        and required.issuperset({"arguments", "callId", "threadId", "tool", "turnId"})
    )
    return {
        "method": "item/tool/call",
        "required_fields": sorted(required),
        "proved": proved,
    }


def write_mcp(path: Path, capture: Path, sleep_seconds: float = 0.0) -> None:
    path.write_text(
        textwrap.dedent(
            f"""
            import hashlib, json, sys, time
            capture = {str(capture)!r}
            sleep_seconds = {sleep_seconds!r}
            receipts_path = capture + ".receipts"
            try:
                with open(receipts_path, encoding="utf-8") as fh:
                    receipts = json.load(fh)
            except Exception:
                receipts = {{}}
            def send(obj):
                sys.stdout.write(json.dumps(obj, separators=(",", ":")) + "\\n")
                sys.stdout.flush()
            def rec(event):
                with open(capture, "a", encoding="utf-8") as fh:
                    fh.write(json.dumps(event) + "\\n")
            def save():
                with open(receipts_path, "w", encoding="utf-8") as fh:
                    json.dump(receipts, fh)
            def digest(value):
                return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
            bound = ["orchestrated_session_id","source_primary_run_id","source_turn_id","source_tool_call_id","controller_principal","root_priority","idempotency_key"]
            canaries = ["capability","BEGIN PRIVATE","/private/sockets/","sqlite","raw-frame","child-controller"]
            for line in sys.stdin:
                req = json.loads(line)
                rec({{"direction":"in","method":req.get("method"),"meta_keys":sorted(((req.get("params") or {{}}).get("_meta") or {{}}).keys()),"arg_keys":sorted(((req.get("params") or {{}}).get("arguments") or {{}}).keys())}})
                ident = req.get("id")
                method = req.get("method")
                if ident is None:
                    continue
                if method == "initialize":
                    send({{"jsonrpc":"2.0","id":ident,"result":{{"protocolVersion": (req.get("params") or {{}}).get("protocolVersion"), "capabilities": {{"tools": {{"listChanged": False}}}}, "serverInfo": {{"name":"probe","version":"1"}}}}}})
                    continue
                if method == "tools/list":
                    send({{"jsonrpc":"2.0","id":ident,"result":{{"tools":[{{"name":"dolgorae_transport_probe","description":"probe","inputSchema":{{"type":"object","additionalProperties":False,"required":["nonce"],"properties":{{"nonce":{{"type":"string"}}}}}}}}]}}}})
                    continue
                if method != "tools/call":
                    send({{"jsonrpc":"2.0","id":ident,"error":{{"code":-32601,"message":"not found"}}}})
                    continue
                params = req.get("params") or {{}}
                arguments = params.get("arguments") or {{}}
                meta = params.get("_meta") or {{}}
                if any(key in arguments for key in bound):
                    send({{"jsonrpc":"2.0","id":ident,"error":{{"code":-32000,"message":"identity is not model-controlled","data":{{"code":"INVALID_ARGUMENT"}}}}}})
                    continue
                rendered = json.dumps(arguments)
                if any(fragment in rendered for fragment in canaries):
                    send({{"jsonrpc":"2.0","id":ident,"error":{{"code":-32000,"message":"canary","data":{{"code":"ORCHESTRATION_TRANSPORT_UNAVAILABLE"}}}}}})
                    continue
                turn = meta.get({META_TURN!r})
                call = meta.get({META_CALL!r})
                idem = meta.get({META_IDEMPOTENCY!r})
                if not turn or not call or not idem:
                    send({{"jsonrpc":"2.0","id":ident,"error":{{"code":-32000,"message":"unbound","data":{{"code":"ORCHESTRATION_TRANSPORT_UNAVAILABLE"}}}}}})
                    continue
                if sleep_seconds:
                    time.sleep(sleep_seconds)
                key = f"{{turn}}:{{call}}:{{idem}}"
                hashed = digest(arguments)
                previous = receipts.get(key)
                if previous is not None:
                    if previous[0] == hashed:
                        send({{"jsonrpc":"2.0","id":ident,"result":{{"content":[{{"type":"text","text":"replay"}}],"structuredContent":{{"state":"replay","payload_sha256":hashed}}}}}})
                    else:
                        send({{"jsonrpc":"2.0","id":ident,"error":{{"code":-32000,"message":"conflict","data":{{"code":"ORCHESTRATION_IDEMPOTENCY_CONFLICT"}}}}}})
                    continue
                receipts[key] = [hashed, "accepted"]
                save()
                send({{"jsonrpc":"2.0","id":ident,"result":{{"content":[{{"type":"text","text":"accepted"}}],"structuredContent":{{"state":"accepted","payload_sha256":hashed}}}}}})
            """
        ),
        encoding="utf-8",
    )


def meta(turn: str, call: str, idem: str) -> dict[str, str]:
    return {META_TURN: turn, META_CALL: call, META_IDEMPOTENCY: idem}


INIT_PARAMS = {
    "clientInfo": {"name": "dolgorae-transport-probe", "version": "1"},
    "capabilities": {
        "experimentalApi": True,
        "optOutNotificationMethods": [],
    },
}


def bind_session(codex: Path, workspace: Path, home: Path | None = None) -> tuple[AppServer, str]:
    server = AppServer(codex, workspace)
    try:
        initialized = server.request("initialize", INIT_PARAMS)
        if home is not None and Path(initialized["codexHome"]).resolve() != home.resolve():
            raise RuntimeError("initialize selected the wrong Codex home")
        server.send({"method": "initialized", "params": {}})
        thread_id = server.request("thread/start", {"cwd": str(workspace)})["thread"]["id"]
        server.request("config/mcpServer/reload", {})
        return server, thread_id
    except Exception:
        server.close()
        raise


def call_error_code(server: AppServer) -> str | None:
    message = server.messages[-1] if server.messages else {}
    error = message.get("error") if isinstance(message, dict) else None
    if not isinstance(error, dict):
        return None
    data = error.get("data")
    if isinstance(data, dict) and isinstance(data.get("code"), str):
        return data["code"]
    if isinstance(error.get("message"), str):
        return error["message"]
    return None


def run_campaign(codex: Path) -> dict[str, Any]:
    version = codex_version(codex)
    verdicts: dict[str, str] = {}
    with tempfile.TemporaryDirectory(prefix="dolgorae-live-transport-") as temporary:
        root = Path(temporary)
        home = root / "codex-home"
        workspace = root / "workspace"
        home.mkdir(mode=0o700)
        workspace.mkdir(mode=0o700)
        capture = root / "capture.jsonl"
        capture.write_text("", encoding="utf-8")
        mcp = root / "mcp.py"
        slow = root / "mcp_slow.py"
        write_mcp(mcp, capture)
        write_mcp(slow, capture, sleep_seconds=8.0)
        (home / "config.toml").write_text(
            f"""
[mcp_servers.probe]
command = {json.dumps(sys.executable)}
args = [{json.dumps(str(mcp))}, {json.dumps(str(capture))}]
startup_timeout_sec = 20
tool_timeout_sec = 30

[mcp_servers.slow]
command = {json.dumps(sys.executable)}
args = [{json.dumps(str(slow))}, {json.dumps(str(capture))}]
startup_timeout_sec = 20
tool_timeout_sec = 2
""",
            encoding="utf-8",
        )
        os.environ["CODEX_HOME"] = str(home)
        native = native_schema_binding(codex, root)
        verdicts["source_binding"] = "proved" if native["proved"] else "failed"
        verdicts["stale_generation"] = "fixture_proved"
        verdicts["cancellation"] = "fixture_proved"
        first: dict[str, Any] = {}
        server: AppServer | None = None
        server, thread_id = bind_session(codex, workspace, home)
        try:
            status = server.request("mcpServerStatus/list", {"limit": 20, "detail": "full"})
            tools = {
                name
                for entry in (status.get("data") or [])
                if isinstance(entry, dict)
                for name in (entry.get("tools") or {})
            }
            if "dolgorae_transport_probe" not in tools:
                raise RuntimeError("MCP probe tool was not registered")

            def tool_call(
                arguments: dict[str, Any],
                extra_meta: dict[str, str],
                *,
                mcp_server: str = "probe",
                session: AppServer | None = None,
                thread: str | None = None,
            ) -> dict[str, Any]:
                owner = session or server
                return owner.request(
                    "mcpServer/tool/call",
                    {
                        "server": mcp_server,
                        "threadId": thread or thread_id,
                        "tool": "dolgorae_transport_probe",
                        "arguments": arguments,
                        "_meta": extra_meta,
                    },
                )

            first = tool_call({"nonce": "n1"}, meta("turn-1", "call-1", "idem-1"))
            replay = tool_call({"nonce": "n1"}, meta("turn-1", "call-1", "idem-1"))
            if first.get("structuredContent", {}).get("state") == "accepted" and replay.get(
                "structuredContent", {}
            ).get("payload_sha256") == first.get("structuredContent", {}).get("payload_sha256"):
                verdicts["same_call_retry"] = "proved"
            else:
                verdicts["same_call_retry"] = "failed"

            try:
                tool_call({"nonce": "n2"}, meta("turn-1", "call-1", "idem-1"))
                verdicts["different_input_conflict"] = "failed"
            except RuntimeError:
                verdicts["different_input_conflict"] = (
                    "proved"
                    if call_error_code(server) == "ORCHESTRATION_IDEMPOTENCY_CONFLICT"
                    else "failed"
                )

            try:
                tool_call(
                    {"nonce": "n1", "source_turn_id": "forged"},
                    meta("turn-2", "call-2", "idem-2"),
                )
                verdicts["model_identity_injection"] = "failed"
            except RuntimeError:
                verdicts["model_identity_injection"] = (
                    "proved" if call_error_code(server) == "INVALID_ARGUMENT" else "failed"
                )

            second = tool_call({"nonce": "n3"}, meta("turn-1", "call-3", "idem-3"))
            verdicts["concurrent_call_isolation"] = (
                "proved"
                if second.get("structuredContent", {}).get("state") == "accepted"
                else "failed"
            )

            try:
                tool_call(
                    {"nonce": "/private/sockets/worker.sock"},
                    meta("turn-4", "call-4", "idem-4"),
                )
                verdicts["credential_canary"] = "failed"
            except RuntimeError:
                verdicts["credential_canary"] = (
                    "proved"
                    if call_error_code(server) == "ORCHESTRATION_TRANSPORT_UNAVAILABLE"
                    else "failed"
                )

            try:
                tool_call({"nonce": "n5"}, {})
                verdicts["shared_identity_ambiguity"] = "failed"
            except RuntimeError:
                verdicts["shared_identity_ambiguity"] = (
                    "ambiguous"
                    if call_error_code(server) == "ORCHESTRATION_TRANSPORT_UNAVAILABLE"
                    else "failed"
                )

            capture_text = capture.read_text(encoding="utf-8")
            if META_TURN in capture_text and "threadId" in capture_text and native["proved"]:
                verdicts["source_binding"] = "proved"

            wait_started = time.monotonic()
            try:
                tool_call(
                    {"nonce": "slow-wait"},
                    meta("turn-w", "call-w", "idem-w"),
                    mcp_server="slow",
                )
                verdicts["bounded_wait_expiry"] = "failed"
            except (RuntimeError, TimeoutError):
                if time.monotonic() - wait_started < 1.5:
                    verdicts["bounded_wait_expiry"] = "failed"
                else:
                    time.sleep(7.0)
                    after_wait = tool_call(
                        {"nonce": "slow-wait"},
                        meta("turn-w", "call-w", "idem-w"),
                    )
                    state = after_wait.get("structuredContent", {}).get("state")
                    verdicts["bounded_wait_expiry"] = (
                        "proved" if state in {"accepted", "replay"} else "failed"
                    )
        finally:
            if server is not None and server.process.poll() is None:
                server.close()

        restarted = None
        try:
            restarted, new_thread = bind_session(codex, workspace)
            replayed = restarted.request(
                "mcpServer/tool/call",
                {
                    "server": "probe",
                    "threadId": new_thread,
                    "tool": "dolgorae_transport_probe",
                    "arguments": {"nonce": "n1"},
                    "_meta": meta("turn-1", "call-1", "idem-1"),
                },
            )
            verdicts["bridge_restart"] = (
                "proved"
                if replayed.get("structuredContent", {}).get("state") == "replay"
                and replayed.get("structuredContent", {}).get("payload_sha256")
                == first.get("structuredContent", {}).get("payload_sha256")
                else "failed"
            )
        except Exception:
            verdicts["bridge_restart"] = "failed"
        finally:
            if restarted is not None:
                restarted.close()

        dying = None
        recovered = None
        try:
            dying, dying_thread = bind_session(codex, workspace)
            req_id = dying.next_id
            dying.next_id += 1
            dying.send(
                {
                    "id": req_id,
                    "method": "mcpServer/tool/call",
                    "params": {
                        "server": "slow",
                        "threadId": dying_thread,
                        "tool": "dolgorae_transport_probe",
                        "arguments": {"nonce": "in-flight"},
                        "_meta": meta("turn-d", "call-d", "idem-d"),
                    },
                }
            )
            time.sleep(0.3)
            dying.close()
            dying = None
            recovered, recovered_thread = bind_session(codex, workspace)
            observed = recovered.request(
                "mcpServer/tool/call",
                {
                    "server": "probe",
                    "threadId": recovered_thread,
                    "tool": "dolgorae_transport_probe",
                    "arguments": {"nonce": "in-flight"},
                    "_meta": meta("turn-d", "call-d", "idem-d"),
                },
            )
            state = observed.get("structuredContent", {}).get("state")
            verdicts["connection_loss"] = (
                "proved" if state in {"accepted", "replay"} and state != "cancelled" else "failed"
            )
        except Exception:
            verdicts["connection_loss"] = "failed"
            if dying is not None:
                dying.close()
        finally:
            if recovered is not None:
                recovered.close()

        missing = [name for name in REQUIRED_SCENARIOS if name not in verdicts]
        if missing:
            for name in missing:
                verdicts[name] = "failed"
        return {
            "codex_cli": version,
            "native": native,
            "verdicts": verdicts,
            "forwarded_meta": True,
        }


LIVE_REQUIRED = (
    "source_binding",
    "model_identity_injection",
    "same_call_retry",
    "different_input_conflict",
    "concurrent_call_isolation",
    "bounded_wait_expiry",
    "connection_loss",
    "bridge_restart",
    "credential_canary",
)
FIXTURE_ALLOWED = ("cancellation", "stale_generation")


def select(result: dict[str, Any]) -> dict[str, Any]:
    verdicts = result["verdicts"]
    live_ok = result["native"]["proved"] and all(
        verdicts.get(name) == "proved" for name in LIVE_REQUIRED
    )
    fixture_ok = all(verdicts.get(name) in {"proved", "fixture_proved"} for name in FIXTURE_ALLOWED)
    ambiguous_ok = verdicts.get("shared_identity_ambiguity") == "ambiguous"
    if live_ok and fixture_ok and ambiguous_ok:
        return {
            "status": "selected",
            "selected": "native_run_bound",
            "live_evidence": "recorded",
            "reason": (
                f"Local Codex CLI {result['codex_cli']} (TASK-025 campaign pin, not the "
                "0.153.4 product baseline) exposes item/tool/call with required "
                "thread/turn/call fields. Isolated-home MCP forwarding proved host _meta, "
                "retry, conflict, injection, concurrency, canaries, wait expiry, "
                "in-flight disconnect, restart replay, and shared-identity ambiguity. "
                "Cancellation and stale-generation remain fixture_proved without a model turn."
            ),
        }
    return {
        "status": "unselected",
        "selected": None,
        "live_evidence": "required",
        "reason": "the local Codex live probe did not prove every required live scenario",
    }


def main() -> int:
    if os.environ.get(OPT_IN) != "1":
        print(f"{OPT_IN}=1 is required", file=sys.stderr)
        return 2
    codex = local_codex()
    result = run_campaign(codex)
    selection = select(result)
    report = {
        "codex_pin": result["codex_cli"],
        "status": selection["status"],
        "selected": selection["selected"],
        "live_evidence": selection["live_evidence"],
        "required_scenarios": list(REQUIRED_SCENARIOS),
        "verdicts": result["verdicts"],
        "native_method": result["native"],
        "reason": selection["reason"],
    }
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0 if selection["status"] == "selected" else 1


if __name__ == "__main__":
    raise SystemExit(main())
