#!/usr/bin/env python3
"""Check H1 byte bindings, descriptor policy coverage and typed wire fixtures."""

from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path
from urllib.parse import urldefrag, urljoin

from google.protobuf import descriptor_pool, json_format, message_factory
from google.protobuf.descriptor_pb2 import FileDescriptorSet
from jsonschema import Draft202012Validator, FormatChecker
from referencing import Registry, Resource

ROOT = Path(__file__).resolve().parents[2]
PROTOCOL = ROOT / "docs/protocol"


def load(name, selected=None):
    relative = "docs/protocol/" + name
    if selected is None:
        return json.loads((ROOT / relative).read_bytes())
    assert relative in selected, f"unselected input: {relative}"
    return json.loads(selected[relative])


def verified_selected_inputs():
    """Verify every selected byte before parsing schemas or checking fixtures."""
    lock = load("dolgorae-gul-consumer-v2.lock.json")
    selected = {}
    for item in lock["artifacts"] + lock["frozen_v1_artifacts"]:
        relative = Path(item["path"])
        assert not relative.is_absolute() and ".." not in relative.parts, "unsafe selected path"
        content = (ROOT / relative).read_bytes()
        assert hashlib.sha256(content).hexdigest() == item["sha256"], f"byte binding mismatch: {relative}"
        if item["path"] in selected:
            assert selected[item["path"]] == content, f"conflicting selected path: {relative}"
        selected[item["path"]] = content
    resources = {}
    references = []

    def visit(resource, base):
        identifier = resource.id()
        if identifier is not None:
            base = urljoin(base, identifier)
            assert base not in resources, f"duplicate selected schema ID: {base}"
            resources[base] = resource
        schema = resource.contents
        if isinstance(schema, dict) and "$ref" in schema:
            references.append(urldefrag(urljoin(base, schema["$ref"]))[0])
        # Resource knows which children are schemas, including the existing
        # embedded specialist-policy schema and its separately scoped $id.
        for child in resource.subresources():
            visit(child, base)

    for name, content in selected.items():
        if not name.endswith(".json"):
            continue
        value = json.loads(content)
        if isinstance(value, dict) and "$id" in value:
            visit(Resource.from_contents(value), value["$id"])
    for target in references:
        assert target in resources, f"unselected schema dependency: {target}"
    # No retriever: every dependency is a verified selected resource.
    registry = Registry().with_resources(resources.items())
    return lock, selected, registry


def validate_normalized_bodies(fixtures, registry):
    for operation, fixture in fixtures["constructed_mutation_inputs"].items():
        validator = Draft202012Validator(
            {"$ref": fixture["identity"]["request_schema"]}, registry=registry,
            format_checker=FormatChecker(),
        )
        errors = list(validator.iter_errors(fixture["normalized_body"]))
        assert not errors, f"normalized {operation}: {errors[0].json_path}: {errors[0].message}"


