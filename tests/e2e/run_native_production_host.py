#!/usr/bin/env python3
"""Original eighteen production host scenarios, with next-HTTP byte witnesses.

Execution is opt-in. Trace emission and offline checks do not qualify a host.
All fixture state, transcripts and subprocess groups belong to one private row.
"""
from __future__ import annotations

import argparse
import copy
import fcntl
import hashlib
import json
import os
import re
import selectors
from pathlib import Path
import shlex
import signal
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from run_native_host_invocation_fixture import host_environment, native_session, run_host
from run_native_workspace_policy_fixture import code_call, code_results, terminal_output

OPT_IN = "DOLGORAE_RUN_NATIVE_PRODUCTION_HOST"
BODY = 4 * 1024 * 1024
ROWS = ("initialK", "replay", "conflict", "concurrent replay", "distinct equal-text",
        "reconnect", "backendrestart", "beforeacceptance loss", "afteracceptance/mirror-before loss",
        "aftermirror replyloss", "physicalhostloss", "exactsessionrecovery", "missingstate",
        "corruptstate", "exposurecanary", "Rfreshobservation", "Sloss/duplicate", "hookbypass")
PINS = {
    "codex": ("codex-cli 0.164.0", "d8990e3ee7acfbc26f91265b12a1a83f0fee40d0e0f8f1f40a37d4ec21f499cf", "codex-0.160.0"),
    "claude": ("2.1.310 (Claude Code)", "ffb64331bc0ee1b69efb7c2c428bd3ab516bc31b75bf60886b356c97c1c25ffd", "claude-code-2.1.295"),
}
DELEGATED = ("Rfreshobservation", "beforeacceptance loss", "afteracceptance/mirror-before loss", "aftermirror replyloss", "physicalhostloss", "exactsessionrecovery")
RESULT_ROWS = ("reconnect", "backendrestart", "exposurecanary", "Sloss/duplicate", *DELEGATED)
CONNECTED = (*DELEGATED, "exposurecanary")
SCHEMA = "dolgorae.external-specialist-request/v5"
CONTROL = "dolgorae.consumer-control/v1"
PRIVATE = "_dolgorae_host_binding"


def encoded(value) -> bytes:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()


def private(path: Path, body: bytes) -> None:
    with os.fdopen(os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600), "wb") as out:
        out.write(body)


def require(value, reason: str) -> None:
    if not value:
        raise RuntimeError(reason)


def trace(row: str) -> dict:
    require(row in ROWS, "unknown_original_row")
    result = row in RESULT_ROWS
    # One executor Turn is two POSTs. Four data-dependent host responses plus
    # the terminal next HTTP request use five; Claude may count tokens once.
    steps = (["prepare qualified Profile and genuine provisioning (0 POST)",
              "assign once through original accepted source; executor tool and final (2 POST)",
              "native collect (POST1)", "native original-view inspection (POST2)",
              "continuation or prepare_accept (POST3)", "status/error or explicit accept (POST4)",
              "decoded next model-facing success/error/continuation witness (POST5)"] if result else
             ["operator/controller/owner/namespace setup (0 POST)", "native K (POST1)",
              "row-specific replay/loss/refusal through production backend; original-key lookup (0 POST)",
              "decoded next model-facing return (POST2)"])
    if row in ("physicalhostloss","exactsessionrecovery"):
        steps = steps[:3]+["inspect original and prepare retained accept, without accepting (POST2)",
                         "observe original bytes/prepared result, explicitly accept, lose reply and host (POST3)",
                         "resume exact session; status of original caller-known request ID (POST4)",
                         "decoded correlated original receipt status, no credit for lost reply (POST5)"]
    return {"row": row, "state": "planned", "deadline_seconds": 90, "cleanup_seconds": 30,
            "combined_post_limit": 8, "delivery_limit": 32, "per_input_bytes": BODY,
            "steps": steps, "executor_posts": 2 if result else 0,
            "host_message_post_max": 5 if result else 4, "count_token_post_reserve": 1,
            "fault": row, "producer": "ordinary20k" if row == "Rfreshobservation" else
            "quote-heavy20k" if row == "backendrestart" else "nonempty_original_and_structured" if result else None,
            "cleanup": "close retained Runs; operator retire exact qualified binding; join exact owned host/backend groups"}


def owner_config(root: Path) -> Path:
    host = json.loads((root / "input.json").read_text())["host"]
    require(host in PINS, "unknown_selected_host")
    return root / host / "owner.json"


def counter(root: Path, kind: str) -> int:
    path = root / "delivery-count"
    with path.open("a+b") as stream:
        fcntl.flock(stream, fcntl.LOCK_EX)
        stream.seek(0)
        number = int(stream.read() or b"0") + 1
        require(number <= 32, "delivery_budget_exceeded")
        stream.seek(0); stream.truncate(); stream.write(str(number).encode()); stream.flush()
        fcntl.flock(stream, fcntl.LOCK_UN)
    with (root / "deliveries.jsonl").open("ab") as out:
        out.write(encoded({"kind": kind, "ordinal": number}) + b"\n")
    return number


def documents(value):
    if isinstance(value, dict):
        yield value
        for child in value.values():
            yield from documents(child)
    elif isinstance(value, list):
        for child in value:
            yield from documents(child)
    elif isinstance(value, str):
        try:
            parsed = json.loads(value)
        except (ValueError, RecursionError):
            return
        if not isinstance(parsed, str):
            yield from documents(parsed)


def native_tool(tools, selected: str, namespace=None):
    if isinstance(tools, list):
        for child in tools:
            found = native_tool(child, selected, namespace)
            if found:
                return found
    elif isinstance(tools, dict):
        name = tools.get("name", "")
        if isinstance(name, str) and name.endswith(selected):
            return name, namespace
        namespace = name if tools.get("type") == "namespace" else namespace
        for key in ("tools", "functions", "function"):
            if key in tools:
                found = native_tool(tools[key], selected, namespace)
                if found:
                    return found
    return None


