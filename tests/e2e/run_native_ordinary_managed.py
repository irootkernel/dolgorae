#!/usr/bin/env python3
"""Opted-in managed producer through the selected ordinary product image."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time

import run_native_integrated_acceptance as integrated
import run_native_ordinary_host as ordinary
import run_native_production_host as host


class ManagedCase(ordinary.OrdinaryCase):
    def observe_runs(self, runs):
        path = self.root / "ordinary-read-runs.json"
        body = host.encoded(runs)
        if path.exists():
            integrated.overwrite_fixture_file(path, body)
        else:
            host.private(path, body)
        return self.helper("observe-runs")

    def operation_timeout(self):
        # The original Send and Task limits remain explicit product inputs.
        # No parent work timer kills Profile or whole-session cleanup.
        return None

    def admin(self, operation, *arguments):
        with (self.root / "operator").open("rb") as operator, (self.root / "controller").open("rb") as controller:
            return self.product("__trusted-specialist-host", operation,
                "--owner-config", self.root / "managed-native-host/owner.json",
                "--operator-fd", operator.fileno(), "--controller-fd", controller.fileno(),
                *arguments, pass_fds=(operator.fileno(), controller.fileno()))

    def prepare(self, responses):
        root = self.root
        self.call("/usr/bin/git", "init", "-q", root / "workspace")
        initialized = self.product("init", root / "workspace")["data"]
        self.workspace = initialized["workspace_id"]
        self.call("/usr/bin/git", "-C", root / "workspace", "config", "user.name", "Native Acceptance")
        self.call("/usr/bin/git", "-C", root / "workspace", "config", "user.email", "native@example.invalid")
        host.private(root / "workspace/file.txt", b"before\n")
        self.call("/usr/bin/git", "-C", root / "workspace", "add", "file.txt")
        self.call("/usr/bin/git", "-C", root / "workspace", "commit", "-q", "-m", "Fixture baseline")
        integrated.overwrite_fixture_file(root / "workspace/.git/info/exclude", b"/.podway/\n/.dolgorae/\n")
        for path, body in (("home/AGENTS.md", b"integrated-common-017\n"),
                           ("roles/general/AGENTS.md", b"integrated-general-017\nDeliver the accepted Dolgorae task only. Report original evidence and gaps; do not claim an Aquarium Role or semantic goal acceptance.\n")):
            target = root / "home/.dolgorae" / path
            if target.exists():
                integrated.overwrite_fixture_file(target, body)
            else:
                target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
                host.private(target, body)
        for name, directory in (("integrated-successor", "profile"), ("integrated-successor-members", "profile-members")):
            config = b'model = "gpt-5.6-luna"\nmodel_reasoning_effort = "low"\n' + integrated.fixture_config(responses.base_url)
            host.private(root / directory / "config.toml", config + b"\n[features]\nplugins = false\nshell_snapshot = false\n")
            host.private(root / directory / "AGENTS.md", (integrated.PROFILE + "\n").encode())
            self.product("profile", "add", name, "--codex-home", root / directory, "--native-subagents", "enabled",
                "--env", "PATH=/usr/bin:/bin:/usr/sbin:/sbin", "--env", "LANG=C", "--env", "LC_ALL=C", "--", self.args.codex)
        policy = ordinary.load(integrated.PROTOCOL / "examples/workspace-policy-input-v3.valid.json")
        policy.update(policy_name="native-successor", approval_policy="fully_delegated")
        policy["roles"][0]["agent_configuration"].update(selected_profile="integrated-successor", model="gpt-5.6-luna", default_effort="low")
        policy["roles"][0]["agent_configuration"].pop("global_profile_binding_sha256", None)
        host.private(root / "policy.json", host.encoded(policy))
        self.product("operator", "credential", "initialize", "--output", root / "operator")
        carrier = root / "home/.dolgorae/controller-carriers/task087/bootstrap/credential"
        carrier.parent.mkdir(mode=0o700, parents=True)
        self.product("controller", "credential", "create", "--kind", "automation", "--instance-id", "ordinary-bootstrap", "--output", carrier)
        self.prepared = self.helper("capture")
        with (root / "serve.stdout").open("wb") as stdout, (root / "serve.stderr").open("wb") as stderr:
            self.serve = subprocess.Popen([str(self.args.binary), "serve", "--socket", str(root / "gateway.sock")],
                env=self.env, cwd=root / "workspace", stdout=stdout, stderr=stderr)
        self.wait(lambda: (root / "gateway.sock").exists() or self.serve.poll() is not None, "ordinary managed serve not ready")
        host.require(self.serve.poll() is None, "ordinary managed serve exited")
        self.helper("bootstrap-start")
        self.helper("bootstrap-close")
        # Policy validation observes an already-running qualified Root Profile.
        # Its preparatory caller Run is real and turnless, with no Policy yet.
        bootstrap = self.product("run", "start", "--workspace", root / "workspace", "--controller-file", carrier,
            "--profile", "integrated-successor", "--control-mode", "managed-agent", "--access", "read-only", "--role", "general",
            "--model", "gpt-5.6-luna", "--effort", "low", "--purpose", "interactive", "--idempotency-key", "root-profile-bootstrap")
        self.root_bootstrap = bootstrap["data"]["run_id"]
        host.private(root / "ordinary-root-bootstrap.json", host.encoded(bootstrap))
        closed = self.product("run", "close", self.root_bootstrap, "--workspace", root / "workspace", "--controller-file", carrier)
        host.require(self.observe_runs([self.root_bootstrap])[0]["state"] == "closed", "Root Profile bootstrap not closed")
        self.root_bootstrap_closed = True
        host.require(responses.posts == 0, "private bootstrap created a model Turn")
        self.product("specialist", "policy", "add", "native-successor", "--workspace", root / "workspace", "--file", root / "policy.json")
        self.product("controller", "credential", "create", "--kind", "interactive-client", "--instance-id", "native-successor",
                     "--orchestration-policy", "native-successor", "--output", root / "controller")
        started = self.product("run", "start", "--workspace", root / "workspace", "--controller-file", root / "controller",
            "--profile", "integrated-successor", "--control-mode", "direct-interactive", "--access", "read-only", "--role", "general",
            "--model", "gpt-5.6-luna", "--effort", "low", "--purpose", "interactive", "--idempotency-key", "native-root")
        self.primary = started["data"]["run_id"]
        host.private(root / "ordinary-managed-root.json", host.encoded(started))
        status = self.observe_runs([self.primary])[0]
        self.controller = status["controller"]
        host.podway_fixture(root, self.args.podway, host.PodwayDistribution.EXECUTABLE,
            self.prepared["candidate"], self.env, None, adapter_id="dolgorae.managed/1", adapter_version=ordinary.load(root / "input.json")["adapter_version"])
        connection = ordinary.load(root / "connection.json")
        shutil.copyfile(root / "controller", root / "managed-native-host/controller")
        draft = {"route": host.PINS["codex"][2], "executable": ordinary.load(root / "input.json")["executor_identity"],
            "owner": {"kind": "managed", "session_ref": self.primary}, "controller_generation": self.controller["generation"],
            "controller_file": str(root / "managed-native-host/controller"), "native_host_root": str(root / "managed-native-host/journal"),
            "allowed_scopes": [{"kind": "orchestrated_session", "reference": self.primary}],
            "tools": ["dolgorae_specialists_native", "dolgorae_specialist_consumer"],
            "caller_scope": {"binding_ref": "local-specialist", "podway_workspace_ref": connection["workspace"],
                "podway_session_ref": connection["session"], "podway_attempt_ref": connection["attempt"], "delegated_placement_ref": "work", "max_rounds": 3},
            "adapter_id": "dolgorae.managed/1", "adapter_version": connection["adapter_version"]}
        host.private(root / "managed-native-host/draft.json", host.encoded(draft))
        self.admin("owner-config-create", "--workspace", root / "workspace", "--request", root / "managed-native-host/draft.json")
        for operation in ("namespace-setup", "host-state-init", "preparation-init", "managed-connect"):
            self.admin(operation)
        host.private(root / "managed-native-host/consumer.json", host.encoded({"kind": "delegated", "connection": connection}))
        self.admin("consumer-init", "--request", root / "managed-native-host/consumer.json")
        self.admin("provisioning-prepare", "--request", root / "ordinary-provisioning-selection.json", "--output", root / "ordinary-sources.json")

    def turn(self, phase, values):
        body = host.encoded({"phase": phase, "root_run": self.primary, "input": values})
        host.private(self.root / f"primary-{phase}-input.json", body)
        current = self.root / "primary-phase.json"
        if current.exists():
            integrated.overwrite_fixture_file(current, body)
        else:
            host.private(current, body)
        self.product("run", "send", self.primary, "--workspace", self.root / "workspace", "--controller-file", self.root / "controller",
            "--message", f"native-primary-{phase}\nConsume the original Dolgorae delivery and structural receipt only. This producer is not an Aquarium native consumer Role; no semantic goal acceptance is authorized.",
            "--idempotency-key", "native-primary-" + phase, "--timeout", "60s")
        return ordinary.load(self.root / f"primary-{phase}-results.json")

    def execute(self):
        candidate = self.prepared["candidate"]
        spawned = self.turn("spawn", {"candidate": candidate, "sources": ordinary.load(self.root / "ordinary-sources.json")})
        spawned = next(row["header"]["facts"] for row in spawned if row["request"].get("operation") == "spawn_specialist")
        member = spawned["member"]["member_ref"]
        self.member = member
        self.member_release_revision = spawned["member"]["revision"]
        integrated.overwrite_fixture_file(self.root / "workspace/file.txt", b"captured after qualified acquire\n")
        package = ordinary.load(self.root / "package.json")["result"]["package"]
        task = {"purpose": "change", "brief": "native-managed-writer", "contexts": [{"id": "delegated-package",
            "content": json.dumps(package, ensure_ascii=False, sort_keys=True, separators=(",", ":")), "provenance": "caller"}],
            "criteria": [], "expected_output": "complete_result"}
        namespace = dict(spawned["receipt"]["namespace"], operation_kind="assign_specialist_task", idempotency_key="native-write")
        selection = {"namespace": namespace, "member_ref": member, "task": task, "execution_intent": "canonical_workspace_write", "deadline_seconds": 600, "target": None}
        host.private(self.root / "managed-native-host/assignment.json", host.encoded(selection))
        self.admin("assignment-prepare", "--request", self.root / "managed-native-host/assignment.json", "--output", self.root / "ordinary-managed-accepted.json")
        accepted = ordinary.load(self.root / "ordinary-managed-accepted.json")
        host.private(self.root / "formal-task.json", host.encoded({"task": task, "execution_intent": "canonical_workspace_write", "deadline_seconds": 600}))
        assigned = self.turn("assignment", {"member_ref": member, "candidate": candidate,
            "accepted_task_digest": self.helper("accepted-task-digest"),
            "controller_ref": "controller:" + self.controller["controller_id"],
            "request": {"schema": host.SCHEMA, "operation": "assign_specialist_task", "scope_kind": "orchestrated_session", "scope_ref": self.primary,
                "member_ref": member, "assignment": {"task_body": task, "accepted_task_identity": accepted, "deadline_seconds": 600},
                "idempotency_key": "native-write", "expected_revision": None}})
        assigned = next(row["header"]["facts"] for row in assigned if row["request"].get("operation") == "assign_specialist_task")
        self.member, self.task = member, assigned["task"]["task_ref"]
        host.private(self.root / "assigned.json", host.encoded({"scope_ref": self.primary, "member_ref": member, "task_ref": self.task}))
        # Terminal Task publication precedes owned Writer cleanup. Keep its
        # ordinary scheduler alive while observing the original disposition.
        next_notice = time.monotonic()
        while True:
            observed = self.call(self.args.result_reader, "--ordinary-managed-read-result", self.root)
            if "observation_error" not in observed:
                break
            error = observed["observation_error"]
            host.require(error["code"] in ("DISPOSITION_NOT_IDLE", "RUN_STATE_CONFLICT", "RECOVERY_REQUIRED"),
                         "original managed disposition observation failed: " + str(error))
            if error["code"] == "RECOVERY_REQUIRED":
                writer = observed["writer_observation"]
                release = writer.get("generation_release")
                host.require(release is not None and release["source"]["run_id"] == member
                             and release["phase"] != "committed"
                             and writer["state"] in ("releasing", "reserved", "blocked_unknown"),
                             "managed observation has no original pending Writer release")
                if writer["state"] == "blocked_unknown":
                    self.admin("member-recover", "--scope-ref", self.primary,
                               "--member-ref", member, "--task-ref", self.task)
            host.require(self.serve is not None and self.serve.poll() is None,
                         "original managed scheduler exited before owned cleanup")
            if time.monotonic() >= next_notice:
                print("WAIT original managed Writer cleanup: " + error["code"], file=sys.stderr, flush=True)
                next_notice = time.monotonic() + 30
            time.sleep(1)
        identity = observed["delegation_identity"]["identity"]
        complete = observed["complete_result"]
        self.helper("observe-images")
        statuses = self.observe_runs([self.primary, member])
        host.require(all(row["state"] == "idle" and row["active_turn_id"] is None
                         and row["identity_verdict"] == "Match" for row in statuses),
                     "original managed Runs not idle after Writer cleanup")
        host.private(self.root / "ordinary-managed-pre-consumer.json", host.encoded(statuses))
        self.serve.terminate()
        self.serve.wait()
        self.serve = None
        execution_fields = ("binding_ref", "adapter_id", "adapter_version", "primary_host", "scope_kind", "scope_ref", "supervisor_ref", "worker_ref", "task_ref", "role_ref", "role_snapshot_digest", "agent_configuration_digest", "accepted_task_digest")
        self.turn("consumer", {"member_ref": member, "task_ref": self.task, "candidate": candidate,
            "assignment_receipt": assigned["receipt"], "result_digest": identity["result_digest"], "execution": {key: identity[key] for key in execution_fields},
            "release_member": True, "release_revision": identity["disposition_proof"]["membership_revision"],
            "originals": {"complete_result": json.dumps(complete, ensure_ascii=False, sort_keys=True, separators=(",", ":")),
                "before": "captured after qualified acquire\n", "after": "1 after\n"}})
        self.member_released = True
        integrated.finish_successor_consumer(self.root)
        self.observed = observed
        return {"root_run": self.primary, "member_ref": member, "task_ref": self.task, "rounds_used": identity["rounds_used"]}

    def cleanup(self):
        # The native release owns the successor member; whole-session Close
        # verifies that release. Cleanup never submits a replacement Task.
        if hasattr(self, "member") and not getattr(self, "member_released", False):
            self.turn("cleanup", {"member_ref": self.member, "release_revision": self.member_release_revision})
            self.member_released = True
        if hasattr(self, "primary"):
            statuses = []
            for status in self.observe_runs(list(dict.fromkeys([self.primary, *([self.member] if hasattr(self, "member") else [])]))):
                host.require(status["active_turn_id"] is None and
                             ((status["state"] == "idle" and status["identity_verdict"] == "Match")
                              or (status["run_id"] == self.member and status["state"] == "closed")),
                             "original managed Run not settled before Close")
                statuses.append(status)
            host.private(self.root / "ordinary-managed-pre-close.json", host.encoded(statuses))
            if self.serve is not None:
                self.serve.terminate()
                self.serve.wait()
                self.serve = None
            closed = self.product("run", "close", self.primary, "--workspace", self.root / "workspace",
                                  "--controller-file", self.root / "controller")
            integrated.require_closed_session(closed["data"], self.primary)
        if (self.root / "ordinary-bootstrap.json").exists() and not (self.root / "ordinary-bootstrap-close.pb").exists():
            self.helper("bootstrap-close")
        if self.serve is not None:
            self.serve.terminate()
            self.serve.wait()
            self.serve = None
        for name in ("integrated-successor-members", "integrated-successor"):
            if name == "integrated-successor-members" and self.prepared is not None:
                self.retire_managed_profile(name, ordinary.load(self.root / "qualified-binding.json")["server_key"])
            elif hasattr(self, "root_bootstrap"):
                host.require(getattr(self, "root_bootstrap_closed", False), "Root bootstrap cleanup remains pending")
                manifest = ordinary.load(self.root / "home/.dolgorae/workspaces" / self.workspace / "runs" / self.root_bootstrap / "manifest.json")
                self.retire_managed_profile(name, manifest["global_profile_binding"]["server_key"])
        return {"cleanup_complete": True, "route": "ordinary whole-session Close and Profile lifecycle"}

    def retire_managed_profile(self, name, old):
        if name == "integrated-successor":
            self.product("profile", "server", "stop", name, "--operator-file", self.root / "operator", "--confirm-server-key", old)
            return
        current = self.product("profile", "server", "status", name)["data"]["server_key"]
        if old != current:
            while True:
                migrated = self.product("profile", "server", "migrate", name, "--operator-file", self.root / "operator",
                    "--confirm-old-server-key", old, "--confirm-new-server-key", current,
                    expected_errors=("PROFILE_SERVER_BUSY", "TRANSPORT_FAILURE", "PROFILE_MISMATCH"))
                if migrated["ok"]:
                    break
                error = migrated["error"]
                if error["code"] == "PROFILE_MISMATCH":
                    host.require(error["details"].get("field") == "old_server_running"
                                 and error["details"].get("actual") is False, "Profile migration identity changed")
                    reset = self.product("profile", "state", "reset", name, "--operator-file", self.root / "operator",
                        "--confirm-server-key", old, "--require-server-absence", expected_errors=("PROFILE_SERVER_BUSY",))
                    if reset["ok"]:
                        break
                    error = reset["error"]
                host.require(error["retryable"] is True, "original managed Profile requires recovery")
                time.sleep(.1)
        self.product("profile", "server", "stop", name, "--operator-file", self.root / "operator", "--confirm-server-key", current)


def main():
    parser = argparse.ArgumentParser()
    for name in ("binary", "helper", "result-reader", "codex", "podway", "podway-archive", "scratch"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--result-reader-sha256", required=True)
    args = parser.parse_args()
    host.require(os.environ.get(host.OPT_IN) == "1", "explicit ordinary managed native opt-in required")
    for name in ("binary", "helper", "result_reader", "codex", "podway", "podway_archive", "scratch"):
        setattr(args, name, getattr(args, name).resolve(strict=True))
    host.require(hashlib.sha256(args.codex.read_bytes()).hexdigest() == host.PINS["codex"][1], "selected Codex image changed")
    host.require(hashlib.sha256(args.result_reader.read_bytes()).hexdigest() == args.result_reader_sha256, "selected managed read-only oracle changed")
    host.PodwayDistribution(args.podway_archive, args.podway)
    os.umask(0o077)
    root = Path(tempfile.mkdtemp(prefix="om-", dir=args.scratch)).resolve()
    for name in ("home", "tmp", "config", "cache", "workspace", "profile", "profile-members", "codex", "claude", "fault", "managed-native-host/journal"):
        (root / name).mkdir(mode=0o700, parents=True)
    host.private(root / "fixture-owned", b"native-production-host/v1\n")
    host.private(root / "delivery-count", b"0")
    args.host, args.scenario, args.executor_model = "codex", "consumer-group", "gpt-5.6-luna"
    case = ManagedCase(args, root)
    version = case.product("version", "--json")["version"].removeprefix("v")
    host.private(root / "input.json", host.encoded({"host": "managed-native-host", "scenario": "managed-producer", "binary": str(args.binary),
        "adapter_version": version, "executor_model": args.executor_model, "ordinary_unbounded": True,
        "executor_identity": {"path": str(args.codex), "sha256": host.PINS["codex"][1], "version": "0.164.0"}}))
    fixture = integrated.SuccessorResponses()
    fixture.producer_only, fixture.consumer_root = True, root
    fixture.managed_dispatch_finished = lambda: None
    fixture.source_check = lambda: integrated.assert_successor_dispatch(root, fixture)
    result, cleanup_errors = {"root": str(root), "passed": False}, []
    try:
        case.prepare(fixture)
        result.update(case.execute(), model_posts=fixture.posts, writer_turns=fixture.writer_steps)
        host.require(fixture.failure is None and fixture.writer_steps == 1 and fixture.steps == 4
                     and fixture.posts == integrated.SUCCESSOR_POSTS
                     and len(fixture.input_sizes) == fixture.posts
                     and getattr(fixture, "dispatch_checks", 0) == 2,
                     "ordinary managed native sequence incomplete")
        observed = case.observed
        facts = {"schema": "dolgorae.native-producer-facts/v1", "identity": observed["delegation_identity"]["identity"],
            "accepted_configuration": observed["accepted_configuration"], "original_task": observed["original_task"],
            "originals": {key: value.decode() for key, value in fixture.finished_reads.items()}, "native_pages": fixture.primary_pages,
            "native_admitted_input": host.admitted_native_input(fixture.managed_input, observed["accepted_configuration"], observed["original_task"]),
            "ordinary_aquarium_role": False, "semantic_goal_pass": False}
        host.private(root / "producer-facts.json", host.encoded(facts))
        result["main_phase_passed"] = True
    except BaseException as error:
        result["failure"] = repr(error)
    finally:
        try:
            result["cleanup"] = case.cleanup()
        except BaseException as error:
            cleanup_errors.append(repr(error))
        if case.serve is not None:
            result["retained_owner"] = {"serve_pid": case.serve.pid, "socket": str(root / "gateway.sock"),
                "root_run": getattr(case, "primary", None), "member_ref": getattr(case, "member", None),
                "task_ref": getattr(case, "task", None), "cleanup_complete": False,
                "next_owner": "Coordinator original-scope recovery"}
        fixture.close()
        result.update(passed=result.get("main_phase_passed", False) and not cleanup_errors and case.serve is None,
                      cleanup_errors=cleanup_errors, steps=case.steps,
                      fixture_failure=fixture.failure, fixture_failure_details=fixture.failure_details,
                      final_artifact_qualification="separate pending binding")
        host.private(root / "ordinary-managed-result.json", host.encoded(result))
        print(json.dumps(result), flush=True)
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
