#!/usr/bin/env python3
"""Original eighteen production host scenarios, with next-HTTP byte witnesses.

Execution is opt-in. Trace emission and offline checks do not qualify a host.
All fixture state, transcripts and subprocess groups belong to one private row.
"""
from __future__ import annotations

import argparse
import copy
import fcntl
import gzip
import io
import hashlib
import json
import os
import re
import selectors
from pathlib import Path
import shlex
import signal
import stat
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from run_native_host_invocation_fixture import ProbeFailure, host_environment, native_session, run_host as _run_host
from run_native_workspace_policy_fixture import ProbeCheckFailure, code_call, code_results, terminal_output

OPT_IN = "DOLGORAE_RUN_NATIVE_PRODUCTION_HOST"
UNSUPPORTED_PEER_SHA256 = "249685e65402049ecad83f1ecef8efee361f6ac979bde686a9ebabd10c685cf2"
UNSUPPORTED_PEER_MANIFEST = "sha256:57cb8463f0f24033f9bd0437ed1943e77b21b754a888e654bd847bec13f4b91d"
UNSUPPORTED_PEER_SOURCE = "e9f4b41d1d09573b7acd443b91fbfb8f378fb8e1"
BODY = 4 * 1024 * 1024
ORIGINAL_ROWS = ("initialK", "replay", "conflict", "concurrent replay", "distinct equal-text",
        "reconnect", "backendrestart", "beforeacceptance loss", "afteracceptance/mirror-before loss",
        "aftermirror replyloss", "physicalhostloss", "exactsessionrecovery", "missingstate",
        "corruptstate", "exposurecanary", "Rfreshobservation", "Sloss/duplicate", "hookbypass")
CONSUMER_ROWS = ("consumer-complete", "consumer-rounds", "consumer-release", "consumer-group", "consumer-group-gaps", "consumer-fences")
ROWS = (*ORIGINAL_ROWS, *CONSUMER_ROWS)
PINS = {
    "codex": ("codex-cli 0.164.0", "d8990e3ee7acfbc26f91265b12a1a83f0fee40d0e0f8f1f40a37d4ec21f499cf", "codex-0.160.0"),
    "claude": ("2.1.310 (Claude Code)", "ffb64331bc0ee1b69efb7c2c428bd3ab516bc31b75bf60886b356c97c1c25ffd", "claude-code-2.1.295"),
}
DELEGATED = ("Rfreshobservation", "beforeacceptance loss", "afteracceptance/mirror-before loss", "aftermirror replyloss", "physicalhostloss", "exactsessionrecovery")
RESULT_ROWS = (*CONSUMER_ROWS, "reconnect", "backendrestart", "exposurecanary", "Sloss/duplicate", *DELEGATED)
CONNECTED = (*DELEGATED, "exposurecanary", *CONSUMER_ROWS)
SCHEMA = "dolgorae.external-specialist-request/v5"
CONTROL = "dolgorae.consumer-control/v1"
PRIVATE = "_dolgorae_host_binding"
PACKAGE_EVIDENCE = 'Exact source evidence: "quoted" 界🙂\n' * 60
OWNERLESS_CANARY = "task086-ownerless-protected-question-canary"


def encoded(value) -> bytes:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()


def private(path: Path, body: bytes) -> None:
    with os.fdopen(os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600), "wb") as out:
        out.write(body)


def require(value, reason: str) -> None:
    if not value:
        raise RuntimeError(reason)


def checked_permission_decline(value):
    def unique(pairs):
        result = {}
        for key, item in pairs:
            require(key not in result, "ownerless_permission_duplicate_field")
            result[key] = item
        return result
    if isinstance(value, str):
        value = json.loads(value, object_pairs_hook=unique)
    require(isinstance(value, dict) and set(value) == {"permissions", "scope"}
            and value["scope"] == "turn", "ownerless_permission_not_policy_declined")
    grants = value["permissions"]
    require(isinstance(grants, dict) and set(grants) <= {"network", "file_system"}
            and all(item is None or item == {} for item in grants.values()),
            "ownerless_permission_not_policy_declined")
    return value


def fixture_failure(error):
    if isinstance(error, ProbeCheckFailure):
        safe = {"native_terminal_output_unverified", "advertised_code_mode_tool_missing",
                "custom_return_correlation_missing_or_duplicate", "custom_return_not_observed_completed_shape",
                "custom_return_label_missing_or_duplicate"}
        return error.kind if error.kind in safe else "native_probe_check_failed"
    return str(error)


def admitted_native_input(request: dict, configuration: dict, original_task: dict) -> dict:
    """Bind the authorized adapter's captured bytes to actual executor HTTP input."""
    def strings(value):
        if isinstance(value, str): return [value]
        if isinstance(value, list): return [text for item in value for text in strings(item)]
        if isinstance(value, dict): return [text for item in value.values() for text in strings(item)]
        return []
    developer = strings(request.get("instructions")) + [text for item in request.get("input", [])
        if item.get("role") == "developer" for text in strings(item.get("content"))]
    instructions = configuration["instructions"]
    sources = [instructions["common"], instructions["role"],
        *[item["source"] for key in ("dependencies", "selected_guidance") for item in instructions[key]]]
    require(all(any(source["body"] in text for text in developer) for source in sources), "admitted_original_role_source_not_in_native_input")
    require(request.get("model") == configuration["model"] and request.get("reasoning", {}).get("effort") == configuration["default_effort"], "admitted_model_or_effort_not_in_native_input")
    expected = json.loads(original_task["body_utf8"])
    matches = []
    for item in request.get("input", []):
        if item.get("role") != "user": continue
        for text in strings(item.get("content")):
            marker = "Accepted task:\n"
            if marker in text and json.JSONDecoder().raw_decode(text.split(marker, 1)[1])[0] == expected:
                matches.append(text)
    require(len(matches) == 1, "original_accepted_task_not_in_native_input")
    return {"source": "actual native executor model request; compared with owner-authorized production capture",
        "developer_input": developer, "accepted_task_input": matches[0], "model": request["model"], "reasoning": request["reasoning"],
        "complete_request": {"byte_length": len(encoded(request)), "sha256": "sha256:" + hashlib.sha256(encoded(request)).hexdigest()}}


class PodwayDistribution:
    """Verify only the selected immutable release; no version-based admission."""
    ARCHIVE = "05e596335ce891192b181ed89dce4fa9ca5dbbe19f4cc5745ea324439e4d2598"
    EXECUTABLE = "dece7fecd90eaf6130b330e2cb70e98532f3e6b73950227cc2ff935a8be8e5ae"
    CATALOG = "768adc7f0ac6bf7bf973018fa66616bef3f449e0b942ccc76ba8db4666126a1d"
    MANIFEST = "sha256:293cfcf7bcaebb55b55b9e7cabb5842df228dbd9afacea5d1ae78474c25ae01b"

    def __init__(self, archive: Path, executable: Path):
        import tarfile
        from jsonschema import Draft202012Validator
        from referencing import Registry, Resource
        require(hashlib.sha256(archive.read_bytes()).hexdigest() == self.ARCHIVE, "selected_podway_archive_changed")
        require(hashlib.sha256(executable.read_bytes()).hexdigest() == self.EXECUTABLE, "selected_podway_executable_changed")
        prefix = "podway-0.2.15-aarch64-apple-darwin/"
        with tarfile.open(archive) as selected:
            members = selected.getmembers()
            require(len(members) == 154 and len({m.name for m in members}) == len(members), "selected_distribution_inventory_changed")
            def body(name):
                member = selected.getmember(prefix + name)
                require(member.isfile() and 0 < member.size <= 2 * 1024 * 1024, "selected_contract_asset_not_bounded_regular")
                return selected.extractfile(member).read()
            binary_member = selected.getmember(prefix + "bin/podway")
            packaged = selected.extractfile(binary_member)
            digest = hashlib.sha256()
            while chunk := packaged.read(1024 * 1024):
                digest.update(chunk)
            require(digest.hexdigest() == self.EXECUTABLE, "packaged_podway_not_selected_executable")
            raw = body("share/podway/contracts/contract-manifest-v2.json")
            self.manifest = json.loads(raw)
            unsigned = {k: v for k, v in self.manifest.items() if k != "digest"}
            canonical = json.dumps(unsigned, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
            require(self.manifest["digest"] == self.MANIFEST == "sha256:" + hashlib.sha256(canonical).hexdigest(), "selected_manifest_self_digest_changed")
            self.schemas = {}
            self.assets = []
            paths = set()
            for asset in self.manifest["assets"]:
                require(asset["path"] not in paths, "duplicate_manifest_asset")
                paths.add(asset["path"])
                value = body("share/podway/" + asset["path"])
                require("sha256:" + hashlib.sha256(value).hexdigest() == asset["sha256"], "manifest_bound_asset_changed")
                self.assets.append(asset)
                if asset["kind"] == "schema":
                    document = json.loads(value)
                    Draft202012Validator.check_schema(document)
                    require(document["$id"] not in self.schemas, "duplicate_selected_schema_id")
                    self.schemas[document["$id"]] = document
            catalog = body("share/podway/release/dolgorae-adapter-contract-v3.json")
            require(hashlib.sha256(catalog).hexdigest() == self.CATALOG, "selected_adapter_catalog_changed")
            self.catalog = json.loads(catalog)
            for pin in self.catalog["contract_surface"]["schema_pins"]:
                require(pin["id"] in self.schemas and "sha256:" + hashlib.sha256(body("share/podway/"+pin["path"])).hexdigest() == pin["sha256"], "adapter_schema_pin_changed")
        require(len(self.assets) == 124 and len(self.schemas) == 88, "selected_asset_or_schema_count_changed")
        self.registry = Registry().with_resources((key, Resource.from_contents(value)) for key, value in self.schemas.items())
        for document in self.schemas.values():
            for node in documents(document):
                if "$ref" in node:
                    reference = node["$ref"]
                    base = reference.split("#", 1)[0]
                    require(not base or base in self.schemas, "selected_schema_reference_not_closed")
        self.validate_tag(self.manifest, "podway.contract-manifest/v2", field="schema_version")

    def validate_tag(self, value, tag, *, field="schema"):
        from jsonschema import Draft202012Validator
        key = "urn:podway:schema:" + tag.removeprefix("podway.").replace("/", ":")
        require(key in self.schemas and value.get(field) == tag, "selected_public_schema_unavailable")
        Draft202012Validator(self.schemas[key], registry=self.registry).validate(value)

    def validate_reply(self, value):
        self.validate_tag(value, value.get("schema", ""))
        if isinstance(value.get("result"), dict) and "schema" in value["result"]:
            self.validate_tag(value["result"], value["result"]["schema"])
        if value.get("command") == "version" and "result" in value:
            identity = value["result"]
            require(identity.get("contract_manifest_digest") == self.MANIFEST
                    and identity.get("source_commit") == "9a3f94c8752298f689832d05aa827a0f6496c1af"
                    and identity.get("build_identity") == "sha256:878cdf9ed33e6655bb5a94343b24f95b90ea57756495bcb5f8c50b924186420e",
                    "runtime_identity_not_selected_distribution")

    def report(self):
        return {"archive_sha256":self.ARCHIVE,"executable_sha256":self.EXECUTABLE,
                "manifest_digest":self.MANIFEST,"catalog_sha256":self.CATALOG,
                "manifest_assets":len(self.assets),"closed_schemas":len(self.schemas),
                "runtime_campaign_executed":False}


def model_request(body: bytes, encoding: str):
    require(0 < len(body) <= BODY,"http_input_outside_bound")
    if encoding == "gzip":
        with gzip.GzipFile(fileobj=io.BytesIO(body)) as stream:
            decoded = stream.read(BODY + 1)
    else:
        require(encoding == "identity","unsupported_native_request_encoding")
        decoded = body
    require(0 < len(decoded) <= BODY,"decoded_http_input_outside_bound")
    data = json.loads(decoded)
    require(isinstance(data,dict),"native_request_not_object")
    return data,decoded


def row_deadline(root: Path) -> float | None:
    if json.loads((root / "input.json").read_text()).get("ordinary_unbounded", False):
        return None
    return float((root / "deadline").read_text())


def timing(root: Path, site: str, outcome: str, started: int, byte_count: int = 0):
    require(site in {"prepare", "assign", "observe", "validation", "host", "backend", "first_byte", "frame", "decode", "journal"}
            and outcome in {"begin", "ok", "error", "timeout", "eof"}, "unknown_timing_bucket")
    path = root / "phase-timings.jsonl"
    flags = os.O_RDWR | os.O_CREAT | os.O_APPEND | os.O_NOFOLLOW
    fd = os.open(path, flags, 0o600)
    with os.fdopen(fd, "ab") as out:
        info = os.fstat(out.fileno())
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid()
                and stat.S_IMODE(info.st_mode) == 0o600 and info.st_nlink == 1,
                "timing_sink_identity_invalid")
        fcntl.flock(out, fcntl.LOCK_EX)
        info = os.fstat(out.fileno())
        require(info.st_size < 256 * 1024 and 0 <= byte_count <= 1024 * 1024 * 1024,
                "timing_sink_bound")
        ordinal = os.pread(out.fileno(), info.st_size, 0).count(b"\n") + 1
        require(ordinal <= 1024, "timing_sink_record_bound")
        now = time.monotonic_ns()
        deadline = row_deadline(root)
        row = {"site": site, "outcome": outcome, "monotonic_ns": now,
               "elapsed_ns": max(0, now-started),
               "remaining_ns": None if deadline is None else max(0, int(deadline*1_000_000_000)-now),
               "byte_count": byte_count, "ordinal": ordinal}
        record = encoded(row)+b"\n"
        require(info.st_size + len(record) <= 256 * 1024, "timing_sink_bound")
        out.write(record)


