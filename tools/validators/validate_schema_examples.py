#!/usr/bin/env python3
"""Validate checked positive protocol examples against their JSON Schemas."""

from __future__ import annotations

import copy
import json
import sys
from pathlib import Path

from jsonschema import Draft202012Validator, FormatChecker
from referencing import Registry, Resource

PROTOCOL = Path(__file__).resolve().parents[2] / "docs" / "protocol"
EXAMPLES = PROTOCOL / "examples"


def schema_name(example_name: str) -> str:
    # Unversioned pre-cutover examples are frozen v1 fixtures (SPEC-006).
    # Explicit v2 names validate successor payloads; executable E2E checks
    # validate the current machine/v2 producers.
    prefixes = {
        "worker-runtime-v2-": "dolgorae-worker-runtime-v2.schema.json",
        "accepted-policy-session-v1-": "dolgorae-accepted-policy-session-v1.schema.json",
        "workspace-execution-": "dolgorae-workspace-execution-v1.schema.json",
        "workspace-policy-": "dolgorae-workspace-execution-v1.schema.json",
        "workspace-machine-v4-": "dolgorae-machine-v4.schema.json",
        "workspace-capability-v2.": "dolgorae-capabilities-v2.schema.json",
        "review-target-v2-": "dolgorae-review-target-v2.schema.json",
        "specialist-review-v4-": "dolgorae-specialist-review-tool-v4.schema.json",
        "error-contract-v4-": "dolgorae-error-contract-v4.json",
        "external-engagement-v4-": "dolgorae-external-specialist-facade-v4.schema.json",
        "error-contract-v2-": "dolgorae-error-contract-v2.json",
        "agent-configuration-snapshot-v2.": "dolgorae-agent-configuration-v2.schema.json",
        "agent-configuration-v2.": "dolgorae-agent-configuration-input-v2.schema.json",
        "agent-configuration.": "dolgorae-agent-configuration-v1.schema.json",
        "collaboration-": "dolgorae-collaboration-tool-v1.schema.json",
        "controller-credential.": "dolgorae-controller-credential-v1.schema.json",
        "external-engagement-v2-": "dolgorae-external-specialist-facade-v2.schema.json",
        "external-engagement-v3-": "dolgorae-external-specialist-facade-v3.schema.json",
        "external-engagement-": "dolgorae-external-specialist-facade-v1.schema.json",
        "idempotency-intent.": "dolgorae-idempotency-intent-v1.schema.json",
        "ledger-state.": "dolgorae-ledger-state-v1.schema.json",
        "one-shot-machine-v2-": "dolgorae-machine-v2.schema.json",
        "one-shot-operation-v1-": "dolgorae-one-shot-operation-v1.schema.json",
        "one-shot-review-observation-v1-": "dolgorae-one-shot-review-observation-v1.schema.json",
        "orchestration-state-v2.": "dolgorae-orchestration-state-v2.schema.json",
        "orchestration-state.": "dolgorae-orchestration-state-v1.schema.json",
        "orchestration-": "dolgorae-orchestration-tool-v1.schema.json",
        "profile-review-ownership-v1-": "dolgorae-profile-review-ownership-v1.schema.json",
        "role-source.": "dolgorae-role-source-v1.schema.json",
        "review-target-": "dolgorae-review-target-v1.schema.json",
        "specialist-policy-input-v2.": "dolgorae-specialist-policy-input-v2.schema.json",
        "specialist-policy-v2.": "dolgorae-specialist-policy-v2.schema.json",
        "specialist-policy.": "dolgorae-specialist-policy-v1.schema.json",
        "specialist-review-mcp-meta.": "dolgorae-specialist-review-mcp-meta-v1.schema.json",
        "specialist-review-request.": "dolgorae-specialist-review-tool-v1.schema.json",
        "specialist-review-result.": "dolgorae-specialist-review-tool-v1.schema.json",
        "specialist-review-idempotency-conflict.": "dolgorae-specialist-review-tool-v1.schema.json",
        "specialist-review-v2-": "dolgorae-specialist-review-tool-v2.schema.json",
        "specialist-review-v3-": "dolgorae-specialist-review-tool-v3.schema.json",
    }
    if example_name in {
        "engagement-call-machine-success.valid.json",
        "specialist-policy-show-machine-success.valid.json",
        "specialist-review-machine-success.valid.json",
    }:
        return "dolgorae-machine-v1.schema.json"
    for prefix, name in prefixes.items():
        if example_name.startswith(prefix):
            return name
    raise ValueError(f"no schema mapping for {example_name}")


