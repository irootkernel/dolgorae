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
    prefixes = {
        "agent-configuration.": "dolgorae-agent-configuration-v1.schema.json",
        "collaboration-": "dolgorae-collaboration-tool-v1.schema.json",
        "controller-credential.": "dolgorae-controller-credential-v1.schema.json",
        "external-engagement-": "dolgorae-external-specialist-facade-v1.schema.json",
        "idempotency-intent.": "dolgorae-idempotency-intent-v1.schema.json",
        "ledger-state.": "dolgorae-ledger-state-v1.schema.json",
        "orchestration-state.": "dolgorae-orchestration-state-v1.schema.json",
        "orchestration-": "dolgorae-orchestration-tool-v1.schema.json",
        "specialist-policy.": "dolgorae-specialist-policy-v1.schema.json",
        "specialist-review-mcp-meta.": "dolgorae-specialist-review-mcp-meta-v1.schema.json",
        "specialist-review-request.": "dolgorae-specialist-review-tool-v1.schema.json",
        "specialist-review-result.": "dolgorae-specialist-review-tool-v1.schema.json",
        "specialist-review-idempotency-conflict.": "dolgorae-specialist-review-tool-v1.schema.json",
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

    if errors:
        print("Schema example validation failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print(f"Schema example validation passed: {len(examples)} positive examples")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
