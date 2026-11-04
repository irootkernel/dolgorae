"""TASK085-only adapters for the existing opted-in production host harness."""
from __future__ import annotations

import copy
import json
import subprocess
import time
from pathlib import Path

import run_native_production_host as host
from run_native_workspace_policy_fixture import code_call, code_results

LOSS = "TASK085_DISCARDED_FIRST_MERGE_RETURN"


def start_control_plane(root: Path, driver: Path, environment: dict):
    outputs = [root / "control-plane-stdout.local", root / "control-plane-stderr.local"]
    for path in outputs:
        host.private(path, b"")
    with outputs[0].open("ab") as stdout, outputs[1].open("ab") as stderr:
        return subprocess.Popen([str(driver), "--task085-control-plane", str(root)],
                                env=environment, stdin=subprocess.PIPE, stdout=stdout, stderr=stderr)


def stop_control_plane(process, deadline: float):
    try:
        process.communicate(input=b"stop\n", timeout=max(0.1, deadline-time.monotonic()))
    except subprocess.TimeoutExpired as error:
        process.kill()
        process.communicate(timeout=1)
        raise RuntimeError("task085_control_plane_shutdown_timed_out") from error
    host.require(process.returncode == 0, "task085_control_plane_failed")


def verify_process_absence(binary: Path, root: Path, environment: dict, deadline: float):
    budget = deadline-time.monotonic()
    host.require(budget > 0, "task085_process_absence_budget_exhausted")
    result = subprocess.run([str(binary), "runtime", "orphan", "inspect", "--owner-root-under", str(root)],
                            env=environment, capture_output=True, text=True, timeout=budget)
    host.require(result.returncode == 0, "task085_process_absence_inspection_failed")
    observed = json.loads(result.stdout)
    host.require(observed.get("ok") is True and isinstance(observed.get("data", {}).get("candidates"), list),
                 "task085_process_absence_inspection_invalid")
    host.private(root / "owned-process-absence.json", host.encoded(observed))
    host.require(not observed["data"]["candidates"], "task085_owned_processes_remain")


def trace(managed: bool) -> dict:
    return {"scenario":"TASK085", "managed":managed, "deadline_seconds":180,
            "cleanup_seconds":60, "owner_cleanup_seconds":30, "orphan_cleanup_seconds":30, "combined_post_limit":12, "semantic_call_limit":8,
            "delivery_limit":32, "per_input_bytes":host.BODY, "source_tasks":1,
            "synthesis_tasks":1, "podway_operations":0,
            "loss":"native code wrapper discards completed merge value" if managed else
                   "proxy discards backend merge reply, restarts backend and replays exact native frame"}


