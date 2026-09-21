#!/usr/bin/env python3
"""Verify the checked public-v1 descriptor and immutable consumer lock."""

from __future__ import annotations

import hashlib
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from google.protobuf import descriptor_pool, json_format, message_factory
from google.protobuf.descriptor_pb2 import FileDescriptorSet

ROOT = Path(__file__).resolve().parents[2]
PROTOCOL = ROOT / "docs" / "protocol"


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def git_blob(path: Path) -> str:
    data = path.read_bytes()
    return hashlib.sha1(f"blob {len(data)}\0".encode() + data).hexdigest()


def load_json(name: str) -> dict:
    return json.loads((PROTOCOL / name).read_text(encoding="utf-8"))


def descriptor_methods(public) -> set[str]:
    return {
        f"{service.name}.{method.name}"
        for service in public.service
        for method in service.method
    }


def validate_fixture_messages(
    file_set: FileDescriptorSet, fixtures: dict, errors: list[str]
) -> None:
    pool = descriptor_pool.DescriptorPool()
    pending = list(file_set.file)
    while pending:
        deferred = []
        for file_proto in pending:
            try:
                pool.Add(file_proto)
            except TypeError:
                deferred.append(file_proto)
        if len(deferred) == len(pending):
            errors.append("descriptor dependencies cannot be loaded for fixture validation")
            return
        pending = deferred
    for fixture in fixtures["positive"]:
        try:
            descriptor = pool.FindMessageTypeByName(fixture["grpc_message"])
            message = message_factory.GetMessageClass(descriptor)()
            json_format.ParseDict(fixture["grpc_json"], message)
        except Exception as exc:
            errors.append(f"fixture {fixture.get('id')} is not valid protobuf JSON: {exc}")


def validate_breaking_baseline(baseline: Path, errors: list[str]) -> None:
    if shutil.which("buf") is None:
        errors.append("buf is unavailable for the checked breaking baseline")
        return
    with tempfile.NamedTemporaryFile(suffix=".binpb") as copied:
        copied.write(baseline.read_bytes())
        copied.flush()
        result = subprocess.run(
            ["buf", "breaking", str(PROTOCOL), "--against", copied.name],
            cwd=ROOT,
            text=True,
            capture_output=True,
        )
    if result.returncode != 0:
        errors.append(f"public v1 breaks the pre-TASK-053 baseline: {result.stderr.strip()}")


