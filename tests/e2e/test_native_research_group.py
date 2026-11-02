#!/usr/bin/env python3
"""Offline guards for the native research-group black-box harness; no native runtime."""
from __future__ import annotations

import json
import os
import subprocess
import tempfile
import threading
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from unittest.mock import patch
from urllib.error import HTTPError
from urllib.request import Request, urlopen

import run_native_research_group as probe
from schema_support import validator


def request(entry: int, previous: int | None = None) -> dict:
    inputs = []
    if previous is not None:
        inputs.append({"type":"message","role":"user","content":f"TASK084_NATIVE_ENTRY_{previous:02}"})
    inputs.append({"type":"message","role":"user","content":f"TASK084_NATIVE_ENTRY_{entry:02}"})
    return {"input":inputs}


class NativeResearchGroupGuards(unittest.TestCase):
    def test_runtime_opt_in_and_exact_pin_precede_scratch_allocation(self):
        with patch.dict(os.environ, {}, clear=True), patch.object(probe, "executable_identity") as identity:
            with self.assertRaises(ValueError):
                probe.campaign(Path("driver"), Path("binary"), Path("codex"), 1, 100)
            identity.assert_not_called()
        with patch.dict(os.environ, {probe.OPT_IN:"1"}, clear=True), patch.object(
                probe, "executable_identity", return_value={"sha256":"0"*64}), patch.object(probe.tempfile, "mkdtemp") as allocate:
            with self.assertRaises(ValueError):
                probe.campaign(Path("driver"), Path("binary"), Path("codex"), 1, 100)
            allocate.assert_not_called()

    def test_current_turn_correlation_does_not_use_history(self):
        self.assertEqual(probe.current_entry(request(12, previous=3)), 12)
        for body in ({}, request(50), {"input":[{"type":"message","role":"user","content":"TASK084_NATIVE_ENTRY_00 TASK084_NATIVE_ENTRY_01"}]}):
            with self.assertRaises(ValueError):
                probe.current_entry(body)

    def test_checked_success_output_and_deliberate_invalid_case(self):
        checked = validator(Path(__file__).resolve().parents[2] / "docs/protocol",
                            "dolgorae-v014-identities-v1.schema.json", "#/$defs/structured_findings")
        for entry in (0, 46, 48, 49):
            item = probe.response_events(entry, entry + 1)[1]["item"]
            checked.validate(json.loads(item["content"][0]["text"]))
        with self.assertRaises(json.JSONDecodeError):
            json.loads(probe.response_events(47, 48)[1]["item"]["content"][0]["text"])

    def test_normal_post_marker_is_once_before_two_held_responses(self):
        with tempfile.TemporaryDirectory(prefix="group-busy-offline-", dir="/tmp") as temporary:
            root = Path(temporary)
            fixture = probe.GroupResponses(root)
            blocked = threading.Event()
            release = threading.Event()
            waiting = []
            waiting_lock = threading.Lock()
            def wait(name, timeout=180):
                if name == "release-normal":
                    with waiting_lock:
                        waiting.append(name)
                        if len(waiting) == 2:
                            blocked.set()
                    if not release.wait(2):
                        raise AssertionError("offline normal barrier timed out")
            try:
                with patch.object(fixture, "wait_for", side_effect=wait), ThreadPoolExecutor(max_workers=2) as executor:
                    pending = [executor.submit(fixture.respond, request(entry)) for entry in (0, 1)]
                    self.assertTrue(blocked.wait(1))
                    marker = root / "normal-post-observed"
                    self.assertTrue(marker.is_file())
                    identity = marker.stat().st_ino
                    self.assertEqual(marker.read_bytes(), b"observed\n")
                    self.assertTrue(all(not future.done() for future in pending))
                    self.assertEqual(fixture.facts()["posts"], 2)
                    release.set()
                    for entry, future in enumerate(pending):
                        self.assertEqual(future.result(timeout=1)[1]["item"]["id"], f"native-group-message-{entry}")
                    self.assertEqual(marker.stat().st_ino, identity)
            finally:
                release.set()
                fixture.close()

    def test_concurrent_unknown_barrier_does_not_block_other_members(self):
        with tempfile.TemporaryDirectory(prefix="group-offline-", dir="/tmp") as temporary:
            root = Path(temporary)
            fixture = probe.GroupResponses(root)
            blocked = threading.Event()
            release = threading.Event()
            def wait(name, timeout=180):
                if name == "release-unknown":
                    blocked.set()
                    if not release.wait(2):
                        raise AssertionError("offline barrier timed out")
            try:
                with patch.object(fixture, "wait_for", side_effect=wait), ThreadPoolExecutor(max_workers=2) as executor:
                    unknown = executor.submit(fixture.respond, request(49))
                    self.assertTrue(blocked.wait(1))
                    normal = executor.submit(fixture.respond, request(0))
                    self.assertEqual(normal.result(timeout=1)[1]["item"]["id"], "native-group-message-0")
                    release.set()
                    unknown.result(timeout=1)
                self.assertEqual(fixture.facts()["posts"], 2)
                with self.assertRaises(ValueError):
                    fixture.respond(request(0))
            finally:
                release.set()
                fixture.close()

    def test_http_refuses_auth_and_oversized_wire_body(self):
        for headers, data in (({"Authorization":"Bearer not-a-credential"}, b"{}"),
                              ({}, b"x"*(probe.MAX_BODY+1))):
            with tempfile.TemporaryDirectory(prefix="group-http-offline-", dir="/tmp") as temporary:
                fixture = probe.GroupResponses(Path(temporary))
                try:
                    try:
                        with urlopen(Request(fixture.base_url + "/responses", data=data, headers=headers), timeout=3):
                            self.fail("invalid loopback request accepted")
                    except HTTPError as error:
                        self.assertEqual(error.code, 400)
                        error.close()
                    except OSError:
                        pass
                    self.assertEqual(fixture.posts, 0)
                    self.assertIsNotNone(fixture.failure)
                finally:
                    fixture.close()

    def test_resource_census_counts_detached_workers_servers_and_descendants(self):
        with tempfile.TemporaryDirectory(prefix="group-rss-offline-", dir="/tmp") as temporary:
            home = Path(temporary)
            profile = home / ".dolgorae/profiles/key"
            profile.mkdir(parents=True)
            (profile / "state.json").write_text(json.dumps({"lifecycle":"ready", "pid":20,
                "drainer_pid":21,"server_key":"key","server_epoch":1}))
            stopped = home / ".dolgorae/profiles/stopped"
            stopped.mkdir(parents=True)
            (stopped / "state.json").write_text(json.dumps({"lifecycle":"stopped", "pid":25,
                "drainer_pid":26,"server_key":"stopped","server_epoch":2}))
            runtime = home / ".dolgorae/workspaces/fixture/runtime/runs"
            runtime.mkdir(parents=True)
            run_id = "019a0000-0000-7000-8000-000000000001"
            (runtime / f"{run_id}.json").write_text(json.dumps({"identity":{
                "pid":23,"uid":os.getuid(),"executable_path":"/tmp/frozen-driver"}}))
            (runtime / f"{run_id}.bootstrap.json").write_text(json.dumps({"run_id":run_id,"session":{}}))
            census = f"10 1 {os.getuid()} 10 /tmp/frozen-driver\n11 10 {os.getuid()} 20 /tmp/frozen-driver\n20 1 {os.getuid()} 30 /tmp/pinned-codex\n21 1 {os.getuid()} 40 /tmp/frozen-driver\n22 1 {os.getuid()} 999 /tmp/unrelated\n23 1 {os.getuid()} 50 /tmp/frozen-driver\n24 23 {os.getuid()} 60 /tmp/owned-descendant\n25 1 {os.getuid()} 500 /tmp/pinned-codex\n26 1 {os.getuid()} 500 /tmp/frozen-driver\n"
            with patch.object(probe.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, census, "")):
                observed = probe.owned_process_sample(10, home, Path("/tmp/frozen-driver"), Path("/tmp/pinned-codex"))
            self.assertEqual(observed["rss_bytes"], 210*1024)
            self.assertEqual(observed["processes"], 6)
            self.assertEqual(observed["live_workers"], 1)
            self.assertEqual(observed["live_profile_servers"], 1)
            self.assertEqual(observed["generations"], {("key",1)})


if __name__ == "__main__":
    unittest.main()