class Responses(host.Responses):
    def __init__(self, root: Path, selected: str):
        self.managed = json.loads((root / "input.json").read_text())["task085_managed"]
        self.step = 0
        self.last_call = None
        self.last_outer = None
        self.merge_lookup = None
        self.synthesis = None
        self.first_observation = None
        self.native_tool_name = None
        self.discovery_call = None
        super().__init__(root, selected)

    def observe(self):
        config = json.loads((self.root / "input.json").read_text())
        environment = host.host_environment(self.root)
        environment[host.OPT_IN] = "1"
        return host.run_phase(Path(config["binary"]), "--task085-observe", self.root,
                              environment, float((self.root / "deadline").read_text()))

    def executor(self, data):
        serialized = json.dumps(data.get("input", []))
        if self.managed and ("TASK085_MANAGED_PRIMARY" in serialized or "task085-root-" in serialized):
            return self.host_reply(data)
        self.executor_posts += 1
        host.require(self.executor_posts <= 2, "task085_executor_post_budget")
        users = [item for item in data.get("input", []) if item.get("type")=="message" and item.get("role")=="user"]
        current = json.dumps(users[-1]) if users else ""
        if "TASK085_SYNTHESIS" in current:
            host.require(self.executor_posts == 2, "synthesis_preceded_original_findings")
            text = "Explicit synthesis of the original TASK085 findings; semantic acceptance remains due."
        else:
            host.require("TASK085_SOURCE" in current and self.executor_posts == 1, "unexpected_task085_executor_turn")
            text = (Path(__file__).resolve().parents[2] / "docs/protocol/examples/v014-identity-findings.valid.json").read_text()
        return self.codex_events({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]},self.posts)

    def received(self, data):
        if self.last_call is None:
            return
        if self.managed and self.step == 1:
            outputs = [v for v in data.get("input", []) if v.get("type")=="custom_tool_call_output" and v.get("call_id")==self.last_outer]
            host.require(len(outputs)==1 and LOSS in json.dumps(outputs[0]), "actual_wrapper_loss_not_observed")
            observation = self.observe()
            host.require(observation["merge_lookup"]["state"]=="accepted" and observation["artifact_verified"], "lost_merge_did_not_commit")
            receipt = observation["merge_lookup"]["receipt"]
            host.require(receipt["receipt_digest"] not in json.dumps(outputs[0]), "discarded_merge_receipt_reached_primary")
            self.first_observation = observation
            host.private(self.root / "task085-wrapper-loss.json", host.encoded({"outer_call":self.last_outer,"loss":LOSS,"accepted_lookup":observation["merge_lookup"],"root":observation["root"]}))
            return
        if self.managed or self.host == "codex":
            output = code_results(data,self.last_outer,[self.last_call])[self.last_call]
            if self.managed:
                host.require(isinstance(output,str),"managed_task085_return_not_native_text")
                header = json.loads(output.partition("\n")[0])
            else:
                emitted = [json.loads(line) for line in (self.root / "tool-replies.jsonl").read_text().splitlines()]
                matches = [item for item in emitted if item["request"] == self.pending[self.last_call]]
                host.require(len(matches) == 1 and matches[0]["reply"].get("result") == output,
                             "protected_code_mode_result_differs_from_actual_mcp")
                host.require(isinstance(output, dict) and output.get("isError") is not True
                             and isinstance(output.get("content"), list) and output["content"],
                             "protected_code_mode_native_result_missing")
                header = json.loads(output["content"][0]["text"])
                self.pending.clear()
        else:
            previous = len(self.returns)
            self.verify_pending(data)
            host.require(len(self.returns)==previous+1,"protected_task085_return_missing")
            header = self.returns[-1]
        host.require(header.get("schema")=="dolgorae.native-specialist-response/v1" and header.get("ok") is True,"native_task085_semantic_refusal")
        facts = header["facts"]
        if header["operation"] == "lookup_operation":
            lookup = facts["lookup"]
            host.require(lookup["state"]=="accepted","native_original_key_not_accepted")
            if lookup["namespace"]["operation_kind"]=="merge_group_results":
                observed = self.observe()
                host.require(lookup==observed["merge_lookup"],"native_merge_lookup_differs_from_original")
                if self.first_observation is None: self.first_observation = observed
                self.merge_lookup = lookup
            else:
                host.require(lookup["namespace"]["operation_kind"]=="assign_synthesis" and lookup["current_outcome"]=="completed" and lookup["result_ref"] and lookup["result_digest"],"native_synthesis_result_missing")
        elif header["operation"] == "merge_group_results":
            if self.managed:
                host.require(facts["receipt"]==self.merge_lookup["receipt"],"managed_merge_replay_replaced_original")
        elif header["operation"] == "assign_synthesis":
            self.synthesis = facts
            host.require(facts["receipt"]["namespace"]["operation_kind"]=="assign_synthesis","synthesis_used_ordinary_receipt")
        elif header["operation"].startswith("await_"):
            host.require(facts["wait_timed_out"] is False,"native_synthesis_wait_timed_out")
        elif header["operation"].startswith("collect_"):
            host.require(facts["task"]["state"] in ("completed","completed_not_delivered","delivered") and header.get("evidence"),"native_synthesis_original_result_not_selected")
        self.witnesses.append({"post":self.posts,"call_id":self.last_call,"operation":header["operation"],"header":header})

    def host_reply(self, data):
        self.host_posts += 1
        host.require(self.host_posts <= 8,"task085_host_message_budget")
        if not self.managed and self.host == "codex" and self.native_tool_name is None:
            if self.discovery_call is None:
                self.discovery_call = "task085-protected-tool-metadata"
                item = code_call(data, self.discovery_call, [])
                item["input"] = ('const found = ALL_TOOLS.filter(t => t.name === "mcp__dolgorae__specialist_native"); '
                                 'if (found.length !== 1) throw new Error("protected native tool metadata missing"); '
                                 'text({label:"native-tool",result:found[0].name});')
                return self.codex_events(item, self.posts)
            name = code_results(data, self.discovery_call, ["native-tool"])["native-tool"]
            host.require(name == "mcp__dolgorae__specialist_native", "protected_native_tool_metadata_changed")
            self.native_tool_name = name
        self.received(data)
        prepared = json.loads((self.root / "task085-prepared.json").read_text())
        operations = ["merge","lookup"] + (["replay"] if self.managed else []) + ["synthesis","await","collect","synthesis_lookup"]
        if self.step == len(operations):
            self.last_call = None
            self.pending.clear()
            if self.managed or self.host=="codex":
                return self.codex_events({"type":"message","role":"assistant","content":[{"type":"output_text","text":"TASK085 native observations complete; no goal acceptance."}]},self.posts)
            return self.claude_events(data,None,None)
        operation = operations[self.step]
        base = {"schema":host.SCHEMA,"scope_kind":prepared["scope_kind"],"scope_ref":prepared["scope_ref"]}
        if operation in ("merge","replay"):
            request = copy.deepcopy(prepared["merge"])
        elif operation in ("lookup","synthesis_lookup"):
            namespace = copy.deepcopy(prepared["namespace"])
            if operation=="synthesis_lookup": namespace.update(operation_kind="assign_synthesis",idempotency_key="task085-synthesis")
            request = {"schema":host.SCHEMA,"operation":"lookup_operation","namespace":namespace}
        elif operation=="synthesis":
            request = {**base,"operation":"assign_synthesis","member_ref":prepared["member_ref"],"merged_findings_digest":self.merge_lookup["result_digest"],"merged_input":self.merge_lookup["result_ref"],"assignment":prepared["synthesis_assignment"],"idempotency_key":"task085-synthesis","expected_revision":None}
        else:
            request = {**base,"operation":("await_" if operation=="await" else "collect_")+("specialist_task" if self.managed else "external_specialist_task"),"member_ref":prepared["member_ref"],"task_ref":self.synthesis["task"]["task_ref"]}
            if operation=="await": request["timeout_seconds"]=60
        self.step += 1
        self.calls += 1
        host.require(self.calls<=8,"task085_semantic_call_budget")
        self.last_call = ("task085-root-" if self.managed else "call_native_" if self.host=="codex" else "toolu_native_")+str(self.calls)
        if self.managed:
            self.last_outer = self.last_call+"-wrapper"
            item = code_call(data,self.last_outer,[(self.last_call,"dolgorae_specialists_native",request)])
            if operation=="merge":
                item["input"] = "await tools.dolgorae_specialists_native("+json.dumps(request)+"); throw new Error("+json.dumps(LOSS)+");"
            item["input"] = '// @exec: {"yield_time_ms": 65000}\n' + item["input"]
            return self.codex_events(item,self.posts)
        self.pending[self.last_call] = copy.deepcopy(request)
        if self.host == "codex":
            self.last_outer = self.last_call + "-wrapper"
            item = code_call(data, self.last_outer, [])
            item["input"] = ("text({label:"+json.dumps(self.last_call)+",result:await tools."
                             +self.native_tool_name+"("+json.dumps(request)+")});")
            item["input"] = '// @exec: {"yield_time_ms": 65000}\n' + item["input"]
            return self.codex_events(item, self.posts)
        selected = host.native_tool(data.get("tools",[]),"specialist_native")
        host.require(selected,"protected_native_task085_tool_missing")
        if self.host=="codex":
            item={"type":"function_call","id":"fc_task085_"+str(self.calls),"call_id":self.last_call,"name":selected[0],"arguments":json.dumps(request)}
            if selected[1]: item["namespace"]=selected[1]
            return self.codex_events(item,self.posts)
        return self.claude_events(data,{"type":"tool_use","id":self.last_call,"name":selected[0],"input":{}},{"type":"input_json_delta","partial_json":json.dumps(request)})

    def extra_calls(self, data, row, evidence):
        return []

    def sample(self):
        from run_native_research_group import owned_process_sample
        config = json.loads((self.root / "input.json").read_text())
        host.require(self.control_process.poll() is None, "task085_control_plane_not_live")
        sample = owned_process_sample(self.control_process.pid,self.root / "home",Path(config["binary"]),Path(config["codex"]))
        host.require(sample["live_workers"]==(2 if self.managed else 1) and sample["live_profile_servers"]<=3 and sample["processes"]<=16,"task085_owned_process_bound_or_liveness")
        sample["generations"] = sorted(sample["generations"])
        return sample

    def finish(self):
        observation = self.observe()
        prepared = json.loads((self.root / "task085-prepared.json").read_text())
        host.require(self.failure is None and self.executor_posts==2 and self.last_call is None,"task085_native_turns_incomplete")
        host.require(observation["merge_lookup"]==self.merge_lookup and observation["artifact_verified"],"original_merge_identity_changed")
        host.require(prepared["task_count_before"] == 1 and self.first_observation["task_count"] == 1
                     and observation["task_count"] == 2, "task085_accepted_task_count_changed")
        source_turn = prepared["member_before"]["latest_turn"]
        host.require(source_turn and self.first_observation["member"]["latest_turn"] == source_turn,
                     "task085_source_turn_changed_during_merge")
        host.require(observation["member"]["latest_turn"] and observation["member"]["latest_turn"] != source_turn,
                     "task085_synthesis_did_not_use_own_turn")
        host.require(observation["rounds"]==prepared["caller_rounds_before"]+1,"synthesis_did_not_spend_exactly_one_round")
        host.require(observation["synthesis_lookup"]["current_outcome"]=="completed" and observation["synthesis_lookup"]["result_ref"],"synthesis_did_not_publish_own_result")
        host.require(observation["synthesis_lookup"]["receipt"]["operation_ref"]!=self.merge_lookup["receipt"]["operation_ref"],"synthesis_reused_merge_operation")
        expected = "managed_native" if self.managed else "external_native"
        host.require(observation["invocation"]["origin"]==expected,"native_origin_substituted")
        host.require(self.first_observation["rounds"]==prepared["caller_rounds_before"],"merge_spent_assignment_round")
        host.require(observation["source_coverage"]==prepared["source_coverage"],"original_source_coverage_changed")
        host.require(self.posts<=12 and self.calls<=8,"task085_case_budget")
        if self.managed:
            first = self.first_observation
            for key in ("runtime","profile"):
                host.require(prepared["root_before"][key]==first["root"][key],"first_merge_replaced_live_primary_"+key)
            for key in ("runtime","thread_id","profile"):
                host.require(first["root"][key]==observation["root"][key] and observation["root"][key],"live_root_identity_changed_"+key)
            host.require(observation["external_context"] is None and observation["invocation"]==first["invocation"],"managed_origin_was_replaced")
            host.require(observation["invocation"]["managed_call"]["session_id"]==prepared["scope_ref"],"managed_root_call_missing")
            host.require(first["root"]["latest_turn"] == observation["root"]["latest_turn"]
                         == observation["invocation"]["managed_call"]["source_turn_id"],
                         "task085_primary_turn_changed")
        else:
            host.require(observation["external_context"] and (self.root / "task085-backend-reply-loss.json").is_file(),"protected_original_invocation_or_loss_missing")
        return {"trace":trace(self.managed),"observations":observation,"owned_process_sample":self.sample(),"witnesses":self.witnesses,"posts":self.posts,"host_posts":self.host_posts,"executor_posts":self.executor_posts,"count_token_posts":self.count_posts,"origin":expected,"passed":True}
