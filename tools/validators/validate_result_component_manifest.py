#!/usr/bin/env python3
"""Validate result discovery examples without changing the frozen H1 validator."""

from __future__ import annotations

import copy
import json
from pathlib import Path

from jsonschema import Draft202012Validator

PROTOCOL = Path(__file__).resolve().parents[2] / "docs" / "protocol"


def main() -> int:
    schema = json.loads((PROTOCOL / "dolgorae-result-component-manifest-v1.schema.json").read_text())
    Draft202012Validator.check_schema(schema)
    validator = Draft202012Validator(schema)
    examples = {
        name: json.loads((PROTOCOL / "examples" / "result-components" / f"{name}.valid.json").read_text())
        for name in ("root", "page")
    }
    for value in examples.values():
        validator.validate(value)

    negatives = []
    extra = copy.deepcopy(examples["root"])
    extra["unrecognized"] = True
    negatives.append(extra)
    old_identity = copy.deepcopy(examples["root"])
    old_identity["result_id"] = "019a0000-0000-4000-8000-000000000002"
    negatives.append(old_identity)
    oversized = copy.deepcopy(examples["root"])
    oversized["identity"]["chunks"][0]["content"]["byte_length"] = 33554433
    negatives.append(oversized)
    missing_owner = copy.deepcopy(examples["page"])
    del missing_owner["components"][0]["content_ref"]["artifact_owner"]
    negatives.append(missing_owner)
    invalid_selector = copy.deepcopy(examples["page"])
    invalid_selector["components"][0]["selector"] = "change:0:unrecognized"
    negatives.append(invalid_selector)
    unbounded_page = copy.deepcopy(examples["page"])
    unbounded_page["components"] *= 257
    negatives.append(unbounded_page)
    for index, value in enumerate(negatives):
        if validator.is_valid(value):
            raise ValueError(f"result component manifest accepted negative case {index}")
    print(f"Result component manifest passed: {len(examples)} examples, {len(negatives)} refusals")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
