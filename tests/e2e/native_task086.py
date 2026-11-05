"""TASK086 protected host calls and bounded original-source coordination."""
from __future__ import annotations

import argparse
import copy
import json
import subprocess
import time
from pathlib import Path

import run_native_production_host as host
from run_native_workspace_policy_fixture import code_call, code_results


def start_control_plane(root, driver, environment, deadline):
    outputs = [root / "task086-control-stdout.local", root / "task086-control-stderr.local"]
    for path in outputs:
        host.private(path, b"")
    with outputs[0].open("ab") as stdout, outputs[1].open("ab") as stderr:
        process = subprocess.Popen([str(driver), "--task086-control-plane", str(root)],
                                   env=environment, stdin=subprocess.PIPE, stdout=stdout, stderr=stderr)
    try:
        while not (root / "task086-control-ready.json").is_file():
            host.require(process.poll() is None and time.monotonic() < deadline,
                         "task086_control_plane_not_ready")
            time.sleep(.025)
        return process
    except BaseException:
        stop_control_plane(process, deadline)
        raise


def stop_control_plane(process, deadline):
    try:
        process.communicate(input=b"stop\n", timeout=max(.1, deadline-time.monotonic()))
    except subprocess.TimeoutExpired as error:
        process.kill()
        process.communicate(timeout=1)
        raise RuntimeError("task086_control_plane_shutdown_timed_out") from error
    host.require(process.returncode == 0, "task086_control_plane_failed")


def prepare_consumer(root, observed, fixture):
    complete = observed["complete_result"]
    provenance = json.loads((root / "group-merge-provenance.json").read_bytes())
    host.require(complete["terminal_outcome"] == "completed" and complete["changes"] == [],
                 "original_synthesis_complete_result_missing")
    host.require(json.loads(fixture.originals["final_response"])["merged_input"] == provenance["merged_body"],
                 "synthesis_result_lost_original_merged_bytes")
    host.require(len(provenance["merge"]["merged_identity"]["sources"]) == 2,
                 "merged_source_identity_coverage_incomplete")
    host.private(root / "producer-identity.json", host.encoded(observed["delegation_identity"]))
    host.private(root / "consumer-originals.json", host.encoded({
        "complete_result": json.dumps(complete, ensure_ascii=False, sort_keys=True, separators=(",", ":")),
        "final_response": fixture.originals["final_response"]}))
    host.private(root / "settled.json", host.encoded({"engagement_revision": observed.get("engagement_revision")}))


