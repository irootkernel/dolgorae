#!/usr/bin/env python3
"""Opt-in 50-task native group measurement using isolated, accountless loopback.

Build the embedded Rust driver with --cfg test --cfg dolgorae_native_acceptance.
This runner never builds, installs, reads account credentials, or alters task DBs.
Failed campaign roots remain available for inspection after owner-service cleanup.
"""
from __future__ import annotations

import argparse
import gzip
import json
import os
import re
import shutil
import stat
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

from orphan_cleanup import cleanup_removed_root
from run_live_instruction_discovery import checked_codex, executable_identity, isolated_environment, private_file
from run_native_caller_pipeline import CODEX_SHA256, CODEX_VERSION
from run_native_workspace_policy_fixture import MAX_BODY, fixture_config

OPT_IN = "DOLGORAE_RUN_NATIVE_RESEARCH_GROUP"
TASKS = 50
MAX_POSTS = 50
MAX_SECONDS = 300
DEFAULT_MAX_RSS = 8 * 1024**3  # Explicit fixture guard; no product memory promise is inferred.


def current_entry(request: Any) -> int:
    if not isinstance(request, dict) or not isinstance(request.get("input"), list):
        raise ValueError("native group request has no input array")
    users = [item for item in request["input"] if isinstance(item, dict)
             and item.get("type") == "message" and item.get("role") == "user"]
    markers = re.findall(r"TASK084_NATIVE_ENTRY_([0-9]{2})", json.dumps(users[-1]) if users else "")
    if len(markers) != 1 or not 0 <= int(markers[0]) < TASKS:
        raise ValueError("current native task marker missing or ambiguous")
    return int(markers[0])


def response_events(entry: int, post: int) -> list[dict]:
    text = "invalid findings output" if entry == 47 else json.dumps({
        "schema": "structured_findings_v1", "findings": [],
        "covered_questions": ["C1"], "limitations": [],
    }, separators=(",", ":"))
    identity = f"native-group-response-{post}"
    return [{"type": "response.created", "response": {"id": identity}},
            {"type": "response.output_item.done", "item": {"type": "message", "role": "assistant",
             "id": f"native-group-message-{entry}", "content": [{"type": "output_text", "text": text}]}},
            {"type": "response.completed", "response": {"id": identity,
             "usage": {"input_tokens": 0, "output_tokens": 0, "total_tokens": 0}}}]