REQUIRED_RELEASE_CASES = {
    'external-idle': True,
    'external-idle-facade': True,
    'external-idle-facade-mismatch': False,
    'external-idle-machine': True,
    'external-idle-machine-mismatch': False,
    'external-released': True,
    'external-released-facade': True,
    'external-released-facade-mismatch': False,
    'external-released-machine': True,
    'external-released-machine-mismatch': False,
    'external-released-null': False,
    'external-idle-proof': False,
    'external-outer-inner': False,
    'external-keyed-receipt': False,
    'external-running': False,
    'external-queued': False,
    'external-claimed': False,
    'external-dispatching': False,
    'external-unresolved': False,
    'external-pending_interaction': False,
    'external-cleanup_pending': False,
    'external-missing-run': False,
    'external-extra-field': False,
    'external-bad-operation-ref': False,
    'external-negative-terminal': False,
    'external-overflow-workload': False,
    'external-zero-controller': False,
    'external-wrong-kind': False,
    'external-wrong-scope': False,
    'external-cross-kind': False,
    'external-cross-scope': False,
    'external-record-requested': True,
    'external-record-requested-completion': False,
    'external-record-requested-failure': False,
    'external-record-requested-action': False,
    'external-record-settling': True,
    'external-record-settling-completion': False,
    'external-record-settling-failure': False,
    'external-record-settling-action': False,
    'external-record-terminal': True,
    'external-record-terminal-completion': False,
    'external-record-terminal-failure': False,
    'external-record-terminal-action': False,
    'external-record-cleanup_failed': True,
    'external-record-cleanup_failed-completion': False,
    'external-record-cleanup_failed-failure': False,
    'external-record-cleanup_failed-action': False,
    'external-record-recovery_needed': True,
    'external-record-recovery_needed-completion': False,
    'external-record-recovery_needed-failure': False,
    'external-record-recovery_needed-action': False,
    'external-record-missing-admitted': False,
    'external-record-extra': False,
    'external-record-negative-admitted': False,
    'external-record-overflow-admitted': False,
    'external-record-kind-scope': False,
    'managed-idle': True,
    'managed-idle-facade': True,
    'managed-idle-facade-mismatch': False,
    'managed-idle-machine': True,
    'managed-idle-machine-mismatch': False,
    'managed-released': True,
    'managed-released-facade': True,
    'managed-released-facade-mismatch': False,
    'managed-released-machine': True,
    'managed-released-machine-mismatch': False,
    'managed-released-null': False,
    'managed-idle-proof': False,
    'managed-outer-inner': False,
    'managed-keyed-receipt': False,
    'managed-running': False,
    'managed-queued': False,
    'managed-claimed': False,
    'managed-dispatching': False,
    'managed-unresolved': False,
    'managed-pending_interaction': False,
    'managed-cleanup_pending': False,
    'managed-missing-run': False,
    'managed-extra-field': False,
    'managed-bad-operation-ref': False,
    'managed-negative-terminal': False,
    'managed-overflow-workload': False,
    'managed-zero-controller': False,
    'managed-wrong-kind': False,
    'managed-wrong-scope': False,
    'managed-cross-kind': False,
    'managed-cross-scope': False,
    'managed-record-requested': True,
    'managed-record-requested-completion': False,
    'managed-record-requested-failure': False,
    'managed-record-requested-action': False,
    'managed-record-settling': True,
    'managed-record-settling-completion': False,
    'managed-record-settling-failure': False,
    'managed-record-settling-action': False,
    'managed-record-terminal': True,
    'managed-record-terminal-completion': False,
    'managed-record-terminal-failure': False,
    'managed-record-terminal-action': False,
    'managed-record-cleanup_failed': True,
    'managed-record-cleanup_failed-completion': False,
    'managed-record-cleanup_failed-failure': False,
    'managed-record-cleanup_failed-action': False,
    'managed-record-recovery_needed': True,
    'managed-record-recovery_needed-completion': False,
    'managed-record-recovery_needed-failure': False,
    'managed-record-recovery_needed-action': False,
    'managed-record-missing-admitted': False,
    'managed-record-extra': False,
    'managed-record-negative-admitted': False,
    'managed-record-overflow-admitted': False,
    'managed-record-kind-scope': False,
}