def validation_schema(example_name: str, schema: dict[str, object]) -> dict[str, object]:
    if example_name.startswith("workspace-capability-v2.invalid-"):
        return {"$ref": "https://dolgorae.local/schema/capabilities/v2#/properties/caller_workspace"}
    if example_name.startswith("workspace-policy-input-v3."):
        return {"$ref": "https://dolgorae.local/schema/workspace-execution/v1#/$defs/policy_input"}
    if example_name.startswith("workspace-policy-snapshot-v3."):
        return {"$ref": "https://dolgorae.local/schema/workspace-execution/v1#/$defs/policy_snapshot"}
    if example_name.startswith("external-engagement-v3-await."):
        return {
            "$ref": (
                "https://dolgorae.local/schema/external-specialist-facade/v3"
                "#/$defs/structured_await_result"
            )
        }
    if example_name.startswith("external-engagement-v3-collect."):
        return {
            "$ref": (
                "https://dolgorae.local/schema/external-specialist-facade/v3"
                "#/$defs/structured_collect_result"
            )
        }
    return schema


def repaired_negative_example(
    example_name: str, instance: dict[str, object]
) -> dict[str, object]:
    repaired = copy.deepcopy(instance)
    if example_name == "external-engagement-v4-assign.invalid-isolated-intent.json":
        repaired["execution_intent"] = "read_only"
    elif example_name == "workspace-execution-start.invalid-isolated-access.json":
        repaired["access"] = "writable"
    elif example_name == "workspace-execution-start.invalid-unresolved-role.json":
        repaired["role"] = "general"
    elif example_name == "workspace-execution-instructions.invalid-source-path.json":
        del repaired["role"]["source_path"]
    elif example_name == "workspace-execution-agent-input.invalid-inline-body.json":
        del repaired["instructions"]
    elif example_name == "workspace-policy-input-v3.invalid-project-fallback.json":
        del repaired["roles"][0]["role_source"]["scope"]
    elif example_name == "workspace-machine-v4-start.invalid-unverified-policy.json":
        repaired["data"]["policy_verification"] = "verified"
    elif example_name == "workspace-machine-v4-start.invalid-read-fallback.json":
        repaired["data"]["effective_access"] = "write"
    elif example_name == "workspace-capability-v2.invalid-unqualified-integration.json":
        repaired["integrated"] = False
    elif example_name == "review-target-v2-head.invalid-worktree-boundary.json":
        repaired["reading_boundary"] = "checked_objects"
    elif example_name == "specialist-review-v4-request.invalid-capture-path.json":
        del repaired["target"]["immutable_root"]
    elif example_name == "specialist-review-v4-result.invalid-unverified-provenance.json":
        repaired["source_assurance"]["candidate_provenance"] = "verified"
    elif example_name == "specialist-review-v4-result.invalid-missing-read-receipt.json":
        for assessment in repaired["verdict"]["criterion_assessments"]:
            for evidence in assessment["evidence"]:
                if evidence["basis"] == "candidate":
                    evidence["read_receipt_id"] = "018f0000-0000-7000-8000-000000061002"
    elif example_name == "error-contract-v4-boundary.invalid-private-details.json":
        del repaired["details"]["controller_credential"]
    elif example_name == "error-contract-v4-direct-review.invalid-capture.json":
        del repaired["details"]["capture_ref"]
    elif example_name == "error-contract-v2-review-output-legacy.invalid-missing-reason.json":
        repaired["details"]["reason"] = "Reviewer output did not match the contract"
    elif example_name == "error-contract-v2-review-output-diagnostic.invalid-extra-details.json":
        del repaired["details"]["diagnostic"]["private_provider_key"]
    elif example_name == "error-contract-v2-review-task.invalid-extra-details.json":
        del repaired["details"]["profile"]
    elif example_name == "error-contract-v2-review-recovery-blocked.invalid-capability.json":
        del repaired["details"]["capability"]
    elif example_name == "one-shot-review-observation-v1-blocked.invalid-private-diagnostic.json":
        del repaired["diagnostic"]["private_provider_key"]
    elif example_name == "accepted-policy-session-v1-captured.invalid-schema-version.json":
        repaired["schema"] = "dolgorae.accepted-policy-session/v1"
    elif example_name == "one-shot-operation-v1-reserved.invalid-reference-version.json":
        repaired["input"]["request_ref"] = "018f0000-0000-7000-8000-000000058001"
    elif example_name == "profile-review-ownership-v1-reserved.invalid-launch-token.json":
        repaired["launch_token"] = "018f0000-0000-7000-8000-000000058006"
    elif example_name == "external-engagement-v3-assign.invalid-structured-purpose.json":
        repaired["task"]["purpose"] = "change"
    elif example_name in {
        "external-engagement-v3-await.invalid-missing-assessments.json",
        "external-engagement-v3-collect.invalid-missing-assessments.json",
    }:
        repaired["tasks"][0]["result"]["criterion_assessments"] = []
    elif example_name == (
        "specialist-review-v3-request.invalid-missing-completion-criterion.json"
    ):
        repaired["criteria"].append(
            {
                "id": "C-example",
                "statement": "The completion claim is demonstrated.",
                "source_context_ids": [],
            }
        )
    else:
        raise ValueError(f"no intended-invalid repair for {example_name}")
    return repaired


