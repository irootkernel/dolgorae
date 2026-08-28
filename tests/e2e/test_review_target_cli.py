#!/usr/bin/env python3
"""Black-box acceptance for immutable review-target capture and settlement."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import stat
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor

from schema_support import assert_valid, validator


def git(repository: pathlib.Path, *arguments: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", "-C", str(repository), *arguments],
        check=check,
        capture_output=True,
        text=True,
    )


def invoke(
    binary: pathlib.Path,
    home: pathlib.Path,
    arguments: list[str],
) -> tuple[subprocess.CompletedProcess[str], dict[str, object]]:
    environment = os.environ.copy()
    environment["HOME"] = str(home)
    completed = subprocess.run(
        [str(binary), *arguments],
        check=False,
        capture_output=True,
        text=True,
        env=environment,
    )
    if completed.stderr:
        raise AssertionError(f"unexpected stderr for {arguments}: {completed.stderr!r}")
    return completed, json.loads(completed.stdout)


def capture(
    binary: pathlib.Path,
    home: pathlib.Path,
    repository: pathlib.Path,
    kind: str,
    owner: pathlib.Path,
    revision: str | None = None,
) -> tuple[subprocess.CompletedProcess[str], dict[str, object]]:
    arguments = [
        "review-target", "capture", "--workspace", str(repository),
        "--kind", kind, "--backend-kind", "e2e-review",
        "--backend-lifecycle-id", f"life-{kind}-{owner.stem}",
        "--settlement-owner-file", str(owner),
    ]
    if revision is not None:
        arguments.extend(["--revision", revision])
    return invoke(binary, home, arguments)


def receipt(path: pathlib.Path, lifecycle: str, state: str = "completed") -> None:
    path.write_text(
        json.dumps(
            {
                "schema": "dolgorae-review-target-terminal-receipt/v1",
                "backend_kind": "e2e-review",
                "backend_lifecycle_id": lifecycle,
                "terminal_state": state,
                "state_revision": 4,
                "evidence_digest": hashlib.sha256(b"stable terminal evidence").hexdigest(),
            },
            separators=(",", ":"),
        ),
        encoding="utf-8",
    )
    path.chmod(0o600)


def assert_read_only_tree(root: pathlib.Path) -> None:
    for path in [root, *root.rglob("*")]:
        mode = stat.S_IMODE(path.stat().st_mode)
        if path.is_dir() and mode != 0o555:
            raise AssertionError(f"capture directory is not 0555: {path}: {oct(mode)}")
        if path.is_file() and mode not in {0o444, 0o555}:
            raise AssertionError(f"capture file is not read-only: {path}: {oct(mode)}")


def validate(binary: pathlib.Path, protocol_root: pathlib.Path) -> None:
    machine = validator(protocol_root, "dolgorae-machine-v1.schema.json")
    contract = validator(protocol_root, "dolgorae-review-target-v1.schema.json")
    with tempfile.TemporaryDirectory(prefix="dolgorae-task014-") as temporary:
        root = pathlib.Path(temporary)
        home = root / "home"
        home.mkdir(mode=0o700)
        (home / "Library" / "Application Support").mkdir(parents=True, mode=0o700)
        repository = root / "repository"
        repository.mkdir(mode=0o700)
        git(repository, "init", "-b", "main")
        git(repository, "config", "user.name", "Dolgorae E2E")
        git(repository, "config", "user.email", "dolgorae@example.invalid")

        (repository / "root.txt").write_text("root\n", encoding="utf-8")
        git(repository, "add", "root.txt")
        git(repository, "commit", "-m", "root")
        root_commit = git(repository, "rev-parse", "HEAD").stdout.strip()
        git(repository, "mv", "root.txt", "renamed.txt")
        (repository / "second.txt").write_text("second\n", encoding="utf-8")
        git(repository, "add", "second.txt")
        git(repository, "commit", "-m", "second")
        head_commit = git(repository, "rev-parse", "HEAD").stdout.strip()
        initialized, initialized_envelope = invoke(binary, home, ["init", str(repository)])
        if initialized.returncode != 0:
            raise AssertionError(f"workspace initialization failed: {initialized_envelope}")

        (repository / "renamed.txt").write_text("staged\n", encoding="utf-8")
        git(repository, "add", "renamed.txt")
        (repository / "renamed.txt").write_text("worktree\n", encoding="utf-8")
        (repository / "untracked.txt").write_text("untracked\n", encoding="utf-8")
        nested_private = repository / "nested" / ".dolgorae"
        nested_private.mkdir(parents=True)
        (nested_private / "local.txt").write_text("private state\n", encoding="utf-8")
        (repository / "second.txt").unlink()
        before_status = git(repository, "status", "--porcelain=v2", "-z").stdout
        before_index = git(repository, "write-tree").stdout.strip()
        before_head = git(repository, "rev-parse", "HEAD").stdout.strip()

        cases = [
            ("workspace", None),
            ("staged", None),
            ("dirty", None),
            ("head", None),
            ("commit", root_commit),
            ("range", f"{root_commit}..{head_commit}"),
            ("range", f"{root_commit}...{head_commit}"),
        ]
        captures: list[tuple[dict[str, object], pathlib.Path]] = []
        for number, (kind, revision) in enumerate(cases):
            owner = root / f"owner-{number}.credential"
            completed, envelope = capture(binary, home, repository, kind, owner, revision)
            if completed.returncode != 0:
                raise AssertionError(f"capture failed for {kind}: {envelope}")
            assert_valid(envelope, machine, f"{kind} capture Machine envelope")
            assert_valid(envelope["data"], contract, f"{kind} capture result")
            data = envelope["data"]
            if str(owner) in completed.stdout or owner.read_text(encoding="utf-8") in completed.stdout:
                raise AssertionError("capture leaked settlement credential or carrier path")
            if stat.S_IMODE(owner.stat().st_mode) != 0o600:
                raise AssertionError("settlement credential is not 0600")
            immutable_root = pathlib.Path(str(data["immutable_root"]))
            if immutable_root.is_relative_to(repository):
                raise AssertionError("capture was published inside the source repository")
            assert_read_only_tree(immutable_root)
            manifest = json.loads((immutable_root.parent / "manifest.json").read_text(encoding="utf-8"))
            assert_valid(manifest, contract, f"{kind} capture manifest")
            if any(str(entry["path"]).startswith(("current/.dolgorae/", "after/.dolgorae/")) for entry in manifest["entries"]):
                raise AssertionError("capture exposed private Dolgorae workspace state")
            if any("/.dolgorae/" in str(entry["path"]) for entry in manifest["entries"]):
                raise AssertionError("capture exposed nested private tool state")
            captures.append((data, owner))

        if (pathlib.Path(str(captures[0][0]["immutable_root"])) / "current" / "renamed.txt").read_text() != "worktree\n":
            raise AssertionError("workspace did not select final worktree bytes")
        staged_root = pathlib.Path(str(captures[1][0]["immutable_root"]))
        if (staged_root / "after" / "renamed.txt").read_text() != "staged\n":
            raise AssertionError("staged capture substituted worktree bytes")
        dirty_root = pathlib.Path(str(captures[2][0]["immutable_root"]))
        if (dirty_root / "after" / "renamed.txt").read_text() != "worktree\n":
            raise AssertionError("dirty capture omitted unstaged bytes")
        if (dirty_root / "after" / "second.txt").exists():
            raise AssertionError("dirty capture failed to represent deletion")
        if not (dirty_root / "after" / "untracked.txt").is_file():
            raise AssertionError("dirty capture omitted non-ignored untracked bytes")
        root_capture = pathlib.Path(str(captures[4][0]["immutable_root"]))
        if any((root_capture / "before").iterdir()):
            raise AssertionError("root commit did not materialize an empty before tree")

        if git(repository, "status", "--porcelain=v2", "-z").stdout != before_status:
            raise AssertionError("capture changed source worktree or index status")
        if git(repository, "write-tree").stdout.strip() != before_index:
            raise AssertionError("capture changed the index")
        if git(repository, "rev-parse", "HEAD").stdout.strip() != before_head:
            raise AssertionError("capture changed HEAD")

        conflict = repository / "conflict.txt"
        conflict.write_text("base\n", encoding="utf-8")
        git(repository, "add", "conflict.txt")
        git(repository, "commit", "-m", "conflict base")
        base_branch = git(repository, "rev-parse", "HEAD").stdout.strip()
        git(repository, "checkout", "-b", "other")
        conflict.write_text("other\n", encoding="utf-8")
        git(repository, "commit", "-am", "other")
        git(repository, "checkout", "main")
        conflict.write_text("main\n", encoding="utf-8")
        git(repository, "commit", "-am", "main")
        git(repository, "merge", "other", check=False)
        conflict_owner = root / "conflict-owner"
        completed, envelope = capture(binary, home, repository, "staged", conflict_owner)
        if completed.returncode == 0 or envelope["error"]["code"] != "REVIEW_TARGET_CONFLICT":
            raise AssertionError(f"unresolved conflict did not fail closed: {envelope}")
        git(repository, "merge", "--abort")
        git(repository, "reset", "--hard", base_branch)

        secret = repository / "untracked-secret.txt"
        secret.write_text("-----BEGIN PRIVATE KEY-----\n", encoding="utf-8")
        secret_owner = root / "secret-owner"
        completed, envelope = capture(binary, home, repository, "workspace", secret_owner)
        if completed.returncode == 0 or envelope["error"]["code"] != "REVIEW_TARGET_SECRET_DETECTED":
            raise AssertionError(f"untracked secret did not fail closed: {envelope}")
        secret.unlink()

        existing_owner = root / "existing-owner"
        existing_owner.write_text("preserve-me", encoding="utf-8")
        existing_owner.chmod(0o600)
        completed, _ = capture(binary, home, repository, "head", existing_owner)
        if completed.returncode == 0 or existing_owner.read_text(encoding="utf-8") != "preserve-me":
            raise AssertionError("failed create-new credential delivery changed an existing file")

        inside_owner = repository / "settlement-owner"
        completed, inside = capture(binary, home, repository, "head", inside_owner)
        if completed.returncode == 0 or inside["error"]["code"] != "INVALID_ARGUMENT" or inside_owner.exists():
            raise AssertionError(f"workspace-local credential carrier was accepted: {inside}")

        invalid_owner = root / "invalid-revision-owner"
        completed, invalid_revision = capture(binary, home, repository, "commit", invalid_owner, "not-a-commit")
        if completed.returncode == 0 or invalid_revision["error"]["code"] != "REVIEW_TARGET_REVISION_INVALID":
            raise AssertionError(f"invalid revision was accepted: {invalid_revision}")

        oversized = repository / "oversized.bin"
        with oversized.open("wb") as output:
            output.truncate(8 * 1024 * 1024 + 1)
        oversized_owner = root / "oversized-owner"
        completed, oversized_result = capture(binary, home, repository, "workspace", oversized_owner)
        if completed.returncode == 0 or oversized_result["error"]["code"] != "REVIEW_TARGET_LIMIT_EXCEEDED":
            raise AssertionError(f"oversized target was accepted: {oversized_result}")
        oversized.unlink()

        tracked_secret = repository / "tracked-secret.txt"
        tracked_secret.write_text("github_pat_abcdefghijklmnopqrstuvwxyz\n", encoding="utf-8")
        git(repository, "add", "tracked-secret.txt")
        git(repository, "commit", "-m", "tracked secret fixture")
        tracked_owner = root / "tracked-secret-owner"
        completed, envelope = capture(binary, home, repository, "head", tracked_owner)
        if completed.returncode == 0 or envelope["error"]["code"] != "REVIEW_TARGET_SECRET_DETECTED":
            raise AssertionError(f"tracked secret did not fail closed: {envelope}")
        git(repository, "reset", "--hard", "HEAD^")

        unsafe = repository / "escaping-link"
        unsafe.symlink_to(root / "outside")
        unsafe_owner = root / "unsafe-owner"
        completed, envelope = capture(binary, home, repository, "workspace", unsafe_owner)
        if completed.returncode == 0 or envelope["error"]["code"] != "REVIEW_TARGET_UNSAFE_FILE":
            raise AssertionError(f"escaping link did not fail closed: {envelope}")
        unsafe.unlink()

        git(repository, "rm", "renamed.txt")
        (repository / "renamed.txt").write_text("recreated\n", encoding="utf-8")
        recreated_owner = root / "recreated-owner"
        completed, envelope = capture(binary, home, repository, "dirty", recreated_owner)
        if completed.returncode != 0:
            raise AssertionError(f"recreated path capture failed: {envelope}")
        recreated_root = pathlib.Path(str(envelope["data"]["immutable_root"]))
        if (recreated_root / "after" / "renamed.txt").read_text() != "recreated\n":
            raise AssertionError("dirty capture did not restore recreated worktree bytes")
        git(repository, "reset", "--hard", "HEAD")

        data, owner = captures[0]
        immutable_root = pathlib.Path(str(data["immutable_root"]))
        target = immutable_root / "current" / "renamed.txt"
        target.chmod(0o600)
        target.write_text("tampered\n", encoding="utf-8")
        terminal = root / "tampered-receipt.json"
        lifecycle = "life-workspace-owner-0"
        receipt(terminal, lifecycle)
        completed, envelope = invoke(binary, home, [
            "review-target", "settle", "--workspace", str(repository),
            "--capture-ref", str(data["capture_ref"]), "--expected-revision", "1",
            "--settlement-owner-file", str(owner), "--terminal-receipt-file", str(terminal),
        ])
        if completed.returncode == 0 or envelope["error"]["code"] != "REVIEW_TARGET_MUTATED":
            raise AssertionError(f"mutated capture did not fail closed: {envelope}")
        if not immutable_root.exists():
            raise AssertionError("failed settlement removed recovery evidence")

        settle_owner = root / "settle-owner"
        completed, envelope = capture(binary, home, repository, "head", settle_owner)
        if completed.returncode != 0:
            raise AssertionError(f"settlement capture failed: {envelope}")
        settle_data = envelope["data"]
        settle_receipt = root / "settle-receipt.json"
        receipt(settle_receipt, "life-head-settle-owner")
        settle_arguments = [
            "review-target", "settle", "--workspace", str(repository),
            "--capture-ref", str(settle_data["capture_ref"]), "--expected-revision", "1",
            "--settlement-owner-file", str(settle_owner),
            "--terminal-receipt-file", str(settle_receipt),
        ]
        stale_arguments = settle_arguments.copy()
        stale_arguments[stale_arguments.index("--expected-revision") + 1] = "2"
        completed, stale = invoke(binary, home, stale_arguments)
        if completed.returncode == 0 or stale["error"]["code"] != "REVIEW_TARGET_STALE_REVISION":
            raise AssertionError(f"stale settlement revision was accepted: {stale}")

        active_receipt = root / "active-receipt.json"
        receipt(active_receipt, "life-head-settle-owner")
        active_document = json.loads(active_receipt.read_text(encoding="utf-8"))
        active_document["terminal_state"] = "active"
        active_receipt.write_text(json.dumps(active_document), encoding="utf-8")
        active_receipt.chmod(0o600)
        active_arguments = settle_arguments.copy()
        active_arguments[active_arguments.index("--terminal-receipt-file") + 1] = str(active_receipt)
        completed, active = invoke(binary, home, active_arguments)
        if completed.returncode == 0 or active["error"]["code"] != "REVIEW_TARGET_TERMINAL_EVIDENCE_INVALID":
            raise AssertionError(f"active settlement receipt was accepted: {active}")

        mismatch_receipt = root / "mismatch-receipt.json"
        receipt(mismatch_receipt, "foreign-lifecycle")
        mismatch_arguments = settle_arguments.copy()
        mismatch_arguments[mismatch_arguments.index("--terminal-receipt-file") + 1] = str(mismatch_receipt)
        completed, mismatch = invoke(binary, home, mismatch_arguments)
        if completed.returncode == 0 or mismatch["error"]["code"] != "REVIEW_TARGET_LIFECYCLE_MISMATCH":
            raise AssertionError(f"mismatched lifecycle receipt was accepted: {mismatch}")

        if not pathlib.Path(str(settle_data["immutable_root"])).exists():
            raise AssertionError("rejected settlement removed recovery evidence")

        settle_owner.chmod(0o644)
        completed, carrier = invoke(binary, home, settle_arguments)
        if completed.returncode == 0 or carrier["error"]["code"] != "REVIEW_TARGET_CARRIER_INVALID":
            raise AssertionError(f"public settlement credential was accepted: {carrier}")
        settle_owner.chmod(0o600)
        with ThreadPoolExecutor(max_workers=2) as pool:
            outcomes = list(pool.map(lambda _: invoke(binary, home, settle_arguments), range(2)))
        successes = [value for completed, value in outcomes if completed.returncode == 0]
        failures = [value for completed, value in outcomes if completed.returncode != 0]
        if not successes or any(
            value["error"]["code"] != "REVIEW_TARGET_SETTLEMENT_CONCURRENT"
            for value in failures
        ):
            raise AssertionError(f"settlement race escaped CAS semantics: {outcomes}")
        if sum(not value["data"]["replay"] for value in successes) != 1:
            raise AssertionError(f"settlement race did not have one mutation winner: {successes}")
        for success in successes:
            assert_valid(success, machine, "settlement Machine envelope")
            assert_valid(success["data"], contract, "settlement result")
        if pathlib.Path(str(settle_data["immutable_root"])).exists():
            raise AssertionError("successful settlement retained source bytes")
        completed, replay = invoke(binary, home, settle_arguments)
        if completed.returncode != 0 or replay["data"]["replay"] is not True:
            raise AssertionError(f"exact settlement replay failed: {replay}")

        timed_data, timed_owner = captures[3]
        timed_receipt = root / "timed-out-receipt.json"
        receipt(timed_receipt, "life-head-owner-3", "timed_out")
        completed, timed = invoke(binary, home, [
            "review-target", "settle", "--workspace", str(repository),
            "--capture-ref", str(timed_data["capture_ref"]), "--expected-revision", "1",
            "--settlement-owner-file", str(timed_owner),
            "--terminal-receipt-file", str(timed_receipt),
        ])
        if completed.returncode != 0 or timed["data"]["state"] != "settled":
            raise AssertionError(f"authoritative timeout did not settle: {timed}")

        crash_owner = root / "crash-owner"
        completed, crash_capture = capture(binary, home, repository, "head", crash_owner)
        if completed.returncode != 0:
            raise AssertionError(f"crash recovery capture failed: {crash_capture}")
        crash_data = crash_capture["data"]
        crash_source = pathlib.Path(str(crash_data["immutable_root"]))
        crash_source.rename(crash_source.parent / ".settlement-source")
        crash_receipt = root / "crash-receipt.json"
        receipt(crash_receipt, "life-head-crash-owner")
        completed, recovered = invoke(binary, home, [
            "review-target", "settle", "--workspace", str(repository),
            "--capture-ref", str(crash_data["capture_ref"]), "--expected-revision", "1",
            "--settlement-owner-file", str(crash_owner),
            "--terminal-receipt-file", str(crash_receipt),
        ])
        if completed.returncode != 0 or recovered["data"]["state"] != "settled":
            raise AssertionError(f"mid-settlement crash recovery failed: {recovered}")

        foreign = root / "foreign-owner"
        foreign.write_text("foreign", encoding="utf-8")
        foreign.chmod(0o600)
        changed = settle_receipt.read_text(encoding="utf-8").replace('"state_revision":4', '"state_revision":5')
        changed_receipt = root / "changed-receipt.json"
        changed_receipt.write_text(changed, encoding="utf-8")
        changed_receipt.chmod(0o600)
        for carrier, terminal_path, code in [
            (foreign, settle_receipt, "REVIEW_TARGET_FOREIGN_OWNER"),
            (settle_owner, changed_receipt, "REVIEW_TARGET_SETTLEMENT_CONFLICT"),
        ]:
            arguments = settle_arguments.copy()
            arguments[arguments.index("--settlement-owner-file") + 1] = str(carrier)
            arguments[arguments.index("--terminal-receipt-file") + 1] = str(terminal_path)
            completed, failed = invoke(binary, home, arguments)
            if completed.returncode == 0 or failed["error"]["code"] != code:
                raise AssertionError(f"settlement rejection mismatch: {failed}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument("--protocol-root", type=pathlib.Path, default=pathlib.Path("docs/protocol"))
    arguments = parser.parse_args()
    validate(arguments.binary.resolve(), arguments.protocol_root.resolve())
    print("Review-target CLI validation passed: scopes, safety, settlement, replay")
    return 0


if __name__ == "__main__":
    sys.exit(main())
