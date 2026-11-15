"""Bounded public Primary native specialist fixture; no private setup."""
import json
import re
from pathlib import Path

from run_live_instruction_discovery import private_file
from run_native_workspace_policy_fixture import ScriptedResponses, code_call, code_results

PREPARATION = "dolgorae.managed-preparation/v1"
SEMANTIC = "dolgorae.external-specialist-request/v5"
NATIVE = "dolgorae_specialists_native"
CONTROL = "dolgorae_specialist_consumer"


def decoded_result(value):
    """Preserve raw evidence and decode the observed code-mode JSON string."""
    for _ in range(3):
        if not isinstance(value, str):
            break
        value = json.loads(value)
    if not isinstance(value, dict):
        raise ValueError("native JSON result is not an object")
    return value


class SpecialistResponses(ScriptedResponses):
    def __init__(self, root: Path, disabled: bool):
        super().__init__(concurrent_requests=True)
        self.root = root
        self.disabled = disabled
        self.steps_by_case = {}
        self.previous = {}
        self.member = None
        self.membership_revision = None
        self.sources = None
        self.candidate = None
        self.accepted = {}
        self.tasks = {}
        self.raw = []
        self.completed_cases = set()
        self.specialist_turns = 0
        self.issued_cases = set()

    def respond(self, request):
        with self.lock:
            self.posts += 1
            if self.posts > 32 or not isinstance(request, dict):
                raise ValueError("native specialist Responses budget exceeded")
            users = [v for v in request.get("input", []) if v.get("type") == "message" and v.get("role") == "user"]
            text = json.dumps(users[-1], ensure_ascii=False) if users else ""
            if "NATIVE_TASK_" in text and "NATIVE_PRIMARY " not in text:
                self.specialist_turns += 1
                if self.specialist_turns > 2:
                    raise ValueError("native specialist Turn budget exceeded")
                case = "inline" if "NATIVE_TASK_INLINE" in text else "artifact"
                output = "NATIVE_SPECIALIST_INLINE" if case == "inline" else "NATIVE_SPECIALIST_ARTIFACT:" + "λ" * 600000
                self.issued_cases.add("specialist-" + case)
                return self.events({"type": "message", "role": "assistant", "id": f"specialist-{case}",
                                    "content": [{"type": "output_text", "text": output}]})
            cleanup = re.search(r"NATIVE_CLEANUP ROOT=([0-9a-f-]{36}) MEMBER=([0-9a-f-]{36}) REV=([0-9]+)", text)
            if cleanup:
                root_ref, member_ref, revision = cleanup.groups()
                if not getattr(self, "cleanup_issued", False):
                    self.cleanup_issued = True
                    return self.events(code_call(request, "native-cleanup", [("release", NATIVE, {"schema": SEMANTIC, "operation": "release_specialist",
                        "scope_kind": "orchestrated_session", "scope_ref": root_ref, "member_ref": member_ref, "expected_revision": int(revision)})]))
                result = decoded_result(code_results(request, "native-cleanup", ["release"])["release"])
                if not result.get("ok") or result["facts"]["member"]["state"] != "released":
                    raise ValueError("native cleanup release refused")
                private_file(self.root / "native-cleanup-release.json", json.dumps(result).encode())
                return self.events({"type": "message", "role": "assistant", "id": "native-cleanup-final",
                                    "content": [{"type": "output_text", "text": "NATIVE_CLEANUP final"}]})
            match = re.search(r"NATIVE_PRIMARY (inline|artifact|disabled) ROOT=([0-9a-f-]{36})", text)
            if not match:
                raise ValueError("native specialist task correlation missing")
            case, root_ref = match.groups()
            step = self.steps_by_case.get(case, 0)
            self.issued_cases.add(case)
            if case in self.previous:
                call_id, labels = self.previous[case]
                results = code_results(request, call_id, labels)
                self.raw.append({"call_id": call_id, "results": results})
                private_file(self.root / f"native-specialist-raw-{self.posts}.json", json.dumps(self.raw, ensure_ascii=False).encode())
                values = {key: decoded_result(value) for key, value in results.items()}
                for label, value in values.items():
                    if label in ("denied", "changed", "foreign", "wrong-member") and value.get("schema") != "dolgorae.managed-preparation-response/v1":
                        raise ValueError("private preparation refusal claimed another contract")
                    if label in ("denied", "changed", "foreign", "wrong-member", "second-spawn", "write-denied"):
                        if value.get("ok") is True or "sources" in value or "accepted_task_identity" in value:
                            raise ValueError("forbidden preparation accepted")
                        error = value.get("error") or value.get("semantic", {}).get("error") or {}
                        if label == "changed" and error.get("code") != "IDEMPOTENCY_CONFLICT":
                            raise ValueError("changed preparation did not conflict with the original key")
                        if label in ("denied", "foreign", "second-spawn", "write-denied") and (error.get("code") != "SPECIALIST_POLICY_DENIED" or error.get("required_action") != "fix_request" or error.get("effect") != "none"):
                            raise ValueError("policy refusal lost its typed code, action or effect")
                        continue
                    if label == "status":
                        data = value.get("data", {})
                        if value.get("ok") is not True or data.get("mode") != "native_only" or data.get("allowed_role_refs") != ([] if self.disabled else ["reviewer"]) or data.get("scope") != {"scope_kind": "orchestrated_session", "scope_ref": root_ref}:
                            raise ValueError("native-only status or role authority failed")
                    elif label in ("prepare", "replay"):
                        if label == "replay" and value["sources"] != self.sources:
                            raise ValueError("preparation replay changed its original sources")
                        self.sources = value["sources"]
                    elif label == "install":
                        self.candidate = value["facts"]["candidate"]
                    elif label == "spawn":
                        self.member = value["facts"]["member"]["member_ref"]
                        self.membership_revision = value["facts"]["member"]["revision"]
                        private_file(self.root / "specialist-member.json", json.dumps(value["facts"]["member"]).encode())
                    elif label == "prepare-task":
                        if value.get("schema") != "dolgorae.managed-preparation-response/v1" or value.get("ok") is not True:
                            raise ValueError("private preparation response contract missing")
                        self.accepted[case] = value["accepted_task_identity"]
                    elif label == "replay-task":
                        if value["accepted_task_identity"] != self.accepted[case] or value.get("lookup", {}).get("state") != "accepted":
                            raise ValueError("accepted original-key preparation replay missing")
                    elif label == "assign":
                        self.tasks[case] = value["facts"]["task"]["task_ref"]
                    elif label == "release":
                        if value.get("ok") is not True or value.get("facts", {}).get("member", {}).get("state") != "released":
                            raise ValueError("native member release was not verified")
                    elif label == "collect":
                        if not value.get("ok") or value["facts"]["task"]["content_availability"] != "available":
                            raise ValueError("native complete result unavailable")
                        private_file(self.root / f"specialist-{case}.json", json.dumps({"task_ref": self.tasks[case], "native_result": value}).encode())
            scope = {"scope_kind": "orchestrated_session", "scope_ref": root_ref}
            prep = {"schema": PREPARATION, "operation": "prepare_provisioning", "role_ref": "reviewer", "idempotency_key": "native-install"}
            task = {"purpose": "research", "brief": "NATIVE_TASK_" + case.upper(), "contexts": [{"id": f"{index:02}" + "c" * 126, "content": f"FIXTURE_CONTEXT_{index}", "provenance": "accountless fixture"} for index in range(64)] if case == "inline" else [], "criteria": [], "expected_output": "text"}
            prepare_task = {"schema": PREPARATION, "operation": "prepare_assignment", "member_ref": self.member,
                            "task_body": task, "deadline_seconds": 120, "idempotency_key": "native-" + case}
            calls = []
            if case == "disabled":
                if step == 0:
                    calls = [("status", CONTROL, {"schema": "dolgorae.consumer-control/v1", "action": "status", "target": {"kind": "context"}})]
                elif step == 1:
                    calls = [("denied", NATIVE, prep)]
            elif case == "inline":
                if step == 0:
                    calls = [("status", CONTROL, {"schema": "dolgorae.consumer-control/v1", "action": "status", "target": {"kind": "context"}})]
                elif step == 1:
                    calls = [("prepare", NATIVE, prep), ("foreign", NATIVE, {**prep, "role_ref": "forbidden", "idempotency_key": "foreign"})]
                elif step == 2:
                    calls = [("replay", NATIVE, prep), ("changed", NATIVE, {**prep, "role_ref": "forbidden"})]
                elif step == 3:
                    calls = [("install", NATIVE, {"schema": SEMANTIC, "operation": "install_provisioning", "sources": self.sources, "idempotency_key": "native-install", "expected_revision": None})]
                elif step == 4:
                    calls = [("spawn", NATIVE, {"schema": SEMANTIC, "operation": "spawn_specialist", **scope, "provisioning_ref": self.candidate["provisioning_ref"], "role_ref": "reviewer", "agent_configuration_digest": self.candidate["agent_configuration_digest"], "idempotency_key": "native-spawn", "expected_revision": None})]
                elif step == 5:
                    calls = [("prepare-task", NATIVE, prepare_task), ("wrong-member", NATIVE, {**prepare_task, "member_ref": "019c1000-0000-7000-8000-000000000001", "idempotency_key": "wrong-member"}),
                             ("second-spawn", NATIVE, {"schema": SEMANTIC, "operation": "spawn_specialist", **scope, "provisioning_ref": self.candidate["provisioning_ref"], "role_ref": "reviewer", "agent_configuration_digest": self.candidate["agent_configuration_digest"], "idempotency_key": "native-second-spawn", "expected_revision": None})]
            if case == "artifact" and step == 0:
                calls = [("prepare-task", NATIVE, prepare_task)]
            assignment_step = 6 if case == "inline" else 1
            if not self.disabled and step == assignment_step:
                calls = [("assign", NATIVE, {"schema": SEMANTIC, "operation": "assign_specialist_task", **scope, "member_ref": self.member,
                    "assignment": {"task_body": task, "accepted_task_identity": self.accepted[case], "deadline_seconds": 120},
                    "idempotency_key": "native-" + case, "expected_revision": None})]
                if case == "inline":
                    denied = {**calls[0][2], "idempotency_key": "native-write-denied", "assignment": {**calls[0][2]["assignment"], "accepted_task_identity": {**self.accepted[case], "execution_intent": "canonical_workspace_write"}}}
                    calls.insert(0, ("write-denied", NATIVE, denied))
            elif not self.disabled and step == assignment_step + 1:
                calls = [("await", NATIVE, {"schema": SEMANTIC, "operation": "await_specialist_task", **scope, "member_ref": self.member, "task_ref": self.tasks[case], "timeout_seconds": 120}), ("replay-task", NATIVE, prepare_task)]
            elif not self.disabled and step == assignment_step + 2:
                calls = [("collect", NATIVE, {"schema": SEMANTIC, "operation": "collect_specialist_task", **scope, "member_ref": self.member, "task_ref": self.tasks[case]})]
            if case == "artifact" and step == assignment_step + 3:
                calls = [("release", NATIVE, {"schema": SEMANTIC, "operation": "release_specialist", **scope, "member_ref": self.member, "expected_revision": self.membership_revision})]
            self.steps_by_case[case] = step + 1
            if calls:
                call_id = f"native-{case}-{step}"
                self.previous[case] = (call_id, [label for label, _, _ in calls])
                return self.events(code_call(request, call_id, calls))
            self.previous.pop(case, None)
            self.completed_cases.add(case)
            return self.events({"type": "message", "role": "assistant", "id": f"primary-{case}",
                                "content": [{"type": "output_text", "text": f"NATIVE_PRIMARY {case} final"}]})

    def events(self, item):
        identity = "native-specialist-response-" + str(self.posts)
        return [{"type": "response.created", "response": {"id": identity}},
                {"type": "response.output_item.done", "item": item},
                {"type": "response.completed", "response": {"id": identity, "usage": {"input_tokens": 0, "output_tokens": 0, "total_tokens": 0}}}]
