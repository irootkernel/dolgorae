#!/usr/bin/env python3
"""Check isolated installation and fail-closed package dependency validation."""

from __future__ import annotations

import copy
import hashlib
import json
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "validators"))

import package_agent_skill as package
from referencing.exceptions import Unresolvable
from validate_agent_skills import SkillValidationError, validate_installed_resources


class InstalledSkillTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="dolgorae-skill-package-")
        self.addCleanup(self.temporary.cleanup)
        self.root = pathlib.Path(self.temporary.name)
        self.installed = self.root / "use-dolgorae"
        self.install(self.installed, succeeds=True)

    def install(self, destination: pathlib.Path, *, succeeds: bool):
        result = subprocess.run(
            [sys.executable, str(ROOT / "tools/validators/package_agent_skill.py"), "install", "--destination", str(destination)],
            cwd=self.root, capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(result.returncode == 0, succeeds, result.stderr)

    def test_installed_layout_resolves_without_checkout_or_network(self):
        # Resource validation receives only the installed root; even the source
        # helper's canonical root is unavailable during this check.
        with patch.object(package, "ROOT", self.root / "absent-checkout"):
            validate_installed_resources(self.installed)

    def test_successor_interaction_contracts_resolve_from_installed_resources(self):
        from schema_support import assert_valid, validator
        resources = self.installed / "resources/protocol"
        for path in sorted((ROOT / "docs/protocol/examples/interaction-v2").glob("*.valid.json")):
            schema = ("dolgorae-public-resolution-v2.schema.json" if path.name.startswith("public-resolution") else "dolgorae-interaction-decision-v2.schema.json" if path.name.startswith("interaction-decision") else "dolgorae-interaction-v2.schema.json")
            assert_valid(json.loads(path.read_bytes()), validator(resources, schema), path.name)
        checked = validator(resources, "dolgorae-interaction-v2.schema.json")
        examples = ROOT / "docs/protocol/examples/interaction-v2"
        marked = json.loads((examples / "interaction-v2-mcp-policy-only.valid.json").read_bytes())
        for field, value in (("policy_only", None), ("policy_only", "UNKNOWN"),
                             ("policy_only", {"RUNTIME_VARIANT_UNSUPPORTED": None}),
                             ("title", "private-canary"), ("server_name", "private-canary"),
                             ("questions", [{}])):
            changed = copy.deepcopy(marked)
            changed["payload"][field] = value
            self.assertFalse(checked.is_valid(changed), field)
        marked["available_decisions"].insert(0, "accept_once")
        self.assertFalse(checked.is_valid(marked))
        ordinary = json.loads((examples / "interaction-v2-mcp.valid.json").read_bytes())
        ordinary["payload"]["policy_only"] = None
        self.assertFalse(checked.is_valid(ordinary))
        del ordinary["payload"]["policy_only"]
        ordinary["payload"]["questions"] = []
        self.assertFalse(checked.is_valid(ordinary))

    def test_successor_event_deliveries_preserve_projection_constraints(self):
        from schema_support import assert_valid, validator
        resources = self.installed / "resources/protocol"
        delivery_schema = validator(resources, "dolgorae-event-delivery-v2.schema.json")
        machine_schema = validator(resources, "dolgorae-machine-v6.schema.json")
        record = {
            "schema_version": 2, "event_schema_version": 2, "cursor": "1",
            "event_id": "018f0000-0000-7000-8000-000000000001",
            "timestamp": "2026-10-01T00:00:00.000000Z", "workspace_id": "a" * 64,
            "run_id": "018f0000-0000-7000-8000-000000000002",
            "thread_id": "thread-1", "turn_id": "turn-1", "server_key": "b" * 64,
            "server_epoch": 1, "type": "interaction.opened",
            "payload": {"request_id": "018f0000-0000-7000-8000-000000000003"},
        }
        for version, kind in ((2, "permission_request"), (2, "mcp_elicitation"), (1, "user_input")):
            record["schema_version"] = record["event_schema_version"] = version
            record["payload"]["interaction_kind"] = kind
            for projection in ("minimal", "operational"):
                delivery = {"schema_version": 2, "kind": "event", "projection": projection,
                            "replay": True, "record": record}
                assert_valid(delivery, delivery_schema, f"{version}/{kind}/{projection}")
                machine = {"schema_version": 6, "ok": True, "command": "run.events",
                           "invocation_id": "018f0000-0000-7000-8000-000000000004", "data": delivery}
                assert_valid(machine, machine_schema, f"machine/{version}/{kind}/{projection}")
        record["type"], record["payload"] = "command.started", {"command": ["Command started"]}
        delivery["projection"] = "operational"
        assert_valid(delivery, delivery_schema, "operational command")
        delivery["projection"] = "minimal"
        self.assertFalse(delivery_schema.is_valid(delivery))
        self.assertFalse(machine_schema.is_valid(machine))

    def test_missing_transitive_dependency_cannot_fall_back_to_checkout(self):
        resources = self.installed / "resources"
        name = "protocol/dolgorae-specialist-review-tool-v1.schema.json"
        (resources / name).unlink()
        with self.assertRaisesRegex(SkillValidationError, "inventory"):
            validate_installed_resources(self.installed)
        # Removing the inventory entry must still fail actual $ref resolution.
        manifest_path = resources / "manifest.json"
        manifest = json.loads(manifest_path.read_bytes())
        del manifest["files"][name]
        manifest_path.write_text(json.dumps(manifest))
        with self.assertRaises(Unresolvable):
            validate_installed_resources(self.installed)

    def test_stale_schema_fails_digest_and_canonical_byte_equality(self):
        resources = self.installed / "resources"
        name = "protocol/dolgorae-specialist-review-tool-v3.schema.json"
        path = resources / name
        path.write_bytes(path.read_bytes() + b"\n")
        with self.assertRaisesRegex(SkillValidationError, "digest differs"):
            validate_installed_resources(self.installed)
        # A self-consistent copied inventory cannot become a competing owner.
        manifest_path = resources / "manifest.json"
        manifest = json.loads(manifest_path.read_bytes())
        manifest["files"][name] = hashlib.sha256(path.read_bytes()).hexdigest()
        manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
        with patch.object(package, "SKILL_ROOT", self.installed):
            with self.assertRaisesRegex(package.PackageError, "canonical source"):
                package.checked_package()

    def test_install_preserves_existing_directory_and_symlink(self):
        before = (self.installed / "SKILL.md").read_bytes()
        self.install(self.installed, succeeds=False)
        self.assertEqual((self.installed / "SKILL.md").read_bytes(), before)
        empty = self.root / "empty"
        empty.mkdir()
        self.install(empty, succeeds=False)
        self.assertEqual(list(empty.iterdir()), [])
        link = self.root / "link"
        link.symlink_to(self.installed, target_is_directory=True)
        self.install(link, succeeds=False)
        self.assertTrue(link.is_symlink())
        self.assertEqual((self.installed / "SKILL.md").read_bytes(), before)

    def test_install_rejects_bad_resources_before_creating_destination(self):
        for fault in ("missing", "stale", "extra"):
            with self.subTest(fault=fault):
                source = self.root / f"source-{fault}"
                shutil.copytree(self.installed, source)
                resource = source / "resources/protocol/dolgorae-specialist-review-tool-v1.schema.json"
                if fault == "missing":
                    resource.unlink()
                elif fault == "stale":
                    resource.write_bytes(resource.read_bytes() + b"\n")
                else:
                    (source / "resources/unexpected.json").write_text("{}")
                destination = self.root / f"destination-{fault}"
                with patch.object(package, "SKILL_ROOT", source):
                    with self.assertRaises(package.PackageError):
                        package.install(destination)
                self.assertFalse(destination.exists())


if __name__ == "__main__":
    unittest.main()
