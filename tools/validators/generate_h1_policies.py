#!/usr/bin/env python3
"""Reproduce checked v2 policy tables from the accepted method contract."""

import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PROTOCOL = ROOT / "docs/protocol"


def write(name, value):
    (PROTOCOL / name).write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")


def methods():
    source = (PROTOCOL / "dolgorae/public/v2/dolgorae.proto").read_text()
    return {
        f"{service}.{method}": {"request": request, "response": response, "stream": bool(stream)}
        for service, body in re.findall(r"service (\w+) \{(.*?)\n\}", source, re.S)
        for method, request, stream, response in re.findall(
            r"rpc (\w+)\((\w+)\) returns \((stream )?(\w+)\)", body
        )
    }


def main():
    contract = (ROOT / "docs/specs/dolgorae-v014-method-and-digest-contract.md").read_text()
    rows = {}
    for name, effect, errors, recovery in re.findall(
        r"^\| `(\w+\.\w+)` \| ([RK SVE]) \| (.*?) \| (.*?) \|$", contract, re.M
    ):
        rows[name] = {"effect_class": effect, "material_errors": re.findall(r"`([A-Z_]+)`", errors),
                      "recovery": recovery}
    for name in methods():
        if name in rows:
            continue
        action = name.split(".")[1]
        if action.startswith(("Get", "Lookup", "Validate", "Plan", "Await", "Collect")):
            effect = "R"
            errors = ["CONTROLLER_MISMATCH", "RUN_STATE_CONFLICT", "RECOVERY_REQUIRED"]
            recovery = "Refresh the same authorized scope and exact identity; unavailable or corrupt state cannot prove absence."
            if action.startswith("Await"):
                recovery = "Timeout ends only this wait; observe the same task/group without cancellation or redispatch."
            elif action.startswith("Collect"):
                errors += ["ARTIFACT_INTEGRITY_FAILURE", "RESULT_CONTENT_UNAVAILABLE"]
                recovery = "Read complete authorized result references and coverage; retain identity when content is unavailable."
            elif action == "LookupOperation":
                errors += ["LOOKUP_NAMESPACE_UNAVAILABLE", "LOOKUP_NAMESPACE_CORRUPT"]
                recovery = "Return exactly accepted/not_accepted/unresolved/absent under retained principal, generation, scope, kind and original key; missing index is recovery, not absence."
            elif action == "ValidateProvisioning":
                errors += ["SOURCE_TOO_LARGE", "SOURCE_CHANGED", "PROFILE_MISMATCH"]
                recovery = "Recapture explicitly selected safe sources before a new validation; validation cannot install or allocate."
            elif action == "PlanMaintenance":
                recovery = "Refresh explicit operator scope and generation; dry-run cannot collect or change state."
        elif action.startswith(("Cancel", "Release", "Close", "Apply")):
            effect = "S"
            errors = ["CONTROLLER_MISMATCH", "RUN_BUSY", "RUN_STATE_CONFLICT", "OUTCOME_UNKNOWN", "RECOVERY_REQUIRED"]
            recovery = "Read the same durable lifecycle owner and current generation/workload proof; acknowledgement is not terminal/released proof."
            if action == "ApplyMaintenance":
                errors += ["MAINTENANCE_PLAN_STALE", "STORAGE_CAPACITY_EXCEEDED", "ARTIFACT_INTEGRITY_FAILURE"]
                recovery = "Resume the recorded maintenance generation after supported recovery; preserve active/unknown references and receipts; capacity refusal differs from corruption."
        else:
            effect = "K"
            errors = ["CONTROLLER_MISMATCH", "IDEMPOTENCY_CONFLICT", "RUN_STATE_CONFLICT", "OUTCOME_UNKNOWN", "RECOVERY_REQUIRED"]
            recovery = "Lookup the original principal/generation/scope/kind/key before replay; accepted returns retained receipt before new revision checks; unresolved cannot redispatch."
            if action == "AssignGroup":
                errors += ["GROUP_LIMIT_EXCEEDED", "GROUP_MANIFEST_INVALID"]
                recovery += " Recover partial acceptance per immutable entry without repeating uncertain tasks."
            elif action == "MergeGroupResults":
                errors += ["FINDINGS_OUTPUT_INVALID", "RESULT_CONTENT_UNAVAILABLE"]
                recovery += " Reproduce sorted exact findings; retain missing/unknown/failed coverage and perform no implicit synthesis."
            elif action == "AssignSynthesis":
                recovery += " Synthesis is a separate ordinary accepted task and consumes its own conservative round."
            elif action == "InstallProvisioning":
                errors += ["PROFILE_MISMATCH", "SOURCE_CHANGED", "SOURCE_TOO_LARGE"]
            elif action.startswith(("Hire", "Spawn")):
                errors += ["POOL_CAPACITY_EXCEEDED", "SPECIALIST_CONFIGURATION_MISMATCH"]
                recovery += " Preserve original member, admission reservation and cleanup owner."
            elif action.startswith("Assign"):
                errors += ["SPECIALIST_NOT_IDLE", "ROUND_LIMIT_EXCEEDED", "WAIT_CYCLE", "WRITER_HELD_WAIT_FORBIDDEN"]
        if name.startswith("MaintenanceService."):
            errors = ["OPERATOR_MISMATCH" if code == "CONTROLLER_MISMATCH" else code for code in errors]
        elif action == "ValidateProvisioning":
            errors.remove("CONTROLLER_MISMATCH")
        rows[name] = {"effect_class": effect, "material_errors": errors, "recovery": recovery}
    discovered = methods()
    assert len(discovered) == 64 and set(rows) == set(discovered)
    for name, row in rows.items():
        row.update(discovered[name])
        row.update({"transparent_retry": False, "runtime_admission": "negotiated_only",
                    "keyed_lookup": "OperationService.LookupOperation" if row["effect_class"] == "K" else None,
                    "missing_mutation_detail": "Unknown" if row["effect_class"] in ("K", "S") else None})
    write("dolgorae-rpc-mutation-policy-v2.json", {
        "contract_id": "dolgorae.rpc-mutation-policy/v2", "schema_version": 2,
        "authority": "docs/specs/dolgorae-v014-method-and-digest-contract.md",
        "effects": {"R": "bounded read", "K": "intent before effect, receipt before acknowledgement",
                    "S": "state mutation with authoritative-state loss recovery", "V": "bounded verification", "E": "read-only event stream"},
        "methods": dict(sorted(rows.items()))})
    historical_mapping = json.loads((PROTOCOL / "dolgorae-grpc-error-mapping-v1.json").read_text())
    statuses = {code: status for status, codes in historical_mapping["status_overrides"].items() for code in codes}
    statuses.update({"LOOKUP_NAMESPACE_CORRUPT": "DATA_LOSS", "LOOKUP_NAMESPACE_UNAVAILABLE": "UNAVAILABLE",
                     "RESULT_CONTENT_UNAVAILABLE": "FAILED_PRECONDITION", "SOURCE_CHANGED": "ABORTED",
                     "GROUP_MANIFEST_INVALID": "INVALID_ARGUMENT", "MAINTENANCE_PLAN_STALE": "ABORTED"})
    for code in ["SOURCE_TOO_LARGE", "GROUP_LIMIT_EXCEEDED", "POOL_CAPACITY_EXCEEDED", "ROUND_LIMIT_EXCEEDED", "STORAGE_CAPACITY_EXCEEDED"]:
        statuses[code] = "RESOURCE_EXHAUSTED"
    codes = sorted({code for row in rows.values() for code in row["material_errors"]} | {
        "INVALID_ARGUMENT", "CONTROLLER_MISMATCH", "PROTOCOL_VERSION_UNSUPPORTED", "INTERNAL_ERROR", "TRANSPORT_FAILURE"})
    write("dolgorae-grpc-error-mapping-v2.json", {
        "contract_id": "dolgorae.grpc-error-mapping/v2", "schema_version": 2, "error_detail_version": 2,
        "detail": "dolgorae.public.v2.DolgoraeErrorDetail",
        "effect_states": ["none", "accepted", "unresolved"],
        "classifications": {"Rejected": {"effect": "none", "resolves_earlier_unknown": False},
            "RetrySameKey": {"new_key": False, "requires_method_lookup_permission": True},
            "Unknown": {"effect": "unresolved", "redispatch": False},
            "RecoveryNeeded": {"retains_receipts": True, "action": "typed_supported_recovery"}},
        "transport_or_missing_detail_on_mutation": "Unknown", "unknown_enum": "fail_closed",
        "infer_from_status_text": False, "error_code_carrier": "closed_by_this_policy_string",
        "unknown_error_code_on_mutation": "Unknown_until_authoritative_lookup_or_state_read",
        "code_status": {code: statuses.get(code, "FAILED_PRECONDITION") for code in codes},
        "common_errors": ["INVALID_ARGUMENT", "CONTROLLER_MISMATCH", "PROTOCOL_VERSION_UNSUPPORTED", "INTERNAL_ERROR", "TRANSPORT_FAILURE"],
        "required_context": ["method_specific_operation_kind", "effect", "classification", "authorized_operation_or_subject_identity_when_known"],
        "unkeyed_subjects": "typed run/member/task/group/merge/engagement/maintenance identities; never fabricate an idempotency key",
        "methods": {k: v["material_errors"] for k, v in sorted(rows.items())}})
    code_cases = "\n".join(f'\tcase "{code}":\n\t\treturn true' for code in codes)
    (PROTOCOL / "generated/gul-consumer-v2/go/client/known_error_codes.go").write_text(
        "// Code generated by generate_h1_policies.py. DO NOT EDIT.\n"
        "package client\n\nfunc knownErrorCode(code string) bool {\n\tswitch code {\n"
        + code_cases + "\n\tdefault:\n\t\treturn false\n\t}\n}\n")
    write("dolgorae-capabilities-v2.json", {
        "contract_id": "dolgorae.capability-policy/v2", "schema_version": 2,
        "protocol_version": 2, "descriptor": "dolgorae-public-v2.descriptor.pb",
        "checked_contract_implies_runtime_support": False, "default_availability": "unavailable",
        "admission_requires": ["supported_protocol_range", "exact_descriptor_sha256", "method_subset",
            "credential_identity", "server_instance_id", "bounds", "required_features"],
        "server_instance_change": "discard_cached_admission_and_renegotiate",
        "unknown_schema_or_enum": "typed_refusal", "methods": sorted(rows)})
    write("dolgorae-grpc-client-policy-v2.json", {
        "contract_id": "dolgorae.grpc-client-policy/v2", "schema_version": 2,
        "transparent_mutation_retries": False, "hedging": False, "wait_for_ready_on_mutations": False,
        "lookup_requires_original_scope_and_key": True, "protected_answer_persistence": False,
        "protected_answer_automatic_replay": False, "secret_resolution": "winning key returns only non-secret receipt; pending secret requires explicit re-entry",
        "receive_message_max_bytes": 8388608, "page_metadata_max_bytes": 1048576,
        "artifact_chunk_max_bytes": 1048576, "refresh_concurrency": 6,
        "heartbeat_heads": "advisory_only_not_ingestion_or_delivery_cursors",
        "timeline_failure": "independent_pending_interaction_refresh_remains_available",
        "refresh": "coalesced_mask_and_page_atomic_ingest_under_captured_heads",
        "unknown_enum": "preserve_readable_opaque_value_but_refuse_decision_or_mutation",
        "settling": "show_attention_until_authoritative_terminal_proof",
        "stream_end": {"RUN_TERMINAL": "fetch_final_snapshots", "SERVER_SHUTDOWN": "renegotiate_then_resume_committed_cursor",
            "SLOW_CONSUMER": "refresh_then_resume_committed_cursor", "TRANSPORT_FAILURE": "not_run_failure"}})
    old = json.loads((PROTOCOL / "dolgorae-gul-consumer-v1.json").read_text())
    subset = sorted(set(old["required_methods"]) | {
        "RunService.ForkRun", "RunService.VerifyRun", "RunService.SetDefaultEffort",
        "RunService.CreateWriteContinuation", "OperationService.LookupOperation",
        "ProvisioningService.ValidateProvisioning",
    } | {name for name in rows if name.startswith(("BrokerService.", "GroupService."))})
    write("dolgorae-gul-consumer-v2.json", {
        "contract_id": "dolgorae.gul-consumer/v2", "schema_version": 2, "protocol_version": 2,
        "descriptor_method_count": len(rows), "required_method_count": len(subset),
        "required_methods": subset, "admission": "negotiate applicable method subset per selected feature; contract publication grants no runtime availability",
        "generated_go_module": "github.com/rootkernel/dolgorae/contracts/gul-consumer-v2",
        "descriptor": "dolgorae-public-v2.descriptor.pb", "lock": "dolgorae-gul-consumer-v2.lock.json",
        "policies": ["dolgorae-capabilities-v2.json", "dolgorae-rpc-mutation-policy-v2.json", "dolgorae-grpc-error-mapping-v2.json", "dolgorae-grpc-client-policy-v2.json"],
        "creation_mode": "immutable requested access provenance; effective access and verification are separate",
        "heartbeat_heads": "advisory only", "timeline_refresh": "independent of pending-interaction refresh; page-atomic under captured head",
        "authorization": "Controller revalidated on every query/page; observer-safe projection cannot expose protected input or credentials",
        "result_collection": "complete artifact bytes verified by length/hash with retained identity on content retirement",
        "G4_cases": [f"G4-{i}" for i in range(1, 8)],
        "foundation_owner": "Gul E15 including E15-T21", "runtime_owner": "Gul E16 after H3",
        "producer_runtime_owners": ["TASK-070", "TASK-078", "TASK-080", "TASK-084", "TASK-085"],
        "gate_A": "TASK-069 independently accepts completed H2 after EPIC-018 handoff"})


if __name__ == "__main__":
    main()
