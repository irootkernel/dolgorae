#!/usr/bin/env python3
"""Accountless current-Aquarium consumption of real producer facts, with native roles."""
from __future__ import annotations

import argparse
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from run_native_host_invocation_fixture import host_environment, native_session, run_host
from run_native_integrated_acceptance import successor_aquarium_sources
from run_native_production_host import PINS, PodwayDistribution, model_request, private as private_bytes, require
from run_native_workspace_policy_fixture import code_results

OPT_IN = "DOLGORAE_RUN_NATIVE_AQUARIUM_CONSUMER"
CASES = ("current", "wrong_candidate", "wrong_goal", "wrong_attempt", "incomplete", "superseded", "wrong_schema")
BODY = 4 * 1024 * 1024
BEGIN, END = "AQ86_CHILD_INPUT_BEGIN\n", "\nAQ86_CHILD_INPUT_END"


def canonical(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()


def private(path, value):
    private_bytes(path, value if isinstance(value, bytes) else canonical(value))


def claude_handback(block):
    """Invert the selected native Agent's observed public hand-back framing."""
    require(isinstance(block, str) and block.startswith("[Subagent hand-back]"), "native_agent_handback_missing")
    framed, separator, suffix = block.rpartition("\nagentId: ")
    require(separator and "The report follows:\n" in framed, "native_agent_handback_incomplete")
    handle = suffix.split("\n", 1)[0]
    require(handle and handle.strip() == handle, "native_agent_handle_missing")
    lines = framed.split("The report follows:\n", 1)[1].split("\n")
    require(all(line.startswith("  ") for line in lines), "native_agent_report_framing_changed")
    return handle, "\n".join(line[2:] for line in lines)


def content(value):
    body = value if isinstance(value, bytes) else canonical(value)
    return {"byte_length": len(body), "sha256": "sha256:" + hashlib.sha256(body).hexdigest()}


def user_text(request, host):
    messages = request.get("input" if host == "codex" else "messages", [])
    users = [v for v in messages if v.get("role") == "user"]
    if not users:
        return ""
    value = users[-1].get("content", [])
    return value if isinstance(value, str) else "\n".join(v.get("text", "") for v in value if isinstance(v, dict))


def prepare(root, role, context):
    source = root / "aquarium-source"
    scripts = source / "skills/goal-evaluator/scripts"
    modules = []
    for name in ("prepare_assignment", "render_dispatch"):
        spec = importlib.util.spec_from_file_location("aq_current_" + name, scripts / (name + ".py"))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        modules.append(module)
    path = "references/executor-guidance.md" if role == "executor" else "skills/goal-evaluator/SKILL.md"
    instructions = (source / path).read_text() + "\n\nResolved ordinary dependency:\n" + (source / "references/ordinary-agent-orchestration.md").read_text()
    assignment = {"schema": "aquarium.assignment/v3", "role": role,
        "work": {"task": "TASK-086", "epic": "EPIC-021", "objective": "Consume complete authentic producer facts and assess this isolated current-consumer placement.", "original_criteria": context["original_criteria"]},
        "target": context["target"], "authority": {"allowed": ["Read only the complete original facts supplied in this assignment", "Return the complete current-v3 report"],
            "excluded": ["Source or runtime mutation", "Podway Record/Accept/completion", "Nested delegation", "Account access", "User or whole-Epic acceptance"]},
        "procedure": context["procedure"], "native": {"harness": context["host"], "agent": "unknown until the Primary observes the actual native handle", "lifecycle": "not-dispatched"},
        "evidence": [{"kind": "complete-original-producer-facts", "content": content(context["facts"])}], "gaps": [],
        "purpose": "Bounded ordinary native collection" if role == "executor" else "Independent ordinary report-only assessment",
        "instructions": instructions, "deliverables": ["Complete source-bound ordinary-v3 report"],
        "success_criteria": ["Preserve all original global criterion rows and current placement applicability; do not infer global passage"], "checkpoint": "consumer-fixture"}
    prepared = modules[0].prepare(source, assignment)
    dispatch = modules[1].render_dispatch(prepared)
    require(prepared["schema"] == "aquarium.assignment/v3" and dispatch["schema"] == "aquarium.prepared-dispatch/v3", "current_ordinary_helper_branch_changed")
    context = {**context, "assignment": prepared, "assignment_content": content(prepared)}
    message = dispatch["dispatch_text"] + "\n" + BEGIN + canonical(context).decode() + END
    require(len(message.encode()) < 1024 * 1024, "ordinary_dispatch_outside_case_bound")
    return message, context


def role_report(context):
    assignment = context["assignment"]
    role = assignment["role"]
    report = {"schema": "aquarium.executor-handoff/v3" if role == "executor" else "aquarium.evaluator-assessment/v3",
        "work": assignment["work"], "target": context["target"], "authority": assignment["authority"], "procedure": context["procedure"],
        "native": {"harness": context["host"], "agent_ref": None, "lifecycle": "report-returned", "private_telemetry": "unknown"},
        "assignment": {"schema": assignment["schema"], "content": context["assignment_content"]},
        "evidence": [{"kind": "complete-original-producer-facts", "content": content(context["facts"]),
            "producer_tasks": [v["identity"]["task_ref"] for v in context["facts"]]}],
        "gaps": [{"owner": "workflow Coordinator", "obligation": "Assess the complete real Task after all required evidence, gates and reviews; this fixture supplies no global pass."}]}
    if role == "executor":
        report.update(deliverables={"delivered": ["Complete original producer facts and immutable source links"], "missing": []},
            changed_artifacts=[], results={"source_count": len(context["facts"]), "originals_consumed": True}, feedback=[], settlement="completed", next_owner="native Evaluator")
    else:
        rows = [{"criterion_id": key, "canonical_source": text, "state": "unverified",
            "applicability": "Original global obligation retained for this exact supplied candidate; only the declared fixture checkpoint is assessed here.",
            "evidence": report["evidence"], "provenance": "Supplied complete producer facts and independent native collection result; no whole-Task check execution.",
            "remaining_gap": "This isolated placement does not establish the complete original Task criterion."}
                for key, text in context["original_criteria"].items()]
        report.update(executor_result=context["executor_result"], checkpoint="consumer-fixture", criteria=rows,
            checkpoint_criteria=[{"criterion_id": "fixture.original-facts-and-applicability", "state": "met", "evidence": report["evidence"]}],
            applicability={"current": True, "target": context["target"], "procedure": context["procedure"]},
            technical_review={"basis": "not-applicable: this bounded fixture is not a technical provider review", "remaining_authority": "none"},
            verification_freshness="This exact supplied candidate and placement only", checkpoint_result="pass", verdict="evidence-needed")
        variant = context["variant"]
        if variant == "wrong_candidate": report["target"] = {**report["target"], "sha256": "sha256:" + "0" * 64}
        if variant == "wrong_goal": report["procedure"] = {**report["procedure"], "goal_revision": report["procedure"]["goal_revision"] + 1}
        if variant == "wrong_attempt": report["procedure"] = {**report["procedure"], "attempt_ref": context["executor_result"]["procedure"]["attempt_ref"]}
        if variant == "incomplete": report["criteria"] = []
        if variant == "wrong_schema": report["schema"] = "aquarium.judge-assessment/v1"
    return report


class NativeConsumer:
    def __init__(self, root, host, case):
        self.root, self.host, self.case = root, host, case
        self.posts = self.counts = self.parent_posts = self.child_posts = 0
        self.failure = None
        self.phase = None
        self.pending = None
        self.handles, self.reports, self.dispatches, self.witnesses = [], {}, {}, []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args): pass
            def do_POST(self):
                try:
                    self.connection.settimeout(5)
                    require(self.headers.get("Transfer-Encoding") is None, "streaming_request_refused")
                    length = int(self.headers.get("Content-Length", "0"))
                    require(0 < length <= BODY, "native_input_outside_bound")
                    body = self.rfile.read(length)
                    data, _ = model_request(body, self.headers.get("Content-Encoding", "identity"))
                    require(self.headers.get("Authorization") in (None, "Bearer sk-ant-fixture-not-a-credential")
                        and self.headers.get("x-api-key") in (None, "sk-ant-fixture-not-a-credential"), "authentication_outside_explicit_fixture_policy")
                    endpoint = self.path.split("?", 1)[0]
                    with owner.lock:
                        owner.posts += 1
                        require(owner.posts <= 32 and (owner.deadline is None or time.monotonic() < owner.deadline), "ordinary_case_budget_exceeded")
                        private(root / f"native-http-{owner.posts}.json", data)
                        if endpoint == "/v1/messages/count_tokens":
                            owner.counts += 1
                            require(owner.counts <= 3, "ordinary_count_token_budget_exceeded")
                            wire = b'{"input_tokens":1}'
                            self.send_response(200); self.send_header("Content-Type", "application/json"); self.send_header("Content-Length", str(len(wire))); self.end_headers(); self.wfile.write(wire); return
                        require(endpoint == ("/v1/responses" if host == "codex" else "/v1/messages"), "unexpected_native_endpoint")
                        events = owner.respond(data)
                    self.send_response(200); self.send_header("Content-Type", "text/event-stream"); self.send_header("Connection", "close"); self.end_headers()
                    for event in events:
                        self.wfile.write(b"event: " + event["type"].encode() + b"\ndata: " + canonical(event) + b"\n\n")
                    self.wfile.flush()
                except (OSError, ValueError, RuntimeError, KeyError) as error:
                    owner.failure = type(error).__name__ + ": " + str(error)
                    private(root / f"native-refusal-{time.monotonic_ns()}.json", {"path": self.path, "failure": owner.failure})
                    self.send_error(400, "bounded native consumer refused input")
        self.lock = threading.Lock()
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.deadline = time.monotonic() + 180

    @property
    def base(self): return f"http://127.0.0.1:{self.server.server_port}"

    def events(self, data, *, text=None, operation=None, arguments=None):
        if self.host == "codex":
            item = {"type": "message", "role": "assistant", "id": "ordinary-message", "content": [{"type": "output_text", "text": text}]} if operation is None else {
                "type": "custom_tool_call", "name": "exec", "namespace": "functions", "call_id": self.pending["call_id"],
                "input": "text({label:" + json.dumps(operation) + ",result:await tools.multi_agent_v1__" + operation + "(" + json.dumps(arguments, ensure_ascii=False) + ")});"}
            return [{"type": "response.created", "response": {"id": f"ordinary-{self.posts}"}}, {"type": "response.output_item.done", "item": item},
                    {"type": "response.completed", "response": {"id": f"ordinary-{self.posts}", "usage": {"input_tokens": 0, "input_tokens_details": None, "output_tokens": 0, "output_tokens_details": None, "total_tokens": 0}}}]
        block = {"type": "text", "text": ""} if operation is None else {"type": "tool_use", "id": self.pending["call_id"], "name": "Agent", "input": {}}
        delta = {"type": "text_delta", "text": text} if operation is None else {"type": "input_json_delta", "partial_json": json.dumps(arguments, ensure_ascii=False)}
        return [{"type": "message_start", "message": {"id": f"ordinary-{self.posts}", "type": "message", "role": "assistant", "model": data.get("model"), "content": [], "stop_reason": None, "stop_sequence": None, "usage": {"input_tokens": 1, "output_tokens": 0}}},
                {"type": "content_block_start", "index": 0, "content_block": block}, {"type": "content_block_delta", "index": 0, "delta": delta}, {"type": "content_block_stop", "index": 0},
                {"type": "message_delta", "delta": {"stop_reason": "end_turn" if operation is None else "tool_use", "stop_sequence": None}, "usage": {"output_tokens": 1}}, {"type": "message_stop"}]

    def call(self, data, operation, arguments):
        call_id = f"ordinary-{self.phase}-{self.posts}-{operation}"
        self.pending = {"operation": operation, "call_id": call_id, "arguments": arguments}
        self.witnesses.append({"post": self.posts, "phase": self.phase, "request": copy.deepcopy(self.pending)})
        return self.events(data, operation=operation, arguments=arguments)

    def claude_result(self, data, pending):
        matches = [block for message in data.get("messages", []) if message.get("role") == "user" for block in message.get("content", []) if isinstance(block, dict) and block.get("type") == "tool_result" and block.get("tool_use_id") == pending["call_id"]]
        require(len(matches) == 1 and not matches[0].get("is_error", False), "actual_claude_agent_return_missing")
        blocks = matches[0].get("content")
        require(isinstance(blocks, list) and all(isinstance(block, dict) and block.get("type") == "text"
            and isinstance(block.get("text"), str) for block in blocks), "complete_claude_agent_text_unavailable")
        handbacks = [block["text"] for block in blocks if block["text"].startswith("[Subagent hand-back]")]
        require(len(handbacks) == 1, "complete_claude_agent_handback_missing_or_ambiguous")
        handle, report = claude_handback(handbacks[0])
        self.witnesses.append({"post": self.posts, "phase": self.phase,
            "actual_native_tool_result": copy.deepcopy(matches[0]), "native_handle": handle,
            "complete_report": content(report.encode())})
        return handle, report

    def next_role(self, data):
        role = "executor" if self.phase == "collect" else "evaluator"
        message, context = prepare(self.root, role, self.context)
        label = f"{self.phase}-{len(self.handles)}"
        self.dispatches[label] = {"message": message, "context": context}
        private(self.root / (label + "-assignment.json"), context["assignment"])
        private(self.root / (label + "-dispatch.txt"), message.encode())
        self.current_label = label
        if self.host == "codex":
            return self.call(data, "spawn_agent", {"message": message, "fork_context": False})
        return self.call(data, "Agent", {"description": "Collect original producer evidence" if role == "executor" else "Assess current consumer applicability", "prompt": message, "subagent_type": "general-purpose", "run_in_background": False})

    def respond(self, data):
        text = user_text(data, self.host)
        if BEGIN in text:
            self.child_posts += 1
            require(self.child_posts <= 3, "ordinary_native_child_budget_exceeded")
            raw = text.rsplit(BEGIN, 1)[1].split(END, 1)[0]
            context = json.loads(raw)
            require(context["host"] == self.host and context["facts"] == self.context["facts"], "native_child_original_input_changed")
            actual_role = context["assignment"]["role"]
            role_path = "references/executor-guidance.md" if actual_role == "executor" else "skills/goal-evaluator/SKILL.md"
            require((self.root / "aquarium-source" / role_path).read_text() in context["assignment"]["instructions"], "operative_role_body_missing")
            report = role_report(context)
            output = canonical(report).decode()
            self.reports[context["assignment_content"]["sha256"]] = output
            private(self.root / f"native-child-{self.child_posts}-report.json", report)
            return self.events(data, text=output)
        self.parent_posts += 1
        require(self.parent_posts <= 20, "ordinary_native_primary_budget_exceeded")
        if self.phase == "decision":
            decision = self.checkpoint("advance", None, None)
            return self.events(data, text=canonical(decision).decode())
        if self.pending is None:
            return self.next_role(data)
        pending, self.pending = self.pending, None
        if self.host == "codex":
            result = code_results(data, pending["call_id"], [pending["operation"]])[pending["operation"]]
            if pending["operation"] == "spawn_agent":
                handle = result.get("agent_id")
                require(isinstance(handle, str) and handle, "actual_native_agent_handle_missing")
                require(handle not in self.handles, "native_collection_and_evaluator_not_distinct")
                self.handles.append(handle); self.current_handle = handle
                return self.call(data, "wait_agent", {"targets": [handle], "timeout_ms": 10000})
            if pending["operation"] == "wait_agent":
                require(result.get("timed_out") is False, "native_role_result_unresolved")
                status = result.get("status", {}).get(self.current_handle)
                require(isinstance(status, dict) and isinstance(status.get("completed"), str), "native_role_not_completed")
                self.current_report = status["completed"]
                self.check_report_bytes()
                return self.call(data, "close_agent", {"target": self.current_handle})
            require(pending["operation"] == "close_agent" and isinstance(result.get("previous_status"), dict) and result["previous_status"].get("completed") == self.current_report, "native_role_close_not_bound_to_original_report")
        else:
            handle, report = self.claude_result(data, pending)
            require(handle not in self.handles, "native_role_handle_reused")
            self.handles.append(handle); self.current_handle, self.current_report = handle, report
            self.check_report_bytes()
        if self.phase == "evaluate" and self.case == "superseded" and not getattr(self, "replacement", False):
            self.replacement = True
            self.superseded = {"handle": self.current_handle, "report": self.current_report}
            self.context["predecessor"] = self.superseded
            return self.next_role(data)
        delivered = self.superseded if self.phase == "evaluate" and self.case == "superseded" else {"handle": self.current_handle, "report": self.current_report}
        report = json.loads(delivered["report"])
        failures = []
        expected_schema = "aquarium.executor-handoff/v3" if self.phase == "collect" else "aquarium.evaluator-assessment/v3"
        if report.get("schema") != expected_schema: failures.append("wrong_schema")
        if report.get("target") != self.context["target"]: failures.append("wrong_candidate")
        if report.get("procedure") != self.context["procedure"]: failures.append("wrong_goal_or_attempt")
        if delivered["handle"] != self.current_handle: failures.append("superseded")
        expected_evidence = [{"kind": "complete-original-producer-facts", "content": content(self.context["facts"]),
            "producer_tasks": [v["identity"]["task_ref"] for v in self.context["facts"]]}]
        if report.get("assignment", {}).get("content") != self.dispatches[self.current_label]["context"]["assignment_content"] or report.get("evidence") != expected_evidence:
            failures.append("incomplete_original_source_binding")
        if self.phase == "evaluate":
            rows = report.get("criteria", [])
            if (len(rows) != len(self.context["original_criteria"]) or {r.get("criterion_id") for r in rows} != set(self.context["original_criteria"])
                    or any(r.get("canonical_source") != self.context["original_criteria"][r["criterion_id"]] or not all(r.get(field) for field in ("applicability", "evidence", "provenance", "remaining_gap")) for r in rows)
                    or report.get("executor_result") != self.context["executor_result"] or report.get("checkpoint_result") != "pass"):
                failures.append("incomplete")
        decision = self.checkpoint("reject_delivery" if failures else "accept_delivery", delivered, failures)
        self.witnesses.append({"post": self.posts, "phase": self.phase, "observed_native_decision": decision})
        return self.events(data, text=canonical(decision).decode())

    def check_report_bytes(self):
        context = self.dispatches[self.current_label]["context"]
        expected = self.reports.get(context["assignment_content"]["sha256"])
        require(expected is not None and self.current_report == expected, "native_complete_report_replaced_by_preview_or_ack")
        self.witnesses.append({"post": self.posts, "phase": self.phase, "native_handle": self.current_handle, "complete_report": content(self.current_report.encode())})

    def checkpoint(self, decision, delivery, failures):
        return {"schema": "aquarium.orchestration-checkpoint/v3", "work": {"task": "TASK-086", "scope": "isolated ordinary consumer fixture"},
            "target": self.context["target"], "authority": {"allowed": ["Record and complete only the current accepted ordinary placement", "Take the declared Primary Decision after applicable assessment"], "excluded": ["User or whole-Epic acceptance", "Fabricated formal receipts"]},
            "procedure": self.context["procedure"], "native": {"harness": self.host, "observed_role_handles": self.handles},
            "evidence": [{"producer_facts": content(self.context["facts"])}], "gaps": [{"owner": "Root", "obligation": "Complete all original Task criteria and final-artifact acceptance"}],
            "decision": decision, "delivery": {"native_handle": delivery["handle"], "report": json.loads(delivery["report"])} if delivery else None,
            "last_observed_state": {"phase": self.phase, "native_handles": self.handles, "procedure": self.context["procedure"]},
            "known_assignments": list(self.dispatches), "current_criteria": self.context["original_criteria"], "consumed_review_ordinals": 0,
            "rejections": failures or [], "pending_operations": [], "next_owner": "Primary", "semantic_goal_pass": False}

    def begin(self, phase, context):
        require(self.pending is None, "prior_native_tool_result_unresolved")
        self.phase, self.context = phase, copy.deepcopy(context)

    def close(self):
        self.server.shutdown(); self.server.server_close(); self.thread.join(timeout=3)
        require(not self.thread.is_alive(), "ordinary_loopback_not_joined")