def validate_successor_cases(registry: Registry, errors: list[str]) -> int:
    """Exercise contract branches from the CW-SCOPE/PROJECT/error requirements.

    Reuse checked payloads as seeds, but keep expected scope and error mappings
    independent of the schemas so a changed conditional fails the gate.
    These are structural instances; runtime digest and provenance checks remain
    Rust obligations.
    """
    checked = 0

    def load_example(name: str) -> dict:
        return json.loads((EXAMPLES / name).read_text(encoding="utf-8"))

    def check(name: str, schema_id: str, instance: dict, valid: bool = True) -> None:
        nonlocal checked
        checked += 1
        validator = Draft202012Validator(
            {"$ref": f"https://dolgorae.local/schema/{schema_id}"},
            registry=registry,
            format_checker=FormatChecker(),
        )
        try:
            actual_errors = list(validator.iter_errors(instance))
            if bool(actual_errors) == valid:
                detail = actual_errors[0].message if actual_errors else "invalid instance accepted"
                errors.append(f"successor case {name}: {detail}")
        except Exception as exc:
            errors.append(f"successor case {name}: {exc}")

    head = load_example("review-target-v2-head.valid.json")
    request = load_example("specialist-review-v4-request.valid.json")
    targets = {}
    scopes = {
        "workspace": ("sampled_mutable", "checked_workspace", "worktree"),
        "staged": ("sampled_mutable", "checked_index", "index"),
        "dirty": ("sampled_mutable", "checked_workspace", "worktree"),
        "head": ("pinned_objects", "checked_objects", "git_object"),
        "commit": ("pinned_objects", "checked_objects", "git_object"),
        "range": ("pinned_objects", "checked_objects", "git_object"),
    }
    for kind, (assurance, boundary, source) in scopes.items():
        target = copy.deepcopy(head)
        target.update(kind=kind, assurance=assurance, reading_boundary=boundary)
        target["entries"][0]["source"] = source
        if source == "worktree":
            target["entries"][0]["blob_oid"] = None
        if kind == "workspace":
            target["resolved_head"] = None
        if kind in {"staged", "dirty", "commit", "range"}:
            target["resolved_base"] = "a" * 40
            target["entries"][0]["side"] = "after"
        selection = {"kind": kind}
        if kind in {"commit", "range"}:
            revision = "HEAD" if kind == "commit" else "main..HEAD"
            target["requested_revision"] = revision
            selection["revision"] = revision
        if kind == "range":
            target["range"] = {
                "mode": "two_dot", "left_oid": "a" * 40,
                "right_oid": target["resolved_head"],
            }
        targets[kind] = target
        check(kind, "review-target/v2", target)
        selected = copy.deepcopy(request)
        selected["target"] = selection
        check(f"{kind}-selection", "specialist-review-tool/v4", selected)
        wrong = copy.deepcopy(target)
        wrong["reading_boundary"] = (
            "checked_workspace" if boundary != "checked_workspace" else "checked_objects"
        )
        check(f"{kind}-wrong-reader", "review-target/v2", wrong, False)
        wrong = copy.deepcopy(selected)
        if kind in {"commit", "range"}:
            del wrong["target"]["revision"]
        else:
            wrong["target"]["revision"] = "HEAD"
        check(f"{kind}-wrong-revision", "specialist-review-tool/v4", wrong, False)

    three_dot = copy.deepcopy(targets["range"])
    three_dot["requested_revision"] = "main...HEAD"
    three_dot["range"]["mode"] = "three_dot"
    three_dot["resolved_base"] = "e" * 40
    check("three-dot-range", "review-target/v2", three_dot)
    for key in ("range", "resolved_base", "requested_revision"):
        wrong = copy.deepcopy(targets["range"])
        wrong[key] = None
        check(f"range-missing-{key}", "review-target/v2", wrong, False)
    wrong = copy.deepcopy(targets["commit"])
    wrong["range"] = targets["range"]["range"]
    check("commit-with-range", "review-target/v2", wrong, False)

    receipt = load_example("review-target-v2-read-receipt.valid.json")
    for name, seed, field in (
        ("entry", head, "entries"),
        ("receipt", receipt, None),
    ):
        for path in ("/outside.py", "../outside.py"):
            wrong = copy.deepcopy(seed)
            entry = wrong[field][0] if field else wrong
            entry["path"] = path
            check(f"{name}-unsafe-{path}", "review-target/v2", wrong, False)
        wrong = copy.deepcopy(seed)
        entry = wrong[field][0] if field else wrong
        entry["byte_length"] = 8388609
        check(f"{name}-oversized-file", "review-target/v2", wrong, False)
    wrong = copy.deepcopy(head)
    wrong["entries"] *= 20001
    check("too-many-entries", "review-target/v2", wrong, False)

    # The public error table owns these actions, phases and outcomes.
    error_actions = {
        "SOURCE_ISOLATION_RETIRED": "migrate_request",
        "ROLE_MIGRATION_REQUIRED": "migrate_role",
        "INSTRUCTION_SOURCE_INVALID": "fix_instruction_source",
        "INSTRUCTION_DISCOVERY_UNSUPPORTED": "select_supported_runtime",
        "WORKSPACE_STARTUP_UNSUPPORTED": "select_supported_runtime",
        "REVIEW_READING_BOUNDARY_UNSUPPORTED": "select_supported_runtime",
        "REVIEW_CANDIDATE_PROVENANCE_INVALID": "inspect_review",
        "REVIEW_TARGET_DRIFTED": "submit_new_review",
        "REVIEW_TARGET_UNAVAILABLE": "restore_target",
    }
    seed_error = load_example("error-contract-v4-review_reading_boundary_unsupported.valid.json")
    review_error = load_example("error-contract-v4-review_target_drifted.valid.json")
    for code, action in error_actions.items():
        error = copy.deepcopy(seed_error)
        error["code"] = code
        error["details"]["required_action"] = action
        if code in {"REVIEW_CANDIDATE_PROVENANCE_INVALID", "REVIEW_TARGET_DRIFTED"}:
            error["details"].update(
                phase="acceptance", outcome="inconclusive",
                review_id=review_error["details"]["review_id"],
            )
        check(code, "error-contract/v4", error)
        wrong = copy.deepcopy(error)
        wrong["details"]["required_action"] = (
            "inspect_review" if action != "inspect_review" else "migrate_request"
        )
        check(f"{code}-wrong-action", "error-contract/v4", wrong, False)
        wrong = copy.deepcopy(error)
        wrong["details"]["outcome"] = "unknown"
        check(f"{code}-wrong-outcome", "error-contract/v4", wrong, False)
        if (
            error["details"]["outcome"] == "not_dispatched"
            and code != "REVIEW_TARGET_UNAVAILABLE"
        ):
            wrong = copy.deepcopy(error)
            wrong["details"]["phase"] = "acceptance"
            check(f"{code}-wrong-phase", "error-contract/v4", wrong, False)
            wrong = copy.deepcopy(error)
            wrong["details"]["run_id"] = "01900000-0000-7000-8000-000000000001"
            check(f"{code}-allocated-run", "error-contract/v4", wrong, False)
        if code in {"REVIEW_CANDIDATE_PROVENANCE_INVALID", "REVIEW_TARGET_DRIFTED"}:
            wrong = copy.deepcopy(error)
            wrong["details"]["review_id"] = None
            check(f"{code}-missing-review-identity", "error-contract/v4", wrong, False)
    unavailable = copy.deepcopy(seed_error)
    unavailable["code"] = "REVIEW_TARGET_UNAVAILABLE"
    unavailable["details"].update(required_action="restore_target", phase="acceptance", outcome="failed")
    check("missing-object-after-admission", "error-contract/v4", unavailable)

    check("policy-input-embedded-id", "specialist-policy-input/v3",
          load_example("workspace-policy-input-v3.valid.json"))
    check("policy-snapshot-embedded-id", "specialist-policy/v3",
          load_example("workspace-policy-snapshot-v3.valid.json"))

    incomplete = copy.deepcopy(request)
    incomplete["criteria"] = []
    check("completion-without-criteria", "specialist-review-tool/v4", incomplete, False)
    assignment = load_example("external-engagement-v4-assign.valid.json")
    assignment["task"]["criteria"] = []
    check("assigned-completion-without-criteria", "external-specialist-facade/v4", assignment, False)

    result = load_example("specialist-review-v4-result.valid.json")
    for field in ("criterion_assessments", "findings"):
        for path in ("/outside.py", "../outside.py"):
            wrong = copy.deepcopy(result)
            if field == "criterion_assessments":
                wrong["verdict"][field][0]["evidence"][0]["path"] = path
            else:
                wrong["verdict"][field][0]["path"] = path
            check(f"review-{field}-unsafe-{path}", "specialist-review-tool/v4", wrong, False)
    result["target"] = targets["staged"]
    result["source_assurance"]["drift_check"] = "unchanged"
    limit = "Sampled drift checks can miss intervening edits; review has no snapshot isolation."
    result["source_assurance"]["evidence_limits"] = [limit]
    result["verdict"]["evidence_limits"] = [limit]
    check("mutable-result", "specialist-review-tool/v4", result)
    for section in ("source_assurance", "verdict"):
        wrong = copy.deepcopy(result)
        wrong[section]["evidence_limits"] = []
        check(f"mutable-result-missing-{section}-limits", "specialist-review-tool/v4", wrong, False)
    wrong = copy.deepcopy(result)
    wrong["source_assurance"]["drift_check"] = "not_applicable"
    check("mutable-result-skipped-drift", "specialist-review-tool/v4", wrong, False)

    machine = load_example("workspace-machine-v4-start.valid.json")
    for command, access, writer in (
        ("run.acquire-write", "write", "active"),
        ("run.release-write", "read", "none"),
    ):
        envelope = copy.deepcopy(machine)
        envelope["command"] = command
        envelope["data"].update(effective_access=access, writer_state=writer)
        check(command, "machine-output/v4", envelope)
        if command == "run.acquire-write":
            wrong = copy.deepcopy(envelope)
            wrong["data"]["writer_generation"] = None
            check("acquire-write-missing-generation", "machine-output/v4", wrong, False)
        wrong = copy.deepcopy(envelope)
        wrong["data"]["thread_id"] = None
        check(f"{command}-missing-thread", "machine-output/v4", wrong, False)
        envelope["data"]["effective_access"] = "read" if access == "write" else "write"
        check(f"{command}-wrong-access", "machine-output/v4", envelope, False)
    reader = copy.deepcopy(machine)
    reader["data"].update(
        requested_access="read-only", effective_access="read",
        writer_state="none", writer_generation=None,
    )
    check("read-only-start", "machine-output/v4", reader)
    reader["data"]["effective_access"] = "write"
    check("read-only-start-wrong-access", "machine-output/v4", reader, False)
    status = copy.deepcopy(machine)
    status["command"] = "run.status"
    check("writer-status", "machine-output/v4", status)
    for field, value in (
        ("policy_verification", "unverified"), ("writer_state", "none"),
        ("writer_generation", None), ("thread_id", None),
    ):
        wrong = copy.deepcopy(status)
        wrong["data"][field] = value
        check(f"writer-status-invalid-{field}", "machine-output/v4", wrong, False)
    wrong = copy.deepcopy(status)
    wrong["data"] = {}
    check("status-missing-projection", "machine-output/v4", wrong, False)
    failure = {key: value for key, value in machine.items() if key != "data"}
    failure.update(ok=False, error=seed_error)
    check("machine-admission-failure", "machine-output/v4", failure)

    capabilities = load_example("workspace-capability-v2.valid.json")
    wrong = copy.deepcopy(capabilities)
    wrong["caller_workspace"]["physical_start"] = "lazy_first_input"
    check("modern-startup-legacy-disclosure", "capabilities/v2", wrong, False)
    envelope = copy.deepcopy(machine)
    envelope.update(command="capabilities", data=capabilities)
    check("machine-capabilities", "machine-output/v4", envelope)
    qualified = copy.deepcopy(capabilities)
    qualified["profile"] = "reviewer"
    qualified["caller_workspace"]["integrated"] = True
    guarantees = ("managed_instructions", "modern_startup", "checked_candidate_reads")
    for name in guarantees:
        qualified["caller_workspace"][name] = {
            "state": "supported", "reason_code": "qualified", "evidence_sha256": "a" * 64,
        }
    check("qualified-capabilities", "capabilities/v2", qualified)
    for name in guarantees:
        wrong = copy.deepcopy(capabilities)
        wrong["caller_workspace"][name] = qualified["caller_workspace"][name]
        check(f"null-profile-supported-{name}", "capabilities/v2", wrong, False)
        wrong = copy.deepcopy(qualified)
        wrong["caller_workspace"][name] = capabilities["caller_workspace"][name]
        check(f"integrated-unqualified-{name}", "capabilities/v2", wrong, False)
        for field, value in (("reason_code", "not_qualified"), ("evidence_sha256", None)):
            wrong = copy.deepcopy(qualified)
            wrong["caller_workspace"][name][field] = value
            check(f"supported-{name}-invalid-{field}", "capabilities/v2", wrong, False)
    wrong = copy.deepcopy(qualified)
    wrong["profile"] = None
    check("integrated-null-profile", "capabilities/v2", wrong, False)
    return checked


