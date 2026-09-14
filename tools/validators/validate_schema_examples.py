#!/usr/bin/env python3
"""Validate checked positive protocol examples against their JSON Schemas."""

from __future__ import annotations

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


def main() -> int:
    registry = Registry()
    for path in sorted(PROTOCOL.glob("*.schema.json")):
        schema = json.loads(path.read_text(encoding="utf-8"))
        registry = registry.with_resource(schema["$id"], Resource.from_contents(schema))

    errors: list[str] = []
    examples = sorted(EXAMPLES.glob("*.valid.json"))
    for path in examples:
        try:
            schema_path = PROTOCOL / schema_name(path.name)
            schema = json.loads(schema_path.read_text(encoding="utf-8"))
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
            *EXAMPLES.glob("external-engagement-v3-*.invalid-*.json"),
            *EXAMPLES.glob("specialist-review-v3-*.invalid-*.json"),
        ]
    )
    for path in negative_examples:
        try:
            schema_path = PROTOCOL / schema_name(path.name)
            schema = json.loads(schema_path.read_text(encoding="utf-8"))
            instance = json.loads(path.read_text(encoding="utf-8"))
            validator = Draft202012Validator(
                schema, registry=registry, format_checker=FormatChecker()
            )
            if not list(validator.iter_errors(instance)):
                errors.append(f"{path.name}: invalid example was accepted")
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
