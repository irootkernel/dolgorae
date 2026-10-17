#!/usr/bin/env python3
"""Exercise verified H1 schema selection without Git, hosts or network access."""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
LOCK = "docs/protocol/dolgorae-gul-consumer-v2.lock.json"
SCHEMA = "docs/protocol/dolgorae-v014-identities-v1.schema.json"


class SelectedRegistryTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(
            prefix="h1-selected-", dir=os.environ.get("TMPDIR")
        )
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.lock = json.loads((ROOT / LOCK).read_bytes())
        paths = {item["path"] for item in self.lock["artifacts"] + self.lock["frozen_v1_artifacts"]}
        for name in paths | {LOCK}:
            target = self.root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / name, target)
        spec = importlib.util.spec_from_file_location(
            "selected_h1_validator", ROOT / "tools/validators/validate_h1_contract.py"
        )
        self.validator = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.validator)
        self.validator.ROOT = self.root
        self.validator.PROTOCOL = self.root / "docs/protocol"

    def write_lock(self):
        (self.root / LOCK).write_text(json.dumps(self.lock) + "\n")

    def test_selected_inputs_validate_without_repository_or_unselected_files(self):
        self.assertFalse((self.root / ".git").exists())
        self.validator.main()

    def test_unselected_malformed_json_does_not_influence_validation(self):
        (self.root / "docs/protocol/unselected.schema.json").write_text("{invalid")
        self.validator.main()

    def test_unselected_schema_id_override_does_not_influence_validation(self):
        schema = json.loads((self.root / SCHEMA).read_bytes())
        override = {"$schema": schema["$schema"], "$id": schema["$id"], "not": {}}
        (self.root / "docs/protocol/unselected-override.schema.json").write_text(json.dumps(override))
        self.validator.main()

    def test_selected_checksum_is_checked_before_schema_parsing(self):
        (self.root / SCHEMA).write_text("{invalid selected JSON")
        with self.assertRaisesRegex(AssertionError, "byte binding mismatch"):
            self.validator.main()

    def test_duplicate_selected_schema_id_is_refused(self):
        name = "docs/protocol/selected-duplicate.schema.json"
        content = (self.root / SCHEMA).read_bytes()
        (self.root / name).write_bytes(content)
        self.lock["artifacts"].append({"path": name, "sha256": hashlib.sha256(content).hexdigest()})
        self.write_lock()
        with self.assertRaisesRegex(AssertionError, "duplicate selected schema ID"):
            self.validator.main()

    def check_inventory_coverage_change(self, change):
        name = "docs/protocol/dolgorae-v014-release-proof-fixtures-v1.json"
        inventory = json.loads((self.root / name).read_bytes())
        change(inventory["cases"])
        content = (json.dumps(inventory) + "\n").encode()
        (self.root / name).write_bytes(content)
        for item in self.lock["artifacts"]:
            if item["path"] == name:
                item["sha256"] = hashlib.sha256(content).hexdigest()
        self.write_lock()
        with self.assertRaisesRegex(AssertionError, "release fixture coverage/outcome drift"):
            self.validator.main()

    def test_required_fixture_deletion_is_refused_after_hash_verification(self):
        self.check_inventory_coverage_change(lambda cases: cases.pop())

    def test_expected_outcome_flip_is_refused_after_hash_verification(self):
        self.check_inventory_coverage_change(
            lambda cases: cases[0].update(valid=not cases[0]["valid"])
        )

    def test_dependency_outside_selection_is_refused_even_when_file_exists(self):
        self.lock["artifacts"] = [item for item in self.lock["artifacts"] if item["path"] != SCHEMA]
        self.assertTrue((self.root / SCHEMA).is_file())
        self.write_lock()
        with self.assertRaisesRegex(AssertionError, "unselected schema dependency"):
            self.validator.main()


if __name__ == "__main__":
    unittest.main()