def run_host(argv, root, env, phase, deadline, **kwargs):
    started = time.monotonic_ns()
    timing(root, "host", "begin", started)
    try:
        result = _run_host(argv, root, env, phase, deadline, **kwargs)
    except Exception:
        timing(root, "host", "error", started)
        raise
    timing(root, "host", "ok", started)
    return result


def trace(row: str) -> dict:
    require(row in ROWS, "unknown_original_row")
    if row in CONSUMER_ROWS:
        return {"row": row, "state": "planned", "deadline_seconds": 180, "cleanup_seconds": 30,
                "combined_post_limit": 72, "host_message_post_max": 60, "delivery_limit": 96,
                "executor_posts": 5 if row == "consumer-group-gaps" else 6 if row in ("consumer-rounds", "consumer-group") else 2, "count_token_post_reserve": 2, "per_input_bytes": BODY,
                "owned_mcp_request_max": 8 if row == "consumer-complete" else 0,
                "steps": ["admit the explicit producer Role/dependencies/guidance and exact package/evidence Task; consumer ordinary roles run separately",
                    "two research Tasks and explicit synthesis (five POSTs; one physical worker loss)" if row == "consumer-group-gaps" else "two research Tasks and explicit synthesis (six POSTs)" if row.startswith("consumer-group") else "three accepted feedback Tasks (six POSTs)" if row == "consumer-rounds" else "one genuine writable executor Task (two POSTs)",
                    "native Primary reads every complete result, final response, change-side and package page",
                    "inspect prepared Record; explicit semantic item Record; inspect original outcome",
                    "inspect prepared Accept; explicit structural Accept; inspect original outcome and receipt",
                    "observe current receipt and running Action; separate Primary completion"],
                "cleanup": "close exact retained Runs, retire qualified binding and join owned processes"}
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
        require(number <= trace(json.loads((root / "input.json").read_text())["scenario"])["delivery_limit"], "delivery_budget_exceeded")
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


def native_tool(request: dict | list, selected: str):
    require(selected in ("specialist_native", "specialist_consumer"), "unknown_production_tool")
    matches = []
    def visit(tools, namespace=None):
        if not isinstance(tools, list):
            return
        for tool in tools:
            if not isinstance(tool, dict):
                continue
            if tool.get("type") == "namespace":
                visit(tool.get("tools"), tool.get("name"))
                continue
            function = tool.get("function", tool)
            if not isinstance(function, dict):
                continue
            name = function.get("name")
            if ((namespace == "mcp__dolgorae" and name == selected)
                    or (namespace is None and name == "mcp__dolgorae__" + selected)):
                if isinstance(function.get("parameters", function.get("input_schema")), dict):
                    matches.append((name, namespace))
    visit(request if isinstance(request,list) else request.get("tools"))
    for item in ([] if isinstance(request,list) else request.get("input", [])):
        if isinstance(item, dict) and item.get("type") == "additional_tools":
            visit(item.get("tools"))
    require(len(matches) == 1, "production_tool_not_advertised_or_ambiguous")
    return matches[0]


def codex_host_config(base_url: str, model: str, proxy_args: list[str]) -> str:
    return (f'model = {json.dumps(model)}\nmodel_provider = "production_host"\ncli_auth_credentials_store = "file"\n'
        '[features]\nhooks = true\n[features.code_mode]\ndirect_only_tool_namespaces = ["mcp__dolgorae"]\n'
        '[model_providers.production_host]\nname = "Numeric loopback host"\n'
        f'base_url = "{base_url}/v1"\nwire_api = "responses"\nrequires_openai_auth = false\nsupports_websockets = false\nrequest_max_retries = 0\nstream_max_retries = 0\n'
        '[mcp_servers.dolgorae]\nrequired = true\n'+f'command = {json.dumps(sys.executable)}\nargs = {json.dumps(proxy_args)}\n'
        f'env = {{ {OPT_IN} = "1" }}\n'
        '[mcp_servers.dolgorae.tools.specialist_native]\napproval_mode = "approve"\n'
        '[mcp_servers.dolgorae.tools.specialist_consumer]\napproval_mode = "approve"\n')


