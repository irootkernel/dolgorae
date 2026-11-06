#!/usr/bin/env python3
"""Opted-in native group response loss through a separately identified ordinary image."""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import threading
import time

import native_task086
import run_native_production_host as host
from run_native_workspace_policy_fixture import code_call, code_results


def load(path):
    return json.loads(path.read_bytes())


def native_root(root):
    return root / load(root / "input.json")["host"]


class OrdinaryCase:
    def __init__(self, args, root):
        self.args, self.root = args, root
        self.env = host.host_environment(root)
        self.env[host.OPT_IN] = "1"
        self.gap = getattr(args, "scenario", "consumer-group-gaps") == "consumer-group-gaps"
        self.group = getattr(args, "scenario", "consumer-group-gaps").startswith("consumer-group")
        self.deadline = None
        self.steps = []
        self.observation_prefix = "ordinary"
        self.serve = None
        self.fault_error = None
        self.fault_thread = None
        self.stop_fault = threading.Event()
        self.prepared = None
        self.engagement = None

    def call(self, image, *arguments, pass_fds=(), expected_errors=()):
        argv = [str(image), *map(str, arguments)]
        result = subprocess.run(argv, env=self.env, cwd=self.root / "workspace",
                                capture_output=True, pass_fds=pass_fds,
                                timeout=self.operation_timeout())
        ordinal = len(self.steps)
        host.private(self.root / f"{self.observation_prefix}-{ordinal:03}.stdout", result.stdout)
        host.private(self.root / f"{self.observation_prefix}-{ordinal:03}.stderr", result.stderr)
        self.steps.append({"argv": argv, "exit": result.returncode, "ordinal": ordinal})
        envelope = json.loads(result.stdout) if result.stdout else None
        expected = (isinstance(envelope, dict) and envelope.get("ok") is False
                    and envelope.get("error", {}).get("code") in expected_errors)
        host.require(result.returncode == 0 or expected, "ordinary command failed: " + str(arguments)
                     + ": " + result.stderr.decode() + result.stdout.decode())
        return envelope

    def operation_timeout(self):
        return None

    def product(self, *arguments, **kwargs):
        return self.call(self.args.binary, *arguments, **kwargs)

    def helper(self, operation):
        return self.call(self.args.helper, operation, self.root)

    def admin(self, operation, *arguments):
        with (self.root / "operator").open("rb") as operator, (native_root(self.root) / "external-controller").open("rb") as controller:
            return self.product("__trusted-specialist-host", operation,
                "--owner-config", native_root(self.root) / "owner.json",
                "--operator-fd", operator.fileno(), "--controller-fd", controller.fileno(),
                *arguments, pass_fds=(operator.fileno(), controller.fileno()))

    def wait(self, predicate, reason):
        while not predicate():
            if threading.current_thread() is self.fault_thread and self.stop_fault.is_set():
                raise RuntimeError("fault observation stopped during failed-case cleanup")
            host.require(not (self.root / "fault/fail-safe-release").exists(), "fault hold released by fail-safe")
            if self.fault_error:
                raise RuntimeError(self.fault_error)
            time.sleep(.025)

    def prepare(self, responses):
        root = self.root
        subprocess.run(["git", "init", "-q", str(root / "workspace")], env=self.env, check=True)
        for key, value in (("user.name", "Isolated fixture"), ("user.email", "fixture@invalid")):
            subprocess.run(["git", "-C", str(root / "workspace"), "config", key, value], env=self.env, check=True)
        host.private(root / "workspace/file.txt", b'original UTF8 evidence: "quoted" ')
        subprocess.run(["git", "-C", str(root / "workspace"), "add", "file.txt"], env=self.env, check=True)
        subprocess.run(["git", "-C", str(root / "workspace"), "commit", "-q", "-m", "Fixture baseline"], env=self.env, check=True)
        initialized = self.product("init", root / "workspace")
        self.workspace = initialized["data"]["workspace_id"]
        carrier = root / "home/.dolgorae/controller-carriers/task087/bootstrap/credential"
        carrier.parent.mkdir(mode=0o700, parents=True)
        for path, instance in ((carrier, "ordinary-bootstrap"), (native_root(root) / "external-controller", "native-host-fixture")):
            self.product("controller", "credential", "create", "--kind", "automation",
                         "--instance-id", instance, "--output", path)
        self.product("operator", "credential", "initialize", "--output", root / "operator")
        host.private(root / "profile/config.toml", (
            f'model = {json.dumps(self.args.executor_model)}\nmodel_reasoning_effort = "low"\n'
            'model_provider = "production_executor"\ncli_auth_credentials_store = "file"\n'
            '[features]\nplugins = false\n'
            + ('request_permissions_tool = true\ntool_search_always_defer_mcp_tools = false\n' if self.args.scenario == "consumer-complete" else '')
            + '[model_providers.production_executor]\n'
            'name = "Numeric loopback executor"\n'
            f'base_url = "{responses.base_url}/executor/v1"\nwire_api = "responses"\n'
            'requires_openai_auth = false\nsupports_websockets = false\n'
            'request_max_retries = 0\nstream_max_retries = 0\n'
            + (f'\n[mcp_servers.fixture]\nurl = "http://127.0.0.1:{responses.interactions.mcp_http.server_port}/mcp"\ndefault_tools_approval_mode = "approve"\n' if self.args.scenario == "consumer-complete" else '')).encode())
        self.product("profile", "add", "host-fixture", "--codex-home", root / "profile",
                     "--native-subagents", "enabled", "--env", "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
                     "--env", "LANG=C", "--env", "LC_ALL=C", "--", self.args.codex)
        (root / "home/.dolgorae/roles/general/AGENTS.md").write_text(
            "Produce the exact accepted task's original evidence and limitations. Make only its explicitly authorized workspace change. Do not accept or advance any workflow.\n")
        self.prepared = self.helper("capture")
        fault = root / "fault"
        host.private(fault / "harness.pid", str(os.getpid()).encode())
        environment = dict(self.env)
        if self.gap:
            environment.update(DYLD_INSERT_LIBRARIES=str(self.args.fault_library), DOLGORAE_NATIVE_FAULT_DIR=str(fault))
        with (root / "serve.stdout").open("wb") as stdout, (root / "serve.stderr").open("wb") as stderr:
            self.serve = subprocess.Popen([str(self.args.binary), "serve", "--socket", str(root / "gateway.sock")],
                                          env=environment, cwd=root / "workspace", stdout=stdout, stderr=stderr)
        self.wait(lambda: (root / "gateway.sock").exists() or self.serve.poll() is not None, "ordinary serve did not become ready")
        host.require(self.serve.poll() is None, "ordinary serve exited")
        host.private(fault / "serve.pid", str(self.serve.pid).encode())
        if self.gap:
            self.helper("serve-identity")
        self.helper("bootstrap-start")
        self.helper("bootstrap-close")
        host.require(responses.posts == 0, "bootstrap unexpectedly created a model request")
        host.podway_fixture(root, self.args.podway, host.PodwayDistribution.EXECUTABLE,
                            self.prepared["candidate"], self.env, self.deadline)
        connection = load(root / "connection.json")
        (native_root(root) / "sessions").mkdir(mode=0o700)
        draft = {"route": host.PINS[self.args.host][2], "executable": load(root / "input.json")["host_identity"],
            "owner": {"kind": "external"}, "controller_generation": 1,
            "controller_file": str(native_root(root) / "external-controller"), "native_host_root": str(native_root(root) / "sessions"),
            "allowed_scopes": [{"kind": "workspace", "reference": self.workspace}, {"kind": "external_engagement", "reference": None}],
            "tools": ["specialist_native", "specialist_consumer"],
            "caller_scope": {"binding_ref": "local-specialist", "podway_workspace_ref": connection["workspace"],
                "podway_session_ref": connection["session"], "podway_attempt_ref": connection["attempt"],
                "delegated_placement_ref": "work", "max_rounds": 3},
            "adapter_id": "dolgorae.external/1", "adapter_version": load(root / "input.json")["adapter_version"]}
        host.private(native_root(root) / "draft.json", host.encoded(draft))
        self.admin("owner-config-create", "--workspace", root / "workspace", "--request", native_root(root) / "draft.json")
        for operation in ("namespace-setup", "host-state-init", "preparation-init"):
            self.admin(operation)
        host.private(native_root(root) / "ordinary-consumer.json", host.encoded({"kind": "delegated", "connection": connection}))
        self.admin("consumer-init", "--request", native_root(root) / "ordinary-consumer.json")
        self.admin("provisioning-prepare", "--request", root / "ordinary-provisioning-selection.json",
                   "--output", root / "ordinary-sources.json")

    def group_inputs(self, hired):
        root = self.root
        namespace = hired["facts"]["receipt"]["namespace"]
        member = hired["facts"]["member"]["member_ref"]
        for index in range(2):
            key = "source-" + str(index)
            task = {"purpose": "research", "brief": "TASK086_PRODUCER: preserve original source identities and full result bytes.\n"
                    + f"TASK086_GROUP_SOURCE_{index}: report an independent claim; preserve disagreements.",
                "contexts": [{"id": "delegated-package", "content": json.dumps(load(root / "package.json")["result"]["package"], ensure_ascii=False, separators=(",", ":")), "provenance": "caller"},
                    {"id": "requirement", "content": "Inspect the original fixture evidence.", "provenance": "caller"}],
                "criteria": [{"id": "C-1", "statement": "Report observed fixture evidence.", "source_context_ids": ["requirement"]}],
                "expected_output": "structured_findings_v1"}
            selected = {"namespace": dict(namespace, operation_kind="assign_external_specialist_task", idempotency_key="ordinary-prepare-" + key),
                "member_ref": member, "task": task, "execution_intent": "read_only", "deadline_seconds": 60, "target": None}
            host.private(native_root(root) / f"ordinary-task-{index}.json", host.encoded(selected))
            self.admin("assignment-prepare", "--request", native_root(root) / f"ordinary-task-{index}.json",
                       "--output", root / f"ordinary-accepted-{index}.json")
        return self.helper("group-inputs")

    def individual_input(self, hired, number):
        member = hired["facts"]["member"]["member_ref"]
        key = f"fixture-round-{number}"
        task = {"purpose": "change", "brief": f"Primary feedback round {number}: make only the accepted fixture change and preserve its original evidence.",
            "contexts": [{"id": "delegated-package", "content": json.dumps(load(self.root / "package.json")["result"]["package"], ensure_ascii=False, separators=(",", ":")), "provenance": "caller"}],
            "criteria": [], "expected_output": "complete_result"}
        selected = {"namespace": dict(hired["facts"]["receipt"]["namespace"], operation_kind="assign_external_specialist_task", idempotency_key=key),
            "member_ref": member, "task": task, "execution_intent": "canonical_workspace_write", "deadline_seconds": 60, "target": None}
        request = native_root(self.root) / f"ordinary-round-{number}.json"
        accepted = self.root / f"ordinary-round-{number}-accepted.json"
        host.private(request, host.encoded(selected))
        self.admin("assignment-prepare", "--request", request, "--output", accepted)
        return {"schema": host.SCHEMA, "operation": "assign_external_specialist_task", "scope_kind": "external_engagement",
            "scope_ref": self.engagement, "member_ref": member, "idempotency_key": key, "expected_revision": None,
            "assignment": {"task_body": task, "accepted_task_identity": load(accepted), "deadline_seconds": 60}}

    def prepare_drift(self):
        role = self.root / "home/.dolgorae/roles/general/AGENTS.md"
        host.private(self.root / "ordinary-original-role.md", role.read_bytes())
        role.write_bytes(b"Changed same-name source after original capture; never replace an accepted task's immutable Role.\n")
        selection = load(self.root / "ordinary-provisioning-selection.json")
        self.admin("provisioning-prepare", "--request", self.root / "ordinary-provisioning-selection.json",
                   "--output", self.root / "ordinary-role-drift-sources.json")
        selection["admission"]["effort"] = "medium"
        host.private(self.root / "ordinary-configuration-drift-selection.json", host.encoded(selection))
        self.admin("provisioning-prepare", "--request", self.root / "ordinary-configuration-drift-selection.json",
                   "--output", self.root / "ordinary-configuration-drift-sources.json")

    def settled_result(self):
        while True:
            observed = self.call(self.args.result_reader, "--ordinary-read-result", self.root)
            if "observation_error" not in observed:
                return observed
            error = observed["observation_error"]
            host.require(error["code"] in ("DISPOSITION_NOT_IDLE", "RUN_STATE_CONFLICT", "RECOVERY_REQUIRED"),
                         "original external observation failed: " + str(error))
            if error["code"] == "RECOVERY_REQUIRED":
                writer = observed["writer_observation"]
                release = writer.get("generation_release")
                assigned = load(self.root / "assigned.json")
                host.require(release is not None and release["source"]["run_id"] == assigned["member_ref"]
                             and release["phase"] != "committed"
                             and writer["state"] in ("releasing", "reserved", "blocked_unknown"),
                             "external observation has no original pending Writer release")
                if writer["state"] == "blocked_unknown":
                    self.admin("member-recover", "--scope-ref", self.engagement,
                               "--member-ref", assigned["member_ref"], "--task-ref", assigned["task_ref"])
            host.require(self.serve is not None and self.serve.poll() is None,
                         "original external Writer cleanup has not settled")
            time.sleep(1)

    def read_merge(self):
        reference = self.call(self.args.result_reader, "--ordinary-read-merge", self.root)["merged_output"]
        run, artifact = reference["artifact_owner"]["run_id"], reference["artifact"]["artifact_id"]
        body = b""
        while True:
            chunk = self.product("run", "--controller-file", native_root(self.root) / "external-controller",
                "artifact", "read", run, artifact, "--workspace", self.root / "workspace",
                "--offset", len(body), "--length", 1024)["data"]
            part = base64.b64decode(chunk["content_base64"], validate=True)
            host.require(chunk["offset"] == len(body) and len(part) == chunk["byte_length"], "merged artifact chunk changed")
            body += part
            if chunk["eof"]:
                break
        host.require(len(body) == reference["content"]["byte_length"]
            and "sha256:" + hashlib.sha256(body).hexdigest() == reference["content"]["sha256"], "merged artifact identity changed")
        merged = load(self.root / "ordinary-merge.json")["facts"]
        output = json.loads(body)
        host.require(output["coverage"] == merged["merged_identity"]["sources"], "merged coverage changed after source recovery")
        host.private(self.root / "ordinary-merge-provenance.json", host.encoded({
            "group": load(self.root / "ordinary-group.json")["facts"], "merge": merged,
            "merged_body": output, "public_artifact": reference, "synthesis": None}))

    def synthesis_input(self, hired):
        task = load(native_root(self.root) / "ordinary-task-0.json")["task"]
        task["brief"] += "\nTASK086_SYNTHESIS: retain the exact immutable merged input and its unresolved source gap."
        task["expected_output"] = "complete_result"
        member = hired["facts"]["member"]["member_ref"]
        namespace = dict(hired["facts"]["receipt"]["namespace"],
                         operation_kind="assign_synthesis", idempotency_key="fixture-synthesis")
        selected = {"namespace": namespace, "member_ref": member, "task": task,
                    "execution_intent": "read_only", "deadline_seconds": 60, "target": None}
        host.private(native_root(self.root) / "ordinary-synthesis-selection.json", host.encoded(selected))
        accepted = self.call(self.args.result_reader, "--ordinary-synthesis-input", self.root)
        host.private(self.root / "ordinary-synthesis-accepted.json", host.encoded(accepted))
        merged = load(self.root / "ordinary-merge.json")["facts"]
        request = {"schema": host.SCHEMA, "operation": "assign_synthesis", "scope_kind": "external_engagement",
            "scope_ref": self.engagement, "member_ref": member, "merged_findings_digest": merged["merged_digest"],
            "merged_input": merged["merged_identity"]["merged_output"], "assignment": {"task_body": task,
            "accepted_task_identity": load(self.root / "ordinary-synthesis-accepted.json"), "deadline_seconds": 60},
            "idempotency_key": "fixture-synthesis", "expected_revision": None}
        host.private(self.root / "ordinary-synthesis-request.json", host.encoded(request))
        return request

    def prepare_consumer(self):
        observed = self.settled_result()
        if self.group:
            native_task086.prepare_consumer(self.root, observed, self.responses)
        else:
            complete = observed["complete_result"]
            host.require(complete["terminal_outcome"] == "completed" and len(complete["changes"]) == 1
                         and complete["changes"][0]["path"] == "file.txt", "original accepted change is missing")
            number = 3 if self.args.scenario == "consumer-rounds" else 1
            originals = {"complete_result": json.dumps(complete, ensure_ascii=False, sort_keys=True, separators=(",", ":")),
                "final_response": self.responses.originals["final_response"],
                "before": ('changed original UTF8 round 2: "quoted" 界🙂\n' * 60) if number == 3 else 'original UTF8 evidence: "quoted" ',
                "after": (f'changed original UTF8 round {number}: "quoted" 界🙂\n') * 60}
            host.private(self.root / "producer-identity.json", host.encoded(observed["delegation_identity"]))
            host.private(self.root / "consumer-originals.json", host.encoded(originals))
            host.private(self.root / "settled.json", host.encoded({"engagement_revision": observed["engagement_revision"]}))
        self.producer_observation = observed

    def fault(self):
        try:
            root = self.root
            self.wait(lambda: (root / "fault/selected-request.ready").exists() and (root / "ordinary-group.json").exists(), "real source1 request and group receipt not observed")
            self.helper("select-worker")
            host.private(root / "fault/release-selection", b"release\n")
            self.wait(lambda: (root / "fault/accepted.ready").exists() and (root / "unknown-post-observed").exists(), "genuine Accepted and original model request missing")
            host.private(root / "release-source1-response", b"release\n")
            accepted = load(root / "fault/accepted.frame")["accepted"]
            observed = None
            def completed():
                nonlocal observed
                observed = self.helper("terminal")
                terminal = observed["last_terminal"]
                return terminal and terminal.get("turn_id") == accepted["turn_id"] and terminal.get("status") == "completed"
            self.wait(completed, "original source1 terminal missing")
            self.wait(lambda: self.helper("fault-worker").get("fault_injected") is True,
                      "original terminal observation did not stabilize before physical fault")
            self.wait(lambda: (root / "fault/recovery.ready").exists(), "next lifecycle recovery hold missing")
        except BaseException as error:
            self.fault_error = repr(error)

    def release_recovery(self):
        host.private(self.root / "fault/release-recovery", b"release\n")

    def restore_worker(self, run):
        observed = self.product("run", "status", run, "--workspace", self.root / "workspace")["data"]
        while observed["identity_verdict"] == "Absent":
            recovered = self.product("run", "--controller-file", native_root(self.root) / "external-controller",
                "recover", run, "--workspace", self.root / "workspace",
                expected_errors=("RUN_BUSY", "RUN_STATE_CONFLICT"))
            observed = self.product("run", "status", run, "--workspace", self.root / "workspace")["data"]
            if recovered.get("ok") is False:
                error = recovered["error"]
                host.require((error["code"] == "RUN_BUSY" and error["retryable"] is True)
                             or observed["identity_verdict"] == "Match", "original Run recovery requires a different action")
            if observed["identity_verdict"] == "Absent":
                time.sleep(.025)
        host.require(observed["identity_verdict"] == "Match" and observed["state"] == "idle"
                     and observed["active_turn_id"] is None, "original Worker not restored before member release")
        selected = load(self.root / "ordinary-selected-worker.json")["identity"]
        held = load(self.root / "fault/accepted.frame")["accepted"]
        host.require(observed["run_id"] == selected["run_id"] and observed["run_generation"] > selected["run_generation"]
                     and observed["thread_id"] == held["thread_id"], "original recovery did not retain Run/Thread with a successor Worker generation")
        host.private(self.root / "ordinary-restored-worker.json", host.encoded(observed))

    def cleanup(self):
        self.stop_fault.set()
        for name in ("release-selection", "release-ack", "release-recovery"):
            path = self.root / "fault" / name
            if not path.exists():
                host.private(path, b"cleanup\n")
        if not (self.root / "release-source1-response").exists():
            host.private(self.root / "release-source1-response", b"cleanup original response\n")
        if self.fault_thread is not None:
            self.fault_thread.join()
        # Logical cleanup is an actual product operation, never inferred from
        # process absence. An unresolved original task remains a failure.
        if self.engagement:
            self.responses.begin_cleanup()
            host.run_host(self.native_argv, self.root, self.env, "cleanup", self.deadline)
            host.require(self.responses.cleanup_complete, "original native engagement close incomplete")
        if (self.root / "ordinary-bootstrap.json").exists() and not (self.root / "ordinary-bootstrap-close.pb").exists():
            self.helper("bootstrap-close")
        if (self.root / "ordinary-bootstrap-close.pb").exists():
            self.retire_profile()
        return {"cleanup_complete": True, "route": "ordinary CLI; original tasks settled before release"}

    def stop_settled_scheduler(self, engagement):
        host.require(all(member["state"] in ("idle", "released") for member in engagement["members"]),
                     "original members have not settled before scheduler shutdown")
        if self.serve is not None:
            self.serve.terminate()
            self.serve.wait()
            self.serve = None

    def retire_profile(self):
        old = load(self.root / "qualified-binding.json")["server_key"]
        current = self.product("profile", "server", "status", "host-fixture")["data"]["server_key"]
        while True:
            migrated = self.product("profile", "server", "migrate", "host-fixture", "--operator-file", self.root / "operator",
                "--confirm-old-server-key", old, "--confirm-new-server-key", current,
                expected_errors=("PROFILE_SERVER_BUSY", "TRANSPORT_FAILURE", "PROFILE_MISMATCH"))
            if migrated["ok"]:
                break
            error = migrated["error"]
            if error["code"] == "PROFILE_MISMATCH":
                host.require(error["details"].get("field") == "old_server_running"
                             and error["details"].get("actual") is False, "Profile migration identity changed")
                # Only the product can prove the complete original generation
                # absent and reset its retained state under lifecycle locks.
                reset = self.product("profile", "state", "reset", "host-fixture", "--operator-file", self.root / "operator",
                    "--confirm-server-key", old, "--require-server-absence", expected_errors=("PROFILE_SERVER_BUSY",))
                if reset["ok"]:
                    break
                host.require(reset["error"]["retryable"] is True,
                             "original Profile absence proof has not settled")
                time.sleep(.1)
                continue
            host.require(error["retryable"] is True,
                         "original Profile migration requires recovery")
            time.sleep(.1)
        self.product("profile", "server", "stop", "host-fixture", "--operator-file", self.root / "operator", "--confirm-server-key", current)