def native_checkpoint(stdout):
    found = []
    for line in stdout.read_bytes().splitlines():
        try: row = json.loads(line)
        except ValueError: continue
        if row.get("type") == "item.completed" and row.get("item", {}).get("type") == "agent_message": found.append(row["item"].get("text", ""))
        if row.get("type") == "assistant":
            found.extend(v["text"] for v in row.get("message", {}).get("content", []) if v.get("type") == "text")
    require(found, "actual_primary_checkpoint_not_returned")
    result = json.loads(found[-1])
    require(result.get("schema") == "aquarium.orchestration-checkpoint/v3", "actual_primary_checkpoint_schema_changed")
    return result



def procedure_document():
    return {"schema": "podway.procedure/v2", "id": "ordinary-consumer", "version": "1", "name": "Current native consumer", "purpose": "Keep producer facts, independent assessment and Primary progression separate.", "goal_tracking": True,
        "node_definitions": {"collect": {"type": "action", "title": "Collect authentic producer facts", "intent": "Receive the actual native collection handoff.", "items": [{"id": "report", "type": "text", "prompt": "Complete native handoff", "required": True, "max_length": 65536, "multiline": True}]},
            "evaluate": {"type": "action", "title": "Independent current assessment", "intent": "Receive an applicable native Evaluator report.", "items": [{"id": "report", "type": "text", "prompt": "Complete current assessment", "required": True, "max_length": 65536, "multiline": True}]},
            "choose": {"type": "decision", "title": "Primary applicability Decision", "objective": "Use only the applicable report.", "prompt": "Advance this fixture?", "options": [{"id": "proceed", "label": "Proceed"}, {"id": "evidence", "label": "Collect evidence"}, {"id": "superseded", "label": "Superseded"}], "reason": {"required": True}, "assessment": {"target": "session_goal", "outcomes": {"proceed": "achieved", "evidence": "not_achieved", "superseded": "superseded"}}},
            "done": {"type": "action", "title": "Retain fixture completion", "intent": "Finish only the isolated fixture.", "items": [{"id": "observed", "type": "confirm", "prompt": "Fixture only", "required": False}]}},
        "graph": {"entry": "collect", "nodes": [{"id": "collect", "use": "collect", "next": "evaluate"}, {"id": "evaluate", "use": "evaluate", "next": "choose"}, {"id": "choose", "use": "choose", "evidence_from": [{"node": "collect", "required": True, "items": ["report"]}, {"node": "evaluate", "required": True, "items": ["report"]}], "routes": {"proceed": {"to": "done", "effect": "advance"}, "evidence": {"to": "evaluate", "effect": "rework"}, "superseded": {"to": "done", "effect": "advance"}}}, {"id": "done", "use": "done", "terminal": True}]}}

