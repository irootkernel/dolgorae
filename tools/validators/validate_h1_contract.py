#!/usr/bin/env python3
"""Check H1 byte bindings, descriptor policy coverage and typed wire fixtures."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

from google.protobuf import descriptor_pool, json_format, message_factory
from google.protobuf.descriptor_pb2 import FileDescriptorSet
from jsonschema import Draft202012Validator
from referencing import Registry, Resource

ROOT = Path(__file__).resolve().parents[2]
PROTOCOL = ROOT / "docs/protocol"


def load(name):
    return json.loads((PROTOCOL / name).read_text())


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def validate_normalized_bodies(fixtures):
    registry = Registry()
    for path in sorted(PROTOCOL.glob("*.json")):
        schema = json.loads(path.read_text())
        if isinstance(schema, dict) and "$id" in schema:
            registry = registry.with_resource(schema["$id"], Resource.from_contents(schema))
    for operation, fixture in fixtures["constructed_mutation_inputs"].items():
        validator = Draft202012Validator(
            {"$ref": fixture["identity"]["request_schema"]}, registry=registry
        )
        errors = list(validator.iter_errors(fixture["normalized_body"]))
        assert not errors, f"normalized {operation}: {errors[0].json_path}: {errors[0].message}"


def main():
    lock = load("dolgorae-gul-consumer-v2.lock.json")
    for item in lock["artifacts"] + lock["frozen_v1_artifacts"]:
        path = ROOT / item["path"]
        assert path.is_file() and digest(path) == item["sha256"], f"byte binding mismatch: {item['path']}"
    paths = [item["path"] for item in lock["artifacts"]]
    assert len(paths) == len(set(paths)), "duplicate H1 artifact"
    assert "docs/protocol/dolgorae-gul-consumer-v2.lock.json" not in paths, "recursive lock identity"
    files = FileDescriptorSet.FromString((PROTOCOL / "dolgorae-public-v2.descriptor.pb").read_bytes())
    public = next(f for f in files.file if f.package == "dolgorae.public.v2")
    assert all(f.package != "dolgorae.public.v1" for f in files.file), "v2 descriptor embeds frozen v1"
    methods = {f"{s.name}.{m.name}": m for s in public.service for m in s.method}
    assert len(methods) == 64
    policies = load("dolgorae-rpc-mutation-policy-v2.json")["methods"]
    assert set(policies) == set(methods), "per-method policy coverage"
    error_policy = load("dolgorae-grpc-error-mapping-v2.json")
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
    profile = load("dolgorae-gul-consumer-v2.json")
    assert set(profile["required_methods"]) <= set(methods)
    assert len(profile["required_methods"]) == profile["required_method_count"]
    capability = load("dolgorae-capabilities-v2.json")
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
    fixtures = load("dolgorae-gul-consumer-v2.fixtures.json")
    validate_normalized_bodies(fixtures)
    fixture_index = {fixture["id"]: fixture for fixture in fixtures["positive"]}
    assert len(fixture_index) == len(fixtures["positive"]), "duplicate positive fixture identity"
    frozen_files = FileDescriptorSet.FromString((PROTOCOL / "dolgorae-public-v1.descriptor.pb").read_bytes())
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
    vectors = json.loads((PROTOCOL / "dolgorae-v014-digest-vectors-v1.json").read_text())
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
    assert lock["later_handoffs"]["H2"]["status"] == "not_started"
    print(f"H1 contract passed: {len(paths)} byte bindings, {len(methods)} methods, "
          f"{len(profile['required_methods'])} Gul methods, frozen v1 and typed wire fixtures")


if __name__ == "__main__":
    main()