class GroupResponses:
    """At most one actual Responses POST per task; requests remain in memory."""

    def __init__(self, root: Path):
        self.root = root
        self.lock = threading.Lock()
        self.stop = threading.Event()
        self.entries: set[int] = set()
        self.posts = 0
        self.active = 0
        self.peak_active = 0
        self.max_wire_bytes = 0
        self.max_decoded_bytes = 0
        self.failure: str | None = None
        self.expected_disconnects: set[int] = set()
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                entry = None
                try:
                    self.connection.settimeout(5)
                    if self.path != "/v1/responses" or self.headers.get("Authorization") is not None:
                        raise ValueError("loopback endpoint or authentication outside contract")
                    size = int(self.headers.get("Content-Length", "0"))
                    if not 0 < size <= MAX_BODY or self.headers.get("Transfer-Encoding") is not None:
                        raise ValueError("loopback wire body outside bound")
                    body = self.rfile.read(size)
                    if len(body) != size:
                        raise ValueError("loopback body incomplete")
                    encoding = self.headers.get("Content-Encoding", "identity")
                    if encoding == "gzip":
                        # Decompress with an output limit rather than materializing arbitrary gzip.
                        import io
                        with gzip.GzipFile(fileobj=io.BytesIO(body)) as compressed:
                            body = compressed.read(MAX_BODY + 1)
                    elif encoding != "identity":
                        raise ValueError("loopback content encoding unsupported")
                    if len(body) > MAX_BODY:
                        raise ValueError("loopback decoded body outside bound")
                    request = json.loads(body)
                    entry = current_entry(request)
                    events = owner.respond(request, size, len(body))
                    wire = "".join(f"event: {event['type']}\ndata: {json.dumps(event)}\n\n" for event in events).encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(wire)))
                    self.end_headers()
                    self.wfile.write(wire)
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    with owner.lock:
                        if entry in {48, 49}:
                            owner.expected_disconnects.add(entry)
                        elif owner.failure is None:
                            owner.failure = "unexpected native Responses disconnect"
                except (OSError, ValueError, TypeError, RuntimeError) as error:
                    with owner.lock:
                        owner.failure = owner.failure or str(error)[:160]
                    try:
                        self.send_error(400, "bounded fixture request refused")
                    except OSError:
                        pass

        class Server(ThreadingHTTPServer):
            daemon_threads = False
            request_queue_size = 16
            # There are thirteen member Runs. Limit even malformed concurrent
            # connections before allocating handler threads.
            slots = threading.BoundedSemaphore(16)

            def process_request(self, request, address):
                self.slots.acquire()
                try:
                    super().process_request(request, address)
                except BaseException:
                    self.slots.release()
                    raise

            def process_request_thread(self, request, address):
                try:
                    super().process_request_thread(request, address)
                finally:
                    self.slots.release()

        self.http = Server(("127.0.0.1", 0), Handler)
        self.worker = threading.Thread(target=self.http.serve_forever, daemon=True)
        self.worker.start()

    @property
    def base_url(self) -> str:
        return f"http://127.0.0.1:{self.http.server_port}/v1"

    def wait_for(self, name: str, timeout: float = 180) -> None:
        deadline = time.monotonic() + timeout
        while not (self.root / name).exists():
            if self.stop.wait(0.025):
                raise ValueError("fixture stopped before native barrier")
            if time.monotonic() >= deadline:
                raise ValueError("native fixture barrier exceeded bound")

    def respond(self, request: dict, wire_size: int = 1, decoded_size: int = 1) -> list[dict]:
        entry = current_entry(request)
        with self.lock:
            self.posts += 1
            if self.posts > MAX_POSTS or entry in self.entries:
                raise ValueError("native group request repeated or outside fifty POST budget")
            self.entries.add(entry)
            post = self.posts
            self.active += 1
            self.peak_active = max(self.peak_active, self.active)
            self.max_wire_bytes = max(self.max_wire_bytes, wire_size)
            self.max_decoded_bytes = max(self.max_decoded_bytes, decoded_size)
            marker = self.root / "normal-post-observed"
            if entry < 48 and not marker.exists():
                private_file(marker, b"observed\n")
        try:
            # Do not hold the observation lock while awaiting another native task.
            self.wait_for("release-normal")
            if entry == 48:
                self.stop.wait(2)  # Actual accepted one-second deadline; no database rewrite.
            elif entry == 49:
                private_file(self.root / "unknown-post-observed", b"observed\n")
                self.wait_for("release-unknown")
            return response_events(entry, post)
        finally:
            with self.lock:
                self.active -= 1

    def facts(self) -> dict:
        with self.lock:
            return {"posts": self.posts, "observed_entries": sorted(self.entries),
                    "maximum_active_responses": self.peak_active, "maximum_wire_bytes": self.max_wire_bytes,
                    "maximum_decoded_bytes": self.max_decoded_bytes,
                    "expected_disconnects": sorted(self.expected_disconnects), "failure": self.failure}

    def close(self):
        self.stop.set()
        self.http.shutdown()
        self.http.server_close()
        self.worker.join(timeout=5)


