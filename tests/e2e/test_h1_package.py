#!/usr/bin/env python3
"""Check committed H1 selection, dirty refusal and portable schema qualification."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
LOCK = "docs/protocol/dolgorae-gul-consumer-v2.lock.json"
SCRIPT = "tools/validators/package_h1.py"
ARTIFACT = "artifacts/contract.txt"


class H1PackageTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="h1-package-", dir=os.environ.get("TMPDIR"))
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        home = self.root / "home"
        home.mkdir()
        self.environment = os.environ.copy()
        for name in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_AUTHOR_NAME", "GIT_AUTHOR_EMAIL", "GIT_COMMITTER_NAME", "GIT_COMMITTER_EMAIL"):
            self.environment.pop(name, None)
        self.environment.update(HOME=str(home), PYTHONDONTWRITEBYTECODE="1")
        self.initialize(self.repo)
        self.write(SCRIPT, (ROOT / SCRIPT).read_bytes())
        self.write(ARTIFACT, b"original contract\n")
        source = json.loads((ROOT / LOCK).read_bytes())
        self.lock = {key: source[key] for key in ("producer", "ancestry", "later_handoffs")}
        self.lock.update(artifacts=[{"path": ARTIFACT, "sha256": hashlib.sha256((self.repo / ARTIFACT).read_bytes()).hexdigest()}], frozen_v1_artifacts=[])
        self.write_lock()
        self.initial = self.commit()

    def git(self, *arguments, repo=None):
        return subprocess.check_output(["git", *arguments], cwd=repo or self.repo, env=self.environment, stderr=subprocess.PIPE)

    def initialize(self, repo):
        self.git("init", "-q", "--initial-branch=main", repo=repo)
        self.git("config", "user.name", "Dolgorae H1 fixture", repo=repo)
        self.git("config", "user.email", "h1-fixture@example.invalid", repo=repo)

    def commit(self, repo=None):
        self.git("add", "--all", repo=repo)
        self.git("commit", "-qm", "H1 isolated fixture", repo=repo)
        return self.git("rev-parse", "HEAD", repo=repo).decode().strip()

    def write(self, name, content):
        path = self.repo / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)

    def write_lock(self):
        self.write(LOCK, (json.dumps(self.lock, indent=2) + "\n").encode())

    def package(self, name="archive", *, revision=None, repo=None, succeeds=True):
        repo = repo or self.repo
        output = self.root / (name + ".tgz")
        command = [sys.executable, "-B", str(repo / SCRIPT), "--output", str(output)]
        if revision is not None:
            command += ["--revision", revision]
        result = subprocess.run(command, cwd=repo, env=self.environment, capture_output=True, text=True)
        self.assertEqual(result.returncode == 0, succeeds, result.stdout + result.stderr)
        self.assertEqual(output.exists(), succeeds, "refusal must not create output")
        return output

    def test_deterministic_archives_and_derived_delivery(self):
        first = self.package("first")
        second = self.package("second")
        self.assertEqual(first.read_bytes(), second.read_bytes())
        with tarfile.open(first) as archive:
            value = json.load(archive.extractfile("dolgorae-h1/delivery.json"))
            self.assertEqual(value["source_commit"], self.initial)
            self.assertEqual(value["producer"], self.lock["producer"])
            self.assertEqual(value["consumer"], self.lock["later_handoffs"]["H2"]["corrected_delta"]["producer"])
            for member in archive:
                self.assertTrue(member.isfile())
                self.assertEqual((member.mode, member.uid, member.gid, member.mtime), (0o644, 0, 0, 0))

    def test_unrelated_working_changes_are_allowed(self):
        self.write("src/delegation_identity.rs", b"unrelated unfinished work\n")
        self.package()

    def test_dirty_selected_artifact_refuses_without_output(self):
        self.write(ARTIFACT, b"dirty\n")
        self.package(succeeds=False)

    def test_dirty_lock_refuses_without_output(self):
        self.write(LOCK, (self.repo / LOCK).read_bytes() + b"\n")
        self.package(succeeds=False)

    def test_missing_working_artifact_refuses_without_output(self):
        (self.repo / ARTIFACT).unlink()
        self.package(succeeds=False)

    def test_missing_working_lock_refuses_without_output(self):
        (self.repo / LOCK).unlink()
        self.package(succeeds=False)

    def test_working_symlink_and_directory_refuse_even_with_matching_bytes(self):
        path = self.repo / ARTIFACT
        target = self.root / "same-bytes"
        target.write_bytes(path.read_bytes())
        path.unlink()
        path.symlink_to(target)
        self.package("symlink", succeeds=False)
        path.unlink()
        path.mkdir()
        self.package("directory", succeeds=False)

    def test_working_lock_symlink_refuses(self):
        path = self.repo / LOCK
        target = self.root / "same-lock"
        target.write_bytes(path.read_bytes())
        path.unlink()
        path.symlink_to(target)
        self.package(succeeds=False)

    def test_working_symlink_ancestor_refuses(self):
        directory = self.repo / "artifacts"
        target = self.root / "same-artifacts"
        directory.rename(target)
        directory.symlink_to(target, target_is_directory=True)
        self.package(succeeds=False)

    def test_explicit_head_uses_committed_bytes_despite_dirty_lock_and_artifact(self):
        self.write(LOCK, b"malformed working lock")
        self.write(ARTIFACT, b"dirty")
        output = self.package(revision="HEAD")
        with tarfile.open(output) as archive:
            self.assertEqual(archive.extractfile("dolgorae-h1/" + ARTIFACT).read(), b"original contract\n")

    def test_explicit_empty_revision_refuses_without_output_with_clean_or_dirty_selection(self):
        for dirty in (False, True):
            with self.subTest(dirty=dirty):
                if dirty:
                    self.write(LOCK, b"malformed working lock")
                    self.write(ARTIFACT, b"dirty selected artifact")
                self.package(f"empty-{dirty}", revision="", succeeds=False)

    def test_explicit_old_revision_is_independent_of_newer_dirty_checkout(self):
        self.write(ARTIFACT, b"newer committed contract\n")
        self.lock["artifacts"][0]["sha256"] = hashlib.sha256((self.repo / ARTIFACT).read_bytes()).hexdigest()
        self.write_lock()
        self.commit()
        self.write(LOCK, b"malformed working lock")
        (self.repo / ARTIFACT).unlink()
        output = self.package(revision=self.initial)
        with tarfile.open(output) as archive:
            self.assertEqual(archive.extractfile("dolgorae-h1/" + ARTIFACT).read(), b"original contract\n")

    def test_missing_committed_artifact_refuses(self):
        (self.repo / ARTIFACT).unlink()
        self.commit()
        self.package(revision="HEAD", succeeds=False)

    def test_missing_committed_lock_refuses(self):
        (self.repo / LOCK).unlink()
        self.commit()
        self.package(revision="HEAD", succeeds=False)

    def test_stale_committed_binding_refuses(self):
        self.write(ARTIFACT, b"changed without rebinding\n")
        self.commit()
        self.package(revision="HEAD", succeeds=False)

    def test_unsafe_and_unnormalized_locked_paths_refuse(self):
        for index, name in enumerate(("../escape", "/absolute", "./artifacts/contract.txt", "artifacts//contract.txt", "artifacts\\contract.txt")):
            with self.subTest(name=name):
                self.lock["artifacts"][0]["path"] = name
                self.write_lock()
                self.commit()
                self.package(str(index), revision="HEAD", succeeds=False)

    def test_committed_symlink_refuses_even_with_self_consistent_hash(self):
        path = self.repo / ARTIFACT
        path.unlink()
        path.symlink_to("target")
        self.lock["artifacts"][0]["sha256"] = hashlib.sha256(b"target").hexdigest()
        self.write_lock()
        self.commit()
        self.package(revision="HEAD", succeeds=False)

    def test_committed_lock_symlink_refuses(self):
        path = self.repo / LOCK
        path.unlink()
        path.symlink_to("target")
        self.commit()
        self.package(revision="HEAD", succeeds=False)

    def test_nonregular_git_mode_refuses(self):
        self.git("update-index", "--cacheinfo", f"160000,{self.initial},{ARTIFACT}")
        self.git("commit", "-qm", "H1 gitlink fixture")
        self.package(revision="HEAD", succeeds=False)

    def test_executable_regular_git_blob_is_supported(self):
        (self.repo / ARTIFACT).chmod(0o755)
        self.commit()
        self.package(revision="HEAD")

    def test_extracted_corrected_package_is_portable_and_rejects_coverage_mutations(self):
        full = self.root / "full-selection"
        full.mkdir()
        self.initialize(full)
        lock = json.loads((ROOT / LOCK).read_bytes())
        names = {row["path"] for row in lock["artifacts"] + lock["frozen_v1_artifacts"]} | {LOCK}
        for name in names:
            target = full / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / name, target)
        self.commit(full)
        archive_path = self.package("full", revision="HEAD", repo=full)
        extracted = self.root / "extracted"
        with tarfile.open(archive_path) as archive:
            archive.extractall(extracted, filter="data")
        package = extracted / "dolgorae-h1"
        self.assertFalse((package / ".git").exists())
        environment = self.environment | {"PATH": str(self.root / "no-executables")}

        def validate(expected=None):
            result = subprocess.run([sys.executable, "-B", "tools/validators/validate_h1_contract.py"], cwd=package, env=environment, capture_output=True, text=True)
            if expected is None:
                self.assertEqual(result.returncode, 0, result.stderr)
            else:
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(expected, result.stderr)

        validate()
        inventory = "docs/protocol/dolgorae-v014-release-proof-fixtures-v1.json"
        released = "docs/protocol/examples/v014-identity-release-external-released.valid.json"
        original_lock = (package / LOCK).read_bytes()
        for mutation in ("delete-case", "flip-outcome", "keyed-substitution"):
            name = released if mutation == "keyed-substitution" else inventory
            original = (package / name).read_bytes()
            value = json.loads(original)
            if mutation == "delete-case":
                value["cases"].pop()
            elif mutation == "flip-outcome":
                value["cases"][0]["valid"] = not value["cases"][0]["valid"]
            else:
                suite = json.loads((package / "docs/protocol/examples/successor-facade-v5-methods.valid.json").read_bytes())
                value["disposition_proof"]["release_receipt"] = next(case for case in suite["cases"] if case["operation"] == "hire_external_specialist")["response"]["receipt"]
            content = (json.dumps(value) + "\n").encode()
            (package / name).write_bytes(content)
            rebound = json.loads(original_lock)
            for row in rebound["artifacts"]:
                if row["path"] == name:
                    row["sha256"] = hashlib.sha256(content).hexdigest()
            (package / LOCK).write_text(json.dumps(rebound) + "\n")
            validate("unexpected schema outcome" if mutation == "keyed-substitution" else "release fixture coverage/outcome drift")
            (package / name).write_bytes(original)
            (package / LOCK).write_bytes(original_lock)


if __name__ == "__main__":
    unittest.main()