class Responses(host.Responses):
    def __init__(self, root: Path, selected: str):
        self.row = json.loads((root / "input.json").read_bytes())["scenario"]
        self.group = self.row.startswith("consumer-group")
        self.gap = self.row == "consumer-group-gaps"
        self.protected_code_mode = selected == "codex"
        self.tools = None
        self.outer = None
        self.last_call = None
        self.last_request = None
        self.delivery_offset = 0
        self.step = 0
        self.group_facts = None
        self.coverage_before_merge = None
        self.synthesis_request = None
        self.synthesis_facts = None
        self.unknown = None
        self.native_metadata = []
        super().__init__(root, selected)

    def phase(self, operation):
        config = json.loads((self.root / "input.json").read_bytes())
        environment = host.host_environment(self.root)
        environment[host.OPT_IN] = "1"
        return host.run_phase(Path(config["binary"]), operation, self.root, environment,
                              float((self.root / "deadline").read_text()))

    def returned_result(self, data, call_id):
        if not self.protected_code_mode:
            texts, error_flag = host.correlated_result(data, self.host, call_id)
            self.actual_header_size = len(texts[0].encode())
            return texts, error_flag
        output = code_results(data, self.outer, [call_id])[call_id]
        rows = [json.loads(line) for line in (self.root / "tool-replies.jsonl").read_bytes().splitlines()]
        actual = [r for r in rows[self.delivery_offset:] if r["request"] == self.pending[call_id]]
        host.require(len(actual) == 1 and actual[0]["reply"].get("result") == output,
                     "protected_code_mode_result_differs_from_actual_mcp")
        host.require(isinstance(output, dict) and isinstance(output.get("content"), list)
                     and output["content"], "protected_native_return_missing")
        self.actual_header_size = len(output["content"][0]["text"].encode())
        return [v["text"] for v in output["content"]], output.get("isError")

    def received(self, data):
        if not self.pending:
            return
        request = self.last_request
        self.verify_pending(data)
        header = self.returns[-1]
        self.native_metadata.append({"post": self.posts, "operation": request.get("operation", request.get("action")),
                                     "ascii_bytes": self.actual_header_size})
        if request.get("request_id") in ("group-as-action-refusal", "cancelled-late"):
            return
        host.require(header.get("ok") is True, "task086_native_semantic_refusal")
        if not self.group:
            return
        operation = request.get("operation")
        if operation in ("assign_group", "merge_group_results", "assign_synthesis"):
            name = {"assign_group": "group", "merge_group_results": "merge", "assign_synthesis": "synthesis"}[operation]
            host.private(self.root / f"task086-native-{name}.json", host.encoded(header))
            if operation == "assign_group":
                self.group_facts = header["facts"]
            elif operation == "assign_synthesis":
                self.synthesis_facts = header["facts"]
                host.private(self.root / "assigned.json", host.encoded({
                    "scope_ref": self.prepared["scope_ref"],
                    "member_ref": self.synthesis_request["member_ref"],
                    "task_ref": self.synthesis_facts["task"]["task_ref"],
                    "assignment_receipt": self.synthesis_facts["receipt"]}))
        elif operation == "await_group":
            host.require(header["facts"]["wait_timed_out"] is False and header["facts"]["predicate_satisfied"] is True,
                         "original_group_wait_not_satisfied")
            self.source_coverage = header["facts"]["coverage"]
            host.private(self.root / "task086-native-await.json", host.encoded(self.source_coverage))
        elif operation == "collect_group" and self.coverage_before_merge is None:
            self.coverage_before_merge = header["facts"]["coverage"]
            rows = self.coverage_before_merge["sources"]
            host.require(len(rows)==2 and rows[0]["entry_key"]=="source-0" and rows[1]["entry_key"]=="source-1"
                         and rows[0]["state"]=="completed"
                         and rows[1]["state"]==("unknown" if self.gap else "completed"),
                         "original_two_source_coverage_not_ready")
            host.private(self.root / "task086-native-coverage.json", host.encoded(self.coverage_before_merge))
        elif operation == "collect_group":
            current = header["facts"]["coverage"]["sources"]
            fields = ("entry_key","worker_ref","task_ref","role_snapshot_digest","agent_configuration_digest","accepted_task_digest")
            host.require([{k:r[k] for k in fields} for r in current]
                         == [{k:r[k] for k in fields} for r in self.coverage_before_merge["sources"]]
                         and all(r["state"]=="completed" for r in current),
                         "original_group_recovery_identity_changed")
        elif operation == "await_external_specialist_task":
            host.require(header["facts"]["wait_timed_out"] is False,"original_synthesis_wait_timed_out")
        elif operation == "lookup_operation":
            lookup = header["facts"]["lookup"]
            kind = request["namespace"]["operation_kind"]
            if kind == "assign_external_specialist_task":
                host.require(lookup["state"] == "accepted" and lookup["current_outcome"] == "unknown"
                             and lookup["accepted_subjects"]["task_ref"] == self.unknown["source"]["task_ref"],
                             "original_native_child_lookup_not_unknown")
                host.require(time.time()*1000 < self.unknown["until_ms"], "original_unknown_window_exceeded")
                if not (self.root / "original-operation-unknown-before.json").exists():
                    host.private(self.root / "original-operation-unknown-before.json", host.encoded(lookup))
                    config = json.loads((self.root / "input.json").read_bytes())
                    connection = json.loads((self.root / "connection.json").read_bytes())
                    direct = argparse.Namespace(binary=Path(config["reference_binary"]),
                        driver=Path(config["binary"]), podway=Path(connection["executable"]), scratch=self.root,
                        unsupported_peer_producer=Path(config["unsupported_peer_producer"]),
                        deadline=min(float((self.root / "deadline").read_text()),
                                     time.monotonic()+(self.unknown["until_ms"]-time.time()*1000)/1000))
                    direct.contract = host.PodwayDistribution(Path(config["podway_archive"]), direct.podway)
                    result = host.qualify_direct_contract(direct)
                    host.private(self.root / "direct-during-unknown.json", host.encoded(result))
                else:
                    host.private(self.root / "original-operation-unknown-after.json", host.encoded(lookup))
                    host.private(self.root / "direct-unknown-complete", b"observed\n")
            elif kind == "merge_group_results":
                host.require(lookup["receipt"] == json.loads((self.root / "task086-native-merge.json").read_bytes())["facts"]["receipt"],
                             "original_merge_lookup_changed")
                self.synthesis_request = self.phase("--task086-merged")
            elif kind == "assign_synthesis":
                host.require(lookup["receipt"] == self.synthesis_facts["receipt"], "original_synthesis_lookup_changed")
        elif operation == "collect_external_specialist_task":
            host.require(header["facts"]["task"]["state"]=="completed" and header.get("evidence")
                         and header["facts"]["task"]["task_ref"]==request["task_ref"]
                         and header["facts"]["task"]["member_ref"]==request["member_ref"],
                         "original_native_task_result_not_selected")
            if self.synthesis_facts is not None and request["task_ref"] == self.synthesis_facts["task"]["task_ref"]:
                prepare_consumer(self.root, self.phase("--task086-finish"), self)

    def emit(self, data, request, tool="specialist_native"):
        request = copy.deepcopy(request)
        remaining = float((self.root / "deadline").read_text()) - time.monotonic()
        host.require(remaining > 0, "original_task086_deadline_exhausted")
        if "timeout_seconds" in request:
            request["timeout_seconds"] = min(request["timeout_seconds"], int(remaining))
        self.calls += 1
        call = ("call_native_" if self.host == "codex" else "toolu_native_") + str(self.calls)
        self.last_call, self.last_request = call, copy.deepcopy(request)
        self.pending[call] = copy.deepcopy(request)
        journal = self.root / "tool-replies.jsonl"
        self.delivery_offset = len(journal.read_bytes().splitlines()) if journal.exists() else 0
        if self.host == "codex":
            self.outer = call + "-wrapper"
            item = code_call(data, self.outer, [])
            item["input"] = ('// @exec: {"yield_time_ms": 65000}\ntext({label:' + json.dumps(call)
                + ',result:await tools.' + self.tools[tool] + '(' + json.dumps(request) + ')});')
            return self.codex_events(item, self.posts)
        selected = host.native_tool(data, tool)
        host.require(selected, "protected_task086_tool_missing")
        return self.claude_events(data, {"type": "tool_use", "id": call, "name": selected[0], "input": {}},
                                  {"type": "input_json_delta", "partial_json": json.dumps(request)})

    def group_call(self):
        base = {"schema": host.SCHEMA, "scope_kind": "external_engagement", "scope_ref": self.prepared["scope_ref"]}
        stages = ["group", "group_lookup", "await_group", "source0"]
        if self.gap:
            stages += ["unknown_before", "unknown_after"]
        stages += ["coverage", "merge", "merge_lookup", "source1"]
        if self.gap:
            stages += ["recovered_coverage"]
        stages += ["synthesis", "synthesis_lookup", "await_synthesis", "collect_synthesis",
                   "settled_synthesis_lookup", "group_refusal"]
        if self.step == len(stages):
            return None
        stage = stages[self.step]
        self.step += 1
        if stage == "group":
            return self.prepared["group_request"], "specialist_native"
        if stage == "synthesis":
            return self.synthesis_request, "specialist_native"
        if stage == "group_refusal":
            return {"schema": host.CONTROL, "action": "prepare_accept", "request_id": "group-as-action-refusal",
                    "evidence_ref": self.group_facts["group_ref"],
                    "summary": "A group has no individual Task delivery receipt."}, "specialist_consumer"
        if stage in ("coverage", "recovered_coverage", "await_group"):
            request = {**base, "operation": "await_group" if stage == "await_group" else "collect_group",
                       "group_ref": self.group_facts["group_ref"]}
            if stage == "await_group":
                request.update(predicate={"kind": "any" if self.gap else "all"}, timeout_seconds=60)
            else:
                request.update(cursor=None, limit=64)
            return request, "specialist_native"
        if stage in ("source0", "source1"):
            source = (self.source_coverage if stage == "source0" else self.coverage_before_merge)["sources"][0 if stage == "source0" else 1]
            return {**base, "operation": "collect_external_specialist_task",
                    "member_ref": source["worker_ref"], "task_ref": source["task_ref"]}, "specialist_native"
        if stage in ("unknown_before", "unknown_after"):
            if stage == "unknown_before":
                self.unknown = self.phase("--task086-unknown")
            return {"schema": host.SCHEMA, "operation": "lookup_operation", "namespace": self.unknown["namespace"]}, "specialist_native"
        if stage == "merge":
            return {**base, "operation": "merge_group_results", "group_ref": self.group_facts["group_ref"],
                    "group_assignment_digest": self.coverage_before_merge["group_assignment_digest"],
                    "sources": self.coverage_before_merge["sources"], "algorithm": "merge-exact-findings-v1",
                    "idempotency_key": "fixture-merge", "expected_revision": None}, "specialist_native"
        if stage.endswith("lookup"):
            if stage == "group_lookup":
                namespace = self.group_facts["receipt"]["namespace"]
            elif stage == "merge_lookup":
                namespace = json.loads((self.root / "task086-native-merge.json").read_bytes())["facts"]["receipt"]["namespace"]
            else:
                namespace = self.synthesis_facts["receipt"]["namespace"]
            return {"schema": host.SCHEMA, "operation": "lookup_operation", "namespace": namespace}, "specialist_native"
        request = {**base, "operation": "await_external_specialist_task" if stage == "await_synthesis" else "collect_external_specialist_task",
                   "member_ref": self.synthesis_request["member_ref"], "task_ref": self.synthesis_facts["task"]["task_ref"]}
        if stage == "await_synthesis":
            request["timeout_seconds"] = 60
        return request, "specialist_native"

    def host_reply(self, data):
        self.host_posts += 1
        host.require(self.host_posts <= 60, "task086_host_message_budget")
        if self.host == "codex" and self.tools is None:
            if self.outer is None:
                self.outer = "task086-protected-tool-metadata"
                item = code_call(data, self.outer, [])
                item["input"] = ('const names=["mcp__dolgorae__specialist_native","mcp__dolgorae__specialist_consumer"]; '
                    'const found=ALL_TOOLS.filter(t=>names.includes(t.name)).map(t=>t.name); '
                    'if(found.length!==2)throw new Error("protected tool metadata missing"); '
                    'text({label:"native-tools",result:found});')
                return self.codex_events(item, self.posts)
            found = code_results(data, self.outer, ["native-tools"])["native-tools"]
            host.require(set(found) == {"mcp__dolgorae__specialist_native", "mcp__dolgorae__specialist_consumer"},
                         "protected_tool_metadata_changed")
            self.tools = {key: "mcp__dolgorae__" + key for key in ("specialist_native", "specialist_consumer")}
        self.received(data)
        if self.group:
            self.prepared = json.loads((self.root / "task086-prepared.json").read_bytes())
            next_call = self.group_call()
            if next_call is not None:
                return self.emit(data, *next_call)
        elif not self.returns:
            assigned = json.loads((self.root / "assigned.json").read_bytes())
            return self.emit(data, {"schema": host.SCHEMA, "operation": "collect_external_specialist_task",
                "scope_kind": "external_engagement", **{k: assigned[k] for k in ("scope_ref", "member_ref", "task_ref")}})
        native = [v for v in self.returns if v.get("schema") == "dolgorae.native-specialist-response/v1"]
        controls = [v for v in self.returns if v.get("schema") == "dolgorae.consumer-control-response/v1"]
        request = self.complete.next(self, native, controls)
        if request is not None:
            return self.emit(data, request, "specialist_consumer")
        host.require(self.complete.witnesses and not self.pending, "task086_complete_next_http_missing")
        ledger = {"streams": {k: {"bytes": len(v), "pages": sum(w["label"] == k for w in self.complete.witnesses)}
                               for k, v in self.complete.finished.items()},
                  "native_metadata": self.native_metadata, "host_posts": self.host_posts,
                  "executor_posts": self.executor_posts, "count_posts": self.count_posts}
        host.private(self.root / "task086-payload-ledger.json", host.encoded(ledger))
        if self.host == "codex":
            return self.codex_events({"type": "message", "role": "assistant", "content": [
                {"type": "output_text", "text": "Original native evidence inspected; no goal acceptance."}]}, self.posts)
        return self.claude_events(data, None, None)