class Responses(native_task086.Responses):
    def __init__(self, case):
        self.case = case
        self.stage = 0
        self.latest = None
        self.source = None
        self.cleanup_mode = False
        self.cleanup_complete = False
        self.consumer_mode = False
        self.individual_phase = "install"
        self.round_number = 1
        self.native_ordinal = 0
        self.drift_phase = None
        self.drift_done = False
        super().__init__(case.root, case.args.host)
        self.protected_code_mode = case.args.host == "codex"

    def begin_cleanup(self):
        with self.host_lock:
            host.private(self.root / f"{self.case.observation_prefix}-pre-cleanup-observation.json", host.encoded({
                "stage": self.stage, "pending": self.pending, "failure": self.failure}))
            self.cleanup_mode = True
            self.stage = 0
            self.pending.clear()
            self.tools, self.outer = None, None

    def host_reply(self, data):
        self.host_posts += 1
        host.require(self.host_posts <= 60, "ordinary host message budget exceeded")
        if self.host == "codex" and self.tools is None:
            if self.outer is None:
                self.outer = "ordinary-protected-tool-metadata"
                item = code_call(data, self.outer, [])
                item["input"] = 'const names=["mcp__dolgorae__specialist_native","mcp__dolgorae__specialist_consumer"]; text({label:"native-tools",result:ALL_TOOLS.filter(t=>names.includes(t.name)).map(t=>t.name)});'
                return self.codex_events(item, self.posts)
            found = code_results(data, self.outer, ["native-tools"])["native-tools"]
            host.require(set(found) == {"mcp__dolgorae__specialist_native", "mcp__dolgorae__specialist_consumer"}, "protected tool metadata missing")
            self.tools = {key: "mcp__dolgorae__" + key for key in ("specialist_native", "specialist_consumer")}
        if self.pending:
            self.verify_pending(data)
            self.latest = self.returns[-1]
            host.require(self.latest.get("ok") is True
                or self.last_request.get("request_id") in ("group-as-action-refusal", "cancelled-late")
                or (self.row == "consumer-rounds" and self.last_request.get("idempotency_key") == "fixture-round-4"),
                "ordinary native semantic refusal: " + str(self.latest))
            prefix = f"{self.case.observation_prefix}-cleanup-native" if self.cleanup_mode else f"{self.case.observation_prefix}-native"
            self.native_ordinal += 1
            host.private(self.root / f"{prefix}-{self.native_ordinal}.json", host.encoded(self.latest))
        drift_request = self.drift_request()
        request = drift_request if drift_request is not None else self.next_request()
        if request is None:
            if self.host == "claude":
                return self.claude_events(data, None, None)
            return self.codex_events({"type": "message", "role": "assistant", "content": [
                {"type": "output_text", "text": "Original owned engagement closed; no specialist Task submitted." if self.cleanup_mode
                 else "Original unknown, Direct progress, synthesis, complete native reads and structural receipt observed."}]}, self.posts)
        if drift_request is None:
            self.stage += 1
        return self.emit(data, request, "specialist_consumer" if request["schema"] == host.CONTROL else "specialist_native")

    def drift_request(self):
        if self.cleanup_mode or self.stage != 1 or self.drift_done:
            return None
        if self.drift_phase is None:
            self.original_install = self.latest
            self.case.prepare_drift()
            self.drift_phase = "role"
        elif self.drift_phase == "role":
            self.role_drift = self.latest["facts"]["candidate"]
            self.drift_phase = "configuration"
        else:
            original = self.original_install["facts"]["candidate"]
            configuration = self.latest["facts"]["candidate"]
            host.require(original["role_snapshot_digest"] != self.role_drift["role_snapshot_digest"]
                         and self.role_drift["role_snapshot_digest"] == configuration["role_snapshot_digest"]
                         and self.role_drift["agent_configuration_digest"] != configuration["agent_configuration_digest"],
                         "ordinary Role/configuration drift did not produce distinct candidates")
            host.private(self.root / "candidate-drift.json", host.encoded({"original": original,
                "same_name_role": self.role_drift, "changed_configuration": configuration, "new_execution_tasks": 0}))
            self.latest = self.original_install
            self.drift_done = True
            return None
        return {"schema": host.SCHEMA, "operation": "install_provisioning",
                "sources": load(self.root / f"ordinary-{self.drift_phase}-drift-sources.json"),
                "idempotency_key": f"fixture-{self.drift_phase}-drift", "expected_revision": None}

    def next_request(self):
        case, root = self.case, self.root
        def base(operation, **values):
            return {"schema": host.SCHEMA, "operation": operation, **values}
        def scoped(operation, **values):
            return base(operation, scope_kind="external_engagement", scope_ref=case.engagement, **values)
        if self.cleanup_mode:
            grouped = (root / "ordinary-group.json").exists()
            if grouped and self.stage == 0:
                return scoped("collect_group", group_ref=load(root / "ordinary-group.json")["facts"]["group_ref"], cursor=None, limit=64)
            if grouped and self.stage == 1:
                for source in self.latest["facts"]["coverage"]["sources"]:
                    if source["state"] != "completed":
                        case.admin("member-recover", "--scope-ref", case.engagement,
                                   "--member-ref", source["worker_ref"], "--task-ref", source["task_ref"])
                if (root / "owned-worker-fault.json").exists() and not (root / "ordinary-restored-worker.json").exists():
                    case.restore_worker(load(root / "ordinary-selected-worker.json")["identity"]["run_id"])
            get_stage = 1 if grouped else 0
            if self.stage == get_stage:
                return base("get_external_engagement", engagement_ref=case.engagement)
            engagement = self.latest["facts"]["engagement"]
            if engagement["state"] == "closed":
                self.cleanup_complete = True
                return None
            host.require(self.stage == get_stage + 1, "original native engagement did not finish closing")
            case.stop_settled_scheduler(engagement)
            return base("close_external_engagement", engagement_ref=case.engagement,
                        expected_revision=engagement["revision"], allow_interrupt=False)
        if self.consumer_mode:
            native = [v for v in self.returns if v.get("schema") == "dolgorae.native-specialist-response/v1"]
            controls = [v for v in self.returns if v.get("schema") == "dolgorae.consumer-control-response/v1"]
            return self.complete.next(self, native, controls)
        if not case.group:
            return self.individual_request()
        if self.stage == 0:
            return base("install_provisioning", sources=load(root / "ordinary-sources.json"), idempotency_key="fixture-install", expected_revision=None)
        if self.stage == 1:
            host.private(root / "ordinary-install.json", host.encoded(self.latest))
            return base("open_external_engagement", workspace_ref=case.workspace, idempotency_key="fixture-result-open", expected_revision=None)
        if self.stage == 2:
            host.private(root / "ordinary-open.json", host.encoded(self.latest))
            case.engagement = self.latest["facts"]["engagement"]["engagement_ref"]
            host.private(root / "fault/engagement.match", case.engagement.encode())
            host.private(root / "fault/request.match", b"TASK086_GROUP_SOURCE_1")
            candidate = load(root / "ordinary-install.json")["facts"]["candidate"]
            return scoped("hire_external_specialist", provisioning_ref=candidate["provisioning_ref"], role_ref="general",
                agent_configuration_digest=candidate["agent_configuration_digest"], idempotency_key="fixture-hire", expected_revision=None)
        if self.stage == 3:
            if case.gap:
                # Bound only the injected hold, starting at group submission.
                # This fail-safe never limits the case or its owner cleanup.
                host.private(root / "fault/until.monotonic-ns",
                    str(time.clock_gettime_ns(time.CLOCK_MONOTONIC) + 180_000_000_000).encode())
                case.fault_thread = threading.Thread(target=case.fault, daemon=True)
                case.fault_thread.start()
            return case.group_inputs(self.latest)
        if self.stage == 4:
            self.group_facts = self.latest["facts"]
            host.private(root / "ordinary-group.json", host.encoded(self.latest))
            return scoped("await_group", group_ref=self.group_facts["group_ref"], predicate={"kind": "any" if case.gap else "all"}, timeout_seconds=60)
        if self.stage == 5:
            if case.gap:
                source = next(v for v in self.latest["facts"]["coverage"]["sources"] if v["entry_key"] == "source-1")
                host.require(source["state"] not in ("completed", "failed", "cancelled", "expired")
                             or (root / "fault/recovery.ready").exists(),
                             "original source1 settled before the required unknown hold")
                case.wait(lambda: (root / "fault/recovery.ready").exists(), "original unknown barrier not reached")
            return scoped("collect_group", group_ref=self.group_facts["group_ref"], cursor=None, limit=64)
        if self.stage == 6:
            self.group_coverage = self.latest["facts"]["coverage"]
            self.source = next(v for v in self.group_coverage["sources"] if v["entry_key"] == "source-1")
            if not case.gap:
                host.require(all(v["state"] == "completed" for v in self.group_coverage["sources"]), "normal group incomplete")
                self.stage = 8
                return scoped("merge_group_results", group_ref=self.group_facts["group_ref"],
                    group_assignment_digest=self.group_coverage["group_assignment_digest"], sources=self.group_coverage["sources"],
                    algorithm="merge-exact-findings-v1", idempotency_key="fixture-merge", expected_revision=None)
            host.require(self.source["state"] == "unknown", "original source1 not unknown")
            selected = load(root / "ordinary-selected-worker.json")["identity"]
            host.require(self.source["worker_ref"] == selected["run_id"], "group source1 differs from the physically faulted Run")
            self.child_namespace = dict(self.group_facts["receipt"]["namespace"], operation_kind="assign_external_specialist_task",
                idempotency_key=f'group:{self.group_facts["group_ref"]}:1:assign')
            return base("lookup_operation", namespace=self.child_namespace)
        if self.stage in (7, 8):
            lookup = self.latest["facts"]["lookup"]
            host.require(lookup["state"] == "accepted" and lookup["current_outcome"] == "unknown"
                and lookup["accepted_subjects"]["task_ref"] == self.source["task_ref"], "public original-key lookup not unknown")
            host.private(root / ("original-operation-unknown-before.json" if self.stage == 7 else "original-operation-unknown-after.json"), host.encoded(lookup))
            if self.stage == 7:
                direct = argparse.Namespace(binary=case.args.binary, driver=case.args.peer_reader, podway=case.args.podway,
                    scratch=root, unsupported_peer_producer=case.args.unsupported_peer_producer, deadline=case.deadline,
                    contract=host.PodwayDistribution(case.args.podway_archive, case.args.podway))
                host.private(root / "direct-during-unknown.json", host.encoded(host.qualify_direct_contract(direct)))
                return base("lookup_operation", namespace=self.child_namespace)
            return scoped("merge_group_results", group_ref=self.group_facts["group_ref"],
                group_assignment_digest=self.group_coverage["group_assignment_digest"], sources=self.group_coverage["sources"],
                algorithm="merge-exact-findings-v1", idempotency_key="fixture-merge", expected_revision=None)
        if self.stage == 9:
            host.private(root / "ordinary-merge.json", host.encoded(self.latest))
            if case.gap:
                case.release_recovery()
            return scoped("await_group", group_ref=self.group_facts["group_ref"], predicate={"kind": "all"}, timeout_seconds=60)
        if self.stage == 10:
            coverage = self.latest["facts"]["coverage"]
            fields = ("entry_key", "worker_ref", "task_ref", "role_snapshot_digest", "agent_configuration_digest", "accepted_task_digest")
            immutable = lambda rows: [{key: row[key] for key in fields} for row in rows]
            host.require(immutable(coverage["sources"]) == immutable(self.group_coverage["sources"])
                and all(v["state"] == "completed" for v in coverage["sources"]), "original group recovery changed identity or remained incomplete")
            host.private(root / "ordinary-recovered-coverage.json", host.encoded(coverage))
            if case.gap:
                case.restore_worker(self.source["worker_ref"])
            case.read_merge()
            candidate = load(root / "ordinary-install.json")["facts"]["candidate"]
            return scoped("hire_external_specialist", provisioning_ref=candidate["provisioning_ref"], role_ref="general",
                agent_configuration_digest=candidate["agent_configuration_digest"], idempotency_key="fixture-synthesis-hire", expected_revision=None)
        if self.stage == 11:
            return case.synthesis_input(self.latest)
        if self.stage == 12:
            self.synthesis = self.latest["facts"]
            request = load(root / "ordinary-synthesis-request.json")
            provenance = load(root / "ordinary-merge-provenance.json")
            provenance["synthesis"] = self.synthesis
            host.private(root / "group-merge-provenance.json", host.encoded(provenance))
            host.private(root / "assigned.json", host.encoded({"scope_ref": case.engagement,
                "member_ref": request["member_ref"], "task_ref": self.synthesis["task"]["task_ref"],
                "assignment_receipt": self.synthesis["receipt"]}))
            return base("lookup_operation", namespace=self.synthesis["receipt"]["namespace"])
        if self.stage == 13:
            lookup = self.latest["facts"]["lookup"]
            host.require(lookup["state"] == "accepted" and lookup["receipt"] == self.synthesis["receipt"],
                         "original synthesis admission lookup changed")
            request = load(root / "ordinary-synthesis-request.json")
            return scoped("await_external_specialist_task", member_ref=request["member_ref"],
                task_ref=self.synthesis["task"]["task_ref"], timeout_seconds=60)
        if self.stage == 14:
            host.require(self.latest["facts"]["wait_timed_out"] is False, "synthesis wait timed out")
            assigned = load(root / "assigned.json")
            return scoped("collect_external_specialist_task", member_ref=assigned["member_ref"], task_ref=assigned["task_ref"])
        if self.stage == 15:
            host.require(self.latest["facts"]["task"]["state"] == "completed" and self.latest["evidence"], "synthesis complete result missing")
            case.prepare_consumer()
            return base("lookup_operation", namespace=self.synthesis["receipt"]["namespace"])
        if self.stage == 16:
            lookup = self.latest["facts"]["lookup"]
            host.require(lookup["state"] == "accepted" and lookup["current_outcome"] == "completed"
                and lookup["receipt"] == self.synthesis["receipt"], "original settled synthesis lookup changed")
            return {"schema": host.CONTROL, "action": "prepare_accept", "request_id": "group-as-action-refusal",
                "evidence_ref": self.group_facts["group_ref"], "summary": "A group cannot supply an individual Task receipt."}
        if self.stage == 17:
            self.consumer_mode = True
            return self.next_request()
        raise RuntimeError("unexpected ordinary native stage")

    def individual_request(self):
        case, root = self.case, self.root
        def request(operation, **fields):
            return {"schema": host.SCHEMA, "operation": operation, **fields}
        def scoped(operation, **fields):
            return request(operation, scope_kind="external_engagement", scope_ref=case.engagement, **fields)
        phase = self.individual_phase
        if phase == "install":
            self.individual_phase = "open"
            return request("install_provisioning", sources=load(root / "ordinary-sources.json"), idempotency_key="fixture-install", expected_revision=None)
        if phase == "open":
            self.candidate = self.latest["facts"]["candidate"]
            self.individual_phase = "hire"
            return request("open_external_engagement", workspace_ref=case.workspace, idempotency_key="fixture-result-open", expected_revision=None)
        if phase == "hire":
            case.engagement = self.latest["facts"]["engagement"]["engagement_ref"]
            self.individual_phase = "assign"
            return scoped("hire_external_specialist", provisioning_ref=self.candidate["provisioning_ref"], role_ref="general",
                agent_configuration_digest=self.candidate["agent_configuration_digest"], idempotency_key="fixture-hire", expected_revision=None)
        if phase == "assign":
            self.hired = self.latest
            self.member = self.hired["facts"]["member"]["member_ref"]
            if self.row == "consumer-complete":
                host.private(root / "ordinary-mcp-member.json", host.encoded(self.member))
                inventory = case.helper("observe-mcp")
                host.require(any(v["name"] == "fixture" and "ask" in v.get("tools", {}) for v in inventory["data"]),
                             "original owned MCP tool inventory is not ready")
                metadata = self.interactions.mcp_requests
                host.require(metadata <= 24, "ordinary MCP preparation metadata budget exceeded")
                self.mcp_pre_dispatch_requests = metadata
                self.interactions.max_mcp_requests = metadata + 8
            self.individual_phase = "wait"
            return case.individual_input(self.hired, self.round_number)
        if phase == "wait":
            assigned = self.latest["facts"]
            host.private(root / f"ordinary-round-{self.round_number}-assigned.json", host.encoded(assigned))
            body = host.encoded({"scope_ref": case.engagement, "member_ref": self.member,
                "task_ref": assigned["task"]["task_ref"], "assignment_receipt": assigned["receipt"]})
            current = root / "assigned.json"
            if current.exists():
                from run_native_integrated_acceptance import overwrite_fixture_file
                overwrite_fixture_file(current, body)
            else:
                host.private(current, body)
            self.individual_phase = "collect"
            return scoped("await_external_specialist_task", member_ref=self.member, task_ref=assigned["task"]["task_ref"], timeout_seconds=60)
        if phase == "collect":
            host.require(self.latest["facts"]["wait_timed_out"] is False
                         and self.latest["facts"]["task"]["state"] == "completed", "original writable Task did not complete")
            case.settled_result()
            self.individual_phase = "collected"
            return scoped("collect_external_specialist_task", member_ref=self.member, task_ref=load(root / "assigned.json")["task_ref"])
        if phase == "collected":
            host.require(self.latest["evidence"] and self.latest["facts"]["task"]["state"] == "completed", "native writable result is missing")
            if self.row == "consumer-rounds":
                if self.round_number < 3:
                    self.round_number += 1
                    self.individual_phase = "wait"
                    return case.individual_input(self.hired, self.round_number)
                self.individual_phase = "exhausted"
                return case.individual_input(self.hired, 4)
            if self.row == "consumer-release":
                case.helper("observe-images")
                self.individual_phase = "release"
                return request("get_external_engagement", engagement_ref=case.engagement)
            self.individual_phase = "consume"
        elif phase == "exhausted":
            host.require(self.latest.get("ok") is False and self.latest["semantic"]["error"]["code"] == "ROUNDS_EXHAUSTED"
                         and self.latest["semantic"]["effect"] == "none", "fourth assignment was not refused before acceptance")
            host.private(root / "round-exhaustion.json", host.encoded(self.latest))
            self.individual_phase = "consume"
        elif phase == "released":
            host.require(self.latest["facts"]["member"]["state"] == "released", "original member release did not settle")
            self.individual_phase = "consume"
        elif phase == "release":
            engagement = self.latest["facts"]["engagement"]
            member = next(v for v in engagement["members"] if v["member_ref"] == self.member)
            case.stop_settled_scheduler(engagement)
            self.individual_phase = "released"
            return scoped("release_external_specialist", member_ref=self.member, expected_revision=member["revision"])
        if self.individual_phase == "consume":
            case.prepare_consumer()
            self.individual_phase = "consumer"
            return scoped("collect_external_specialist_task", member_ref=self.member, task_ref=load(root / "assigned.json")["task_ref"])
        if self.individual_phase == "consumer":
            host.require(self.latest["evidence"], "settled native consumer evidence is missing")
            self.consumer_mode = True
            return self.next_request()
        raise RuntimeError("unexpected ordinary individual phase: " + phase)


