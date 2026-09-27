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
        "orchestration-state-v2.": "dolgorae-orchestration-state-v2.schema.json",
        "orchestration-state.": "dolgorae-orchestration-state-v1.schema.json",
        "orchestration-": "dolgorae-orchestration-tool-v1.schema.json",
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
    if example_name == "error-contract-v2-review-output-legacy.invalid-missing-reason.json":
        repaired["details"]["reason"] = "Reviewer output did not match the contract"
    elif example_name == "error-contract-v2-review-output-diagnostic.invalid-extra-details.json":
        del repaired["details"]["diagnostic"]["private_provider_key"]
    elif example_name == "error-contract-v2-review-task.invalid-extra-details.json":
        del repaired["details"]["profile"]
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

    if errors:
        print("Schema example validation failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print(
        "Schema example validation passed: "
        f"{len(examples)} positive and {len(negative_examples)} negative examples"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