class Backend:
    """One production stdio connection. Native handshake bytes are forwarded."""
    def __init__(self, binary, owner, environment, root):
        self.root=root
        self.process=subprocess.Popen([str(binary),"__trusted-specialist-host","host-mcp","--owner-config",str(owner)],env=environment,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
        self.buffer=b""
        self.selector=selectors.DefaultSelector()
        os.set_blocking(self.process.stdout.fileno(),False)
        self.selector.register(self.process.stdout,selectors.EVENT_READ)

    def exchange(self, raw: bytes):
        require(len(raw)<=BODY,"native_mcp_frame_outside_bound")
        frame=json.loads(raw)
        if frame.get("method")=="tools/call": counter(self.root,"production_stdio")
        self.process.stdin.write(raw if raw.endswith(b"\n") else raw+b"\n");self.process.stdin.flush()
        if "id" not in frame: return None
        deadline=float((self.root / "deadline").read_text())
        while time.monotonic()<deadline:
            if b"\n" in self.buffer:
                line,self.buffer=self.buffer.split(b"\n",1)
                reply=json.loads(line)
                if "id" not in reply:
                    sys.stdout.buffer.write(line+b"\n");sys.stdout.buffer.flush()
                    continue
                require(reply["id"]==frame["id"],"production_stdio_correlation_missing")
                return reply
            if not self.selector.select(min(.1,max(.001,deadline-time.monotonic()))): continue
            chunk=os.read(self.process.stdout.fileno(),65536)
            require(chunk,"production_backend_closed_without_reply")
            self.buffer+=chunk
            require(len(self.buffer)<=16*1024*1024,"production_backend_message_bound_exceeded")
        raise RuntimeError("production_backend_deadline")

    def replay_handshake(self):
        frames=(self.root / "native-handshake.jsonl").read_bytes().splitlines()
        require(frames and json.loads(frames[0]).get("method")=="initialize","actual_native_handshake_missing")
        for raw in frames:
            reply=self.exchange(raw)
            if json.loads(raw).get("method")=="initialize":
                original=json.loads((self.root / "production-initialize.json").read_bytes())
                require(reply==original,"restarted_production_negotiation_changed")

    def close(self):
        self.selector.close()
        if self.process.stdin and not self.process.stdin.closed: self.process.stdin.close();self.process.stdin=None
        try: self.process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            self.process.kill();self.process.wait(timeout=2)
        if self.process.stdout: self.process.stdout.close()
        if self.process.stderr:
            error=self.process.stderr.read();self.process.stderr.close()
            if error:
                name="backend-stderr-"+str(self.process.pid)+".local"
                if not (self.root / name).exists(): private(self.root / name,error)


def rpc(binary: Path, owner: Path, frame: dict, environment: dict, root: Path) -> dict:
    # Deliberate cold replay only. Reuse the selected host's actual handshake;
    # no fixture-selected protocolVersion/serverInfo is introduced.
    backend=Backend(binary,owner,environment,root)
    try:
        backend.replay_handshake()
        return backend.exchange(encoded(frame))
    finally: backend.close()


def consumer_record(root: Path) -> dict:
    """Fault timing only; never read a key, prepared input or receipt."""
    with sqlite3.connect(owner_config(root).with_name("consumer.sqlite").as_uri()+"?mode=ro",uri=True) as db:
        records=db.execute("SELECT json_extract(record,'$.state'),json_extract(record,'$.child') FROM requests WHERE request_id=?",("native-accept",)).fetchall()
    require(len(records)==1,"original_consumer_timing_missing_or_ambiguous")
    return {"state":records[0][0],"child":records[0][1]}


def cut_before_mirror(backend: Backend, frame: dict, raw: bytes, binary: Path, owner: Path, environment: dict, root: Path) -> dict:
    counter(root,"production_accept_mirror_before_loss")
    process=backend.process
    stopped=False
    try:
        process.stdin.write(raw);process.stdin.flush()
        deadline=min(float((root / "deadline").read_text()),time.monotonic()+8)
        before=None
        while time.monotonic()<deadline and process.poll() is None:
            record=consumer_record(root)
            if record["state"]=="running" and record.get("child"):
                os.kill(process.pid,signal.SIGSTOP);stopped=True
                before=consumer_record(root)
                require(before["state"]=="running","mirror_before_window_already_closed")
                break
            time.sleep(.0005)
        require(stopped and before,"actual_running_child_cut_not_reached")
        # SQLite supplies timing only. Never take a key, receipt, model input or
        # recovery decision from it. The following PID is never signalled.
        child=before["child"]
        while time.monotonic()<deadline:
            state=subprocess.run(["/bin/ps","-p",str(child),"-o","stat="],capture_output=True,text=True,timeout=1).stdout.strip()
            if not state or state.startswith("Z"): break
            time.sleep(.005)
        else: raise RuntimeError("paused_backend_child_did_not_finish")
        require(consumer_record(root)["state"] in ("running","uncertain"),"mirror_was_published_before_loss")
        process.kill();process.wait(timeout=2);stopped=False
        recovered=lookup(root,environment)
        require(any(d.get("effect")=="committed" for d in documents(recovered)),"original_public_key_did_not_recover_committed_child")
        after=consumer_record(root)
        require(after["child"]==child and after["state"]=="committed","recovery_replaced_original_child")
        private(root / "product-commit-before-mirror.json",encoded({"request_id":"native-accept","before_state":before["state"],"after_state":after["state"],"original_child_unchanged":True,"instrumentation":"fault_timing_only; recovery uses host-lookup original native key and selected public outcome/receipt"}))
        response=rpc(binary,owner,frame,environment,root)
        require(consumer_record(root)["child"]==child,"cold_replay_launched_another_mutator")
        return response
    finally:
        if process.poll() is None:
            if stopped: os.kill(process.pid,signal.SIGCONT)
            process.kill();process.wait(timeout=2)


def lookup(root: Path, environment: dict, *, original: bool = False) -> dict:
    config = json.loads((root / "input.json").read_text())
    event = json.loads((root / ("original-hook.json" if original else "last-hook.json")).read_text())
    completed = subprocess.run([config["binary"], "__trusted-specialist-host", "host-lookup",
                               "--owner-config", str(owner_config(root)),
                               "--session", event["session_id"], "--call", event["tool_use_id"]],
                              env=environment, capture_output=True, timeout=5)
    require(completed.returncode == 0, "original_native_key_lookup_failed")
    return json.loads(completed.stdout)


def hook(root: Path) -> None:
    config = json.loads((root / "input.json").read_text())
    body = sys.stdin.buffer.read(BODY + 1)
    require(len(body) <= BODY, "native_hook_input_outside_bound")
    event = json.loads(body)
    require(all(isinstance(event.get(k), str) and event[k] for k in ("session_id", "tool_use_id", "tool_name")), "native_hook_key_missing")
    # This is a private fixture journal, never a source of a manufactured key.
    path = root / "last-hook.json"
    with os.fdopen(os.open(path, os.O_CREAT | os.O_TRUNC | os.O_WRONLY, 0o600), "wb") as out:
        out.write(body)
    result = subprocess.run([config["binary"], "__trusted-specialist-host", "host-hook", "--owner-config", str(owner_config(root))], input=body, capture_output=True, timeout=8)
    if result.returncode==0:
        updated=json.loads(result.stdout).get("hookSpecificOutput",{}).get("updatedInput",{})
        if PRIVATE in updated:
            key=hashlib.sha256(encoded(updated[PRIVATE])).hexdigest()
            binding=root / ("carrier-"+key+".json")
            if not binding.exists(): private(binding,encoded({"call":event["tool_use_id"],"session":event["session_id"]}))
    sys.stdout.buffer.write(result.stdout)
    raise SystemExit(result.returncode)


def native_call_id(root: Path, frame: dict) -> str:
    params=frame["params"]
    if isinstance(params.get("_meta",{}).get("callId"),str): return params["_meta"]["callId"]
    cap=params.get("arguments",{}).get(PRIVATE)
    require(cap is not None,"native_call_carrier_missing")
    key=hashlib.sha256(encoded(cap)).hexdigest()
    binding=json.loads((root / ("carrier-"+key+".json")).read_bytes())
    return binding["call"]


def proxy(root: Path) -> None:
    config=json.loads((root / "input.json").read_text())
    binary,owner=Path(config["binary"]),owner_config(root)
    environment=dict(os.environ)
    backend=Backend(binary,owner,environment,root)
    handshake=[]
    try:
        for raw in sys.stdin.buffer:
            require(len(raw)<=BODY,"native_mcp_frame_outside_bound")
            frame=json.loads(raw);method=frame.get("method")
            if method in ("initialize","notifications/initialized"):
                handshake.append(raw.rstrip(b"\n"))
                reply=backend.exchange(raw)
                with os.fdopen(os.open(root / "native-handshake.jsonl",os.O_WRONLY|os.O_CREAT|os.O_TRUNC,0o600),"wb") as out: out.write(b"\n".join(handshake)+b"\n")
                if method=="initialize":
                    with os.fdopen(os.open(root / "production-initialize.json",os.O_CREAT|os.O_TRUNC|os.O_WRONLY,0o600),"wb") as out: out.write(encoded(reply))
                    with os.fdopen(os.open(root / "native-negotiations.jsonl",os.O_CREAT|os.O_APPEND|os.O_WRONLY,0o600),"ab") as out: out.write(encoded({"request":frame,"actual_reply":reply,"backend_pid":backend.process.pid})+b"\n")
                    sys.stdout.buffer.write(encoded(reply)+b"\n");sys.stdout.buffer.flush()
                continue
            if method!="tools/call":
                reply=backend.exchange(raw)
                if reply is not None: sys.stdout.buffer.write(encoded(reply)+b"\n");sys.stdout.buffer.flush()
                continue
            call_id=native_call_id(root,frame)
            row=config["scenario"];args=frame["params"]["arguments"]
            if row=="hookbypass":
                changed=copy.deepcopy(frame);changed["params"].get("_meta",{}).clear();changed["params"]["arguments"].pop(PRIVATE,None)
                response=backend.exchange(encoded(changed))
                require("error" in response or response.get("result",{}).get("isError") is True,"hook_bypass_dispatched")
            elif row in ("missingstate","corruptstate"):
                path=owner.with_name("invocations.sqlite");before=path.read_bytes()
                if row=="missingstate": path.rename(path.with_suffix(".held"))
                else: path.write_bytes(b"corrupt row fixture\n")
                try: response=backend.exchange(raw)
                finally:
                    if row=="missingstate": path.with_suffix(".held").rename(path)
                    else: path.write_bytes(before)
                require("error" in response or response.get("result",{}).get("isError") is True,"damaged_state_not_refused")
            else:
                if row=="beforeacceptance loss" and args.get("action")=="accept":
                    original=lookup(root,environment)
                    require(not any(d.get("effect")=="committed" for d in documents(original)),"preacceptance_has_effect")
                    private(root / "beforeacceptance-lookup.json",encoded(original))
                    old_pid=backend.process.pid
                    backend.process.kill();backend.process.wait(timeout=2)
                    backend.close();backend=Backend(binary,owner,environment,root);backend.replay_handshake()
                    private(root / "beforeacceptance-backend-cut.json",encoded({"old_pid":old_pid,"new_pid":backend.process.pid,"cut":"SIGKILL before original accept dispatch","original_effect_uncommitted":True}))
                if row=="afteracceptance/mirror-before loss" and args.get("action")=="accept":
                    response=cut_before_mirror(backend,frame,raw,binary,owner,environment,root)
                    backend.close();backend=Backend(binary,owner,environment,root);backend.replay_handshake()
                else: response=backend.exchange(raw)
                if row=="backendrestart" and args.get("operation"):
                    old_pid=backend.process.pid
                    backend.close();backend=Backend(binary,owner,environment,root);backend.replay_handshake()
                    repeated=backend.exchange(raw)
                    old=next((d.get("evidence") for d in documents(response) if d.get("schema")=="dolgorae.native-specialist-response/v1"),None)
                    new=next((d.get("evidence") for d in documents(repeated) if d.get("schema")=="dolgorae.native-specialist-response/v1"),None)
                    require(old and new and {k:v for k,v in old.items() if k!="selection_ref"}=={k:v for k,v in new.items() if k!="selection_ref"},"cold_reconstruction_changed_original_views")
                    private(root / "actual-backend-restart.json",encoded({"old_pid":old_pid,"new_pid":backend.process.pid,"original_views_unchanged":True}))
                if row=="replay" or (row=="Sloss/duplicate" and args.get("operation")=="close_external_engagement"):
                    if row=="Sloss/duplicate":
                        state_replay={"request":{k:v for k,v in args.items() if k!=PRIVATE},"first":response}
                        private(root / "state-replay.json",encoded(state_replay))
                    repeated=rpc(binary,owner,frame,environment,root)
                    if row=="Sloss/duplicate":
                        # Retain both real replies before a failed assertion can
                        # close the proxy and hide the original S disposition.
                        state_replay["replay"]=repeated
                        (root / "state-replay.json").write_bytes(encoded(state_replay))
                        first=next((d for d in documents(response) if d.get("schema")=="dolgorae.native-specialist-response/v1"),None)
                        require(first and first.get("ok") is True and first.get("facts",{}).get("engagement",{}).get("state")=="closed",
                                "original_state_close_did_not_complete")
                    require(repeated==response,"exact_delivery_replay_changed_result")
                if row=="concurrent replay":
                    with ThreadPoolExecutor(max_workers=4) as pool: replies=list(pool.map(lambda _:rpc(binary,owner,frame,environment,root),range(4)))
                    require(all(reply==response for reply in replies),"concurrent_replay_changed_result")
                if row=="conflict":
                    changed=copy.deepcopy(frame);changed["params"]["arguments"]["idempotency_key"]="different-original-input"
                    refused=backend.exchange(encoded(changed))
                    require("error" in refused or refused.get("result",{}).get("isError") is True,"changed_input_was_accepted")
                if row in ("aftermirror replyloss","physicalhostloss","exactsessionrecovery") and args.get("action")=="accept":
                    before=consumer_record(root);recovered=lookup(root,environment)
                    require(any(d.get("effect")=="committed" for d in documents(recovered)),"original_acceptance_not_recovered")
                    if not (root / "original-lookup.json").exists(): private(root / "original-lookup.json",encoded(recovered))
                    if row!="aftermirror replyloss" and not (root / "host-loss-resumed").exists():
                        private(root / "original-hook.json",(root / "last-hook.json").read_bytes())
                        private(root / "lost-tool-call.json",encoded({"native_call_id":call_id,"request_id":"native-accept","child":before["child"]}))
                        private(root / "host-loss-ready",b"ready\n")
                        while time.monotonic()<float((root / "deadline").read_text()): time.sleep(.01)
                        raise RuntimeError("host_loss_not_owned_by_parent")
                    response=rpc(binary,owner,frame,environment,root)
                    require(consumer_record(root)["child"]==before["child"],"lost_reply_replay_launched_another_mutator")
            with os.fdopen(os.open(root / "tool-replies.jsonl",os.O_CREAT|os.O_APPEND|os.O_WRONLY,0o600),"ab") as out:
                # No opaque carrier enters this observation. Native call ID is
                # sourced from actual metadata/authenticated hook output only.
                public_args={k:v for k,v in args.items() if k!=PRIVATE}
                out.write(encoded({"native_call_id":call_id,"request":public_args,"reply":response})+b"\n")
            sys.stdout.buffer.write(encoded(response)+b"\n");sys.stdout.buffer.flush()
    finally: backend.close()


def correlated_result(data: dict, host: str, call_id: str) -> tuple[list[str], bool | None]:
    """Only the current correlated tool-result carrier, never user/history text."""
    if host=="codex":
        matches=[v for v in data.get("input",[]) if isinstance(v,dict) and v.get("type")=="function_call_output" and v.get("call_id")==call_id]
        require(len(matches)==1,"current_correlated_function_output_missing")
        output=matches[0].get("output")
        require(isinstance(output,list) and all(isinstance(v,dict) and v.get("type")=="input_text" and isinstance(v.get("text"),str) for v in output),"selected_codex_content_items_changed")
        require(output and re.fullmatch(r"Wall time: [0-9]+\.[0-9]{4} seconds\nOutput:",output[0]["text"]),"selected_codex_wall_time_header_missing")
        return [v["text"] for v in output[1:]], None
    matches=[]
    for message in data.get("messages",[]):
        if message.get("role")!="user" or not isinstance(message.get("content"),list): continue
        matches.extend(v for v in message["content"] if isinstance(v,dict) and v.get("type")=="tool_result" and v.get("tool_use_id")==call_id)
    require(len(matches)==1,"current_correlated_claude_tool_result_missing")
    output=matches[0].get("content")
    if isinstance(output,str):
        require(matches[0].get("is_error") is True,"selected_claude_string_result_without_error")
        trailer=re.search(r"\n\n<system-reminder>\n<total_tokens>[0-9]+ tokens left</total_tokens>\n</system-reminder>$",output)
        if trailer: output=output[:trailer.start()]
        return [output],True
    require(isinstance(output,list) and all(isinstance(v,dict) and v.get("type")=="text" and isinstance(v.get("text"),str) for v in output),"selected_claude_text_blocks_changed")
    return [v["text"] for v in output],matches[0].get("is_error",False)


def expected_error() -> dict:
    # Outside-code known answer: a syntactically valid, absent private cursor
    # fails the existing selection lookup as invalid_input/decode/not_called.
    return {"schema":"dolgorae.consumer-control-response/v1","action":"inspect","ok":False,"request_id":None,
            "error":{"code":"invalid_input","phase":"decode","effect":"not_called","recovery":"fix_request"}}


class Responses:
    def __init__(self, root: Path, host: str):
        self.root, self.host = root, host
        self.posts = 0
        self.host_posts = 0
        self.executor_posts = 0
        self.count_posts = 0
        self.calls = 0
        self.failure = None
        self.originals = {}
        self.witnesses = []
        self.returns = []
        self.pending = {}
        self.cursors = {}
        self.coverage = set()
        self.lock = threading.Lock()
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args): pass
            def do_POST(self):
                try:
                    self.connection.settimeout(5)
                    require(self.headers.get("Transfer-Encoding") is None, "streaming_request_refused")
                    length = int(self.headers.get("Content-Length", "0"))
                    require(0 < length <= BODY, "http_input_outside_bound")
                    body = self.rfile.read(length)
                    with fixture.lock:
                        require(time.monotonic() <= float((root / "deadline").read_text()), "row_deadline_exceeded")
                        fixture.posts += 1
                        require(fixture.posts <= 8, "combined_host_executor_count_token_budget_exceeded")
                        data = json.loads(body)
                        require(PRIVATE.encode() not in body, "private_capability_in_next_http")
                        private(root / f"http-{fixture.posts}.json", body)
                        endpoint = self.path.split("?")[0]
                        if endpoint == "/v1/messages/count_tokens":
                            fixture.count_posts += 1
                            answer = b'{"input_tokens":1}'
                            self.send_response(200); self.send_header("Content-Type", "application/json"); self.send_header("Content-Length", str(len(answer))); self.end_headers(); self.wfile.write(answer); return
                        require(endpoint in ("/v1/responses", "/v1/messages", "/executor/v1/responses"), "unexpected_endpoint")
                        executor = endpoint == "/executor/v1/responses"
                        events = fixture.executor(data) if executor else fixture.host_reply(data)
                    self.send_response(200); self.send_header("Content-Type", "text/event-stream"); self.send_header("Connection", "close"); self.end_headers()
                    for event in events:
                        self.wfile.write(b"event: " + event["type"].encode() + b"\ndata: " + encoded(event) + b"\n\n")
                    self.wfile.flush()
                except (OSError, ValueError, RuntimeError) as error:
                    fixture.failure = str(error)
                    private(root / f"http-refusal-{fixture.posts}.json", encoded({"post":fixture.posts,"reason":str(error)}))
                    self.send_error(400, "bounded production fixture refused request")
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True); self.thread.start()

    @property
    def base_url(self): return f"http://127.0.0.1:{self.server.server_port}"

    @staticmethod
    def codex_events(item, number):
        return [{"type": "response.created", "response": {"id": f"resp_production_{number}"}}, {"type": "response.output_item.done", "item": item}, {"type": "response.completed", "response": {"id": f"resp_production_{number}", "usage": {"input_tokens": 0, "output_tokens": 0, "total_tokens": 0, "input_tokens_details": None, "output_tokens_details": None}}}]

    def executor(self, data):
        self.executor_posts += 1
        require(self.executor_posts <= 2, "executor_turn_budget_exceeded")
        if self.executor_posts == 1:
            if (self.root / "change-layout.json").is_file():
                layout = json.loads((self.root / "change-layout.json").read_text())
                program = "from pathlib import Path; p=Path(" + repr(layout["relative"]) + "); [x.unlink() for x in p.glob('before-*')]; [(p/f'after-{n:05}').write_bytes(b'') for n in range(10000)]"
                command = shlex.join([sys.executable, "-c", program]) + " && printf 'native-production-original-evidence\\n'"
            else:
                command = "cat file.txt"
            item = code_call(data, "executor-evidence", [("executor-evidence", "exec_command", {"cmd": command, "yield_time_ms": 10000})])
        else:
            code, output = terminal_output(code_results(data, "executor-evidence", ["executor-evidence"])["executor-evidence"])
            require(code == 0 and output == ("native-production-original-evidence\n" if (self.root / "change-layout.json").is_file() else 'original UTF8 evidence: "quoted" '), "executor_terminal_result_mismatch")
            if (self.root / "change-layout.json").is_file():
                text = 'Original final: "quoted" \\ slash\n界🙂\t' + "full original evidence\n" * 90
            else:
                verdict = {"summary": 'Original "quoted" evidence 界🙂 ' + "result " * 190, "findings": [], "criterion_assessments": [{"criterion_id": "C-1", "status": "met", "explanation": "Observed the fixture", "evidence": [{"basis": "context", "description": 'Checked "quoted" 界🙂 context', "path": None, "line_start": None, "line_end": None, "context_id": "requirement"}], "remaining_gap": None}], "evidence_limits": [], "overall_assessment": "requirements_met"}
                text = json.dumps(verdict, ensure_ascii=False, indent=2)
                self.originals["structured_output"]=json.dumps(verdict,ensure_ascii=False,sort_keys=True,separators=(",",":"))
            self.originals["final_response"] = text
            private(self.root / "executor-original.txt", text.encode())
            item = {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}
        return self.codex_events(item, self.posts)

    def verify_pending(self, data):
        if not self.pending: return
        rows=[json.loads(line) for line in (self.root / "tool-replies.jsonl").read_bytes().splitlines()]
        for call_id,request in self.pending.items():
            emitted=[v for v in rows if v["native_call_id"]==call_id]
            lost=self.root / "lost-tool-call.json"
            if (self.root / "host-loss-resumed").exists() and lost.is_file():
                loss=json.loads(lost.read_bytes())
                if loss["native_call_id"]==call_id:
                    require(not emitted and request.get("action")=="accept" and request.get("request_id")==loss["request_id"],"host_loss_did_not_withhold_original_accept_reply")
                    # No returned-byte credit: recovery must return a NEW,
                    # correlated status for the caller's original request ID.
                    continue
            require(len(emitted)==1,"actual_mcp_result_call_binding_missing_or_duplicate")
            require(emitted[0]["request"]==request,"actual_mcp_request_changed_before_delivery")
            texts,error_flag=correlated_result(data,self.host,call_id)
            if "error" in emitted[0]["reply"]:
                row=json.loads((self.root / "input.json").read_text())["scenario"]
                require(row in ("hookbypass","missingstate","corruptstate"),"unexpected_native_rpc_refusal")
                message=emitted[0]["reply"]["error"]["message"]
                require(isinstance(message,str) and any(message in text for text in texts),"correlated_native_rpc_refusal_lost")
                if error_flag is not None: require(error_flag is True,"native_rpc_error_flag_lost")
                self.witnesses.append({"post":self.posts,"call_id":call_id,"tag":"rpc_refusal","message":message})
                continue
            content=emitted[0]["reply"].get("result",{}).get("content")
            require(isinstance(content,list) and content and all(v.get("type")=="text" for v in content),"actual_mcp_text_result_missing")
            # The selected Claude image appends this separate host reminder to
            # the tool_result. It is never source content or witness credit.
            if self.host == "claude" and len(texts) == len(content)+1:
                require(re.fullmatch(r"<system-reminder>\n<total_tokens>[0-9]+ tokens left</total_tokens>\n</system-reminder>",texts[-1]),"unexpected_native_result_trailer")
                texts=texts[:-1]
                require(texts[-1].endswith("\n"),"selected_claude_reminder_separator_missing")
                texts[-1]=texts[-1][:-1]
            require(texts and len(texts)==len(content),"native_result_block_count_changed")
            if self.host == "claude":
                require(all(text == block["text"] for text,block in zip(texts,content)),"selected_claude_text_framing_changed")
            metadata=json.loads(content[0]["text"])
            require(content[0]["text"].isascii() and texts[0]==content[0]["text"],"native_ascii_metadata_changed")
            self.returns.append(metadata)
            if request.get("action")=="status" and request.get("target")=={"kind":"request","request_id":"native-accept"}:
                original=json.loads((self.root / "original-lookup.json").read_bytes())
                prior=next((d for d in documents(original) if d.get("schema")=="dolgorae.consumer-control-response/v1" and d.get("data",{}).get("request_id")=="native-accept"),None)
                require(prior and metadata.get("ok") is True and metadata.get("data",{}).get("effect")=="committed","resumed_original_acceptance_not_recovered")
                require(all(metadata["data"].get(k)==prior["data"].get(k) for k in ("outcome","receipt")),"resumed_original_receipt_changed")
                self.coverage.add("recovered_acceptance_status")
                self.witnesses.append({"post":self.posts,"call_id":call_id,"tag":"recovered_acceptance_status","sha256":hashlib.sha256(texts[0].encode()).hexdigest()})
            if request.get("action")=="inspect":
                bad=request.get("range",{}).get("cursor")=="missing-cursor"
                if bad:
                    require(metadata==expected_error() and len(content)==1,"wrong_emitted_error_known_answer")
                    if error_flag is not None: require(error_flag is True,"native_error_flag_lost")
                    tag="error:inspect"
                    witness_bytes=texts[0].encode()
                else:
                    view=request["view"]["kind"]
                    require(view in self.originals,"independent_fixture_original_missing")
                    source=self.originals[view].encode()
                    selection=request["selection_ref"]
                    span=request["range"]
                    if span["kind"]=="start": offset=span["offset"]
                    else:
                        cursor=self.cursors.get(span["cursor"])
                        require(cursor and cursor["selection"]==selection and cursor["view"]==view,"continuation_not_bound_to_verified_original")
                        offset=cursor["offset"]
                    end=min(len(source),offset+1024)
                    while True:
                        try: original=source[offset:end].decode();break
                        except UnicodeDecodeError: end-=1
                    require(original and len(content)==2 and content[1]["text"]==original,"wrong_emitted_original_page")
                    require(texts[1].encode()==source[offset:end],"native_original_page_bytes_changed")
                    page=metadata.get("data",{})
                    require(metadata.get("ok") is True and metadata.get("action")=="inspect" and page.get("kind")=="page" and "text" not in page,"native_page_header_shape_changed")
                    require(page.get("encoding")=="utf8" and page.get("source_digest")=="sha256:"+hashlib.sha256(source).hexdigest() and page.get("source_length")==len(source)
                            and page.get("offset")==offset and page.get("returned_length")==end-offset,"native_page_metadata_not_bound_to_independent_original")
                    if end<len(source):
                        require(isinstance(page.get("continuation"),str) and page["continuation"],"required_original_continuation_missing")
                        self.cursors[page["continuation"]]={"selection":selection,"view":view,"offset":end}
                    else: require(page.get("continuation") is None,"invented_original_continuation")
                    if error_flag is not None: require(error_flag is False,"native_success_flag_lost")
                    tag=view+":"+span["kind"]
                    witness_bytes=source[offset:end]
                self.coverage.add(tag)
                self.witnesses.append({"post":self.posts,"call_id":call_id,"tag":tag,"bytes":len(witness_bytes),"sha256":hashlib.sha256(witness_bytes).hexdigest()})
        self.pending.clear()

    def host_reply(self, data):
        self.host_posts += 1
        require(self.host_posts <= 5, "host_message_budget_exceeded")
        self.verify_pending(data)
        native=[d for d in self.returns if d.get("schema")=="dolgorae.native-specialist-response/v1"]
        controls=[d for d in self.returns if d.get("schema")=="dolgorae.consumer-control-response/v1"]
        config = json.loads((self.root / "input.json").read_text())
        row = config["scenario"]
        prepared = json.loads((self.root / "prepared.json").read_text())
        call = None
        evidence = None
        tool = "specialist_native"
        resumed=(self.root / "host-loss-resumed").exists()
        if resumed:
            require((self.root / "original-lookup.json").is_file(), "original_recovery_missing")
            tool="specialist_consumer"
            if "recovered_acceptance_status" not in self.coverage:
                call={"schema":CONTROL,"action":"status","target":{"kind":"request","request_id":"native-accept"}}
        elif prepared["requires_result"]:
            assigned = json.loads((self.root / "assigned.json").read_text())
            if self.host_posts == 1:
                call = {"schema": SCHEMA, "operation": "collect_external_specialist_task", "scope_kind": "external_engagement", **{k: assigned[k] for k in ("scope_ref", "member_ref", "task_ref")}}
            else:
                evidence = next((d["evidence"] for d in reversed(native) if d.get("evidence")), None)
                require(evidence, "native_collect_did_not_select_original_result")
                tool = "specialist_consumer"
                if self.host_posts == 2:
                    call = {"schema": CONTROL, "action": "inspect", "selection_ref": evidence["selection_ref"], "view": {"kind": "final_response"}, "range": {"kind": "start", "offset": 0}}
                elif self.host_posts == 3:
                    if row == "reconnect":
                        call = None
                    elif row in ("physicalhostloss","exactsessionrecovery"):
                        require("final_response:start" in self.coverage,"accept_before_primary_received_original_evidence")
                        require(any(d.get("action")=="prepare_accept" and d.get("ok") is True for d in controls),"accept_preparation_failed")
                        call={"schema":CONTROL,"action":"accept","request_id":"native-accept","decision":"accept"}
                    elif row in DELEGATED:
                        call = {"schema": CONTROL, "action": "prepare_accept", "request_id": "native-accept", "evidence_ref": evidence["selection_ref"], "summary": "Primary inspected original result and applicable evidence."}
                    else:
                        page = next((d["data"] for d in reversed(controls) if d.get("data", {}).get("kind") == "page"), None)
                        require(page and page.get("continuation"), "original_continuation_missing")
                        call = {"schema": CONTROL, "action": "inspect", "selection_ref": evidence["selection_ref"], "view": {"kind": "final_response"}, "range": {"kind": "continue", "cursor": page["continuation"]}}
                elif self.host_posts == 4:
                    if row == "reconnect":
                        page=next((d["data"] for d in reversed(controls) if d.get("data",{}).get("kind")=="page"),None)
                        require(page and page.get("continuation"),"reconnect_original_continuation_missing")
                        call={"schema":CONTROL,"action":"inspect","selection_ref":evidence["selection_ref"],"view":{"kind":"final_response"},"range":{"kind":"continue","cursor":page["continuation"]}}
                    elif row == "Sloss/duplicate":
                        tool = "specialist_native"
                        revision = json.loads((self.root / "settled.json").read_text())["engagement_revision"]
                        call = {"schema":SCHEMA,"operation":"close_external_engagement","engagement_ref":assigned["scope_ref"],"expected_revision":revision,"allow_interrupt":False}
                    elif row in DELEGATED:
                        require(any(d.get("action") == "prepare_accept" and d.get("ok") is True for d in controls), "accept_preparation_failed")
                        call = {"schema": CONTROL, "action": "accept", "request_id": "native-accept", "decision": "accept"}
                    elif row == "backendrestart":
                        call={"schema":CONTROL,"action":"inspect","selection_ref":evidence["selection_ref"],"view":{"kind":"final_response"},"range":{"kind":"continue","cursor":"missing-cursor"}}
                    else:
                        call = {"schema": CONTROL, "action": "inspect", "selection_ref": evidence["selection_ref"], "view": {"kind": "structured_output"}, "range": {"kind": "start", "offset": 0}}
        elif self.host_posts == 1 or (row == "distinct equal-text" and self.host_posts == 2):
            call = copy.deepcopy(prepared["initial_request"])
            if row=="distinct equal-text" and self.host_posts==2: call["idempotency_key"]="native-open-2"
        if call is not None:
            self.calls += 1
            call_id=("call_native_" if self.host=="codex" else "toolu_native_")+str(self.calls)
            self.pending[call_id]=copy.deepcopy(call)
            selected = native_tool(data.get("tools", []), tool)
            require(selected, "production_tool_not_advertised")
            if self.host == "codex":
                item = {"type": "function_call", "id": f"fc_native_{self.calls}", "call_id": f"call_native_{self.calls}", "name": selected[0], "arguments": json.dumps(call)}
                if selected[1]: item["namespace"] = selected[1]
                events=self.codex_events(item,self.posts)
                for extra in self.extra_calls(data,row,evidence if prepared["requires_result"] else None):
                    self.calls+=1;extra_id="call_native_"+str(self.calls);self.pending[extra_id]=copy.deepcopy(extra)
                    name,namespace=native_tool(data.get("tools",[]),"specialist_consumer")
                    extra_item={"type":"function_call","id":"fc_native_"+str(self.calls),"call_id":extra_id,"name":name,"arguments":json.dumps(extra)}
                    if namespace: extra_item["namespace"]=namespace
                    events.insert(-1,{"type":"response.output_item.done","item":extra_item})
                return events
            block = {"type": "tool_use", "id": f"toolu_native_{self.calls}", "name": selected[0], "input": {}}
            delta = {"type": "input_json_delta", "partial_json": json.dumps(call)}
        else:
            if prepared["requires_result"]:
                require(self.witnesses, "next_http_original_bytes_unproved")
            if self.host == "codex":
                return self.codex_events({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Production fixture complete."}]}, self.posts)
            block, delta = {"type": "text", "text": ""}, {"type": "text_delta", "text": "Production fixture complete."}
        return self.claude_events(data, block if call is not None else None, delta if call is not None else None)

    def extra_calls(self,data,row,evidence):
        if row in ("physicalhostloss","exactsessionrecovery") and self.host_posts==2:
            require(evidence,"original_evidence_selection_missing")
            return [{"schema":CONTROL,"action":"prepare_accept","request_id":"native-accept","evidence_ref":evidence["selection_ref"],"summary":"Primary inspected original result and applicable evidence."}]
        if row=="exposurecanary":
            if self.host_posts==1: return [{"schema":CONTROL,"action":"status","target":{"kind":"context"}}]
            if self.host_posts==2:
                context=next((v["data"]["context_ref"] for v in reversed(self.returns) if v.get("action")=="status" and v.get("data",{}).get("mode")=="delegated"),None)
                require(context and evidence,"record_context_or_original_evidence_missing")
                return [{"schema":CONTROL,"action":"prepare_record","request_id":"native-record","context_ref":context,"evidence_ref":evidence["selection_ref"],"operations":[{"item_id":"result","record":{"type":"text","value":"Original native evidence inspected."}}]}]
            if self.host_posts==3:
                require(any(v.get("action")=="prepare_record" and v.get("ok") is True for v in self.returns),"record_preparation_missing")
                return [{"schema":CONTROL,"action":"record","request_id":"native-record","decision":"record"}]
        if self.host_posts!=4 or row not in ("exposurecanary","reconnect"): return []
        require(evidence,"original_evidence_selection_missing")
        selection=evidence["selection_ref"]
        extra=[]
        if row=="reconnect": extra.append({"schema":CONTROL,"action":"inspect","selection_ref":selection,"view":{"kind":"structured_output"},"range":{"kind":"start","offset":0}})
        extra.append({"schema":CONTROL,"action":"inspect","selection_ref":selection,"view":{"kind":"final_response"},"range":{"kind":"continue","cursor":"missing-cursor"}})
        return extra

    def claude_events(self, data, block, delta):
        call = block is not None
        if block is None:
            block, delta = {"type":"text","text":""}, {"type":"text_delta","text":"Production fixture complete."}
        events=[{"type":"message_start","message":{"id":"msg_native_"+str(self.posts),"type":"message","role":"assistant","model":data.get("model"),"content":[],"stop_reason":None,"stop_sequence":None,"usage":{"input_tokens":1,"output_tokens":0}}}]
        pairs=[(block,delta)]
        config=json.loads((self.root / "input.json").read_text())
        evidence=next((v.get("evidence") for v in reversed(self.returns) if v.get("evidence")),None)
        if call:
            for extra in self.extra_calls(data,config["scenario"],evidence):
                self.calls+=1;extra_id="toolu_native_"+str(self.calls);self.pending[extra_id]=copy.deepcopy(extra)
                name,_=native_tool(data.get("tools",[]),"specialist_consumer")
                pairs.append(({"type":"tool_use","id":extra_id,"name":name,"input":{}},{"type":"input_json_delta","partial_json":json.dumps(extra)}))
        for index,(content,change) in enumerate(pairs):
            events.extend([{"type":"content_block_start","index":index,"content_block":content},{"type":"content_block_delta","index":index,"delta":change},{"type":"content_block_stop","index":index}])
        events.extend([{"type":"message_delta","delta":{"stop_reason":"tool_use" if call else "end_turn","stop_sequence":None},"usage":{"output_tokens":1}},{"type":"message_stop"}])
        return events


    def close(self):
        self.server.shutdown(); self.server.server_close(); self.thread.join(timeout=3)
        require(not self.thread.is_alive(), "loopback_thread_not_joined")


def run_phase(driver: Path, mode: str, root: Path, environment: dict, deadline: float) -> dict:
    name=mode.removeprefix("--")+"-"+str(time.monotonic_ns())
    try:
        completed = subprocess.run([str(driver), mode, str(root)], env=environment, capture_output=True,
                                   timeout=max(.1, deadline - time.monotonic()))
    except subprocess.TimeoutExpired as error:
        private(root / (name+".stdout"),error.stdout or b"")
        private(root / (name+".stderr"),error.stderr or b"")
        raise
    private(root / (name+".stdout"), completed.stdout)
    private(root / (name+".stderr"), completed.stderr)
    require(completed.returncode == 0, "production_fixture_" + mode + "_failed")
    return json.loads(completed.stdout)


def podway_fixture(root: Path, executable: Path, digest: str, candidate: dict, environment: dict, deadline: float,
                   *, adapter_id: str = "dolgorae.external/1", adapter_version: str | None = None) -> None:
    require(hashlib.sha256(executable.read_bytes()).hexdigest() == digest, "selected_podway_executable_changed")
    worktree = root / "workspace"
    setup_calls = 0
    def command(*args):
        nonlocal setup_calls
        setup_calls += 1
        selection = [] if args[0] in ("procedure", "version") else ["--worktree", str(worktree)]
        argv = [str(executable), "--contract-generation", "1", *selection, "--json", *args]
        result = subprocess.run(argv, cwd=worktree, env=environment, capture_output=True, timeout=max(.1, deadline-time.monotonic()))
        private(root / f"podway-setup-{setup_calls}.json", encoded({"argv":argv,"exit":result.returncode,"stdout":result.stdout.decode(),"stderr":result.stderr.decode()}))
        require(result.returncode == 0, "selected_public_podway_setup_failed")
        return json.loads(result.stdout)
    command("init", "--idempotency-key", "fixture-init")
    procedures = worktree / ".podway/procedures"
    procedures.mkdir(mode=0o700, parents=True, exist_ok=True)
    path = procedures / "native-host.yaml"
    # The confirmation records fixture setup, not the Specialist's conclusion.
    source = f'''schema: podway.procedure/v2
id: native-host
version: "1"
name: Native host original result
purpose: Inspect the original delegated result before explicit acceptance.
node_definitions:
  work:
    type: action
    title: Inspect original result
    intent: Decide from original evidence.
    items:
      - id: setup
        type: confirm
        prompt: Confirm the isolated fixture has been prepared.
        required: true
      - id: result
        type: text
        prompt: Record the inspected original result.
        required: false
        min_length: 0
        max_length: 256
        multiline: true
graph:
  entry: work
  nodes:
    - id: work
      use: work
      terminal: true
      delegation:
        binding_ref: local-specialist
        role_ref: general
        role_snapshot_digest: {candidate['role_snapshot_digest']}
        max_rounds: 3
        after_accept: retain
'''
    private(path, source.encode())
    relative = str(path.relative_to(worktree))
    preview = command("procedure", "preview", relative)
    command("start", "--procedure", relative, "--expect-procedure-digest", preview["result"]["procedure_digest"],
            "--task", "Inspect original native result", "--idempotency-key", "fixture-start")
    command("begin", "--idempotency-key", "fixture-begin")
    command("check", "setup", "--idempotency-key", "fixture-confirm-setup")
    package = command("delegation", "package")
    status = command("status")
    version = command("version", "--identity")
    private(root / "package.json", encoded(package))
    connection = {"executable": str(executable), "executable_sha256": "sha256:"+digest,
                  "manifest_digest": version["result"]["contract_manifest_digest"], "worktree": str(worktree),
                  "workspace": status["workspace"]["uuid"], "session": status["result"]["session"]["id"],
                  "attempt": status["result"]["current"]["attempt"]["attempt_id"], "placement": "work",
                  "adapter_id": adapter_id, "adapter_version": adapter_version if adapter_version is not None else json.loads((root / "input.json").read_text())["adapter_version"],
                  "permitted_items": ["setup", "result"]}
    private(root / "connection.json", encoded(connection))


def qualify(args, host: str, row: str) -> dict:
    require(os.environ.get(OPT_IN) == "1", "explicit_production_host_opt_in_required")
    executable = getattr(args, host).resolve(strict=True)
    version, digest, route = PINS[host]
    require(hashlib.sha256(executable.read_bytes()).hexdigest() == digest, "selected_native_image_changed")
    root = Path(tempfile.mkdtemp(prefix=f"t077-host-{host}-{ROWS.index(row):02d}-", dir=args.scratch)).resolve()
    root.chmod(0o700)
    for name in ("home", "codex", "claude", "tmp", "config", "cache", "workspace", "profile"):
        (root / name).mkdir(mode=0o700)
    private(root / "fixture-owned", b"native-production-host/v1\n")
    private(root / "delivery-count", b"0")
    deadline = time.monotonic()+90
    private(root / "deadline", str(deadline).encode())
    environment = host_environment(root)
    environment[OPT_IN] = "1"
    result = subprocess.run([str(executable), "--version"], env=environment, capture_output=True, text=True, timeout=5)
    require(result.returncode == 0 and result.stdout.strip() == version, "selected_native_version_changed")
    binary = args.binary.resolve(strict=True)
    binary_version = subprocess.run([str(binary), "version", "--json"], env=environment, capture_output=True, timeout=5)
    require(binary_version.returncode == 0, "product_version_failed")
    product_version = json.loads(binary_version.stdout)["version"].removeprefix("v")
    config = {"host":host, "host_identity":{"path":str(executable),"version":"0.164.0" if host == "codex" else "2.1.310", "sha256":digest},
              "route":route,"scenario":row,"binary":str(args.driver.resolve(strict=True)),"reference_binary":str(binary),"codex":str(args.codex.resolve(strict=True)),
              "executor_model":args.executor_model,"host_model":getattr(args,host+"_model"),"adapter_version":product_version,
              "trace_sha256":hashlib.sha256(encoded(trace(row))).hexdigest()}
    private(root / "input.json", encoded(config))
    fixture = Responses(root,host)
    passed = False
    observations = None
    original_failure = None
    try:
        private(root / "profile/config.toml", (f'model = {json.dumps(args.executor_model)}\nmodel_reasoning_effort = "low"\n'
                'model_provider = "production_executor"\ncli_auth_credentials_store = "file"\n'
                '[features]\nplugins = false\n[model_providers.production_executor]\nname = "Numeric loopback executor"\n'
                f'base_url = "{fixture.base_url}/executor/v1"\nwire_api = "responses"\nrequires_openai_auth = false\nsupports_websockets = false\nrequest_max_retries = 0\nstream_max_retries = 0\n').encode())
        prepared = run_phase(args.driver, "--prepare", root, environment, deadline)
        require(prepared["dolgorae_version"] == product_version,"fixture_and_reference_product_versions_differ")
        if prepared["requires_result"]:
            if row in CONNECTED:
                podway_fixture(root,args.podway.resolve(strict=True),args.podway_sha256,prepared["candidate"],environment,deadline)
            run_phase(args.driver,"--assign",root,environment,deadline)
            observed=run_phase(args.driver,"--observe",root,environment,deadline)
            require(observed.get("task",{}).get("state")=="completed","actual_producer_did_not_complete")
            complete=observed.get("complete_result")
            require(isinstance(complete,dict) and complete.get("terminal_outcome")=="completed", "actual_producer_did_not_complete")
            if row in ("Rfreshobservation","backendrestart"):
                require(len(complete.get("changes",[])) == 20000,"actual_twenty_thousand_change_producer_missing")
            private(root / "settled.json",encoded({"engagement_revision":observed.get("engagement_revision")}))
            private(root / "producer-observation.json", encoded({"result_bytes":len(encoded(complete)),"change_count":len(complete.get("changes",[])),"result_digest":observed["task"].get("result_digest")}))
        script=Path(__file__).resolve()
        hook_command=shlex.join([sys.executable,str(script),"--hook",str(root)])
        hooks={"hooks":{"PreToolUse":[{"matcher":"^mcp__dolgorae__.*$","hooks":[{"type":"command","command":hook_command,"timeout":10}]}]}}
        private(root / "settings.json",encoded(hooks))
        proxy_args=[str(script),"--proxy",str(root)]
        if host == "codex":
            private(root / "codex/hooks.json",encoded(hooks))
            host_config=(f'model = {json.dumps(args.codex_model)}\nmodel_provider = "production_host"\ncli_auth_credentials_store = "file"\n'
                '[features]\nhooks = true\n[model_providers.production_host]\nname = "Numeric loopback host"\n'
                f'base_url = "{fixture.base_url}/v1"\nwire_api = "responses"\nrequires_openai_auth = false\nsupports_websockets = false\nrequest_max_retries = 0\nstream_max_retries = 0\n'
                '[mcp_servers.dolgorae]\n'+f'command = {json.dumps(sys.executable)}\nargs = {json.dumps(proxy_args)}\n'
                f'env = {{ {OPT_IN} = "1" }}\n'
                '[mcp_servers.dolgorae.tools.specialist_native]\napproval_mode = "approve"\n'
                '[mcp_servers.dolgorae.tools.specialist_consumer]\napproval_mode = "approve"\n')
            private(root / "codex/config.toml",host_config.encode())
            argv=[str(executable),"--no-daemon","--dangerously-bypass-hook-trust","-a","never","exec","--skip-git-repo-check","--sandbox","read-only","--json","--cd",str(root / "workspace"),"Execute the bounded production fixture calls."]
        else:
            environment.update(ANTHROPIC_BASE_URL=fixture.base_url,ANTHROPIC_API_KEY="sk-ant-fixture-not-a-credential")
            private(root / "mcp.json",encoded({"mcpServers":{"dolgorae":{"type":"stdio","command":sys.executable,"args":proxy_args}}}))
            argv=[str(executable),"--restricted","--setting-sources","","--settings",str(root / "settings.json"),"--strict-mcp-config","--mcp-config",str(root / "mcp.json"),"--tools","","--permission-mode","dontAsk","--permission-prompts","none","--model",args.claude_model,"-p","--output-format","stream-json","--verbose","Execute the bounded production fixture calls."]
        private(root / "selected-launch.json",encoded({"argv":argv,"model":config["host_model"],"route":route,"image":config["host_identity"],"protocol":"actual tools/list and native metadata retained in private frames"}))
        crash=row in ("physicalhostloss","exactsessionrecovery")
        output=run_host(argv,root,environment,"initial",deadline,crash_marker=root / "host-loss-ready" if crash else None,
                        require_claude_journal=crash and host=="claude")
        session=native_session(output,host)
        if row=="reconnect":
            resume=([str(executable),"--no-daemon","--dangerously-bypass-hook-trust","-a","never","exec","resume","--skip-git-repo-check","--json",session,"Continue the original bounded evidence inspection."] if host=="codex" else argv[:-1]+["--resume",session,"Continue the original bounded evidence inspection."])
            resumed=run_host(resume,root,environment,"resumed",deadline)
            require(native_session(resumed,host)==session,"reconnect_replaced_native_session")
        if crash:
            original=json.loads((root / "original-lookup.json").read_text())
            require(lookup(root,environment,original=True)==original,"original_key_changed_before_resume")
            private(root / "host-loss-resumed", b"same original session and key\n")
            resume=([str(executable),"--no-daemon","--dangerously-bypass-hook-trust","-a","never","exec","resume","--skip-git-repo-check","--json",session,"Observe the original bounded result."] if host=="codex" else argv[:-1]+["--resume",session,"Observe the original bounded result."])
            resumed=run_host(resume,root,environment,"resumed",deadline)
            require(native_session(resumed,host)==session,"exact_native_session_was_replaced")
            require(lookup(root,environment,original=True)==original,"original_key_changed_after_resume")
            require(consumer_record(root)["child"]==json.loads((root / "lost-tool-call.json").read_bytes())["child"],"resumed_status_replaced_original_child")
        require(fixture.failure is None,"actual_loopback_rejected: "+str(fixture.failure))
        require(fixture.host_posts>=2,"next_model_http_missing")
        require(not fixture.pending,"current_tool_result_not_returned_to_model")
        required=set()
        if prepared["requires_result"]: required.add("final_response:start")
        if row in ("exposurecanary","reconnect","backendrestart","Sloss/duplicate"): required.add("final_response:continue")
        if row in ("exposurecanary","reconnect"): required.update(("structured_output:start","error:inspect"))
        if row=="backendrestart": required.add("error:inspect")
        if crash: required.add("recovered_acceptance_status")
        require(required<=fixture.coverage,"planned_current_call_original_witness_coverage_missing: "+str(sorted(required-fixture.coverage)))
        final_observation=run_phase(args.driver,"--observe",root,environment,deadline)
        if not prepared["requires_result"]:
            expected=0 if row in ("missingstate","corruptstate","hookbypass") else 2 if row=="distinct equal-text" else 1
            require(final_observation["accepted_operations"]==expected,"native_K_effect_count_wrong")
        if row=="backendrestart": require((root / "actual-backend-restart.json").is_file(),"persistent_backend_was_not_restarted")

        if row=="exposurecanary":
            require(any(d.get("action")=="record" and d.get("ok") is True and d.get("data",{}).get("effect")=="committed" for d in fixture.returns),"actual_public_record_outcome_unproved")
        if row in DELEGATED:
            action="status" if crash else "accept"
            require(any(d.get("action")==action and d.get("request_id")=="native-accept" and d.get("ok") is True and d.get("data",{}).get("effect")=="committed" for d in fixture.returns),"actual_public_acceptance_receipt_unproved")
        require(row!="beforeacceptance loss" or (root / "beforeacceptance-backend-cut.json").is_file(),"actual_preacceptance_cut_unproved")
        require(row!="afteracceptance/mirror-before loss" or (root / "product-commit-before-mirror.json").is_file(),"actual_mirror_before_cut_unproved")
        for path in root.glob("*-std*.local"):
            require(PRIVATE.encode() not in path.read_bytes(),"private_capability_exposed_in_native_output")
        observations={"host":host,"row":row,"passed":True,"version":version,"executable_sha256":digest,"binary_sha256":hashlib.sha256(Path(config["binary"]).read_bytes()).hexdigest(),"reference_binary_sha256":hashlib.sha256(binary.read_bytes()).hexdigest(),
                      "posts":fixture.posts,"host_posts":fixture.host_posts,"executor_posts":fixture.executor_posts,"count_token_posts":fixture.count_posts,
                      "deliveries":int((root / "delivery-count").read_text()),"next_http_original_witnesses":fixture.witnesses,"required_witness_tags":sorted(required),"native_session":session,
                      "input_sha256":hashlib.sha256((root / "input.json").read_bytes()).hexdigest(),"trace_sha256":config["trace_sha256"],"owned_fixture":str(root)}
        passed=True
        return observations
    except Exception as error:
        original_failure=type(error).__name__+": "+str(error)
        raise
    finally:
        cleanup_deadline=time.monotonic()+30
        cleanup_error = None
        close_error = None
        try:
            cleanup=run_phase(args.driver,"--cleanup",root,environment,cleanup_deadline)
            require(cleanup.get("cleanup_complete") is True,"owned_product_cleanup_incomplete")
            if observations is not None: observations["cleanup_complete"]=True
        except Exception as error:
            cleanup_error=error
            passed=False
            if observations is not None: observations["cleanup_complete"]=False
        try:
            fixture.close()
        except Exception as error:
            close_error=error
            passed=False
        private(root / "row-result.json",encoded({"passed":passed,"observations":observations,"original_failure":original_failure,"cleanup_failure":str(cleanup_error) if cleanup_error else None,"loopback_close_failure":str(close_error) if close_error else None,"loopback_failure":fixture.failure,"retained_private_fixture":str(root)}))
        if original_failure is None and (cleanup_error or close_error):
            raise cleanup_error or close_error


def main() -> int:
    if len(sys.argv)==3 and sys.argv[1] in ("--hook","--proxy"):
        root=Path(sys.argv[2]).resolve(strict=True)
        require(os.environ.get(OPT_IN)=="1","private_row_operation_requires_opt_in")
        (hook if sys.argv[1]=="--hook" else proxy)(root)
        return 0
    parser=argparse.ArgumentParser()
    parser.add_argument("--trace-only",action="store_true")
    parser.add_argument("--binary",type=Path)
    parser.add_argument("--driver",type=Path)
    parser.add_argument("--codex",type=Path)
    parser.add_argument("--claude",type=Path)
    parser.add_argument("--podway",type=Path)
    parser.add_argument("--podway-sha256",default="4ff3c8d3162d9ff05f434101add9c7b81abfa04dd522da7c7016a0033ac0a461")
    parser.add_argument("--codex-model")
    parser.add_argument("--claude-model")
    parser.add_argument("--executor-model")
    parser.add_argument("--scratch",type=Path)
    args=parser.parse_args()
    if args.trace_only:
        print(json.dumps({"schema":"dolgorae.production-host-traces/v1","executed":False,"rows":[{"host":host,**trace(row)} for host in PINS for row in ROWS]},sort_keys=True))
        return 0
    require(os.environ.get(OPT_IN)=="1","explicit_production_host_opt_in_required")
    require(all(getattr(args,key) for key in ("binary","driver","codex","claude","podway","codex_model","claude_model","executor_model","scratch")),"exact_selected_inputs_required")
    args.scratch=args.scratch.resolve(strict=True)
    results=[]
    # Serial keep-going: a failed row never supplies another row's evidence.
    for host in PINS:
        for row in ROWS:
            try: results.append(qualify(args,host,row))
            except (OSError,ValueError,RuntimeError,subprocess.SubprocessError) as error:
                results.append({"host":host,"row":row,"passed":False,"failure":str(error)})
    print(json.dumps({"schema":"dolgorae.production-native-host/v1","passed":all(r["passed"] for r in results),"rows":results},sort_keys=True))
    return 0 if all(r["passed"] for r in results) else 1


if __name__=="__main__":
    raise SystemExit(main())
