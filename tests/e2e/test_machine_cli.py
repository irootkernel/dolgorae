#!/usr/bin/env python3
"""Black-box validation of the version and Machine v1 CLI contracts."""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import stat
import subprocess
import sys

from schema_support import assert_valid, validator


def validate(binary: pathlib.Path, protocol_root: pathlib.Path) -> None:
    machine = validator(protocol_root, "dolgorae-machine-v1.schema.json")
    version_schema = validator(protocol_root, "dolgorae-version-v1.schema.json")
    cases = (
        ("--help",),
        ("help",),
        ("help", "runtime"),
        ("runtime", "capabilities"),
    )
    for arguments in cases:
        completed = subprocess.run(
            [str(binary), *arguments],
            check=True,
            capture_output=True,
            text=True,
        )
        if completed.stderr:
            raise AssertionError(f"unexpected stderr for {arguments}: {completed.stderr}")
        instance = json.loads(completed.stdout)
        assert_valid(instance, machine, f"output for {arguments}")
        if not completed.stdout.endswith("\n"):
            raise AssertionError(f"machine output lacks final LF for {arguments}")
        if arguments == ("runtime", "capabilities"):
            features = instance["data"]["features"]
            # The worker now revalidates the SCM_RIGHTS Controller descriptor
            # against the reset-journal-reconciled binding under the Run
            # mutation lock before every mutating effect, so this flag is
            # advertised true.
            if features["worker_controller_revalidation"] is not True:
                raise AssertionError(
                    "worker_controller_revalidation must be true once the "
                    "worker performs authoritative credential revalidation"
                )
            # safe_client_projection stays false: no production caller reads a
            # complete client-safe observation (Controller metadata plus
            # pending interactions) yet, so advertising it would claim
            # observer behavior the runtime does not implement.
            if features["safe_client_projection"] is not False:
                raise AssertionError(
                    "safe_client_projection must stay false until a real "
                    "safe-observer projection caller exists"
                )
        elif arguments[0] in {"--help", "help"}:
            if instance["command"] != "help":
                raise AssertionError(f"help used the wrong command: {arguments}")
            if not instance["data"]["text"].startswith("Usage: dolgorae"):
                raise AssertionError(f"help omitted usage text: {arguments}")
            if len(arguments) == 1 and "  version" not in instance["data"]["text"]:
                raise AssertionError(f"top-level help omitted version: {arguments}")

    text_version = None
    for arguments in (
        ("version",),
        ("--version",),
        ("-V",),
        ("--human", "version"),
        ("--human", "--version"),
        ("--human", "-V"),
        ("version", "--human"),
    ):
        completed = subprocess.run(
            [str(binary), *arguments], check=True, capture_output=True, text=True
        )
        if completed.stderr:
            raise AssertionError(f"unexpected version stderr for {arguments}: {completed.stderr}")
        if not completed.stdout.startswith("dolgorae v") or not completed.stdout.endswith("\n"):
            raise AssertionError(f"invalid text version for {arguments}: {completed.stdout!r}")
        if "\n" in completed.stdout[:-1]:
            raise AssertionError(f"text version is not one line for {arguments}")
        if text_version is None:
            text_version = completed.stdout.removeprefix("dolgorae ").removesuffix("\n")
        elif completed.stdout != f"dolgorae {text_version}\n":
            raise AssertionError(f"version aliases disagree for {arguments}: {completed.stdout!r}")

    json_version = subprocess.run(
        [str(binary), "version", "--json"],
        check=True,
        capture_output=True,
        text=True,
    )
    if json_version.stderr:
        raise AssertionError(f"unexpected JSON version stderr: {json_version.stderr}")
    expected_json = f'{{"name":"dolgorae","version":"{text_version}"}}\n'
    if json_version.stdout != expected_json:
        raise AssertionError(
            f"JSON version does not match the compact contract: {json_version.stdout!r}"
        )
    assert_valid(json.loads(json_version.stdout), version_schema, "JSON version output")

    conflicting_version_modes = subprocess.run(
        [str(binary), "--human", "version", "--json"],
        check=False,
        capture_output=True,
        text=True,
    )
    if (
        conflicting_version_modes.returncode != 2
        or conflicting_version_modes.stdout
        or "INVALID_ARGUMENT" not in conflicting_version_modes.stderr
    ):
        raise AssertionError(
            "version output mode conflict did not use the human syntax-error boundary: "
            f"{conflicting_version_modes!r}"
        )

    home = pathlib.Path(os.environ["HOME"])
    controller = home / "controller.json"
    operator_one = home / "operator-1.json"
    operator_two = home / "operator-2.json"
    credential_cases = (
        (
            "controller",
            "credential",
            "create",
            "--kind",
            "automation",
            "--instance-id",
            "machine-e2e",
            "--subject-id",
            "pipeline",
            "--output",
            str(controller),
        ),
        (
            "operator",
            "credential",
            "initialize",
            "--output",
            str(operator_one),
        ),
        (
            "operator",
            "credential",
            "rotate",
            "--operator-file",
            str(operator_one),
            "--output",
            str(operator_two),
        ),
    )
    for arguments in credential_cases:
        completed = subprocess.run(
            [str(binary), *arguments], check=True, capture_output=True, text=True
        )
        instance = json.loads(completed.stdout)
        assert_valid(instance, machine, f"credential output for {arguments[:3]}")
        if "capability" in completed.stdout:
            raise AssertionError("credential command leaked capability material")
    for path in (controller, operator_one, operator_two):
        if stat.S_IMODE(path.stat().st_mode) != 0o600:
            raise AssertionError(f"credential mode is not 0600: {path}")

    unknown = subprocess.run(
        [str(binary), "definitely-unknown"],
        check=False,
        capture_output=True,
        text=True,
    )
    if unknown.returncode != 2 or unknown.stderr:
        raise AssertionError(
            f"unknown command boundary failed: status={unknown.returncode} "
            f"stderr={unknown.stderr!r}"
        )
    unknown_envelope = json.loads(unknown.stdout)
    assert_valid(unknown_envelope, machine, "unknown-command Machine envelope")
    if unknown_envelope["command"] != "unknown":
        raise AssertionError("unknown command used the wrong command identifier")
    if unknown_envelope["error"]["code"] != "INVALID_ARGUMENT":
        raise AssertionError("unknown command used the wrong error code")

    human_help = subprocess.run(
        [str(binary), "--human", "help", "runtime"],
        check=True,
        capture_output=True,
        text=True,
    )
    if human_help.stderr or not human_help.stdout.startswith("Usage: dolgorae runtime"):
        raise AssertionError(f"human help boundary failed: {human_help!r}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument(
        "--protocol-root", type=pathlib.Path, default=pathlib.Path("docs/protocol")
    )
    arguments = parser.parse_args()
    validate(arguments.binary.resolve(), arguments.protocol_root.resolve())
    print("CLI validation passed: compact version, envelopes, errors, human boundary")
    return 0


if __name__ == "__main__":
    sys.exit(main())