def validate_release_proof_fixtures(selected, registry):
    """Check the bounded portable release inventory, including intended repairs.

    Cross-record comparisons belong to the Rust contract tests. This validator
    checks only schema facts with selected, local dependencies.
    """
    inventory = load("dolgorae-v014-release-proof-fixtures-v1.json", selected)
    assert inventory["schema"] == "dolgorae.release-proof-fixtures/v1"
    assert inventory["contract_only"] is True and inventory["runtime_acceptance"] is False
    cases = inventory["cases"]
    ids = [case["id"] for case in cases]
    assert len(ids) == len(set(ids)), "duplicate release fixture ID"
    assert {case["id"]: case["valid"] for case in cases} == REQUIRED_RELEASE_CASES, "release fixture coverage/outcome drift"
    prefixes = {
        "v014-identity-": "dolgorae-v014-identities-v1.schema.json",
        "v014-state-": "dolgorae-successor-state-v1.schema.json",
        "successor-facade-v5-": "dolgorae-external-specialist-facade-v5.schema.json",
        "successor-machine-v5-": "dolgorae-machine-v5.schema.json",
    }
    seen = set()
    for case in cases:
        path = Path(case["path"])
        assert path.as_posix() == case["path"] and path.parent == Path("docs/protocol/examples") and path.suffix == ".json", "unsafe release fixture path"
        assert case["path"] not in seen, "duplicate release fixture path"
        seen.add(case["path"])
        absolute = ROOT / path
        assert not absolute.is_symlink() and absolute.is_file(), f"missing release fixture: {path}"
        name = next((schema for prefix, schema in prefixes.items() if path.name.startswith(prefix)), None)
        assert name is not None, f"unrouted release fixture: {path}"
        validator = Draft202012Validator(load(name, selected), registry=registry, format_checker=FormatChecker())
        assert case["path"] in selected, f"unselected release fixture: {path}"
        instance = json.loads(selected[case["path"]])
        errors = list(validator.iter_errors(instance))
        assert bool(errors) is not case["valid"], f"release fixture {case['id']}: unexpected schema outcome"
        if not case["valid"]:
            repair = case["repair"]
            repaired = copy.deepcopy(instance)
            parent = repaired
            for component in repair["path"][:-1]:
                parent = parent[component]
            key = repair["path"][-1]
            if repair.get("remove"):
                del parent[key]
            else:
                parent[key] = repair["value"]
            assert not list(validator.iter_errors(repaired)), f"release fixture {case['id']}: single-defect repair failed"
    print(f"Release proof schemas passed: {len(cases)} fixed cases, outcomes and repairs")


