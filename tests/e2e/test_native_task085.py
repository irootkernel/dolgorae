#!/usr/bin/env python3
"""Offline loss-witness guards; these do not qualify native TASK085 execution."""
from __future__ import annotations

import copy
import json
import subprocess
import tempfile
import unittest
from unittest import mock
from pathlib import Path

import native_task085 as adapter


class Task085LossWitness(unittest.TestCase):
    def test_process_absence_requires_zero_owned_candidates(self):
        for verdict in (None, "owned", "orphan", "unverifiable"):
            with self.subTest(verdict=verdict), tempfile.TemporaryDirectory(prefix="task085-absence-", dir="/tmp") as temporary:
                root = Path(temporary)
                candidates = [] if verdict is None else [{"verdict":verdict,"pid":123}]
                result = subprocess.CompletedProcess([], 0, json.dumps({"ok":True,"data":{"candidates":candidates}}), "")
                with mock.patch.object(adapter.subprocess, "run", return_value=result) as call:
                    if verdict is None:
                        adapter.verify_process_absence(Path("/fixture/product"),root,{"HOME":str(root / "home")},adapter.time.monotonic()+10)
                    else:
                        with self.assertRaisesRegex(RuntimeError, "task085_owned_processes_remain"):
                            adapter.verify_process_absence(Path("/fixture/product"),root,{"HOME":str(root / "home")},adapter.time.monotonic()+10)
                self.assertEqual(call.call_args.args[0], ["/fixture/product","runtime","orphan","inspect","--owner-root-under",str(root)])
                self.assertEqual(call.call_args.kwargs["env"], {"HOME":str(root / "home")})
                self.assertEqual(json.loads((root / "owned-process-absence.json").read_text())["data"]["candidates"], candidates)

    def test_protected_code_mode_return_must_equal_the_actual_mcp_reply(self):
        for changed in (False, True):
            with self.subTest(changed=changed), tempfile.TemporaryDirectory(prefix="task085-mcp-", dir="/tmp") as temporary:
                root = Path(temporary)
                header = {"schema":"dolgorae.native-specialist-response/v1", "ok":True,
                          "operation":"merge_group_results", "facts":{}}
                actual = {"content":[{"type":"text", "text":json.dumps(header)}], "isError":False}
                (root / "tool-replies.jsonl").write_text(json.dumps({"request":{"original":"request"}, "reply":{"result":actual}})+"\n")
                returned = copy.deepcopy(actual)
                if changed: returned["content"][0]["text"] += "changed"
                peer = adapter.Responses.__new__(adapter.Responses)
                peer.root, peer.managed, peer.host = root, False, "codex"
                peer.last_call, peer.last_outer = "inner-label", "actual-outer-call"
                peer.pending = {peer.last_call:{"original":"request"}}
                peer.posts, peer.witnesses = 3, []
                frame = {"input":[{"type":"custom_tool_call_output", "call_id":peer.last_outer,
                                   "output":[{"type":"input_text", "text":"Script completed\nWall time 0.1 seconds\nOutput:\n"},
                                             {"type":"input_text", "text":json.dumps({"label":peer.last_call,"result":returned})}]}]}
                if changed:
                    with self.assertRaisesRegex(RuntimeError, "differs_from_actual_mcp"): peer.received(frame)
                    self.assertEqual(peer.witnesses, [])
                else:
                    peer.received(frame)
                    self.assertEqual(peer.witnesses[0]["header"], header)

    def test_control_plane_join_is_required_even_after_its_timeout(self):
        process = mock.Mock(returncode=-9)
        process.communicate.side_effect = [subprocess.TimeoutExpired("owned-control", 0.1), (b"", b"")]
        with self.assertRaisesRegex(RuntimeError, "shutdown_timed_out"):
            adapter.stop_control_plane(process, 0)
        process.kill.assert_called_once_with()
        self.assertEqual(process.communicate.call_count, 2)

    def test_control_plane_failure_cannot_credit_clean_shutdown(self):
        for code in (1, -9):
            with self.subTest(code=code):
                process = mock.Mock(returncode=code)
                with self.assertRaisesRegex(RuntimeError, "control_plane_failed"):
                    adapter.stop_control_plane(process, 0)

    def test_extra_tasks_or_turns_cannot_be_credited_as_model_free_merge(self):
        for axis in ("merge-task", "extra-synthesis-task", "merge-turn", "missing-source-turn", "shared-synthesis-turn"):
            with self.subTest(axis=axis), tempfile.TemporaryDirectory(prefix="task085-counts-", dir="/tmp") as temporary:
                root = Path(temporary)
                prepared = {"task_count_before": 1, "member_before": {"latest_turn": "source-turn"}}
                (root / "task085-prepared.json").write_text(json.dumps(prepared))
                first = {"task_count": 1, "member": {"latest_turn": "source-turn"}}
                observation = {"merge_lookup": {"state": "accepted"}, "artifact_verified": True,
                               "task_count": 2, "member": {"latest_turn": "synthesis-turn"}}
                if axis == "merge-task": first["task_count"] = 2
                elif axis == "extra-synthesis-task": observation["task_count"] = 3
                elif axis == "merge-turn": first["member"]["latest_turn"] = "extra-turn"
                elif axis == "missing-source-turn": prepared["member_before"]["latest_turn"] = None
                else: observation["member"]["latest_turn"] = "source-turn"
                (root / "task085-prepared.json").write_text(json.dumps(prepared))
                peer = adapter.Responses.__new__(adapter.Responses)
                peer.root, peer.failure, peer.executor_posts, peer.last_call = root, None, 2, None
                peer.merge_lookup, peer.first_observation = observation["merge_lookup"], first
                peer.observe = lambda: copy.deepcopy(observation)
                with self.assertRaisesRegex(RuntimeError, "task085_(accepted_task_count_changed|source_turn_changed_during_merge|synthesis_did_not_use_own_turn)"):
                    peer.finish()

    def test_observation_uses_the_selected_isolated_case_environment(self):
        with tempfile.TemporaryDirectory(prefix="task085-observe-", dir="/tmp") as temporary:
            root = Path(temporary)
            (root / "input.json").write_text(json.dumps({"binary": "/fixture/driver"}))
            (root / "deadline").write_text("180")
            peer = adapter.Responses.__new__(adapter.Responses)
            peer.root = root
            with mock.patch.object(adapter.host, "run_phase", return_value={"observed": True}) as run:
                self.assertEqual(peer.observe(), {"observed": True})
            args = run.call_args.args
            self.assertEqual(args[:3], (Path("/fixture/driver"), "--task085-observe", root))
            expected = adapter.host.host_environment(root)
            expected[adapter.host.OPT_IN] = "1"
            self.assertEqual(args[3], expected)
            self.assertEqual(args[3]["HOME"], str(root / "home"))
            self.assertEqual(args[3]["CODEX_HOME"], str(root / "codex"))

    def fixture(self, root):
        peer = adapter.Responses.__new__(adapter.Responses)
        peer.root = root
        peer.managed = True
        peer.step = 1
        peer.last_call = "task085-root-1"
        peer.last_outer = "task085-root-1-wrapper"
        peer.first_observation = None
        observation = {"merge_lookup":{"state":"accepted","receipt":{"receipt_digest":"sha256:original-receipt"}},
                       "artifact_verified":True,"root":{"thread_id":"original-thread"}}
        peer.observe = lambda: copy.deepcopy(observation)
        return peer, observation

    def test_only_correlated_discarded_return_can_credit_the_lost_reply(self):
        with tempfile.TemporaryDirectory(prefix="task085-loss-", dir="/tmp") as temporary:
            peer, observation = self.fixture(Path(temporary))
            frame = {"input":[{"type":"custom_tool_call_output","call_id":peer.last_outer,
                               "output":"Script failed: " + adapter.LOSS}]}
            peer.received(frame)
            self.assertEqual(peer.first_observation, observation)
            witness = json.loads((Path(temporary) / "task085-wrapper-loss.json").read_text())
            self.assertEqual(witness["outer_call"], peer.last_outer)
            self.assertEqual(witness["accepted_lookup"], observation["merge_lookup"])

    def test_uncommitted_wrong_call_or_leaked_receipt_cannot_prove_loss(self):
        for axis in ("uncommitted", "wrong-call", "leaked-receipt", "no-loss"):
            with self.subTest(axis=axis), tempfile.TemporaryDirectory(prefix="task085-loss-", dir="/tmp") as temporary:
                peer, observation = self.fixture(Path(temporary))
                frame = {"input":[{"type":"custom_tool_call_output","call_id":peer.last_outer,
                                   "output":"Script failed: " + adapter.LOSS}]}
                if axis == "uncommitted": observation["merge_lookup"]["state"] = "absent"
                elif axis == "wrong-call": frame["input"][0]["call_id"] = "another-call"
                elif axis == "leaked-receipt": frame["input"][0]["output"] += " sha256:original-receipt"
                else: frame["input"][0]["output"] = "Script completed"
                with self.assertRaises(RuntimeError): peer.received(frame)
                self.assertIsNone(peer.first_observation)
                self.assertFalse((Path(temporary) / "task085-wrapper-loss.json").exists())


def run_offline() -> None:
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(Task085LossWitness)
    result = unittest.TextTestRunner(verbosity=1).run(suite)
    if not result.wasSuccessful():
        raise AssertionError("TASK085 offline native adapter guards failed")


if __name__ == "__main__":
    unittest.main()
