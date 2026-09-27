#!/usr/bin/env python3
"""Check isolated installation and fail-closed package dependency validation."""

from __future__ import annotations

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
