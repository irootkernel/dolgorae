"""Shared JSON Schema helpers for Dolgorae black-box tests."""

from __future__ import annotations

import json
from pathlib import Path

from jsonschema import Draft202012Validator
from referencing import Registry, Resource


def validator(protocol_root: Path, name: str) -> Draft202012Validator:
    registry = Registry()
    for path in sorted(protocol_root.glob("*.json")):
        document = json.loads(path.read_text(encoding="utf-8"))
        if isinstance(document, dict) and "$id" in document:
            registry = registry.with_resource(
                document["$id"], Resource.from_contents(document)
            )
    schema = json.loads((protocol_root / name).read_text(encoding="utf-8"))
    return Draft202012Validator(schema, registry=registry)


def assert_valid(instance: object, checked: Draft202012Validator, label: str) -> None:
    errors = sorted(checked.iter_errors(instance), key=lambda error: list(error.path))
    if errors:
        locations = "; ".join(
            f"{list(error.absolute_path)}: {error.message}" for error in errors
        )
        raise AssertionError(f"invalid {label}: {locations}")
