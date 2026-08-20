#!/usr/bin/env python3
"""Parse all JSON and meta-validate active Draft 2020-12 JSON Schemas."""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

from jsonschema import Draft202012Validator

ROOT = Path(__file__).resolve().parents[2]


def no_duplicates(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for key, value in pairs:
        if key in out:
            raise ValueError(f"duplicate key: {key}")
        out[key] = value
    return out


def main() -> int:
    errors: list[str] = []
    parsed = 0
    schemas = 0
    for path in sorted(ROOT.rglob("*.json")):
        try:
            value = json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=no_duplicates)
            parsed += 1
            if isinstance(value, dict) and value.get("$schema") == "https://json-schema.org/draft/2020-12/schema":
                Draft202012Validator.check_schema(value)
                schemas += 1
        except Exception as exc:
            errors.append(f"{path.relative_to(ROOT)}: {exc}")
    if errors:
        print("JSON/schema validation failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print(f"JSON/schema validation passed: {parsed} JSON documents, {schemas} Draft 2020-12 schemas")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
