#!/usr/bin/env python3
"""Black-box validation of workspace behavior, output, and file permissions."""

from __future__ import annotations

import argparse
import json
import os
import pathlib
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


def validate(binary: pathlib.Path, protocol_root: pathlib.Path) -> None:
    machine = validator(protocol_root, "dolgorae-machine-v1.schema.json")
    workspace_record = validator(protocol_root, "dolgorae-workspace-record-v1.schema.json")
    portable_policy = validator(
        protocol_root, "dolgorae-portable-workspace-policy-v1.schema.json"
    )
    local_profiles = validator(
        protocol_root, "dolgorae-local-profile-registry-v1.schema.json"
    )

    with tempfile.TemporaryDirectory(prefix="dolgorae-task002-validator-") as temporary:
        root = pathlib.Path(temporary)
        home = root / "home"
        application_support = home / "Library" / "Application Support"
        application_support.mkdir(parents=True, mode=0o700)
        repository = root / "repository"
        repository.mkdir(mode=0o700)
        subprocess.run(
            ["git", "-C", str(repository), "init", "-b", "main"],
            check=True,
            capture_output=True,
            text=True,
        )
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

        repeated = run(binary, ["init", str(repository)], home)
        if repeated.returncode != 0:
            raise AssertionError(f"repeated init failed: {repeated.stdout}")
        repeated_envelope = json.loads(repeated.stdout)
        assert_valid(repeated_envelope, machine, "repeated-init Machine envelope")
        if repeated_envelope["data"]["created"] is not False:
            raise AssertionError("repeated initialization did not report created:false")
        if (repository / "untracked.txt").read_text(encoding="utf-8") != "preserve\n":
            raise AssertionError("initialization changed a pre-existing untracked file")

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

        state_root = (
            application_support
            / "Dolgorae"
            / "workspaces"
            / workspace_id
        )
        record = json.loads((state_root / "workspace.json").read_text(encoding="utf-8"))
        assert_valid(record, workspace_record, "workspace record")
        policy_text = (repository / ".dolgorae" / "config.yaml").read_text(
            encoding="utf-8"
        )
        if policy_text != "schema_version: 1\nmode: git\n":
            raise AssertionError(f"unexpected portable policy bytes: {policy_text!r}")
        policy = {"schema_version": 1, "mode": "git"}
        assert_valid(policy, portable_policy, "portable workspace policy")
        profiles_text = (state_root / "local.yaml").read_text(encoding="utf-8")
        if profiles_text != "schema_version: 1\nprofiles: {}\n":
            raise AssertionError(f"unexpected local profile bytes: {profiles_text!r}")
        profiles = {"schema_version": 1, "profiles": {}}
        assert_valid(profiles, local_profiles, "local profile registry")

        for directory in [state_root, state_root / "runtime" / "locks", state_root / "orchestration"]:
            if stat.S_IMODE(directory.stat().st_mode) != 0o700:
                raise AssertionError(f"unsafe directory mode: {directory}")
        for file_path in [state_root / "workspace.json", state_root / "local.yaml"]:
            if stat.S_IMODE(file_path.stat().st_mode) != 0o600:
                raise AssertionError(f"unsafe file mode: {file_path}")

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
        lock_root.rmdir()
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


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument(
        "--protocol-root", type=pathlib.Path, default=pathlib.Path("docs/protocol")
    )
    arguments = parser.parse_args()
    validate(arguments.binary.resolve(), arguments.protocol_root.resolve())
    print(
        "Workspace CLI validation passed: init, repeat, schemas, modes, preservation, refusal, lock identity"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
