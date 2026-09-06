#!/usr/bin/env python3
"""Black-box validation of workspace behavior, output, and file permissions."""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import shutil
import stat
import subprocess
import sys
import tempfile

from schema_support import assert_valid, validator


def run(binary: pathlib.Path, arguments: list[str], home: pathlib.Path) -> subprocess.CompletedProcess[str]:
    environment = os.environ.copy()
    environment["HOME"] = str(home)
    return subprocess.run(
        [str(binary), *arguments],
        check=False,
        capture_output=True,
        text=True,
        env=environment,
    )


def git(repository: pathlib.Path, *arguments: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", "-C", str(repository), *arguments],
        check=True,
        capture_output=True,
        text=True,
    )


def require_supported_git() -> None:
    completed = subprocess.run(
        ["git", "--version"], check=True, capture_output=True, text=True
    )
    match = re.match(r"git version (\d+)\.(\d+)", completed.stdout)
    if match is None or tuple(map(int, match.groups())) < (2, 39):
        raise AssertionError(
            f"Git 2.39 or later is required for E2E, got {completed.stdout.strip()!r}"
        )


def validate(binary: pathlib.Path, protocol_root: pathlib.Path) -> None:
    require_supported_git()
    machine = validator(protocol_root, "dolgorae-machine-v2.schema.json")
    workspace_record = validator(protocol_root, "dolgorae-workspace-record-v1.schema.json")
    portable_policy = validator(
        protocol_root, "dolgorae-portable-workspace-policy-v1.schema.json"
    )
    global_profiles = validator(
        protocol_root, "dolgorae-global-profile-registry-v1.schema.json"
    )
    home_state = validator(protocol_root, "dolgorae-home-state-v2.schema.json")
    binding_history = validator(
        protocol_root, "dolgorae-profile-binding-history-v1.schema.json"
    )

    with tempfile.TemporaryDirectory(prefix="dolgorae-task002-validator-") as temporary:
        root = pathlib.Path(temporary)
        home = root / "home"
        home.mkdir(mode=0o700)
        for existing_empty_home in (False, True):
            refused_home = root / f"refused-home-{existing_empty_home}"
            refused_home.mkdir(mode=0o700)
            global_root = refused_home / ".dolgorae"
            if existing_empty_home:
                global_root.mkdir(mode=0o700)
            incompatible = root / f"incompatible-{existing_empty_home}"
            incompatible.mkdir(mode=0o700)
            policy = incompatible / ".dolgorae"
            policy.mkdir(mode=0o700)
            (policy / "config.yaml").write_text("schema_version: 1\nmode: git\n", encoding="utf-8")
            (policy / ".gitignore").write_text("/exports/\n", encoding="utf-8")
            before = {path.name: path.read_bytes() for path in policy.iterdir()}
            refused = run(binary, ["init", "--non-git", str(incompatible)], refused_home)
            envelope = json.loads(refused.stdout)
            assert_valid(envelope, machine, "incompatible-init Machine envelope")
            if refused.returncode != 4 or envelope["error"]["code"] != "WORKSPACE_INITIALIZATION_CONFLICT":
                raise AssertionError(f"incompatible policy was not refused: {refused!r}")
            if global_root.exists() != existing_empty_home or (global_root.exists() and list(global_root.iterdir())):
                raise AssertionError("rejected initialization mutated global home state")
            if {path.name: path.read_bytes() for path in policy.iterdir()} != before:
                raise AssertionError("rejected initialization changed portable policy")
        repository = root / "repository"
        repository.mkdir(mode=0o700)
        git(repository, "init", "-b", "main")
        git(repository, "config", "user.name", "Dolgorae E2E")
        git(repository, "config", "user.email", "dolgorae@example.invalid")
        (repository / "tracked.txt").write_text("initial\n", encoding="utf-8")
        git(repository, "add", "tracked.txt")
        git(repository, "commit", "-m", "initial")
        linked = root / "linked"
        git(repository, "worktree", "add", "-b", "linked", str(linked))
        (repository / "tracked.txt").write_text("dirty\n", encoding="utf-8")
        (repository / "untracked.txt").write_text("preserve\n", encoding="utf-8")

        initialized = run(binary, ["init", str(repository)], home)
        if initialized.returncode != 0 or initialized.stderr:
            raise AssertionError(
                f"init failed: status={initialized.returncode} "
                f"stdout={initialized.stdout!r} stderr={initialized.stderr!r}"
            )
        initialized_envelope = json.loads(initialized.stdout)
        assert_valid(initialized_envelope, machine, "init Machine envelope")
        if initialized_envelope["data"]["created"] is not True:
            raise AssertionError("first initialization did not report created:true")
        workspace_id = initialized_envelope["data"]["workspace_id"]
        home_entries = {entry.name for entry in home.iterdir()}
        if home_entries != {".dolgorae"}:
            raise AssertionError(f"unexpected per-user state roots: {home_entries!r}")

        repeated = run(binary, ["init", str(repository)], home)
        if repeated.returncode != 0:
            raise AssertionError(f"repeated init failed: {repeated.stdout}")
        repeated_envelope = json.loads(repeated.stdout)
        assert_valid(repeated_envelope, machine, "repeated-init Machine envelope")
        if repeated_envelope["data"]["created"] is not False:
            raise AssertionError("repeated initialization did not report created:false")
        if (repository / "tracked.txt").read_text(encoding="utf-8") != "dirty\n":
            raise AssertionError("initialization changed a pre-existing tracked modification")
        if (repository / "untracked.txt").read_text(encoding="utf-8") != "preserve\n":
            raise AssertionError("initialization changed a pre-existing untracked file")

        linked_initialized = run(binary, ["init", str(linked)], home)
        if linked_initialized.returncode != 0 or linked_initialized.stderr:
            raise AssertionError(
                f"linked worktree init failed: {linked_initialized.stdout!r} "
                f"{linked_initialized.stderr!r}"
            )
        linked_envelope = json.loads(linked_initialized.stdout)
        assert_valid(linked_envelope, machine, "linked-worktree Machine envelope")
        if linked_envelope["data"]["workspace_id"] == workspace_id:
            raise AssertionError("distinct Git worktrees received the same workspace ID")

        nested = repository / "nested" / "directory"
        nested.mkdir(parents=True)
        environment = os.environ.copy()
        environment["HOME"] = str(home)
        inspected = subprocess.run(
            [str(binary), "workspace", "inspect"],
            check=False,
            capture_output=True,
            text=True,
            env=environment,
            cwd=nested,
        )
        if inspected.returncode != 0 or inspected.stderr:
            raise AssertionError(f"upward workspace inspection failed: {inspected.stdout}")
        inspected_envelope = json.loads(inspected.stdout)
        assert_valid(inspected_envelope, machine, "workspace-inspect Machine envelope")
        if inspected_envelope["data"]["workspace_id"] != workspace_id:
            raise AssertionError("upward discovery selected the wrong workspace")

        state_root = home / ".dolgorae" / "workspaces" / workspace_id
        record = json.loads((state_root / "workspace.json").read_text(encoding="utf-8"))
        assert_valid(record, workspace_record, "workspace record")
        if record["initial_git_baseline"]["tracked_changes"] != ["tracked.txt"]:
            raise AssertionError("workspace record did not preserve the tracked Git baseline")
        if record["initial_git_baseline"]["untracked_paths"] != ["untracked.txt"]:
            raise AssertionError("workspace record did not preserve the untracked Git baseline")
        policy_text = (repository / ".dolgorae" / "config.yaml").read_text(
            encoding="utf-8"
        )
        if policy_text != "schema_version: 1\nmode: git\n":
            raise AssertionError(f"unexpected portable policy bytes: {policy_text!r}")
        policy = {"schema_version": 1, "mode": "git"}
        assert_valid(policy, portable_policy, "portable workspace policy")
        if (state_root / "local.yaml").exists():
            raise AssertionError("workspace initialization created a local Profile registry")
        profiles_text = (home / ".dolgorae" / "profiles.yaml").read_text(encoding="utf-8")
        profiles = {"schema_version": 1, "profiles": {}}
        assert_valid(profiles, global_profiles, "global profile registry")
        assert_valid(
            json.loads((home / ".dolgorae" / "state.json").read_text(encoding="utf-8")),
            home_state,
            "Dolgorae home generation",
        )
        assert_valid(
            json.loads(
                (home / ".dolgorae" / "profile-bindings.json").read_text(
                    encoding="utf-8"
                )
            ),
            binding_history,
            "global Profile binding history",
        )

        for directory in [state_root, state_root / "runtime" / "locks", state_root / "orchestration"]:
            if stat.S_IMODE(directory.stat().st_mode) != 0o700:
                raise AssertionError(f"unsafe directory mode: {directory}")
        for file_path in [
            state_root / "workspace.json",
            home / ".dolgorae" / "profiles.yaml",
            home / ".dolgorae" / "profile-bindings.json",
            home / ".dolgorae" / "state.json",
        ]:
            if stat.S_IMODE(file_path.stat().st_mode) != 0o600:
                raise AssertionError(f"unsafe file mode: {file_path}")

        policy_before_rediscovery = {
            path.name: path.read_bytes()
            for path in (repository / ".dolgorae").iterdir()
            if path.is_file()
        }
        state_root = home / ".dolgorae" / "workspaces" / workspace_id
        shutil.rmtree(state_root)
        rediscovered = run(binary, ["init", str(repository)], home)
        if rediscovered.returncode != 0 or rediscovered.stderr:
            raise AssertionError(f"rediscovery init failed: {rediscovered.stdout}")
        rediscovered_envelope = json.loads(rediscovered.stdout)
        assert_valid(rediscovered_envelope, machine, "rediscovery-init Machine envelope")
        if rediscovered_envelope["data"]["workspace_id"] != workspace_id:
            raise AssertionError("rediscovery changed the deterministic workspace ID")
        if rediscovered_envelope["data"]["created"] is not True:
            raise AssertionError("rediscovery did not report created:true")
        policy_after_rediscovery = {
            path.name: path.read_bytes()
            for path in (repository / ".dolgorae").iterdir()
            if path.is_file()
        }
        if policy_after_rediscovery != policy_before_rediscovery:
            raise AssertionError("rediscovery changed portable policy bytes")
        rediscovered_repeat = run(binary, ["init", str(repository)], home)
        if json.loads(rediscovered_repeat.stdout)["data"]["created"] is not False:
            raise AssertionError("repeated rediscovery did not report created:false")


        uninitialized = root / "uninitialized"
        uninitialized.mkdir(mode=0o700)
        refused = run(
            binary,
            [
                "run",
                "start",
                "--workspace",
                str(uninitialized),
                "--profile",
                "default",
                "--control-mode",
                "direct-interactive",
                "--execution-lane",
                "shared-readonly",
                "--required-assurance",
                "best-effort-personal-alpha",
                "--require-capability",
                "workspace",
                "--purpose",
                "implementation",
                "--idempotency-key",
                "task002-validator",
            ],
            home,
        )
        if refused.returncode != 3:
            raise AssertionError(f"uninitialized start returned {refused.returncode}: {refused.stdout}")
        refused_envelope = json.loads(refused.stdout)
        assert_valid(refused_envelope, machine, "uninitialized-start Machine envelope")
        if refused_envelope["error"]["code"] != "WORKSPACE_NOT_INITIALIZED":
            raise AssertionError("uninitialized start returned the wrong error")

        lock_root = state_root / "runtime" / "locks"
        displaced_lock_root = lock_root.with_name("locks-displaced")
        lock_root.rename(displaced_lock_root)
        lock_root.mkdir(mode=0o700)
        replaced = run(
            binary,
            ["workspace", "inspect", "--workspace", str(repository)],
            home,
        )
        if replaced.returncode != 6:
            raise AssertionError(f"replaced lock root returned {replaced.returncode}: {replaced.stdout}")
        replaced_envelope = json.loads(replaced.stdout)
        assert_valid(replaced_envelope, machine, "replaced-lock Machine envelope")
        if replaced_envelope["error"]["code"] != "RUNTIME_PATH_COLLISION":
            raise AssertionError("replaced lock root returned the wrong error")

        git_inside = root / "git-inside"
        git_inside.mkdir(mode=0o700)
        git(git_inside, "init", "-b", "main")
        non_git = run(binary, ["init", "--non-git", str(git_inside)], home)
        if non_git.returncode != 4:
            raise AssertionError(
                f"--non-git inside Git returned {non_git.returncode}: {non_git.stdout}"
            )
        non_git_envelope = json.loads(non_git.stdout)
        assert_valid(non_git_envelope, machine, "non-git-inside-Git Machine envelope")
        if non_git_envelope["error"]["code"] != "WORKSPACE_INITIALIZATION_CONFLICT":
            raise AssertionError("--non-git inside Git returned the wrong error")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument(
        "--protocol-root", type=pathlib.Path, default=pathlib.Path("docs/protocol")
    )
    arguments = parser.parse_args()
    validate(arguments.binary.resolve(), arguments.protocol_root.resolve())
    print(
        "Workspace CLI validation passed: Git baseline, worktrees, schemas, isolation, refusals"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