def main() -> int:
    registry = Registry()
    for path in sorted(PROTOCOL.glob("*.json")):
        schema = json.loads(path.read_text(encoding="utf-8"))
        if isinstance(schema, dict) and "$id" in schema:
            registry = registry.with_resource(schema["$id"], Resource.from_contents(schema))

    errors: list[str] = []
    examples = sorted(EXAMPLES.glob("*.valid.json"))
    for path in examples:
        try:
            schema_path = PROTOCOL / schema_name(path.name)
            schema = validation_schema(
                path.name, json.loads(schema_path.read_text(encoding="utf-8"))
            )
            instance = json.loads(path.read_text(encoding="utf-8"))
            validator = Draft202012Validator(
                schema, registry=registry, format_checker=FormatChecker()
            )
            for error in validator.iter_errors(instance):
                location = "/".join(map(str, error.absolute_path)) or "<root>"
                errors.append(f"{path.name}:{location}: {error.message}")
        except Exception as exc:
            errors.append(f"{path.name}: {exc}")

    negative_examples = sorted(
        [
            *EXAMPLES.glob("error-contract-v2-*.invalid-*.json"),
            *EXAMPLES.glob("external-engagement-v3-*.invalid-*.json"),
            *EXAMPLES.glob("specialist-review-v3-*.invalid-*.json"),
            *EXAMPLES.glob("one-shot-*.invalid-*.json"),
            *EXAMPLES.glob("profile-review-ownership-v1-*.invalid-*.json"),
            *EXAMPLES.glob("workspace-*.invalid-*.json"),
            *EXAMPLES.glob("review-target-v2-*.invalid-*.json"),
            *EXAMPLES.glob("specialist-review-v4-*.invalid-*.json"),
            *EXAMPLES.glob("error-contract-v4-*.invalid-*.json"),
            *EXAMPLES.glob("external-engagement-v4-*.invalid-*.json"),
            *EXAMPLES.glob("accepted-policy-session-v1-*.invalid-*.json"),
        ]
    )
    recognized_examples = {
        *examples,
        *EXAMPLES.glob("*.invalid-*.json"),
    }
    for path in sorted(EXAMPLES.glob("*.json")):
        if path not in recognized_examples:
            errors.append(
                f"{path.name}: example name must end in .valid.json or .invalid-<reason>.json"
            )
    for path in negative_examples:
        try:
            schema_path = PROTOCOL / schema_name(path.name)
            schema = validation_schema(
                path.name, json.loads(schema_path.read_text(encoding="utf-8"))
            )
            instance = json.loads(path.read_text(encoding="utf-8"))
            validator = Draft202012Validator(
                schema, registry=registry, format_checker=FormatChecker()
            )
            validation_errors = list(validator.iter_errors(instance))
            if not validation_errors:
                errors.append(f"{path.name}: invalid example was accepted")
                continue
            repaired = repaired_negative_example(path.name, instance)
            repaired_errors = list(validator.iter_errors(repaired))
            if repaired_errors:
                errors.append(
                    f"{path.name}: repairing the named defect did not make the example valid: "
                    f"{repaired_errors[0].message}"
                )
        except Exception as exc:
            errors.append(f"{path.name}: {exc}")

    successor_cases = validate_successor_cases(registry, errors)
    if errors:
        print("Schema example validation failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print(
        "Schema example validation passed: "
        f"{len(examples)} positive and {len(negative_examples)} negative examples; "
        f"{successor_cases} successor branch cases"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