def owned_process_sample(driver_pid: int, home: Path, driver: Path, codex: Path) -> dict:
    """Read-only resource census. PID observations never authorize signalling."""
    result = subprocess.run(["/bin/ps", "-axo", "pid=,ppid=,uid=,rss=,comm="],
                            capture_output=True, text=True, check=True, timeout=5)
    rows = {}
    for line in result.stdout.splitlines():
        parts = line.split(None, 4)
        if len(parts) == 5:
            pid, parent, uid, rss = map(int, parts[:4])
            rows[pid] = (parent, uid, rss * 1024, parts[4])
    owned = {driver_pid} if driver_pid in rows else set()
    live_workers = 0
    for path in home.glob(".dolgorae/workspaces/*/runtime/runs/*.json"):
        if not re.fullmatch(r"[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\.json", path.name):
            continue
        metadata = path.lstat()
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid() or metadata.st_size > 131072:
            raise ValueError("fixture worker record outside observation bound")
        identity = json.loads(path.read_bytes())["identity"]
        worker = rows.get(identity["pid"])
        if (worker and worker[1] == os.getuid() == identity["uid"]
                and worker[3] == identity["executable_path"] == str(driver)):
            owned.add(identity["pid"])
            live_workers += 1
    generations = set()
    live_servers = 0
    for path in home.glob(".dolgorae/profiles/*/state.json"):
        metadata = path.lstat()
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid() or metadata.st_size > 131072:
            raise ValueError("fixture Profile state outside observation bound")
        value = json.loads(path.read_bytes())
        if value.get("lifecycle") != "ready":
            continue
        server = rows.get(value.get("pid"))
        if server and server[1] == os.getuid() and server[3] == str(codex):
            live_servers += 1
            generations.add((value["server_key"], value["server_epoch"]))
            owned.add(value["pid"])
        drainer = rows.get(value.get("drainer_pid"))
        if drainer and drainer[1] == os.getuid() and drainer[3] == str(driver):
            owned.add(value["drainer_pid"])
    for _ in range(32):
        children = {pid for pid, row in rows.items() if row[0] in owned and row[1] == os.getuid()}
        if children <= owned:
            break
        owned |= children
    return {"rss_bytes": sum(rows[pid][2] for pid in owned if rows[pid][1] == os.getuid()),
            "processes": len(owned), "live_workers": live_workers,
            "live_profile_servers": live_servers, "generations": generations}