def main() -> int:
    manifest_path = PROTOCOL / "dolgorae-public-v1.descriptor.json"
    manifest = load_json("dolgorae-public-v1.descriptor.json")
    source = PROTOCOL / manifest["source"]
    descriptor = PROTOCOL / manifest["descriptor"]
    baseline = PROTOCOL / manifest["buf_breaking"]["baseline"]
    contract = load_json("dolgorae-gul-consumer-v1.json")
    fixtures = load_json("dolgorae-gul-consumer-v1.fixtures.json")
    lock = load_json("dolgorae-gul-consumer-v1.lock.json")
    conformance = load_json("dolgorae-grpc-conformance-v1.json")
    capabilities = load_json("dolgorae-capabilities-v1.schema.json")
    error_mapping = load_json("dolgorae-grpc-error-mapping-v1.json")
    mutation_policy = load_json("dolgorae-rpc-mutation-policy-v1.json")
    errors: list[str] = []
    if digest(source) != manifest["source_sha256"]:
        errors.append("source digest does not match the descriptor manifest")
    if digest(descriptor) != manifest["descriptor_sha256"]:
        errors.append("descriptor digest does not match the descriptor manifest")
    if digest(baseline) != manifest["buf_breaking"]["baseline_sha256"]:
        errors.append("breaking baseline digest does not match the descriptor manifest")

    for artifact in lock["artifacts"]:
        path = PROTOCOL / artifact["path"]
        if not path.is_file() or digest(path) != artifact["sha256"]:
            errors.append(f"consumer lock digest mismatch: {artifact['path']}")
    if git_blob(source) != lock["source_revision"]["proto_git_blob"]:
        errors.append("consumer lock proto Git blob identity mismatch")
    if git_blob(descriptor) != lock["source_revision"]["descriptor_git_blob"]:
        errors.append("consumer lock descriptor Git blob identity mismatch")

    file_set = FileDescriptorSet.FromString(descriptor.read_bytes())
    public = next(
        (item for item in file_set.file if item.name == manifest["source"]), None
    )
    if public is None:
        errors.append("public proto is absent from the descriptor set")
    else:
        methods = descriptor_methods(public)
        required_methods = set(contract["required_methods"])
        if len(methods) != 36 or contract["descriptor_method_count"] != 36:
            errors.append(f"public descriptor must declare exactly 36 methods, found {len(methods)}")
        if len(required_methods) != 27 or contract["required_method_count"] != 27:
            errors.append("Gul consumer profile must contain exactly 27 unique methods")
        if not required_methods <= methods:
            errors.append("Gul consumer profile contains methods absent from the descriptor")
        if required_methods & set(contract["unavailable_until_later_tasks"]):
            errors.append("required and unavailable method sets overlap")
        orchestration = next(
            (service for service in public.service if service.name == "OrchestrationService"),
            None,
        )
        if orchestration is None or [item.name for item in orchestration.method] != [
            "GetOrchestratedSession",
            "ListOrchestratedSessionResults",
        ]:
            errors.append("OrchestrationService does not expose the exact two frozen reads")
        messages = {item.name: item for item in public.message_type}
        for name in (
            "ListProfilesRequest",
            "GetProfileRequest",
            "ListProfileDiagnosticsRequest",
        ):
            message = messages.get(name)
            if message is None:
                errors.append(f"{name} is absent")
                continue
            if any(field.name == "workspace" or field.number == 2 for field in message.field):
                errors.append(f"{name} still exposes workspace field 2")
            if "workspace" not in message.reserved_name:
                errors.append(f"{name} does not reserve workspace")
            if not any(item.start == 2 and item.end == 3 for item in message.reserved_range):
                errors.append(f"{name} does not reserve field 2")
        start = messages.get("StartRunRequest")
        start_fields = {} if start is None else {field.name: field.number for field in start.field}
        if start_fields.get("workspace") != 2 or start_fields.get("profile_name") != 5:
            errors.append("StartRunRequest does not preserve workspace=2 and profile_name=5")

        expected_fields = {
            "session.context",
            "session.session_id",
            "session.primary_run",
            "session.aggregate_revision",
            "session.lifecycle",
            "session.composition",
            "session.approval_policy",
            "session.specialist_policy_name",
            "session.specialist_policy_revision",
            "session.specialist_policy_sha256",
            "session.nonretired_member_count",
            "session.nonterminal_spawn_count",
            "session.pending_approval_count",
            "session.accepted_unfinished_task_count",
            "session.unknown_outcome_task_count",
            "session.published_result_count",
            "session.close_intent",
            "session.close_progress",
            "session.close_operation_id",
            "session.recovery_classification",
            "session.required_action",
            "session.captured_at",
            "session.source_revision",
            "session.availability",
            "results.context",
            "results.captured_publication_head",
            "results.source_revision",
            "results.captured_at",
            "results.items.result_id",
            "results.items.task_id",
            "results.items.specialist_run",
            "results.items.specialist_role",
            "results.items.publication_order",
            "results.items.published_at",
            "results.items.format",
            "results.items.byte_length",
            "results.items.sha256",
            "results.items.artifact",
            "results.items.artifact_owner",
            "results.next_page_cursor",
        }
        source_fields = {item["field"] for item in contract["field_sources"]}
        if source_fields != expected_fields:
            errors.append("consumer field-sourceability matrix is incomplete or contains extras")
        fixture_ids = (
            {item["id"] for item in fixtures["positive"]}
            | {item["id"] for item in fixtures["negative"]}
            | {item["id"] for item in fixtures["close_outcomes"]}
            | set(fixtures["fixture_groups"])
        )
        if any(item["fixture"] not in fixture_ids for item in contract["field_sources"]):
            errors.append("consumer field-sourceability matrix references an unknown fixture")
        semantics = contract["session_semantics"]
        if set(semantics["lifecycle"]) != {
            "CREATING",
            "ACTIVE",
            "DEGRADED",
            "RECOVERING",
            "COMPLETING",
            "ABORTING",
            "COMPLETED",
            "ABORTED",
        }:
            errors.append("orchestrated lifecycle mapping is not exhaustive")
        if set(semantics["close_progress"]) != {
            "NONE",
            "SETTLING",
            "COMPLETED",
            "ABORTED",
            "RECOVERY_REQUIRED",
            "OUTCOME_UNKNOWN",
        }:
            errors.append("session close progress mapping is not exhaustive")

    profile = conformance["consumer_profiles"]["dolgorae.gul-consumer/v1"]
    if profile["required_methods"] != contract["required_methods"]:
        errors.append("conformance consumer profile differs from the immutable contract")
    if conformance["delivery_stages"]["MILESTONE-BH1-P"]["required_methods"] != contract[
        "required_methods"
    ]:
        errors.append("conformance BH1-P delivery stage differs from the immutable contract")
    stage = capabilities["properties"]["grpc_methods"]["x-stageRequirements"]
    if stage["MILESTONE-BH1-P"] != contract["required_methods"]:
        errors.append("capability BH1-P profile differs from the immutable contract")
    method_coverage = fixtures["method_coverage"]
    if {item["method"] for item in method_coverage} != set(contract["required_methods"]):
        errors.append("consumer fixture method coverage is not the exact 27-method profile")
    if len(method_coverage) != 27 or any(
        not item.get("positive") or not item.get("negative") for item in method_coverage
    ):
        errors.append("every consumer method requires positive and negative fixture coverage")

    credential = PROTOCOL / "dolgorae-controller-credential-v1.schema.json"
    credential_digest = digest(credential)
    advertised_digest = capabilities["properties"]["controller_credential"]["properties"][
        "schema_sha256"
    ]["const"]
    if credential_digest != advertised_digest:
        errors.append("advertised controller credential digest differs from distributed bytes")

    close_errors = error_mapping["method_overrides"].get("RunService.CloseRun", {})
    in_progress = close_errors.get("SESSION_CLOSE_IN_PROGRESS", {})
    if (
        in_progress.get("status") != "FAILED_PRECONDITION"
        or in_progress.get("retry_classification") != "RETRY_CLASSIFICATION_FORBIDDEN"
        or set(in_progress.get("required_fields", [])) != {"run_id", "operation_id"}
    ):
        errors.append("SESSION_CLOSE_IN_PROGRESS mapping is incomplete")
    close_policy = next(
        item for item in mutation_policy["mutations"] if item["rpc"] == "RunService.CloseRun"
    )
    if close_policy.get("accepted_session_intent_retry") != "forbidden":
        errors.append("CloseRun accepted-intent retry is not forbidden")

    validate_fixture_messages(file_set, fixtures, errors)
    validate_breaking_baseline(baseline, errors)

    if errors:
        print("Public descriptor validation failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print(
        "Public descriptor validation passed: 36-method descriptor, 27-method "
        "consumer lock, fixtures, digests, and breaking baseline are exact"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
