#!/usr/bin/env python3
"""Validate live working-tree consistency of the 65-method current-v2 contract."""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

from google.protobuf.descriptor_pb2 import FileDescriptorSet

ROOT = Path(__file__).resolve().parents[2]
PROTOCOL = ROOT / "docs/protocol"
EXPECTED_METHOD_COUNT = 65


def load_json(name: str) -> dict:
    return json.loads((PROTOCOL / name).read_text(encoding="utf-8"))


def parse_go_known_error_codes(path: Path) -> list[str]:
    text = path.read_text(encoding="utf-8")
    return re.findall(r'case "([A-Z0-9_]+)":', text)


def main() -> int:
    errors: list[str] = []

    descriptor_path = PROTOCOL / "dolgorae-public-v2.descriptor.pb"
    files = FileDescriptorSet.FromString(descriptor_path.read_bytes())
    public = next((item for item in files.file if item.package == "dolgorae.public.v2"), None)
    if public is None:
        errors.append("dolgorae.public.v2 package is absent from dolgorae-public-v2.descriptor.pb")
        return report(errors, 0, 0)
    if any(item.package == "dolgorae.public.v1" for item in files.file):
        errors.append("dolgorae-public-v2.descriptor.pb must not embed frozen dolgorae.public.v1")

    methods = {
        f"{service.name}.{method.name}": method
        for service in public.service
        for method in service.method
    }
    if len(methods) != EXPECTED_METHOD_COUNT:
        errors.append(
            f"current-v2 descriptor must declare {EXPECTED_METHOD_COUNT} methods, found {len(methods)}"
        )

    error_mapping = load_json("dolgorae-grpc-error-mapping-v2.json")
    mutation_policy = load_json("dolgorae-rpc-mutation-policy-v2.json")
    capabilities = load_json("dolgorae-capabilities-v2.json")
    consumer_profile = load_json("dolgorae-gul-consumer-v2.json")

    error_methods = error_mapping["methods"]
    policy_methods = mutation_policy["methods"]

    if set(error_methods) != set(methods):
        missing = sorted(set(methods) - set(error_methods))
        extra = sorted(set(error_methods) - set(methods))
        errors.append(
            f"dolgorae-grpc-error-mapping-v2.json method keys drift: missing={missing} extra={extra}"
        )
    if set(policy_methods) != set(methods):
        missing = sorted(set(methods) - set(policy_methods))
        extra = sorted(set(policy_methods) - set(methods))
        errors.append(
            f"dolgorae-rpc-mutation-policy-v2.json method keys drift: missing={missing} extra={extra}"
        )
    if set(capabilities["methods"]) != set(methods):
        missing = sorted(set(methods) - set(capabilities["methods"]))
        extra = sorted(set(capabilities["methods"]) - set(methods))
        errors.append(
            f"dolgorae-capabilities-v2.json method list drift: missing={missing} extra={extra}"
        )
    if consumer_profile["descriptor_method_count"] != len(methods):
        errors.append(
            "dolgorae-gul-consumer-v2.json descriptor_method_count does not match descriptor"
        )
    advertised_profile_methods = set(consumer_profile["required_methods"]) | set(
        consumer_profile.get("optional_methods", [])
    )
    if not advertised_profile_methods <= set(methods):
        unknown = sorted(advertised_profile_methods - set(methods))
        errors.append(
            f"dolgorae-gul-consumer-v2.json references methods absent from descriptor: {unknown}"
        )

    code_status = set(error_mapping["code_status"])
    common_errors = set(error_mapping["common_errors"])
    if not common_errors <= code_status:
        errors.append(
            f"common_errors missing from code_status: {sorted(common_errors - code_status)}"
        )

    go_codes_path = PROTOCOL / "generated/gul-consumer-v2/go/client/known_error_codes.go"
    go_code_list = parse_go_known_error_codes(go_codes_path)
    if len(go_code_list) != len(set(go_code_list)):
        errors.append("known_error_codes.go contains duplicate error code cases")
    go_codes = set(go_code_list)

    if code_status != go_codes:
        missing_in_go = sorted(code_status - go_codes)
        extra_in_go = sorted(go_codes - code_status)
        errors.append(
            f"code_status vs known_error_codes.go drift: missing_in_go={missing_in_go} extra_in_go={extra_in_go}"
        )

    for name, method in sorted(methods.items()):
        error_codes = error_methods.get(name)
        policy_row = policy_methods.get(name)
        if error_codes is None or policy_row is None:
            continue
        error_set = set(error_codes)
        policy_set = set(policy_row["material_errors"])
        if len(error_codes) != len(error_set):
            errors.append(f"{name}: duplicate error codes in dolgorae-grpc-error-mapping-v2.json")
        if len(policy_row["material_errors"]) != len(policy_set):
            errors.append(f"{name}: duplicate material_errors in dolgorae-rpc-mutation-policy-v2.json")
        missing_status = sorted(error_set - code_status)
        if missing_status:
            errors.append(f"{name}: error codes missing from code_status: {missing_status}")
        missing_go = sorted(error_set - go_codes)
        if missing_go:
            errors.append(f"{name}: error codes missing from known_error_codes.go: {missing_go}")
        if error_set != policy_set:
            errors.append(
                f"{name}: error-mapping vs mutation-policy mismatch: "
                f"mapping_only={sorted(error_set - policy_set)} "
                f"policy_only={sorted(policy_set - error_set)}"
            )
        if method.input_type.rsplit(".", 1)[-1] != policy_row["request"]:
            errors.append(f"{name}: request type mismatch in mutation policy")
        if method.output_type.rsplit(".", 1)[-1] != policy_row["response"]:
            errors.append(f"{name}: response type mismatch in mutation policy")
        if method.server_streaming != policy_row["stream"]:
            errors.append(f"{name}: streaming flag mismatch in mutation policy")
        if not policy_row["material_errors"] or not policy_row["recovery"] or policy_row["transparent_retry"]:
            errors.append(f"{name}: invalid mutation policy invariants")
        if policy_row["effect_class"] == "K" and policy_row["keyed_lookup"] != "OperationService.LookupOperation":
            errors.append(f"{name}: K-method must use OperationService.LookupOperation")
        if policy_row["effect_class"] in ("K", "S") and policy_row["missing_mutation_detail"] != "Unknown":
            errors.append(f"{name}: mutating method must classify missing detail as Unknown")

    return report(errors, len(methods), len(code_status))


def report(errors: list[str], method_count: int, code_count: int) -> int:
    if errors:
        print("Current-v2 contract validation failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print(
        f"Current-v2 contract validation passed: {method_count} methods, "
        f"{code_count} error codes aligned across descriptor, mappings, policy, and Go client"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