def campaign(driver: Path, binary: Path, codex: Path, pool_limit: int, max_rss: int, with_merge: bool = False) -> dict:
    if os.environ.get(OPT_IN) != "1":
        raise ValueError("explicit native research-group opt-in required")
    if pool_limit not in {1, 4} or max_rss <= 0:
        raise ValueError("select K=1 or K=4 and a positive fixture RSS guard")
    driver_identity = executable_identity(driver)
    binary_identity = executable_identity(binary)
    if executable_identity(codex)["sha256"] != CODEX_SHA256:
        raise ValueError("selected native executable does not match the qualified pin")
    root = Path(tempfile.mkdtemp(prefix="dolgorae-native-group-", dir="/tmp")).resolve()
    root.chmod(0o700)
    fixture = None
    process = None
    passed = False
    failure = None
    cleanup_ok = False
    summary = None
    measurement = {"samples": 0, "peak_owned_rss_bytes": 0, "peak_owned_processes": 0,
                   "peak_live_workers": 0, "peak_live_profile_servers": 0, "fixture_rss_guard_bytes": max_rss}
    generations = set()
    environment = isolated_environment(root)
    environment[OPT_IN] = "1"
    try:
        for name in ("home", "profile", "tmp", "config", "cache", "workspace"):
            (root / name).mkdir(mode=0o700)
        private_file(root / "fixture-owned", b"native-research-group/v1\n")
        checked_codex(codex, f"codex-cli {CODEX_VERSION}", environment)
        fixture = GroupResponses(root)
        private_file(root / "profile/config.toml", b'model = "gpt-5.6-luna"\nmodel_reasoning_effort = "low"\n'
                     + fixture_config(fixture.base_url) + b'\n[features]\nplugins = false\nshell_snapshot = false\n')
        private_file(root / "executable-inputs.json", json.dumps({"driver":driver_identity,"binary":binary_identity,
                     "codex":executable_identity(codex),"configured_pool_k":pool_limit}).encode())
        with open(root / "driver.stdout", "xb", opener=lambda p, f: os.open(p, f, 0o600)) as stdout, \
                open(root / "driver.stderr", "xb", opener=lambda p, f: os.open(p, f, 0o600)) as stderr:
            command = [str(driver)] + (["--with-merge"] if with_merge else []) + [str(root), str(codex), str(pool_limit)]
            process = subprocess.Popen(command, cwd=root,
                                       env=environment, stdout=stdout, stderr=stderr)
            deadline = time.monotonic() + MAX_SECONDS
            while process.poll() is None:
                sample = owned_process_sample(process.pid, root / "home", driver, codex)
                measurement["samples"] += 1
                measurement["peak_owned_rss_bytes"] = max(measurement["peak_owned_rss_bytes"], sample["rss_bytes"])
                measurement["peak_owned_processes"] = max(measurement["peak_owned_processes"], sample["processes"])
                measurement["peak_live_workers"] = max(measurement["peak_live_workers"], sample["live_workers"])
                measurement["peak_live_profile_servers"] = max(measurement["peak_live_profile_servers"], sample["live_profile_servers"])
                generations |= sample["generations"]
                if sample["rss_bytes"] > max_rss or sample["live_profile_servers"] > pool_limit:
                    raise ValueError("actual owned resource observation exceeded fixture guard")
                if time.monotonic() >= deadline:
                    raise ValueError("native campaign exceeded whole-run deadline")
                time.sleep(0.1)
        if process.returncode:
            raise ValueError("native group driver failed; no automatic retry")
        summary = json.loads((root / "driver.stdout").read_text())
        if not summary.get("passed") or fixture.failure or not 49 <= fixture.posts <= MAX_POSTS:
            raise ValueError("native group observations incomplete or Responses contract failed")
        if not {47, 49}.issubset(fixture.entries) or not set(range(47)).issubset(fixture.entries):
            raise ValueError("native failure or successful task output not actually requested")
        if measurement["samples"] == 0 or not generations or measurement["peak_live_workers"] == 0:
            raise ValueError("actual native resource census missing")
        if len(generations) > pool_limit:
            raise ValueError("actual native Profile generations exceeded configured K")
        if executable_identity(driver) != driver_identity or executable_identity(binary) != binary_identity:
            raise ValueError("frozen executable changed during native campaign")
        if executable_identity(codex)["sha256"] != CODEX_SHA256:
            raise ValueError("selected native executable changed during campaign")
        passed = True
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        failure = str(error)
    finally:
        if fixture:
            fixture.stop.set()
        try:
            if (root / "fixture-owned").exists():
                clean = subprocess.Popen([str(driver), "--cleanup", str(root)], cwd=root, env=environment,
                                         stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                # communicate's timeout preserves the child for diagnosis;
                # unlike run(timeout=...), it never kills a PID implicitly.
                clean_stdout, clean_stderr = clean.communicate(timeout=60)
                private_file(root / "cleanup.stdout", clean_stdout)
                private_file(root / "cleanup.stderr", clean_stderr)
                if clean.returncode:
                    raise ValueError("owner-service cleanup refused or remains unresolved")
            if process and process.poll() is None:
                # The live driver is our unreaped direct child. Do not signal it:
                # owner-service cleanup makes its production operation resolve.
                process.wait(timeout=15)
            cleanup_removed_root(binary, root, allow_existing_root=True)
            cleanup_ok = True
        except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
            passed = False
            failure = failure or str(error)
        if fixture:
            fixture.close()
        measurement["observed_profile_generations"] = len(generations)
        report = {"schema":"dolgorae.native-research-group-campaign/v1", "passed":passed,
                  "configured_pool_k":pool_limit, "driver":summary, "group":json.loads((root / "native-group-result.json").read_text()) if (root / "native-group-result.json").exists() else None, "measurement":measurement,
                  "cleanup_reconciliation":json.loads((root / "cleanup-reconciliation.json").read_text()) if (root / "cleanup-reconciliation.json").exists() else None,
                  "responses":fixture.facts() if fixture else None, "cleanup_verified":cleanup_ok,
                  "real_accounts_used":False, "failure":failure, "retained_root":str(root) if not passed else None}
        private_file(root / "campaign-report.json", json.dumps(report).encode())
        if passed and cleanup_ok:
            shutil.rmtree(root)
    if not passed:
        raise ValueError(f"native group campaign failed; retained evidence at {root}: {failure}")
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--driver", type=Path, required=True)
    parser.add_argument("--binary", type=Path, required=True, help="same frozen product candidate; official orphan owner")
    parser.add_argument("--codex", type=Path, required=True)
    parser.add_argument("--pool-limit", type=int, choices=(1, 4), required=True)
    parser.add_argument("--with-merge", action="store_true", help="also verify Controller merge, original-key replay and artifact bytes")
    parser.add_argument("--max-owned-rss-bytes", type=int, default=DEFAULT_MAX_RSS)
    args = parser.parse_args()
    try:
        print(json.dumps(campaign(args.driver.resolve(), args.binary.resolve(), args.codex.resolve(),
                                  args.pool_limit, args.max_owned_rss_bytes, args.with_merge)))
        return 0
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(json.dumps({"schema":"dolgorae.native-research-group-campaign/v1","passed":False,"failure":str(error)}))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