def main():
    lock, selected, registry = verified_selected_inputs()
    validate_release_proof_fixtures(selected, registry)
    paths = [item["path"] for item in lock["artifacts"]]
    assert len(paths) == len(set(paths)), "duplicate H1 artifact"
    assert "docs/protocol/dolgorae-gul-consumer-v2.lock.json" not in paths, "recursive lock identity"
    files = FileDescriptorSet.FromString(selected["docs/protocol/dolgorae-public-v2.descriptor.pb"])
    public = next(f for f in files.file if f.package == "dolgorae.public.v2")
    assert all(f.package != "dolgorae.public.v1" for f in files.file), "v2 descriptor embeds frozen v1"
    methods = {f"{s.name}.{m.name}": m for s in public.service for m in s.method}
    assert len(methods) == 64
    policies = load("dolgorae-rpc-mutation-policy-v2.json", selected)["methods"]
    assert set(policies) == set(methods), "per-method policy coverage"
    error_policy = load("dolgorae-grpc-error-mapping-v2.json", selected)
    assert set(error_policy["methods"]) == set(methods)
    for codes in error_policy["methods"].values():
        assert set(codes) <= set(error_policy["code_status"])
    for name, method in methods.items():
        row = policies[name]
        assert method.input_type.rsplit(".", 1)[-1] == row["request"], name
        assert method.output_type.rsplit(".", 1)[-1] == row["response"], name
        assert method.server_streaming == row["stream"], name
        assert row["material_errors"] and row["recovery"] and not row["transparent_retry"], name
        if row["effect_class"] == "K":
            assert row["keyed_lookup"] == "OperationService.LookupOperation", name
        if row["effect_class"] in ("K", "S"):
            assert row["missing_mutation_detail"] == "Unknown", name
    profile = load("dolgorae-gul-consumer-v2.json", selected)
    assert set(profile["required_methods"]) <= set(methods)
    assert len(profile["required_methods"]) == profile["required_method_count"]
    capability = load("dolgorae-capabilities-v2.json", selected)
    assert set(capability["methods"]) == set(methods)
    assert not capability["checked_contract_implies_runtime_support"]
    assert capability["default_availability"] == "unavailable"
    pool = descriptor_pool.DescriptorPool()
    remaining = list(files.file)
    while remaining:
        loaded = []
        for file in remaining:
            try:
                pool.Add(file)
                loaded.append(file)
            except TypeError:
                pass
        assert loaded, "descriptor dependency cycle/missing input"
        remaining = [file for file in remaining if file not in loaded]
    fixtures = load("dolgorae-gul-consumer-v2.fixtures.json", selected)
    validate_normalized_bodies(fixtures, registry)
    fixture_index = {fixture["id"]: fixture for fixture in fixtures["positive"]}
    assert len(fixture_index) == len(fixtures["positive"]), "duplicate positive fixture identity"
    frozen_files = FileDescriptorSet.FromString(selected["docs/protocol/dolgorae-public-v1.descriptor.pb"])
    frozen_public = next(file for file in frozen_files.file if file.package == "dolgorae.public.v1")
    frozen_methods = {f"{service.name}.{method.name}" for service in frozen_public.service for method in service.method}
    assert set(fixtures["method_cases"]) == set(methods) - frozen_methods
    for name, case in fixtures["method_cases"].items():
        assert case["effect_class"] == policies[name]["effect_class"], name
        for side, expected in [("request", methods[name].input_type), ("response", methods[name].output_type),
                               ("error", ".dolgorae.public.v2.DolgoraeErrorDetail")]:
            fixture = fixture_index[case[side]]
            assert fixture["grpc_message"] == expected.lstrip("."), name
            assert fixture["grpc_json"], name
    for fixture in fixtures["positive"]:
        descriptor = pool.FindMessageTypeByName(fixture["grpc_message"])
        message = message_factory.GetMessageClass(descriptor)()
        json_format.ParseDict(fixture["grpc_json"], message, ignore_unknown_fields=False)
        encoded = message.SerializeToString(deterministic=True)
        decoded = type(message).FromString(encoded)
        assert decoded == message and encoded, fixture["id"]
    vectors = load("dolgorae-v014-digest-vectors-v1.json", selected)
    submit_digest = next(item["expected_digest"] for item in vectors["vectors"]
                         if item["id"] == "mutation-submit-turn")
    assert fixture_index["lookup_submit_turn_running"]["grpc_json"]["accepted"]["receipt"]["accepted_input_digest"] == submit_digest
    for fixture in fixtures["negative"]:
        descriptor = pool.FindMessageTypeByName(fixture["grpc_message"])
        message = message_factory.GetMessageClass(descriptor)()
        try:
            json_format.ParseDict(fixture["grpc_json"], message, ignore_unknown_fields=False)
        except json_format.ParseError:
            assert fixture["expected"] == "strict_decode_refusal", fixture["id"]
        else:
            assert fixture["expected"] == "decode_preserves_value_but_decision_refuses", fixture["id"]
            field = descriptor.fields_by_name["requested_mode"]
            assert message.requested_mode not in field.enum_type.values_by_number, fixture["id"]
    assert len(lock["selected_external_sources"]) == 11
    assert len(lock["native_feasibility"]["hosts"]) == 2
    assert lock["producer"] == {"epic": "EPIC-023", "task": "TASK-091", "requirements_commit": "664c6648ec5c69f78733a58d5c46e88b335459ba"}
    assert lock["ancestry"] == {"original_h1_commit": "55ba623fce231192226d29ba718153e18b6644a3", "original_h1_lock_sha256": "4af0ff5ee310d25564541004a4a45ffd600e4a1ca1c0bacfa986ba8cac80f049", "correction_design_commit": "664c6648ec5c69f78733a58d5c46e88b335459ba"}
    h2 = lock["later_handoffs"]["H2"]
    assert h2["historical"]["status"] == "accepted_for_original_package"
    assert h2["historical"]["package_commit"] == lock["ancestry"]["original_h1_commit"]
    assert h2["corrected_delta"]["status"] == "pending"
    assert h2["corrected_delta"]["consumer"] == "TASK-092/EPIC-023"
    assert "docs/specs/dolgorae-v014-release-proof-correction.md" in selected
    assert "docs/architecture-decision-records/README.md" in selected
    print(f"H1 contract passed: {len(paths)} byte bindings, {len(methods)} methods, "
          f"{len(profile['required_methods'])} Gul methods, frozen v1 and typed wire fixtures")


if __name__ == "__main__":
    main()