class Backend:
    """One production stdio connection. Native handshake bytes are forwarded."""
    def __init__(self, binary, owner, environment, root):
        self.root=root
        configuration=json.loads((root / "input.json").read_text())
        if "ordinary_fault_library" in configuration:
            require(environment.get(OPT_IN)=="1", "ordinary_fault_requires_native_opt_in")
            require(Path(binary).resolve(strict=True)==Path(configuration["binary"]).resolve(strict=True),
                    "ordinary_fault_product_image_changed")
            library=Path(configuration["ordinary_fault_library"])
            require(library.is_absolute() and library.is_file() and not library.is_symlink()
                    and hashlib.sha256(library.read_bytes()).hexdigest()==configuration["ordinary_fault_library_sha256"],
                    "ordinary_fault_library_changed")
            # Native hire/group can create Workers in this MCP process before
            # the gateway scheduler dispatches them. Instrument that actual
            # producer too; each fault still selects one exact PID and Run.
            environment=dict(environment, DYLD_INSERT_LIBRARIES=str(library),
                             DOLGORAE_NATIVE_FAULT_DIR=str(root / "fault"))
        self.process=subprocess.Popen([str(binary),"__trusted-specialist-host","host-mcp","--owner-config",str(owner)],env=environment,stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
        self.buffer=b""
        self.selector=selectors.DefaultSelector()
        os.set_blocking(self.process.stdout.fileno(),False)
        self.selector.register(self.process.stdout,selectors.EVENT_READ)

    def exchange(self, raw: bytes):
        require(len(raw)<=BODY,"native_mcp_frame_outside_bound")
        started = time.monotonic_ns()
        timing(self.root, "backend", "begin", started, len(raw))
        frame=json.loads(raw)
        if frame.get("method")=="tools/call": counter(self.root,"production_stdio")
        self.process.stdin.write(raw if raw.endswith(b"\n") else raw+b"\n");self.process.stdin.flush()
        if "id" not in frame: return None
        deadline=row_deadline(self.root)
        while deadline is None or time.monotonic()<deadline:
            if b"\n" in self.buffer:
                line,self.buffer=self.buffer.split(b"\n",1)
                timing(self.root, "frame", "ok", started, len(line))
                reply=json.loads(line)
                timing(self.root, "decode", "ok", started, len(line))
                if "id" not in reply:
                    sys.stdout.buffer.write(line+b"\n");sys.stdout.buffer.flush()
                    continue
                require(reply["id"]==frame["id"],"production_stdio_correlation_missing")
                timing(self.root, "backend", "ok", started, len(line))
                return reply
            interval = .1 if deadline is None else min(.1,max(.001,deadline-time.monotonic()))
            if not self.selector.select(interval): continue
            chunk=os.read(self.process.stdout.fileno(),65536)
            if not chunk:
                timing(self.root, "backend", "eof", started)
            require(chunk,"production_backend_closed_without_reply")
            if not self.buffer:
                timing(self.root, "first_byte", "ok", started, len(chunk))
            self.buffer+=chunk
            require(len(self.buffer)<=16*1024*1024,"production_backend_message_bound_exceeded")
        timing(self.root, "backend", "timeout", started)
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
                if row=="TASK085" and args.get("operation")=="merge_group_results":
                    # Discard the first product reply before forwarding anything
                    # to the native host, then recover this exact protected call.
                    committed=response
                    old_pid=backend.process.pid
                    backend.close();backend=Backend(binary,owner,environment,root);backend.replay_handshake()
                    response=backend.exchange(raw)
                    require(response==committed,"task085_original_protected_replay_changed")
                    private(root / "task085-backend-reply-loss.json",encoded({"original_native_call":call_id,"old_backend":old_pid,"new_backend":backend.process.pid,"original_reply":committed,"recovered_reply":response,"loss":"product reply discarded before native return"}))
                if row.startswith("consumer-group") and args.get("operation") in ("assign_group","merge_group_results","assign_synthesis"):
                    repeated=backend.exchange(raw)
                    def accepted(reply):
                        content=reply.get("result",{}).get("content",[])
                        require(content,"original_native_key_return_missing")
                        header=json.loads(content[0]["text"])
                        require(header.get("ok") is True,"original_native_key_replay_refused")
                        return header["facts"]["receipt"]
                    require(accepted(repeated)==accepted(response),"task086_exact_native_key_replay_changed")
                    private(root/("task086-replay-"+args["operation"]+".json"),encoded({
                        "native_call_id":call_id,"request":{k:v for k,v in args.items() if k!=PRIVATE},
                        "original_reply":response,"replay_reply":repeated}))
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
            journal_started = time.monotonic_ns()
            with os.fdopen(os.open(root / "tool-replies.jsonl",os.O_CREAT|os.O_APPEND|os.O_WRONLY,0o600),"ab") as out:
                # No opaque carrier enters this observation. Native call ID is
                # sourced from actual metadata/authenticated hook output only.
                public_args={k:v for k,v in args.items() if k!=PRIVATE}
                out.write(encoded({"native_call_id":call_id,"request":public_args,"reply":response})+b"\n")
            timing(root, "journal", "ok", journal_started)
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


class CompleteConsumer:
    """One bounded actual consumer sequence; no model acknowledgement is evidence."""
    RECORD = "Primary inspected every original page and retained assessment gaps."
    SUMMARY = "Primary verified the exact accepted Task, complete result and original change bytes."
    LABELS = {"complete_result", "final_response", "before", "after", "package",
              "package_evidence", "record_input", "record_outcome", "accept_input", "accept_outcome", "accept_receipt"}

    def __init__(self, root):
        self.root, self.stage = root, 0
        self.queue, self.page, self.finished, self.witnesses = [], None, {}, []
        self.record_observation = self.accept_package = None
        inputs=json.loads((root / "input.json").read_text())
        self.archive=Path(inputs["podway_archive"])
        self.contract=None
        self.required_labels = set(self.LABELS)
        self.record = self.RECORD

    def public(self, *args, **kwargs):
        from run_native_integrated_acceptance import successor_public
        value=successor_public(self.root, *args, **kwargs)
        if self.contract is None:
            connection=json.loads((self.root / "connection.json").read_text())
            self.contract=PodwayDistribution(self.archive,Path(connection["executable"]))
        self.contract.validate_reply(value)
        return value

    def read(self, label, selection, view, expected=None, declared=None):
        require(label not in self.finished, "consumer_view_read_twice")
        self.queue.append({"label": label, "selection": selection, "view": view,
                           "expected": expected, "declared": declared, "body": b"", "cursor": None})

    def read_call(self):
        if self.page is None and self.queue:
            self.page = self.queue.pop(0)
        if self.page is None:
            return None
        p = self.page
        return {"schema": CONTROL, "action": "inspect", "selection_ref": p["selection"], "view": p["view"],
                "range": {"kind": "continue", "cursor": p["cursor"]} if p["cursor"] else {"kind": "start", "offset": 0}}

    def check_page(self, metadata, texts, error_flag, request, post, call_id):
        p = self.page
        require(p and request == self.read_call(), "consumer_read_call_binding_changed")
        require(metadata.get("ok") is True and error_flag in (None, False) and len(texts) == 2,
                "consumer_original_page_not_returned")
        data, raw = metadata["data"], texts[1].encode()
        offset, size = len(p["body"]), data.get("source_length")
        require(isinstance(size, int) and 0 < size <= 32768 and 0 < len(raw) <= 1024
                and data.get("kind") == "page" and data.get("encoding") == "utf8"
                and data.get("offset") == offset and data.get("returned_length") == len(raw)
                and offset + len(raw) <= size, "consumer_page_outside_frozen_bound")
        if p["expected"] is not None:
            require(raw == p["expected"][offset:offset + len(raw)] and size == len(p["expected"]),
                    "consumer_original_bytes_changed")
        if p["declared"] is not None:
            require(data["source_digest"] == p["declared"]["digest"] and size == p["declared"]["byte_length"],
                    "consumer_prepared_or_outcome_source_changed")
        p["body"] += raw
        self.witnesses.append({"label": p["label"], "post": post, "call_id": call_id,
            "offset": offset, "bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest(),
            "source_digest": data["source_digest"], "source_length": size})
        p["cursor"] = data.get("continuation")
        if p["cursor"] is not None:
            require(isinstance(p["cursor"], str) and p["cursor"] and len(p["body"]) < size,
                    "consumer_invented_continuation")
        else:
            require(len(p["body"]) == size and "sha256:" + hashlib.sha256(p["body"]).hexdigest() == data["source_digest"],
                    "consumer_incomplete_original_read")
            self.finished[p["label"]] = p["body"]
            self.page = None

    def check_input(self, kind):
        import uuid
        value = json.loads(self.finished[kind + "_input"])
        require(uuid.UUID(value["idempotency_key"]).version == 7, "consumer_original_key_not_provider_allocated")
        connection = json.loads((self.root / "connection.json").read_text())
        expected = {"workspace_uuid": connection["workspace"], "session_id": connection["session"],
                    "attempt_id": connection["attempt"], "idempotency_key": value["idempotency_key"]}
        if kind == "record":
            observation = self.record_observation["result"]
            item = next(i for i in observation["active_items"] if i["item_id"] == "result")
            expected.update(schema="podway.item-record-many-input/v1", session_revision=observation["status"]["session"]["revision"],
                operations=[{"item_id": "result", "expected_item_revision": item["revision"],
                    "record": {"type": "text", "value": self.record}}])
        else:
            identity = json.loads((self.root / "producer-identity.json").read_text())["identity"]
            fields = ("binding_ref", "adapter_id", "adapter_version", "primary_host", "scope_kind", "scope_ref",
                      "supervisor_ref", "worker_ref", "task_ref", "role_ref", "role_snapshot_digest",
                      "agent_configuration_digest", "accepted_task_digest")
            expected.update(schema="podway.delegation-accept-input/v1", session_revision=self.accept_package["result"]["session_revision"],
                package_digest=self.accept_package["result"]["package_digest"], execution={k: identity[k] for k in fields},
                rounds_used=identity["rounds_used"], result_digest=identity["result_digest"], worker_disposition=identity["worker_disposition"], accepted=True, summary=self.SUMMARY)
        canonical = json.dumps(expected, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
        require(self.finished[kind + "_input"] == canonical, "consumer_prepared_input_not_exact_contract")
        private(self.root / f"consumer-prepared-{kind}.json", canonical)
        return value

    def next(self, responses, native, controls):
        if self.read_call() is not None:
            return self.read_call()
        def latest(action):
            value = next((v for v in reversed(controls) if v.get("action") == action), None)
            require(value and value.get("ok") is True, "consumer_control_not_successful:" + action)
            return value["data"]
        evidence = next((d["evidence"] for d in reversed(native) if d.get("evidence")), None)
        require(evidence, "consumer_original_result_selection_missing")
        selection = evidence["selection_ref"]
        stage = self.stage
        self.stage += 1
        if stage == 0:
            return {"schema": CONTROL, "action": "status", "target": {"kind": "context"}}
        if stage == 1:
            context = latest("status")["context_ref"]
            self.context = context
            originals = json.loads((self.root / "consumer-originals.json").read_text())
            if "before" not in originals:
                self.required_labels -= {"before", "after"}
            for label in (k for k in ("complete_result", "final_response", "before", "after") if k in originals):
                view = {"kind": "change", "index": 0, "side": label} if label in ("before", "after") else {"kind": label}
                self.read(label, selection, view, originals[label].encode())
            package = json.loads((self.root / "package.json").read_text())["result"]["package"]
            self.read("package", context, {"kind": "package"}, json.dumps(package, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode())
            require(len(package["evidence"])==1 and package["evidence"][0]["resolution"]=="resolved","actual_upstream_package_evidence_missing")
            self.read("package_evidence",context,{"kind":"package_evidence","index":0},json.dumps({"kind":"text","value":PACKAGE_EVIDENCE},ensure_ascii=False,sort_keys=True,separators=(",",":")).encode())
            return self.read_call()
        if stage == 2:
            require(set(self.finished) == self.required_labels - {"record_input", "record_outcome", "accept_input", "accept_outcome", "accept_receipt"}, "consumer_source_read_incomplete")
            config=json.loads((self.root / "input.json").read_text())
            if config["scenario"].startswith("consumer-group"):
                provenance=json.loads((self.root / "group-merge-provenance.json").read_text())
                self.record=f"Group {provenance['group']['group_ref']}; merge {provenance['merge']['merged_digest']}; synthesis {provenance['synthesis']['task']['task_ref']}; conflicts/gaps unresolved."
                require(len(self.record)<=256,"group_consumer_provenance_exceeds_declared_item_bound")
            self.record_observation = self.public("observe")
            return {"schema": CONTROL, "action": "prepare_record", "request_id": "native-record", "context_ref": self.context,
                    "evidence_ref": selection, "operations": [{"item_id": "result", "record": {"type": "text", "value": self.record}}]}
        if stage == 3:
            prepared = latest("prepare_record")["input"]
            self.record_selection = prepared["selection_ref"]
            self.read("record_input", self.record_selection, {"kind": "prepared_input"}, declared=prepared)
            return self.read_call()
        if stage == 4:
            self.record_input = self.check_input("record")
            return {"schema": CONTROL, "action": "record", "request_id": "native-record", "decision": "record"}
        if stage == 5:
            record = latest("record")
            require(record["effect"] == "committed", "consumer_record_not_committed")
            outcome = self.public("--idempotency-key", self.record_input["idempotency_key"], "outcome", "lookup")["result"]["outcome"]["response"]
            self.read("record_outcome", self.record_selection, {"kind": "outcome"},
                      json.dumps(outcome, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode(), record["outcome"])
            return self.read_call()
        if stage == 6:
            self.accept_package = self.public("delegation", "package")
            return {"schema": CONTROL, "action": "prepare_accept", "request_id": "native-accept", "evidence_ref": selection, "summary": self.SUMMARY}
        if stage == 7:
            prepared = latest("prepare_accept")["input"]
            self.accept_selection = prepared["selection_ref"]
            self.read("accept_input", self.accept_selection, {"kind": "prepared_input"}, declared=prepared)
            return self.read_call()
        if stage == 8:
            self.accept_input = self.check_input("accept")
            import uuid
            refusals=[]
            before=self.public("observe")["result"]
            for field,bad in (("role_ref","unadmitted-role"),("role_snapshot_digest","sha256:"+"0"*64)):
                payload=copy.deepcopy(self.accept_input)
                payload["idempotency_key"]=str(uuid.uuid4())
                payload["execution"][field]=bad
                refusals.append(self.public("delegation","accept","--stdin",payload=payload,expected_error="DELEGATION_BINDING_MISMATCH"))
            payload=copy.deepcopy(self.accept_input)
            payload.update(idempotency_key=str(uuid.uuid4()),rounds_used=4)
            refusals.append(self.public("delegation","accept","--stdin",payload=payload,expected_error="DELEGATION_BINDING_MISMATCH"))
            after=self.public("observe")["result"]
            require(before["status"]==after["status"] and before["active_items"]==after["active_items"],"invalid_acceptance_changed_current_state")
            private(self.root/"consumer-preaccept-refusals.json",encoded(refusals))
            return {"schema": CONTROL, "action": "accept", "request_id": "native-accept", "decision": "accept"}
        if stage == 9:
            accepted = latest("accept")
            require(accepted["effect"] == "committed", "consumer_accept_not_committed")
            outcome = self.public("--idempotency-key", self.accept_input["idempotency_key"], "outcome", "lookup")["result"]["outcome"]["response"]
            self.read("accept_outcome", self.accept_selection, {"kind": "outcome"},
                      json.dumps(outcome, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode(), accepted["outcome"])
            self.read("accept_receipt", self.accept_selection, {"kind": "receipt"}, declared=accepted["receipt"])
            return self.read_call()
        if stage == 11:
            refused=next((v for v in reversed(controls) if v.get("request_id")=="cancelled-late"),None)
            require(refused and refused.get("ok") is False and refused.get("error",{}).get("effect")=="not_called"
                    and refused.get("error",{}).get("code") in ("context_stale","post_read_failure"),"cancelled_late_delivery_was_not_refused")
            self.fences["cancelled_native_refusal"]=refused
            private(self.root/"consumer-fence-observations.json",encoded(self.fences))
            private(self.root/"consumer-full-pages.json",encoded(self.witnesses))
            return None
        require(stage == 10 and set(self.finished) == self.required_labels, "consumer_required_full_reads_missing")
        connection = json.loads((self.root / "connection.json").read_text())
        receipt = self.public("delegation", "receipt", "--session", connection["session"], "--attempt", connection["attempt"])
        require(json.loads(self.finished["accept_receipt"])["result"]["receipt"] == receipt["result"]["receipt"]
                and receipt["result"]["applicability"] == "accepted", "consumer_original_receipt_not_current")
        from run_native_integrated_acceptance import finish_successor_consumer
        original=self.public("--idempotency-key",self.accept_input["idempotency_key"],"outcome","lookup")["result"]["outcome"]["response"]
        require(self.public("delegation","accept","--stdin",payload=self.accept_input)==original,"cold_original_accept_replay_changed")
        conflict=copy.deepcopy(self.accept_input)
        conflict["summary"]+=" changed"
        self.public("delegation","accept","--stdin",payload=conflict,expected_error="IDEMPOTENCY_KEY_REUSED")
        if json.loads((self.root / "input.json").read_text())["scenario"] == "consumer-fences":
            # A cosmetic item edit makes the real receipt stale permanently, even
            # after reverting the visible text. Its original-key replay stays exact.
            self.public("set","result",self.record+" ","--idempotency-key","primary-cosmetic-edit")
            stale=self.public("observe")["result"]["status"]
            require(stale["delegation"]["state"]=="stale_acceptance","cosmetic_change_did_not_stale_receipt")
            self.public("set","result",self.record,"--idempotency-key","primary-cosmetic-revert")
            self.public("complete","--idempotency-key","primary-stale-completion",expected_error="DELEGATION_RECEIPT_STALE")
            require(self.public("delegation","accept","--stdin",payload=self.accept_input)==original,"stale_original_accept_replay_changed")
            # Restore delivery through a new Action attempt, never relabel the old
            # Task or accept it against the new package/caller scope.
            self.public("retry","--reason","Explicit current consumer recovery","--idempotency-key","primary-fresh-attempt")
            fresh=self.public("observe")["result"]["status"]
            require(fresh["current"]["attempt"]["attempt_id"]!=self.accept_input["attempt_id"],"fresh_attempt_not_allocated")
            revision=fresh["goal_revision"]
            revised=self.public("goal","revise","--goal","Revalidate the exact changed fixture candidate", "--criterion","proof=Preserve original native evidence", "--rework-to","work", "--reason","Explicit new goal applicability", "--if-goal-revision",str(revision), "--idempotency-key","primary-new-goal")
            current=self.public("observe")["result"]["status"]
            require(current["goal_revision"]>revision,"goal_revision_not_advanced")
            historical=self.public("delegation","receipt","--session",connection["session"],"--attempt",connection["attempt"])
            require(historical["result"]["receipt"]==receipt["result"]["receipt"],"old_attempt_receipt_relabelled")
            import uuid
            late=copy.deepcopy(self.accept_input)
            late["idempotency_key"]=str(uuid.uuid4())
            self.public("delegation","accept","--stdin",payload=late,expected_error="SESSION_REVISION_CONFLICT")
            cancelled=self.public("cancel","--reason","Explicit consumer cancellation","--idempotency-key","primary-cancel")
            after_cancel=self.public("observe")["result"]["status"]
            require(after_cancel["session"]["lifecycle"]=="cancelled","consumer_session_not_cancelled")
            self.fences={"original":original,"stale":stale,"fresh":fresh,"goal_revision":revised,"current":current,"historical":historical,"cancelled":cancelled}
            return {"schema":CONTROL,"action":"prepare_accept","request_id":"cancelled-late","evidence_ref":selection,"summary":"Original delivery cannot become acceptance after cancellation."}
        else:
            finish_successor_consumer(self.root)
        private(self.root / "consumer-full-pages.json", encoded(self.witnesses))
        return None


class Responses:
    protected_code_mode = False

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
        self.host_lock = threading.Lock()
        row = json.loads((root / "input.json").read_text())["scenario"]
        self.budget = trace(row)
        self.complete = CompleteConsumer(root) if row in CONSUMER_ROWS else None
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args): pass
            def do_POST(self):
                post_number = 0
                try:
                    self.connection.settimeout(5)
                    require(self.headers.get("Transfer-Encoding") is None, "streaming_request_refused")
                    length = int(self.headers.get("Content-Length", "0"))
                    require(0 < length <= BODY, "http_input_outside_bound")
                    body = self.rfile.read(length)
                    require(len(body)==length,"incomplete_native_http_input")
                    data,decoded=model_request(body,self.headers.get("Content-Encoding","identity"))
                    require(self.headers.get("Authorization") in (None,"Bearer sk-ant-fixture-not-a-credential") and self.headers.get("x-api-key") in (None,"sk-ant-fixture-not-a-credential"),"authentication_outside_explicit_fixture_policy")
                    with fixture.lock:
                        deadline = row_deadline(root)
                        require(deadline is None or time.monotonic() <= deadline, "row_deadline_exceeded")
                        fixture.posts += 1
                        post_number = fixture.posts
                        require(fixture.posts <= (12 if json.loads((root / "input.json").read_text())["scenario"] == "TASK085" else fixture.budget["combined_post_limit"]), "combined_host_executor_count_token_budget_exceeded")
                        require(PRIVATE.encode() not in decoded, "private_capability_in_next_http")
                        private(root / f"http-{post_number}.json", decoded)
                        private(root / f"http-{post_number}-wire.json",encoded({"encoding":self.headers.get("Content-Encoding","identity"),"wire_bytes":len(body),"decoded_bytes":len(decoded),"wire_sha256":hashlib.sha256(body).hexdigest()}))
                        endpoint = self.path.split("?")[0]
                        if endpoint == "/v1/messages/count_tokens":
                            fixture.count_posts += 1
                            require(fixture.count_posts<=fixture.budget["count_token_post_reserve"],"count_token_post_budget_exceeded")
                            answer = b'{"input_tokens":1}'
                            self.send_response(200); self.send_header("Content-Type", "application/json"); self.send_header("Content-Length", str(len(answer))); self.end_headers(); self.wfile.write(answer); return
                        require(endpoint in ("/v1/responses", "/v1/messages", "/executor/v1/responses"), "unexpected_endpoint")
                        executor = endpoint == "/executor/v1/responses"
                    if not executor:
                        with fixture.host_lock:
                            events = fixture.host_reply(data)
                    if executor:
                        events = fixture.executor(data) if json.loads((root / "input.json").read_text())["scenario"] == "TASK085" else fixture.executor(data, post_number)
                    self.send_response(200); self.send_header("Content-Type", "text/event-stream"); self.send_header("Connection", "close"); self.end_headers()
                    for event in events:
                        self.wfile.write(b"event: " + event["type"].encode() + b"\ndata: " + encoded(event) + b"\n\n")
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    if not ((root/"owned-worker-fault.json").is_file() and endpoint == "/executor/v1/responses" and "TASK086_GROUP_SOURCE_1" in json.dumps(data)):
                        fixture.failure="unexpected_native_connection_loss"
                except (OSError, ValueError, RuntimeError) as error:
                    fixture.failure = fixture_failure(error)
                    private(root / f"http-refusal-{post_number}.json", encoded({"post":post_number,"reason":fixture.failure}))
                    self.send_error(400, "bounded production fixture refused request")
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True); self.thread.start()

    @property
    def base_url(self): return f"http://127.0.0.1:{self.server.server_port}"

    @staticmethod
    def codex_events(item, number):
        return [{"type": "response.created", "response": {"id": f"resp_production_{number}"}}, {"type": "response.output_item.done", "item": item}, {"type": "response.completed", "response": {"id": f"resp_production_{number}", "usage": {"input_tokens": 0, "output_tokens": 0, "total_tokens": 0, "input_tokens_details": None, "output_tokens_details": None}}}]

    def executor(self, data, post_number=None):
        with self.lock:
            post_number = self.posts if post_number is None else post_number
            self.executor_posts += 1
            require(self.executor_posts <= self.budget["executor_posts"], "executor_turn_budget_exceeded")
            row=json.loads((self.root/"input.json").read_text())["scenario"]
            group=row.startswith("consumer-group")
            users=[v for v in data.get("input",[]) if v.get("role")=="user"]
            user=json.dumps(users[-1],ensure_ascii=False) if users else ""
            sources=re.findall(r"TASK086_GROUP_SOURCE_([01])",user) if group else []
            synthesis=group and "TASK086_SYNTHESIS" in user
            feedback=re.findall(r"Primary feedback round ([1-4])",user)
            key="synthesis" if synthesis else "source-"+sources[-1] if sources else "round-"+feedback[-1] if feedback else "round-1"
            if group: require(synthesis or len(set(sources))==1,"group_turn_not_original_accepted_source")
            self.executor_turn_posts=getattr(self,"executor_turn_posts",{})
            self.executor_inputs=getattr(self,"executor_inputs",{})
            self.executor_inputs.setdefault(key, data)
            step=self.executor_turn_posts.get(key,0)+1
            self.executor_turn_posts[key]=step
            require(step<=2,"original_executor_task_repeated")
        call_id="executor-evidence-"+key if self.complete else "executor-evidence"
        unknown=row=="consumer-group-gaps" and key=="source-1"
        def findings():
            context=b"Inspect the original fixture evidence."
            require("Inspect the original fixture evidence." in user,"research_context_not_original_accepted_bytes")
            finding={"finding_key":"independent-claim","claim":f"Independent {key} preserves its different claim.",
                "kind":"observation","stance":"supports","confidence":"medium",
                "evidence":[{"kind":"accepted_context","identity":"requirement","content":{"byte_length":len(context),"sha256":"sha256:"+hashlib.sha256(context).hexdigest()}}],
                "covered_questions":["C-1"],"limitations":["Semantic resolution remains with the Primary/Judge."]}
            return json.dumps({"schema":"structured_findings_v1","findings":[finding],"covered_questions":["C-1"],"limitations":[]},ensure_ascii=False)
        if unknown:
            require(step==1,"unknown_original_task_was_redispatched")
            private(self.root/"unknown-post-observed",b"actual original model request observed\n")
            while not (self.root/"release-source1-response").exists():
                deadline = row_deadline(self.root)
                require(deadline is None or time.monotonic()<deadline,"owned_worker_fault_barrier_timeout")
                time.sleep(.025)
            text=findings()
        elif step==1:
            if group:
                if synthesis:
                    texts=[v.get("text","") for v in users[-1].get("content",[]) if isinstance(v,dict)]
                    candidates=[text.rsplit("\n",1)[-1] for text in texts if "Immutable merged findings input" in text]
                    require(len(candidates)==1,"synthesis_turn_missing_immutable_merged_input")
                    self.merged_input=json.loads(candidates[0])
                    require(self.merged_input["schema"]=="dolgorae.merged-findings-output/v1"
                            and len(self.merged_input["coverage"])==2
                            and self.merged_input["groups"][0]["conflict"] is (row!="consumer-group-gaps"),"synthesis_input_lost_conflict_or_coverage")
                command="cat file.txt"
            elif self.complete:
                round_number=int(key.removeprefix("round-"))
                after=(f'changed original UTF8 round {round_number}: "quoted" 界🙂\n')*60
                program="from pathlib import Path; Path('file.txt').write_bytes("+repr(after.encode())+")"
                command=shlex.join([sys.executable,"-c",program])+" && printf 'native-production-original-evidence\\n'"
            elif (self.root/"change-layout.json").is_file():
                layout=json.loads((self.root/"change-layout.json").read_text())
                program="from pathlib import Path; p=Path("+repr(layout["relative"])+"); [x.unlink() for x in p.glob('before-*')]; [(p/f'after-{n:05}').write_bytes(b'') for n in range(10000)]"
                command=shlex.join([sys.executable,"-c",program])+" && printf 'native-production-original-evidence\\n'"
            else:
                command="cat file.txt"
            item=code_call(data,call_id,[(call_id,"exec_command",{"cmd":command,"yield_time_ms":30000,"shell":"/bin/sh","login":False})])
            if row == "consumer-complete":
                permission=code_call(data,call_id,[("ownerless-permission","request_permissions",{"permissions":{"file_system":{"read":[str(self.root/"workspace")]}},"reason":"Bounded private specialist permission request"})])
                item["input"]=permission["input"]+"\ntext({label:'ownerless-mcp',result:await tools.mcp__fixture__ask({})});\n"+item["input"]
            return self.codex_events(item,post_number)
        else:
            labels=["ownerless-permission","ownerless-mcp",call_id] if row == "consumer-complete" else [call_id]
            returned=code_results(data,call_id,labels)
            if row == "consumer-complete":
                permission = checked_permission_decline(returned["ownerless-permission"])
                require(self.interactions.mcp_decision=="decline" and self.interactions.failure is None,"ownerless_mcp_form_not_policy_declined")
                private(self.root/"actual-ownerless-native-declines.json",encoded({"permission":returned["ownerless-permission"],"permission_decoded":permission,"mcp_tool_result":returned["ownerless-mcp"],"mcp_decision":self.interactions.mcp_decision,"mcp_response":self.interactions.mcp_response,"mcp_requests":self.interactions.mcp_requests}))
            code,output=terminal_output(returned[call_id])
            expected="native-production-original-evidence\n" if (self.complete and not group) or (self.root/"change-layout.json").is_file() else 'original UTF8 evidence: "quoted" '
            require(code==0 and output==expected,"executor_terminal_result_mismatch")
            if group:
                text=json.dumps({"summary":"Exact immutable merged input; conflicts and gaps remain unresolved.","merged_input":self.merged_input},ensure_ascii=False,sort_keys=True,separators=(",",":")) if synthesis else findings()
            elif self.complete or (self.root/"change-layout.json").is_file():
                text='Original final: "quoted" \\ slash\n界🙂\t'+"full original evidence\n"*90
            else:
                verdict={"summary":'Original "quoted" evidence 界🙂 '+"result "*190,"findings":[],"criterion_assessments":[{"criterion_id":"C-1","status":"met","explanation":"Observed the fixture","evidence":[{"basis":"context","description":'Checked "quoted" 界🙂 context',"path":None,"line_start":None,"line_end":None,"context_id":"requirement"}],"remaining_gap":None}],"evidence_limits":[],"overall_assessment":"requirements_met"}
                text=json.dumps(verdict,ensure_ascii=False,indent=2)
                self.originals["structured_output"]=json.dumps(verdict,ensure_ascii=False,sort_keys=True,separators=(",",":"))
        self.originals["final_response"]=text
        path=self.root/f"executor-{key}-original.txt" if self.complete else self.root/"executor-original.txt"
        private(path,text.encode())
        return self.codex_events({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]},post_number)

    def returned_result(self, data, call_id):
        return correlated_result(data,self.host,call_id)

    def verify_pending(self, data):
        if not self.pending: return
        rows=[json.loads(line) for line in (self.root / "tool-replies.jsonl").read_bytes().splitlines()]
        for call_id,request in self.pending.items():
            emitted=([v for v in rows[self.delivery_offset:] if v["request"]==request]
                     if self.protected_code_mode else [v for v in rows if v["native_call_id"]==call_id])
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
            texts,error_flag=self.returned_result(data,call_id)
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
            if request.get("request_id") == "group-as-action-refusal":
                require(metadata.get("ok") is False and metadata.get("error",{}).get("effect") == "not_called",
                        "group_was_accepted_as_action_evidence")
                private(self.root / "group-as-action-refusal.json", encoded(metadata))
            if request.get("action")=="status" and request.get("target")=={"kind":"request","request_id":"native-accept"}:
                original=json.loads((self.root / "original-lookup.json").read_bytes())
                prior=next((d for d in documents(original) if d.get("schema")=="dolgorae.consumer-control-response/v1" and d.get("data",{}).get("request_id")=="native-accept"),None)
                require(prior and metadata.get("ok") is True and metadata.get("data",{}).get("effect")=="committed","resumed_original_acceptance_not_recovered")
                require(all(metadata["data"].get(k)==prior["data"].get(k) for k in ("outcome","receipt")),"resumed_original_receipt_changed")
                self.coverage.add("recovered_acceptance_status")
                self.witnesses.append({"post":self.posts,"call_id":call_id,"tag":"recovered_acceptance_status","sha256":hashlib.sha256(texts[0].encode()).hexdigest()})
            if request.get("action")=="inspect" and self.complete:
                self.complete.check_page(metadata,texts,error_flag,request,self.posts,call_id)
                self.coverage.add(request["view"]["kind"] + ":" + request["range"]["kind"])
                continue
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
        require(self.host_posts <= self.budget["host_message_post_max"], "host_message_budget_exceeded")
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
        elif row.startswith("consumer-group") and self.host_posts <= 2:
            provenance=json.loads((self.root / "group-merge-provenance.json").read_text())
            if self.host_posts == 1:
                assigned=json.loads((self.root / "assigned.json").read_text())
                call={"schema":SCHEMA,"operation":"collect_group","scope_kind":"external_engagement","scope_ref":assigned["scope_ref"],"group_ref":provenance["group"]["group_ref"],"cursor":None,"limit":64}
            else:
                original=next((v for v in reversed(native) if v.get("operation")=="collect_group"),None)
                current=original.get("facts",{}).get("coverage",{}).get("sources") if original else None
                fields=("entry_key","worker_ref","task_ref","role_snapshot_digest","agent_configuration_digest","accepted_task_digest")
                immutable=lambda rows:[{k:r[k] for k in fields} for r in rows]
                require(current and immutable(current)==immutable(provenance["coverage"]["coverage"]["sources"]),"native_group_source_identity_changed")
                private(self.root/"native-current-group-coverage.json",encoded(original["facts"]["coverage"]))
                tool="specialist_consumer"
                call={"schema":CONTROL,"action":"prepare_accept","request_id":"group-as-action-refusal","evidence_ref":original["facts"]["coverage"]["group_ref"],"summary":"A group has no individual Task delivery receipt."}
        elif prepared["requires_result"]:
            assigned = json.loads((self.root / "assigned.json").read_text())
            if self.host_posts == (3 if row.startswith("consumer-group") else 1):
                call = {"schema": SCHEMA, "operation": "collect_external_specialist_task", "scope_kind": "external_engagement", **{k: assigned[k] for k in ("scope_ref", "member_ref", "task_ref")}}
            else:
                evidence = next((d["evidence"] for d in reversed(native) if d.get("evidence")), None)
                require(evidence, "native_collect_did_not_select_original_result")
                tool = "specialist_consumer"
                if self.complete:
                    call = self.complete.next(self, native, controls)
                elif self.host_posts == 2:
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
            selected = native_tool(data, tool)
            if self.host == "codex":
                item = {"type": "function_call", "id": f"fc_native_{self.calls}", "call_id": f"call_native_{self.calls}", "name": selected[0], "arguments": json.dumps(call)}
                if selected[1]: item["namespace"] = selected[1]
                events=self.codex_events(item,self.posts)
                for extra in self.extra_calls(data,row,evidence if prepared["requires_result"] else None):
                    self.calls+=1;extra_id="call_native_"+str(self.calls);self.pending[extra_id]=copy.deepcopy(extra)
                    name,namespace=native_tool(data,"specialist_consumer")
                    extra_item={"type":"function_call","id":"fc_native_"+str(self.calls),"call_id":extra_id,"name":name,"arguments":json.dumps(extra)}
                    if namespace: extra_item["namespace"]=namespace
                    events.insert(-1,{"type":"response.output_item.done","item":extra_item})
                return events
            block = {"type": "tool_use", "id": f"toolu_native_{self.calls}", "name": selected[0], "input": {}}
            delta = {"type": "input_json_delta", "partial_json": json.dumps(call)}
        else:
            if prepared["requires_result"]:
                require(self.complete.witnesses if self.complete else self.witnesses, "next_http_original_bytes_unproved")
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
                name,_=native_tool(data,"specialist_consumer")
                pairs.append(({"type":"tool_use","id":extra_id,"name":name,"input":{}},{"type":"input_json_delta","partial_json":json.dumps(extra)}))
        for index,(content,change) in enumerate(pairs):
            events.extend([{"type":"content_block_start","index":index,"content_block":content},{"type":"content_block_delta","index":index,"delta":change},{"type":"content_block_stop","index":index}])
        events.extend([{"type":"message_delta","delta":{"stop_reason":"tool_use" if call else "end_turn","stop_sequence":None},"usage":{"output_tokens":1}},{"type":"message_stop"}])
        return events


    def close(self):
        self.server.shutdown(); self.server.server_close(); self.thread.join(timeout=3)
        require(not self.thread.is_alive(), "loopback_thread_not_joined")


def run_phase(driver: Path, mode: str, root: Path, environment: dict, deadline: float | None) -> dict:
    started = time.monotonic_ns()
    site = mode.removeprefix("--")
    measured = site in {"prepare", "assign", "observe"}
    if measured: timing(root, site, "begin", started)
    name=site+"-"+str(started)
    try:
        completed = subprocess.run([str(driver), mode, str(root)], env=environment, capture_output=True,
                                   timeout=None if deadline is None else max(.1, deadline - time.monotonic()))
    except subprocess.TimeoutExpired as error:
        if measured: timing(root, site, "timeout", started)
        private(root / (name+".stdout"),error.stdout or b"")
        private(root / (name+".stderr"),error.stderr or b"")
        raise
    private(root / (name+".stdout"), completed.stdout)
    private(root / (name+".stderr"), completed.stderr)
    if measured: timing(root, site, "ok" if completed.returncode == 0 else "error", started, len(completed.stdout))
    require(completed.returncode == 0, "production_fixture_" + mode + "_failed")
    validation_started = time.monotonic_ns()
    result = json.loads(completed.stdout)
    if measured: timing(root, "validation", "ok", validation_started, len(completed.stdout))
    return result


def podway_fixture(root: Path, executable: Path, digest: str, candidate: dict, environment: dict, deadline: float | None,
                   *, adapter_id: str = "dolgorae.external/1", adapter_version: str | None = None) -> None:
    require(hashlib.sha256(executable.read_bytes()).hexdigest() == digest, "selected_podway_executable_changed")
    worktree = root / "workspace"
    scenario=(json.loads((root/"input.json").read_text()) if (root/"input.json").is_file() else {}).get("scenario")
    setup_calls = 0
    def command(*args):
        nonlocal setup_calls
        setup_calls += 1
        selection = [] if args[0] in ("procedure", "version", "help") else ["--worktree", str(worktree)]
        argv = [str(executable), "--contract-generation", "1", *selection, "--json", *args]
        result = subprocess.run(argv, cwd=worktree, env=environment, capture_output=True,
                                timeout=None if deadline is None else max(.1, deadline-time.monotonic()))
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
        after_accept: {"release" if (json.loads((root / "input.json").read_text()) if (root / "input.json").is_file() else {}).get("scenario") == "consumer-release" else "retain"}
'''
    if scenario == "consumer-fences":
        source = source.replace("node_definitions:\n", "goal_tracking: true\nnode_definitions:\n", 1)
    if scenario in CONSUMER_ROWS:
        source=source.replace("node_definitions:\n  work:", "manual_rework:\n  allowed_targets: [work]\nnode_definitions:\n  source:\n    type: action\n    title: Capture original source\n    intent: Retain the original evidence.\n    items:\n      - id: input\n        type: text\n        prompt: Original evidence\n        required: true\n        max_length: 4096\n        multiline: true\n  work:")
        source=source.replace("entry: work", "entry: source").replace("  nodes:\n    - id: work", "  nodes:\n    - id: source\n      use: source\n      next: work\n    - id: work")
        source=source.replace("      use: work\n      terminal:", "      use: work\n      evidence_from:\n        - node: source\n          required: true\n          items: [input]\n      terminal:")
    if scenario == "consumer-fences":
        source = source.replace("graph:\n", '''  assess:
    type: decision
    title: Assess the fixture goal
    objective: Determine the current goal outcome from original evidence.
    prompt: Which goal outcome applies?
    options:
      - id: achieved
        label: Achieved
        criteria: Every current criterion has original supporting evidence and no unresolved gap.
      - id: not-achieved
        label: Not achieved
        criteria: At least one current criterion remains unmet or unverified in the original evidence.
      - id: superseded
        label: Superseded
        criteria: The current goal no longer describes the intended fixture outcome.
    reason:
      required: true
      prompt: Explain each criterion's evidence and the selected current goal outcome.
    assessment:
      target: session_goal
      outcomes:
        achieved: achieved
        not-achieved: not_achieved
        superseded: superseded
  finish:
    type: action
    title: Finish after goal assessment
    intent: Close only after the current goal assessment.
graph:
''', 1).replace("      terminal: true\n", "      next: assess\n", 1)
        source += '''    - id: assess
      use: assess
      evidence_from:
        - node: work
          required: true
          items: [result]
      routes:
        achieved:
          to: finish
          effect: advance
        not-achieved:
          to: finish
          effect: advance
        superseded:
          to: finish
          effect: advance
    - id: finish
      use: finish
      terminal: true
'''
    private(path, source.encode())
    relative = str(path.relative_to(worktree))
    preview = command("procedure", "preview", relative)
    command("start", "--procedure", relative, "--expect-procedure-digest", preview["result"]["procedure_digest"],
            "--task", "Inspect original native result", "--idempotency-key", "fixture-start")
    scenario=(json.loads((root/"input.json").read_text()) if (root/"input.json").is_file() else {}).get("scenario")
    goal_args=["--goal","Inspect the exact native fixture candidate","--criterion","proof=Preserve original native evidence"] if scenario == "consumer-fences" else []
    command("begin",*goal_args,"--idempotency-key","fixture-begin")
    if scenario in CONSUMER_ROWS:
        command("set","input",PACKAGE_EVIDENCE,"--idempotency-key","fixture-original-evidence")
        command("complete","--idempotency-key","fixture-source-complete")
        private(root/"package-evidence.txt",PACKAGE_EVIDENCE.encode())
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


def qualify(args, host: str, row: str, *, task085: bool = False, managed: bool = False) -> dict:
    require(os.environ.get(OPT_IN) == "1", "explicit_production_host_opt_in_required")
    executable = getattr(args, host).resolve(strict=True)
    version, digest, route = PINS[host]
    require(hashlib.sha256(executable.read_bytes()).hexdigest() == digest, "selected_native_image_changed")
    if task085:
        require(hashlib.sha256(args.codex.resolve(strict=True).read_bytes()).hexdigest()==PINS["codex"][1],"task085_executor_codex_pin_changed")
    prefix = f"task085-{host}-managed-" if task085 and managed else f"task085-{host}-protected-" if task085 else f"h{host[0]}{ROWS.index(row):02d}-"
    root = Path(tempfile.mkdtemp(prefix=prefix, dir=args.scratch)).resolve()
    root.chmod(0o700)
    for name in ("home", "codex", "claude", "tmp", "config", "cache", "workspace", "profile"):
        (root / name).mkdir(mode=0o700)
    private(root / "fixture-owned", b"native-production-host/v1\n")
    private(root / "delivery-count", b"0")
    deadline = time.monotonic()+(180 if task085 else trace(row)["deadline_seconds"])
    private(root / "deadline", str(deadline).encode())
    private(root / "deadline-unix-ms", str(int((time.time()+(180 if task085 else trace(row)["deadline_seconds"]))*1000)).encode())
    environment = host_environment(root)
    environment[OPT_IN] = "1"
    result = subprocess.run([str(executable), "--version"], env=environment, capture_output=True, text=True, timeout=5)
    require(result.returncode == 0 and result.stdout.strip() == version, "selected_native_version_changed")
    binary = args.binary.resolve(strict=True)
    binary_version = subprocess.run([str(binary), "version", "--json"], env=environment, capture_output=True, timeout=5)
    require(binary_version.returncode == 0, "product_version_failed")
    product_version = json.loads(binary_version.stdout)["version"].removeprefix("v")
    if task085:
        import native_task085
        selected_trace = native_task085.trace(managed)
    else:
        selected_trace = trace(row)
    config = {"host":host,"task085_managed":managed, "host_identity":{"path":str(executable),"version":"0.164.0" if host == "codex" else "2.1.310", "sha256":digest},
              "route":route,"scenario":row,"binary":str(args.driver.resolve(strict=True)),"reference_binary":str(binary),"codex":str(args.codex.resolve(strict=True)),
              "executor_model":args.executor_model,"host_model":getattr(args,host+"_model"),"adapter_version":product_version,
              "trace_sha256":hashlib.sha256(encoded(selected_trace)).hexdigest(),"python":sys.executable,"script":str(Path(__file__).resolve())}
    if not task085:
        config["podway_distribution"]=args.contract.report()
        config["podway_archive"]=str(args.podway_archive.resolve(strict=True))
        config["unsupported_peer_producer"]=str(args.unsupported_peer_producer.resolve(strict=True))
    private(root / "input.json", encoded(config))
    if task085:
        fixture = native_task085.Responses(root,host)
    elif row in CONSUMER_ROWS:
        import native_task086
        fixture = native_task086.Responses(root,host)
    else:
        fixture = Responses(root,host)
    passed = False
    observations = None
    original_failure = None
    control_process = None
    try:
        if row == "consumer-complete":
            from run_native_caller_pipeline import InteractionResponses
            fixture.interactions=InteractionResponses(root/"workspace",max_mcp_requests=8,protected_canary=OWNERLESS_CANARY)
        private(root / "profile/config.toml", (f'model = {json.dumps(args.executor_model)}\nmodel_reasoning_effort = "low"\n'
                'model_provider = "production_executor"\ncli_auth_credentials_store = "file"\n'
                '[features]\nplugins = false\n'+('request_permissions_tool = true\ntool_search_always_defer_mcp_tools = false\n' if row == "consumer-complete" else '')+
                '[model_providers.production_executor]\nname = "Numeric loopback executor"\n'
                f'base_url = "{fixture.base_url}/executor/v1"\nwire_api = "responses"\nrequires_openai_auth = false\nsupports_websockets = false\nrequest_max_retries = 0\nstream_max_retries = 0\n'+
                (f'\n[mcp_servers.fixture]\nurl = "http://127.0.0.1:{fixture.interactions.mcp_http.server_port}/mcp"\ndefault_tools_approval_mode = "approve"\n' if row == "consumer-complete" else '')).encode())
        if row in CONSUMER_ROWS:
            require(args.aquarium_archive, "selected_aquarium_archive_required")
            from run_native_integrated_acceptance import successor_aquarium_sources
            successor_aquarium_sources(root, args.aquarium_archive)
        if task085:
            control_process = native_task085.start_control_plane(root, args.driver, environment)
            fixture.control_process = control_process
        prepared = run_phase(args.driver, "--task085-prepare" if task085 else "--prepare", root, environment, deadline)
        require(prepared["dolgorae_version"] == product_version,"fixture_and_reference_product_versions_differ")
        if not task085 and prepared["requires_result"]:
            if row in CONNECTED:
                podway_fixture(root,args.podway.resolve(strict=True),args.podway_sha256,prepared["candidate"],environment,deadline)
            run_phase(args.driver,"--assign",root,environment,deadline)
            if row.startswith("consumer-group"):
                control_process = native_task086.start_control_plane(root,args.driver,environment,deadline)
                fixture.control_process = control_process
            else:
                observed=run_phase(args.driver,"--observe",root,environment,deadline)
                require(observed.get("task",{}).get("state")=="completed","actual_producer_did_not_complete")
                complete=observed.get("complete_result")
                require(isinstance(complete,dict) and complete.get("terminal_outcome")=="completed", "actual_producer_did_not_complete")
                if row in ("Rfreshobservation","backendrestart"):
                    require(len(complete.get("changes",[])) == 20000,"actual_twenty_thousand_change_producer_missing")
                if row in CONSUMER_ROWS:
                    originals={"complete_result":json.dumps(complete,ensure_ascii=False,sort_keys=True,separators=(",",":")),
                               "final_response":fixture.originals["final_response"]}
                    if row.startswith("consumer-group"):
                        require(complete["changes"] == [],"read_only_group_synthesis_changed_workspace")
                        provenance=json.loads((root / "group-merge-provenance.json").read_text())
                        require(json.loads(originals["final_response"])["merged_input"] == provenance["merged_body"],"synthesis_result_lost_original_merged_bytes")
                        require(len(provenance["merge"]["merged_identity"]["sources"]) == 2,"merged_source_identity_coverage_incomplete")
                    else:
                        require(len(complete["changes"]) == 1 and complete["changes"][0]["path"] == "file.txt", "consumer_change_source_missing")
                        originals.update(before=('changed original UTF8 round 2: "quoted" 界🙂\n' * 60) if row == "consumer-rounds" else 'original UTF8 evidence: "quoted" ',
                            after=(f'changed original UTF8 round {3 if row == "consumer-rounds" else 1}: "quoted" 界🙂\n') * 60)
                    private(root / "producer-identity.json", encoded(observed["delegation_identity"]))
                    private(root / "consumer-originals.json", encoded(originals))
                private(root / "settled.json",encoded({"engagement_revision":observed.get("engagement_revision")}))
                private(root / "producer-observation.json", encoded({"result_bytes":len(encoded(complete)),"change_count":len(complete.get("changes",[])),"result_digest":observed["task"].get("result_digest")}))
        if task085 and managed:
            run_phase(args.driver,"--task085-run-managed",root,environment,deadline)
            observations={"host":host,"row":"TASK085-managed",**fixture.finish(),"owned_fixture":str(root)}
            passed=True
            return observations
        script=Path(__file__).resolve()
        hook_command=shlex.join([sys.executable,str(script),"--hook",str(root)])
        hooks={"hooks":{"PreToolUse":[{"matcher":"^mcp__dolgorae__.*$","hooks":[{"type":"command","command":hook_command,"timeout":10}]}]}}
        private(root / "settings.json",encoded(hooks))
        proxy_args=[str(script),"--proxy",str(root)]
        if host == "codex":
            private(root / "codex/hooks.json",encoded(hooks))
            host_config=codex_host_config(fixture.base_url,args.codex_model,proxy_args)
            if row in CONSUMER_ROWS:
                host_config=host_config.replace('[features.code_mode]\ndirect_only_tool_namespaces = ["mcp__dolgorae"]\n',"")
            if task085:
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
        if task085:
            observations={"host":host,"row":"TASK085-protected",**fixture.finish(),"native_session":session,"owned_fixture":str(root)}
            passed=True
            return observations
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
        if row in CONSUMER_ROWS:
            require(fixture.executor_posts == (5 if row == "consumer-group-gaps" else 6 if row in ("consumer-rounds", "consumer-group") else 2), "accepted_rounds_do_not_match_actual_executor_turns")
            rounds = json.loads((root / "accepted-rounds.json").read_text())
            identity = json.loads((root / "producer-identity.json").read_text())["identity"]
            require(len(rounds) == identity["rounds_used"] == identity["accepted_assignments"] and identity["unresolved_assignments"] == 0,
                    "caller_round_export_not_actual_accepted_tasks")
            require(len({r["task_ref"] for r in rounds}) == len(rounds) and (row.startswith("consumer-group") or len({r["member_ref"] for r in rounds}) == 1),
                    "feedback_reuse_did_not_preserve_real_task_and_member_identities")
            if row == "consumer-rounds": require((root / "round-exhaustion.json").is_file(), "round_exhaustion_not_observed")
            if row.startswith("consumer-group"): require((root / "group-as-action-refusal.json").is_file(),"group_as_action_refusal_not_observed")
            if row == "consumer-release": require(identity["worker_disposition"] == "released" and identity["disposition_proof"]["release_receipt"] is not None, "settled_release_proof_missing")
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
        if row in CONSUMER_ROWS:
            def canonical(value): return json.dumps(value,ensure_ascii=False,sort_keys=True,separators=(",",":")).encode()
            original=fixture.complete.finished
            require(original["complete_result"]==canonical(final_observation["complete_result"]),"producer_complete_identity_changed_after_native_read")
            facts={"schema":"dolgorae.native-producer-facts/v1","source_kind":"authorized source-owner collect and original native page returns",
                "identity":final_observation["delegation_identity"]["identity"],"accepted_configuration":final_observation["accepted_configuration"],
                "original_task":final_observation["original_task"],"originals":{k:v.decode() for k,v in original.items()},
                "native_pages":fixture.complete.witnesses,"ordinary_aquarium_role":False,"semantic_goal_pass":False}
            key="synthesis" if row.startswith("consumer-group") else "round-3" if row == "consumer-rounds" else "round-1"
            facts["native_admitted_input"]=admitted_native_input(fixture.executor_inputs[key],facts["accepted_configuration"],facts["original_task"])
            drift=json.loads((root/"candidate-drift.json").read_text())
            require(drift["original"]["role_snapshot_digest"]==facts["identity"]["role_snapshot_digest"]
                and drift["original"]["agent_configuration_digest"]==facts["identity"]["agent_configuration_digest"]
                and drift["same_name_role"]["role_snapshot_digest"]!=facts["identity"]["role_snapshot_digest"]
                and drift["same_name_role"]["agent_configuration_digest"]!=drift["changed_configuration"]["agent_configuration_digest"],"original_task_reinterpreted_after_role_or_configuration_drift")
            facts["independent_drift_observations"]=drift
            if row == "consumer-complete":
                require(len(final_observation["policy_declines"])==2,"production_ownerless_policy_decisions_missing")
                require(all(OWNERLESS_CANARY.encode() not in path.read_bytes() for path in root.glob("http-*.json")),"protected_mcp_question_in_model_traffic")
                require(OWNERLESS_CANARY not in json.dumps(final_observation),"protected_mcp_question_in_public_observation")
                facts["ownerless_native_policy_declines"]={"native":json.loads((root/"actual-ownerless-native-declines.json").read_text()),"production_policy_decisions":final_observation["policy_declines"],"task_terminal":final_observation["task"]["state"]}
            private(root/"producer-facts.json",canonical(facts))
        if not prepared["requires_result"]:
            expected=0 if row in ("missingstate","corruptstate","hookbypass") else 2 if row=="distinct equal-text" else 1
            require(final_observation["accepted_operations"]==expected,"native_K_effect_count_wrong")
        if row=="backendrestart": require((root / "actual-backend-restart.json").is_file(),"persistent_backend_was_not_restarted")

        if row=="exposurecanary":
            require(any(d.get("action")=="record" and d.get("ok") is True and d.get("data",{}).get("effect")=="committed" for d in fixture.returns),"actual_public_record_outcome_unproved")
        if row in CONSUMER_ROWS:
            require((root / ("consumer-fence-observations.json" if row == "consumer-fences" else "consumer-completion.json")).is_file() and set(fixture.complete.finished) == fixture.complete.required_labels,"actual_full_consumer_not_completed")
        if row in DELEGATED:
            action="status" if crash else "accept"
            require(any(d.get("action")==action and d.get("request_id")=="native-accept" and d.get("ok") is True and d.get("data",{}).get("effect")=="committed" for d in fixture.returns),"actual_public_acceptance_receipt_unproved")
        require(row!="beforeacceptance loss" or (root / "beforeacceptance-backend-cut.json").is_file(),"actual_preacceptance_cut_unproved")
        require(row!="afteracceptance/mirror-before loss" or (root / "product-commit-before-mirror.json").is_file(),"actual_mirror_before_cut_unproved")
        for path in root.glob("*-std*.local"):
            require(PRIVATE.encode() not in path.read_bytes(),"private_capability_exposed_in_native_output")
        observations={"host":host,"row":row,"passed":True,"version":version,"executable_sha256":digest,"binary_sha256":hashlib.sha256(Path(config["binary"]).read_bytes()).hexdigest(),"reference_binary_sha256":hashlib.sha256(binary.read_bytes()).hexdigest(),
                      "posts":fixture.posts,"host_posts":fixture.host_posts,"executor_posts":fixture.executor_posts,"count_token_posts":fixture.count_posts,
                      "deliveries":int((root / "delivery-count").read_text()),"next_http_original_witnesses":fixture.complete.witnesses if fixture.complete else fixture.witnesses,"required_witness_tags":sorted(required),"native_session":session,
                      "input_sha256":hashlib.sha256((root / "input.json").read_bytes()).hexdigest(),"trace_sha256":config["trace_sha256"],"owned_fixture":str(root)}
        passed=True
        return observations
    except Exception as error:
        original_failure=type(error).__name__+": "+str(error)
        raise
    finally:
        cleanup_deadline=time.monotonic()+30
        cleanup_error = None
        control_error = None
        close_error = None
        if control_process is not None and not task085:
            try:
                native_task086.stop_control_plane(control_process, cleanup_deadline)
            except Exception as error:
                control_error=error
            control_process=None
        try:
            cleanup=run_phase(args.driver,"--cleanup",root,environment,cleanup_deadline)
            require(cleanup.get("cleanup_complete") is True,"owned_product_cleanup_incomplete")
        except Exception as error:
            cleanup_error=error
        if control_process is not None:
            try:
                (native_task085 if task085 else native_task086).stop_control_plane(control_process, cleanup_deadline)
            except Exception as error:
                control_error=error
        if task085:
            try:
                from orphan_cleanup import cleanup_removed_root
                orphan_deadline = time.monotonic()+30
                cleanup_removed_root(binary,root,allow_existing_root=True)
                native_task085.verify_process_absence(binary,root,environment,orphan_deadline)
                if observations is not None: observations["owned_process_absence_verified"]=True
            except Exception as error:
                cleanup_error=RuntimeError(f"{cleanup_error}; orphan cleanup: {error}") if cleanup_error else error
        if cleanup_error or control_error:
            passed=False
        if observations is not None: observations["cleanup_complete"]=not (cleanup_error or control_error)
        try:
            fixture.close()
        except Exception as error:
            close_error=error
            passed=False
        if getattr(fixture,"interactions",None) is not None:
            try:
                fixture.interactions.close()
            except Exception as error:
                close_error=error
                passed=False
        private(root / "row-result.json",encoded({"passed":passed,"observations":observations,"original_failure":original_failure,"cleanup_failure":str(cleanup_error) if cleanup_error else None,"control_plane_stop_failure":str(control_error) if control_error else None,"loopback_close_failure":str(close_error) if close_error else None,"loopback_failure":fixture.failure,"retained_private_fixture":str(root)}))
        if original_failure is None and (cleanup_error or control_error or close_error):
            raise cleanup_error or control_error or close_error


def qualify_direct_contract(args) -> dict:
    """Actual public Direct/compatibility paths, with no external executor ledger."""
    require(os.environ.get(OPT_IN) == "1", "explicit_public_consumer_opt_in_required")
    root=Path(tempfile.mkdtemp(prefix="dc-",dir=args.scratch)).resolve()
    root.chmod(0o700)
    for name in ("home","tmp","config","cache","workspace","peer","unsupported"):
        (root/name).mkdir(mode=0o700)
    environment=host_environment(root)
    environment[OPT_IN]="1"
    private(root/"fixture-owned",b"native-production-host/v1\n")
    private(root/"input.json",encoded({"scenario":"direct-contract","host":"codex"}))
    requested_deadline=getattr(args,"deadline",float("inf"))
    deadline=None if requested_deadline is None else min(time.monotonic()+90,requested_deadline)
    def operation_timeout(cap):
        if deadline is None:
            return None
        require(time.monotonic()<deadline,"direct_contract_deadline_exceeded")
        return min(cap,deadline-time.monotonic())
    product=subprocess.run([str(args.binary.resolve(strict=True)),"version","--json"],env=environment,capture_output=True,timeout=operation_timeout(5))
    private(root/"product-version.json",encoded({"exit":product.returncode,"stdout":product.stdout.decode(),"stderr":product.stderr.decode()}))
    require(product.returncode==0,"frozen_product_version_failed")
    adapter_version=json.loads(product.stdout)["version"].removeprefix("v")
    unsupported=root/"unsupported"
    ordinal=0
    def command(workspace,*argv,generation=1,error=None):
        nonlocal ordinal
        ordinal+=1
        selection=[] if argv[0] in ("procedure","version","help") else ["--worktree",str(workspace)]
        producer=args.unsupported_peer_producer if workspace==unsupported else args.podway
        selected=[str(producer.resolve(strict=True)),"--contract-generation",str(generation),*selection,"--json",*argv]
        result=subprocess.run(selected,cwd=workspace,env=environment,capture_output=True,timeout=operation_timeout(10))
        private(root/f"public-{ordinal:03}.json",encoded({"argv":selected,"exit":result.returncode,"stdout":result.stdout.decode(),"stderr":result.stderr.decode()}))
        value=json.loads(result.stdout)
        if workspace!=unsupported:
            args.contract.validate_reply(value)
        else:
            require(value.get("schema")=="podway.output/v4" and isinstance(value.get("result"),dict),
                    "actual_negative_producer_output_missing")
        require((result.returncode != 0 and value.get("code")==error) if error else result.returncode == 0,"direct_contract_public_result_changed")
        return value
    document={"schema":"podway.procedure/v2","id":"native-direct","version":"1","name":"Actual Direct qualification",
        "purpose":"Keep Direct progression independent from delegated recovery.","manual_rework":{"allowed_targets":["work"]},
        "node_definitions":{"work":{"type":"action","title":"Direct work","intent":"Record isolated evidence.",
            "items":[{"id":"result","type":"text","prompt":"Direct result","required":True}]}},
        "graph":{"entry":"work","nodes":[{"id":"work","use":"work","terminal":True}]}}
    def start(workspace,generation):
        result=subprocess.run(["/usr/bin/git","init","-q",str(workspace)],env=environment,capture_output=True,timeout=operation_timeout(5))
        require(result.returncode==0,"isolated_direct_git_setup_failed")
        private(workspace/"procedure.json",encoded(document))
        command(workspace,"init","--idempotency-key","direct-init",generation=generation)
        preview=command(workspace,"procedure","preview","procedure.json",generation=generation)
        command(workspace,"start","--procedure","procedure.json","--expect-procedure-digest",preview["result"]["procedure_digest"],"--task","Isolated Direct progress","--idempotency-key","direct-start",generation=generation)
        command(workspace,"begin","--idempotency-key","direct-begin",generation=generation)
        return command(workspace,"observe",generation=generation)
    direct=root/"workspace"
    initial=start(direct,1)
    version=command(direct,"version","--identity")
    command(direct,"help","delegation.package")
    command(direct,"job","list",error="COMMAND_RETIRED")
    command(direct,"set","result","Actual Direct evidence","--idempotency-key","direct-record")
    completed=command(direct,"complete","--idempotency-key","direct-complete")
    closed=command(direct,"observe")
    require(closed["result"]["status"]["session"]["lifecycle"]=="completed","actual_direct_did_not_complete")
    reactivated=command(direct,"rework","--to","work","--reason","Explicit bounded revalidation","--idempotency-key","direct-rework")
    refreshed=command(direct,"observe")
    require(reactivated["result"]["reactivated"] is True and refreshed["result"]["status"]["session"]["lifecycle"]=="running",
            "reactivation_notice_did_not_refresh_status")
    require(refreshed["result"]["status"]["current"]["attempt"]["attempt_id"]!=initial["result"]["status"]["current"]["attempt"]["attempt_id"],"direct_rework_reused_old_attempt")
    command(direct,"set","result","Actual Direct revalidation","--idempotency-key","direct-record-2")
    command(direct,"complete","--idempotency-key","direct-complete-2")
    peer=root/"peer"
    peer_observed=start(peer,2)
    status=peer_observed["result"]["status"]
    connection={"executable":str(args.podway.resolve(strict=True)),"executable_sha256":"sha256:"+PodwayDistribution.EXECUTABLE,
        "manifest_digest":PodwayDistribution.MANIFEST,"worktree":str(peer),"workspace":peer_observed["workspace"]["uuid"],
        "session":status["session"]["id"],"attempt":status["current"]["attempt"]["attempt_id"],"placement":"work",
        "adapter_id":"dolgorae.external/1","adapter_version":adapter_version,"permitted_items":[]}
    private(root/"peer-connection.json",encoded(connection))
    producer=args.unsupported_peer_producer.resolve(strict=True)
    require(hashlib.sha256(producer.read_bytes()).hexdigest()==UNSUPPORTED_PEER_SHA256,
            "unsupported_peer_producer_changed")
    identity=subprocess.run([str(producer),"--json","version","--identity"],env=environment,
                           capture_output=True,timeout=operation_timeout(5))
    private(root/"negative-producer-identity.json",encoded({"exit":identity.returncode,
        "stdout":identity.stdout.decode(),"stderr":identity.stderr.decode()}))
    producer_identity=json.loads(identity.stdout)
    require(identity.returncode==0 and producer_identity["result"]["source_commit"]==UNSUPPORTED_PEER_SOURCE
            and producer_identity["result"]["contract_manifest_digest"]==UNSUPPORTED_PEER_MANIFEST,
            "unsupported_peer_producer_identity_changed")
    incompatible=start(unsupported,1)
    incompatible_status=incompatible["result"]["status"]
    negative_connection={**connection,"worktree":str(unsupported),"workspace":incompatible["workspace"]["uuid"],
        "session":incompatible_status["session"]["id"],
        "attempt":incompatible_status["current"]["attempt"]["attempt_id"]}
    private(root/"unsupported-peer-connection.json",encoded(negative_connection))
    def snapshot():
        facts={}
        for workspace in [peer,unsupported]:
            for path in sorted((workspace/".podway").rglob("*")):
                info=path.lstat()
                require(stat.S_ISREG(info.st_mode) or stat.S_ISDIR(info.st_mode),"unexpected_peer_state_entry")
                facts[str(path.relative_to(root))]={"mode":stat.S_IMODE(info.st_mode),"device":info.st_dev,
                    "inode":info.st_ino,"kind":"file" if stat.S_ISREG(info.st_mode) else "directory",
                    "sha256":hashlib.sha256(path.read_bytes()).hexdigest() if stat.S_ISREG(info.st_mode) else None}
        return facts
    before=snapshot()
    refused=run_phase(args.driver,"--peer-read",root,environment,None if deadline is None else min(deadline,time.monotonic()+35))
    require(snapshot()==before,"unsupported_peer_read_changed_consumer_state")
    args.contract.validate_reply(refused["compatible_public_response"])
    args.contract.validate_reply(refused["unsupported_peer"]["actual_public_response"])
    final={"schema":"dolgorae.native-direct-contract/v1","passed":True,"owned_fixture":str(root),
        "version":version,"direct_completion":completed,"reactivation":reactivated,"refreshed":refreshed,
        "compatible_generation_read":refused["compatible_public_response"],
        "unsupported_peer":refused["unsupported_peer"],"state_bytes_unchanged":True,"public_calls":ordinal,
        "distribution":args.contract.report(),"native_model_turns":0}
    private(root/"direct-contract-result.json",encoded(final))
    return final


def main() -> int:
    if len(sys.argv)==3 and sys.argv[1]=="--direct-during-unknown":
        root=Path(sys.argv[2]).resolve(strict=True)
        require(os.environ.get(OPT_IN)=="1" and (root/"fixture-owned").read_bytes()==b"native-production-host/v1\n","explicit_owned_unknown_case_required")
        require((root/"owned-worker-fault.json").is_file() and not (root/"direct-unknown-complete").exists(),"unknown_barrier_not_held")
        config=json.loads((root/"input.json").read_text())
        connection=json.loads((root/"connection.json").read_text())
        args=argparse.Namespace(binary=Path(config["reference_binary"]),driver=Path(config["binary"]),podway=Path(connection["executable"]),scratch=root,
            unsupported_peer_producer=Path(config["unsupported_peer_producer"]))
        args.contract=PodwayDistribution(Path(config["podway_archive"]),args.podway)
        result=qualify_direct_contract(args)
        private(root/"direct-during-unknown.json",encoded(result))
        print(json.dumps(result,sort_keys=True))
        return 0
    if len(sys.argv)==3 and sys.argv[1] in ("--hook","--proxy"):
        root=Path(sys.argv[2]).resolve(strict=True)
        require(os.environ.get(OPT_IN)=="1","private_row_operation_requires_opt_in")
        (hook if sys.argv[1]=="--hook" else proxy)(root)
        return 0
    parser=argparse.ArgumentParser()
    parser.add_argument("--trace-only",action="store_true")
    parser.add_argument("--task085",action="store_true",help="only three bounded native merge/synthesis cases; no Podway")
    parser.add_argument("--binary",type=Path)
    parser.add_argument("--driver",type=Path)
    parser.add_argument("--codex",type=Path)
    parser.add_argument("--claude",type=Path)
    parser.add_argument("--podway",type=Path)
    parser.add_argument("--podway-archive",type=Path)
    parser.add_argument("--unsupported-peer-producer",type=Path)
    parser.add_argument("--verify-distribution-only",action="store_true")
    parser.add_argument("--direct-contract-only",action="store_true")
    parser.add_argument("--podway-sha256",default="dece7fecd90eaf6130b330e2cb70e98532f3e6b73950227cc2ff935a8be8e5ae")
    parser.add_argument("--codex-model")
    parser.add_argument("--claude-model")
    parser.add_argument("--executor-model")
    parser.add_argument("--scratch",type=Path)
    parser.add_argument("--aquarium-archive",type=Path)
    parser.add_argument("--rows",nargs="+",choices=ROWS,default=list(ROWS))
    parser.add_argument("--hosts",nargs="+",choices=tuple(PINS),default=list(PINS))
    args=parser.parse_args()
    if args.trace_only and args.task085:
        import native_task085
        print(json.dumps({"schema":"dolgorae.task085-native-traces/v1","executed":False,"cases":[{"host":selected,**native_task085.trace(managed)} for selected,managed in (("codex",False),("claude",False),("codex",True))]},sort_keys=True))
        return 0
    if args.verify_distribution_only:
        require(args.podway_archive and args.podway,"exact_selected_distribution_inputs_required")
        selected=PodwayDistribution(args.podway_archive.resolve(strict=True),args.podway.resolve(strict=True))
        print(json.dumps(selected.report(),sort_keys=True))
        return 0
    if args.trace_only:
        print(json.dumps({"schema":"dolgorae.production-host-traces/v1","executed":False,"rows":[{"host":host,**trace(row)} for host in args.hosts for row in args.rows]},sort_keys=True))
        return 0
    require(os.environ.get(OPT_IN)=="1","explicit_production_host_opt_in_required")
    keys=("binary","driver","codex","claude","codex_model","claude_model","executor_model","scratch")
    require(all(getattr(args,key) for key in keys) and (args.task085 or args.podway),"exact_selected_inputs_required")
    args.scratch=args.scratch.resolve(strict=True)
    if args.task085:
        results=[]
        for selected,managed in (("codex",False),("claude",False),("codex",True)):
            try: results.append(qualify(args,selected,"TASK085",task085=True,managed=managed))
            except (OSError,ValueError,RuntimeError,subprocess.SubprocessError,ProbeFailure) as error:
                results.append({"host":selected,"managed":managed,"passed":False,"failure":str(error)})
        print(json.dumps({"schema":"dolgorae.task085-native/v1","passed":all(r["passed"] for r in results),"cases":results},sort_keys=True))
        return 0 if all(r["passed"] for r in results) else 1
    require(args.podway_archive,"selected_podway_distribution_required")
    require(args.unsupported_peer_producer,"actual_unsupported_peer_producer_required")
    args.contract=PodwayDistribution(args.podway_archive.resolve(strict=True),args.podway.resolve(strict=True))
    if args.direct_contract_only:
        result=qualify_direct_contract(args)
        print(json.dumps(result,sort_keys=True))
        return 0
    results=[]
    # Serial keep-going: a failed row never supplies another row's evidence.
    for host in args.hosts:
        for row in args.rows:
            try: results.append(qualify(args,host,row))
            except (OSError,ValueError,RuntimeError,subprocess.SubprocessError,ProbeFailure) as error:
                results.append({"host":host,"row":row,"passed":False,"failure":fixture_failure(error)})
    print(json.dumps({"schema":"dolgorae.production-native-host/v1","passed":all(r["passed"] for r in results),"rows":results},sort_keys=True))
    return 0 if all(r["passed"] for r in results) else 1


if __name__=="__main__":
    raise SystemExit(main())