def main():
    parser = argparse.ArgumentParser()
    for key in ("binary", "helper", "fault-library", "peer-reader", "result-reader", "codex", "podway", "podway-archive", "unsupported-peer-producer", "scratch"):
        parser.add_argument("--" + key, required=True, type=Path)
    parser.add_argument("--host", choices=("codex", "claude"), default="codex")
    parser.add_argument("--scenario", choices=host.CONSUMER_ROWS, default="consumer-group-gaps")
    parser.add_argument("--claude", type=Path)
    parser.add_argument("--claude-model")
    parser.add_argument("--executor-model", required=True)
    parser.add_argument("--codex-model", required=True)
    parser.add_argument("--peer-reader-sha256", required=True)
    parser.add_argument("--result-reader-sha256", required=True)
    args = parser.parse_args()
    host.require(os.environ.get(host.OPT_IN) == "1", "explicit ordinary native opt-in required")
    for key in ("binary", "helper", "fault_library", "peer_reader", "result_reader", "codex", "podway", "podway_archive", "unsupported_peer_producer", "scratch"):
        setattr(args, key, getattr(args, key).resolve(strict=True))
    host.require(hashlib.sha256(args.codex.read_bytes()).hexdigest() == host.PINS["codex"][1], "selected Codex image changed")
    host.require(hashlib.sha256(args.peer_reader.read_bytes()).hexdigest() == args.peer_reader_sha256, "selected read-only peer oracle changed")
    host.require(hashlib.sha256(args.result_reader.read_bytes()).hexdigest() == args.result_reader_sha256, "selected source-result reader changed")
    host.PodwayDistribution(args.podway_archive, args.podway)
    if args.host == "claude":
        host.require(args.claude is not None and args.claude_model, "selected Claude image and model required")
        args.claude = args.claude.resolve(strict=True)
        host.require(hashlib.sha256(args.claude.read_bytes()).hexdigest() == host.PINS["claude"][1], "selected Claude image changed")
    os.umask(0o077)
    root = Path(tempfile.mkdtemp(prefix="og-", dir=args.scratch)).resolve()
    for name in ("home", "tmp", "config", "cache", "codex", "claude", "profile", "workspace", "fault"):
        (root / name).mkdir(mode=0o700)
    host.private(root / "fixture-owned", b"native-production-host/v1\n")
    host.private(root / "delivery-count", b"0")
    environment = host.host_environment(root)
    version = subprocess.run([str(args.binary), "version", "--json"], env=environment, capture_output=True, check=True)
    product_version = json.loads(version.stdout)["version"].removeprefix("v")
    config = {"host": args.host, "scenario": args.scenario, "binary": str(args.binary), "ordinary_unbounded": True,
        "executor_identity": {"path": str(args.codex), "sha256": host.PINS["codex"][1]},
        "reference_binary": str(args.binary), "executor_model": args.executor_model, "adapter_version": product_version,
        "podway_archive": str(args.podway_archive), "host_identity": {"path": str(getattr(args, args.host)), "version": "0.164.0" if args.host == "codex" else "2.1.310", "sha256": host.PINS[args.host][1]}}
    if args.scenario == "consumer-group-gaps":
        config.update(ordinary_fault_library=str(args.fault_library), ordinary_fault_library_sha256=hashlib.sha256(args.fault_library.read_bytes()).hexdigest())
    host.private(root / "input.json", host.encoded(config))
    case = OrdinaryCase(args, root)
    responses = Responses(case)
    if args.scenario == "consumer-complete":
        from run_native_caller_pipeline import InteractionResponses
        responses.interactions = InteractionResponses(root / "workspace", max_mcp_requests=32, protected_canary=host.OWNERLESS_CANARY)
    case.responses = responses
    failure, cleanup = None, None
    try:
        case.prepare(responses)
        script = Path(host.__file__).resolve()
        command = shlex.join([sys.executable, str(script), "--hook", str(root)])
        host.private(native_root(root) / "hooks.json", host.encoded({"hooks": {"PreToolUse": [{"matcher": "^mcp__dolgorae__.*$", "hooks": [{"type": "command", "command": command, "timeout": 10}]}]}}))
        proxy = [str(script), "--proxy", str(root)]
        if args.host == "codex":
            config = host.codex_host_config(responses.base_url, args.codex_model, proxy)
            config = config.replace('[features.code_mode]\ndirect_only_tool_namespaces = ["mcp__dolgorae"]\n', "")
            host.private(native_root(root) / "config.toml", config.encode())
            argv = [str(args.codex), "--no-daemon", "--dangerously-bypass-hook-trust", "-a", "never", "exec", "--skip-git-repo-check", "--sandbox", "read-only", "--json", "--cd", str(root / "workspace"), "Execute the bounded production fixture calls."]
        else:
            case.env.update(ANTHROPIC_BASE_URL=responses.base_url, ANTHROPIC_API_KEY="sk-ant-fixture-not-a-credential")
            host.private(root / "settings.json", (native_root(root) / "hooks.json").read_bytes())
            host.private(root / "mcp.json", host.encoded({"mcpServers": {"dolgorae": {"type": "stdio", "command": sys.executable, "args": proxy}}}))
            argv = [str(args.claude), "--restricted", "--setting-sources", "", "--settings", str(root / "settings.json"),
                "--strict-mcp-config", "--mcp-config", str(root / "mcp.json"), "--tools", "", "--permission-mode", "dontAsk",
                "--permission-prompts", "none", "--model", args.claude_model, "-p", "--output-format", "stream-json", "--verbose",
                "Execute the bounded production fixture calls."]
        case.native_argv = argv
        output = host.run_host(argv, root, case.env, "initial", case.deadline)
        host.require(responses.failure is None and responses.consumer_mode and responses.complete.finished.keys() == responses.complete.required_labels
            and not responses.pending, "ordinary native sequence incomplete: " + str(responses.failure))
        if not (root / "ordinary-live-images.json").exists():
            case.helper("observe-images")
        observed = case.producer_observation
        identity = observed["delegation_identity"]["identity"]
        complete = json.dumps(observed["complete_result"], ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
        rounds = 3 if case.group or args.scenario == "consumer-rounds" else 1
        expected_posts = ({"source-0": 2, "source-1": 1 if case.gap else 2, "synthesis": 2} if case.group
                          else {f"round-{number}": 2 for number in range(1, rounds + 1)})
        host.require(responses.complete.finished["complete_result"] == complete
            and identity["rounds_used"] == identity["accepted_assignments"] == rounds
            and identity["unresolved_assignments"] == 0
            and responses.executor_turn_posts == expected_posts,
            "original source recovery or synthesis accounting changed")
        if args.scenario == "consumer-release":
            host.require(identity["worker_disposition"] == "released"
                         and identity["disposition_proof"]["release_receipt"] is not None, "settled release proof is missing")
        if args.scenario == "consumer-rounds":
            host.require((root / "round-exhaustion.json").exists(), "fourth round refusal is missing")
        facts = {"schema": "dolgorae.native-producer-facts/v1",
            "source_kind": "ordinary-image native collection and independent owner-authorized read",
            "identity": identity, "accepted_configuration": observed["accepted_configuration"],
            "original_task": observed["original_task"], "originals": {k: v.decode() for k, v in responses.complete.finished.items()},
            "native_pages": responses.complete.witnesses, "ordinary_aquarium_role": False, "semantic_goal_pass": False,
            "native_admitted_input": host.admitted_native_input(responses.executor_inputs["synthesis" if case.group else f"round-{rounds}"],
                observed["accepted_configuration"], observed["original_task"])}
        drift = load(root / "candidate-drift.json")
        host.require(drift["original"]["role_snapshot_digest"] == identity["role_snapshot_digest"]
                     and drift["original"]["agent_configuration_digest"] == identity["agent_configuration_digest"],
                     "accepted original candidate changed after Role/configuration drift")
        facts["independent_drift_observations"] = drift
        if args.scenario == "consumer-complete":
            host.require(len(observed["policy_declines"]) == 2
                         and all(host.OWNERLESS_CANARY.encode() not in path.read_bytes() for path in root.glob("http-*.json")),
                         "original ownerless policy decisions or protected-question boundary changed")
            facts["ownerless_native_policy_declines"] = {"native": load(root / "actual-ownerless-native-declines.json"),
                "production_policy_decisions": observed["policy_declines"], "metadata_requests": responses.mcp_pre_dispatch_requests,
                "runtime_requests": responses.interactions.mcp_requests - responses.mcp_pre_dispatch_requests}
        host.private(root / "producer-facts.json", host.encoded(facts))
        result = {"native_session": host.native_session(output, args.host), "host_posts": responses.host_posts,
            "executor_posts": responses.executor_posts, "instrumented": case.gap,
            "fault": load(root / "owned-worker-fault.json") if case.gap else None}
    except BaseException as error:
        failure = repr(error)
        result = {}
    finally:
        try:
            cleanup = case.cleanup()
        except BaseException as error:
            cleanup = {"cleanup_complete": False, "failure": repr(error)}
        if case.gap and ((root / "fault/fail-safe-release").exists() or case.fault_error is not None):
            failure = failure or "injected hold fail-safe or fault observer failure: " + str(case.fault_error)
        if case.serve is not None:
            result["retained_owner"] = {"serve_pid": case.serve.pid, "socket": str(root / "gateway.sock"),
                "engagement_ref": case.engagement, "cleanup_complete": False,
                "next_owner": "Coordinator original-scope recovery"}
        responses.server.shutdown()
        responses.server.server_close()
        if hasattr(responses, "interactions"):
            responses.interactions.close()
        result.update(root=str(root), stage="ordinary-image native runtime; artifact qualification is separate", failure=failure,
            passed=failure is None and cleanup["cleanup_complete"] and case.serve is None, cleanup=cleanup, steps=case.steps)
        host.private(root / "ordinary-feasibility.json", host.encoded(result))
        print(json.dumps(result, ensure_ascii=False), flush=True)
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