def qualify(args, host, case):
    deadline = None if getattr(args, "unbounded", False) else time.monotonic() + 180
    root = Path(tempfile.mkdtemp(prefix=host[0] + "-", dir=args.scratch)).resolve(); root.chmod(0o700)
    for name in ("home", "tmp", "config", "cache", "workspace", "codex", "claude"):
        (root / name).mkdir(mode=0o700)
    successor_aquarium_sources(root, args.aquarium_archive, current_roles=True)
    facts = [json.loads(path.read_text()) for path in args.producer_facts]
    require(0 < len(facts) <= 3 and sum(path.stat().st_size for path in args.producer_facts) <= 512 * 1024, "complete_producer_facts_outside_fixture_bound")
    require(all(v.get("schema") == "dolgorae.native-producer-facts/v1" and v.get("semantic_goal_pass") is False
        and v.get("identity", {}).get("complete_result") and v.get("native_admitted_input") for v in facts), "authentic_complete_producer_facts_required")
    original_criteria = json.loads(args.task_criteria.read_text())
    require(set(original_criteria) == {"mappings", "runtime", "recovery", "delivery"}, "original_task_criteria_changed")
    environment = host_environment(root)
    distribution = PodwayDistribution(args.podway_archive, args.podway)
    public_calls = []
    def public(*argv):
        selection = [] if argv[0] in ("procedure", "version") else ["--worktree", str(root / "workspace")]
        command = [str(args.podway), "--contract-generation", "1", *selection, "--json", *argv]
        require(deadline is None or time.monotonic() < deadline, "ordinary_case_deadline_exceeded")
        index = len(public_calls)
        record = {"argv": command, "exit": None, "response": None}
        public_calls.append(record)
        private(root / f"ordinary-public-{index:03d}-attempt.json", record)
        try:
            result = subprocess.run(command, env=environment, cwd=root / "workspace", capture_output=True,
                                    timeout=None if deadline is None else min(10, deadline - time.monotonic()))
        except Exception as error:
            private(root / f"ordinary-public-{index:03d}-failure.json", {"error": type(error).__name__})
            raise
        private(root / f"ordinary-public-{index:03d}.stdout", result.stdout)
        private(root / f"ordinary-public-{index:03d}.stderr", result.stderr)
        record["exit"] = result.returncode
        value = json.loads(result.stdout); distribution.validate_reply(value)
        record["response"] = value
        require(result.returncode == 0, "actual_ordinary_consumer_public_command_failed")
        return value
    def observe(): return public("observe")
    def scope(observation):
        status = observation["result"]["status"]
        return {"workspace_ref": observation["workspace"]["uuid"], "session_ref": status["session"]["id"], "node": status["current"]["node"]["graph_node_id"], "attempt_ref": status["current"]["attempt"]["attempt_id"], "goal_revision": status["goal_revision"]}
    def fences(observation):
        s = scope(observation); revision = observation["result"]["status"]["session"]["revision"]
        return ["--if-workspace-uuid", s["workspace_ref"], "--if-session-id", s["session_ref"], "--if-session-revision", str(revision), "--if-attempt", s["attempt_ref"]]
    document = procedure_document()
    setup = subprocess.run(["/usr/bin/git", "init", "-q", str(root / "workspace")], env=environment, capture_output=True, timeout=None if deadline is None else 5)
    require(setup.returncode == 0, "ordinary_fixture_git_setup_failed")
    private(root / "workspace/procedure.json", canonical(document))
    public("init", "--idempotency-key", "ordinary-init")
    preview = public("procedure", "preview", "procedure.json")
    public("start", "--procedure", "procedure.json", "--expect-procedure-digest", preview["result"]["procedure_digest"], "--task", "Current producer consumption", "--idempotency-key", "ordinary-start")
    public("begin", "--goal", "Qualify this exact current-consumer placement", "--criterion", "proof=Complete producer provenance and current applicability", "--idempotency-key", "ordinary-begin")
    executable = getattr(args, host); require(hashlib.sha256(executable.read_bytes()).hexdigest() == PINS[host][1], "selected_native_host_changed")
    if host == "codex": require(hashlib.sha256(executable.with_name("codex-code-mode-host").read_bytes()).hexdigest() == "416721e33af8822041dc650793512b3e58d43e0be4f3a5c6187eb4dfaad3148a", "selected_code_mode_companion_changed")
    peer = NativeConsumer(root, host, case)
    peer.deadline = deadline
    passed = False
    session = None
    try:
        if host == "codex":
            config = f'model = {json.dumps(args.codex_model)}\nmodel_provider = "ordinary_fixture"\ncli_auth_credentials_store = "file"\n[features]\nmulti_agent = true\nplugins = false\nshell_snapshot = false\n[model_providers.ordinary_fixture]\nname = "Current ordinary consumer"\nbase_url = "{peer.base}/v1"\nwire_api = "responses"\nrequires_openai_auth = false\nsupports_websockets = false\nrequest_max_retries = 0\nstream_max_retries = 0\n'
            (root / "codex/config.toml").write_text(config); (root / "codex/config.toml").chmod(0o600)
        else: environment.update(ANTHROPIC_BASE_URL=peer.base, ANTHROPIC_API_KEY="sk-ant-fixture-not-a-credential")
        target = {**content(facts), "basis": "complete immutable producer facts", "producer_tasks": [v["identity"]["task_ref"] for v in facts]}
        base = {"host": host, "facts": facts, "target": target, "original_criteria": original_criteria, "variant": case}
        decisions, executor_result = [], None
        for phase in ("collect", "evaluate", "decision"):
            before = observe(); current_scope = scope(before)
            require(isinstance(current_scope["goal_revision"], int) and current_scope["goal_revision"] > 0,
                    "ordinary_fixture_requires_actual_goal_revision")
            context = {**base, "procedure": current_scope, "executor_result": executor_result}
            peer.begin(phase, context)
            prompt = "AQ86_PRIMARY: This isolated qualification explicitly authorizes the requested same-host native collection Executor and independent Goal Evaluator. Supply each complete current ordinary assignment. Never issue formal Worker/Judge receipts. Return the explicit current applicability checkpoint and allowed placement effects.\n" + canonical(context).decode()
            if host == "codex":
                argv = [str(executable), "--no-daemon", "-a", "never", "exec"]
                argv += ["resume", "--skip-git-repo-check", "--json", session, prompt] if session else ["--skip-git-repo-check", "--sandbox", "read-only", "--json", "--cd", str(root / "workspace"), prompt]
            else:
                argv = [str(executable), "--restricted", "--setting-sources", "", "--tools", "Agent", "--permission-mode", "dontAsk", "--permission-prompts", "none", "--model", args.claude_model, "-p", "--output-format", "stream-json", "--verbose"]
                if session: argv += ["--resume", session]
                argv += [prompt]
            peer.stdout = root / (phase + "-stdout.local")
            private(root / (phase + "-launch.json"), {"argv": argv, "environment": environment,
                "selected_host_sha256": PINS[host][1], "purpose": "accountless current ordinary consumer placement only"})
            output = run_host(argv, root, environment, phase, peer.deadline)
            observed_session = native_session(output, host)
            require(session is None or observed_session == session, "ordinary_primary_native_session_replaced")
            session = observed_session
            decision = native_checkpoint(output); decisions.append(decision)
            require(peer.failure is None and decision["procedure"] == current_scope, "actual_current_primary_decision_unavailable")
            if phase == "decision":
                require(decision["decision"] == "advance", "primary_fixture_decision_not_explicit")
                public("goal", "assess-criterion", "proof", "--status", "satisfied",
                    "--reason", "The original producer facts, complete native collection and independent current assessment prove this isolated placement only.",
                    "--evidence", "collect", "--evidence", "evaluate", *fences(before),
                    "--if-goal-revision", str(current_scope["goal_revision"]), "--idempotency-key", "ordinary-assess-proof")
                assessed = observe()
                require(scope(assessed) == current_scope, "ordinary_goal_assessment_changed_placement")
                public("decide", "--option", "proceed", "--reason", "The applicable current native checkpoint permits fixture progression only.", *fences(assessed),
                    "--if-goal-revision", str(current_scope["goal_revision"]), "--idempotency-key", "ordinary-decide")
                terminal = observe()
                completion_scope = scope(terminal)
                require(all(completion_scope[key] == current_scope[key] for key in
                    ("workspace_ref", "session_ref", "goal_revision"))
                    and completion_scope["node"] == "done"
                    and completion_scope["attempt_ref"] != current_scope["attempt_ref"],
                    "ordinary_decision_changed_completion_placement")
                public("complete", *fences(terminal), "--idempotency-key", "ordinary-finish")
                break
            if decision["decision"] == "reject_delivery":
                after = observe()
                require(before["result"]["status"] == after["result"]["status"] and before["result"]["active_items"] == after["result"]["active_items"], "inadmissible_placement_recorded_or_advanced")
                require(phase == "evaluate" and case != "current", "unexpected_native_delivery_rejection")
                break
            require(decision["decision"] == "accept_delivery" and decision["delivery"]["native_handle"] == peer.current_handle, "native_primary_did_not_accept_current_role")
            report = decision["delivery"]["report"]
            items = [item for item in before["result"]["active_items"] if item["item_id"] == "report"]
            require(len(items) == 1 and type(items[0]["revision"]) is int, "ordinary_current_report_item_unavailable")
            public("set", "report", canonical(report).decode(),
                "--if-workspace-uuid", current_scope["workspace_ref"], "--if-session-id", current_scope["session_ref"],
                "--if-attempt", current_scope["attempt_ref"], "--if-item-revision", str(items[0]["revision"]),
                "--idempotency-key", "ordinary-record-" + phase)
            recorded = observe()
            require(scope(recorded) == current_scope, "ordinary_record_changed_placement")
            public("complete", *fences(recorded), "--idempotency-key", "ordinary-complete-" + phase)
            executor_result = {"native_handle": decision["delivery"]["native_handle"], "report": content(report), "procedure": current_scope} if phase == "collect" else executor_result
        require((case == "current" and len(decisions) == 3 and observe()["result"]["status"]["session"]["lifecycle"] == "completed") or
                (case != "current" and decisions[-1]["decision"] == "reject_delivery"), "required_ordinary_outcome_not_observed")
        require(len(peer.handles) == (3 if case == "superseded" else 2) and len(set(peer.handles)) == len(peer.handles), "actual_independent_native_roles_missing")
        passed = True
        result = {"schema": "dolgorae.native-current-aquarium/v1", "host": host, "case": case, "passed": True, "owned_fixture": str(root),
            "posts": peer.posts, "child_posts": peer.child_posts, "native_handles": peer.handles, "primary_native_session": session,
            "decisions": decisions, "native_witnesses": peer.witnesses, "producer_facts": [content(path.read_bytes()) for path in args.producer_facts],
            "formal_aquarium_v1_qualified": False, "semantic_model_reasoning_verified": False, "global_task_pass": False}
        private(root / "ordinary-result.json", result)
        return result
    finally:
        joined = False
        try:
            peer.close()
            joined = True
        finally:
            private(root / "ordinary-native-state.json", {"handles": peer.handles, "witnesses": peer.witnesses, "pending": peer.pending, "phase": peer.phase})
            private(root / "ordinary-public-calls.json", public_calls)
            private(root / "ordinary-cleanup.json", {"owned_hosts_reaped_by_run_host": True, "loopback_joined": joined, "case_passed": passed, "native_failure": peer.failure})


def main():
    parser = argparse.ArgumentParser()
    for name in ("codex", "claude", "podway", "podway-archive", "aquarium-archive", "task-criteria", "scratch"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--producer-facts", type=Path, nargs="+", required=True)
    parser.add_argument("--codex-model", required=True)
    parser.add_argument("--claude-model", required=True)
    parser.add_argument("--hosts", nargs="+", choices=PINS, default=list(PINS))
    parser.add_argument("--cases", nargs="+", choices=CASES, default=list(CASES))
    parser.add_argument("--unbounded", action="store_true", help="Run without a whole-case or parent command execution timer")
    args = parser.parse_args()
    require(os.environ.get(OPT_IN) == "1", "explicit_current_native_consumer_opt_in_required")
    for name in ("codex", "claude", "podway", "podway_archive", "aquarium_archive", "task_criteria", "scratch"):
        setattr(args, name, getattr(args, name).resolve(strict=True))
    args.producer_facts = [path.resolve(strict=True) for path in args.producer_facts]
    results = []
    for host in args.hosts:
        for case in args.cases:
            try: results.append(qualify(args, host, case))
            except Exception as error: results.append({"host": host, "case": case, "passed": False, "failure": type(error).__name__ + ": " + str(error)})
    print(json.dumps({"schema": "dolgorae.native-current-aquarium-campaign/v1", "passed": all(v["passed"] for v in results), "cases": results, "global_task_pass": False}, sort_keys=True))
    return 0 if all(v["passed"] for v in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
