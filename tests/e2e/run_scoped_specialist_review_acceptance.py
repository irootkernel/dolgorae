#!/usr/bin/env python3
"""Opt-in live acceptance for immutable-target Specialist Review v2."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import subprocess

from jsonschema import Draft202012Validator, FormatChecker
from referencing import Registry, Resource

from run_specialist_review_acceptance import digest, run, workspace_fingerprint

OPT_IN = "DOLGORAE_RUN_LIVE_SCOPED_SPECIALIST_REVIEW"
PINNED_VERSION = "codex-cli 0.158.0"
PROTOCOL = pathlib.Path(__file__).resolve().parents[2] / "docs" / "protocol"


def registry() -> Registry:
    result = Registry()
    for path in sorted(PROTOCOL.glob("*.schema.json")):
        schema = json.loads(path.read_text(encoding="utf-8"))
        result = result.with_resource(schema["$id"], Resource.from_contents(schema))
    return result


def validate(schema_name: str, value: object, resources: Registry) -> None:
    schema = json.loads((PROTOCOL / schema_name).read_text(encoding="utf-8"))
    errors = list(
        Draft202012Validator(
            schema, registry=resources, format_checker=FormatChecker()
        ).iter_errors(value)
    )
    if errors:
        locations = ["/".join(map(str, error.absolute_path)) or "<root>" for error in errors]
        raise ValueError(f"checked schema rejected live result at {locations}")


def executable_evidence(codex: pathlib.Path) -> dict[str, object]:
    canonical = codex.resolve(strict=True)
    version = subprocess.run(
        [str(canonical), "--version"], check=True, capture_output=True, text=True
    ).stdout.strip()
    if version != PINNED_VERSION:
        raise ValueError(f"expected {PINNED_VERSION!r}, got {version!r}")
    metadata = canonical.stat()
    content = hashlib.sha256()
    with canonical.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            content.update(chunk)
    return {
        "version": version.removeprefix("codex-cli "),
        "resolved_path": str(canonical),
        "device": metadata.st_dev,
        "inode": metadata.st_ino,
        "sha256": content.hexdigest(),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--workspace", required=True, type=pathlib.Path)
    parser.add_argument("--profile", required=True)
    parser.add_argument("--codex", required=True, type=pathlib.Path)
    parser.add_argument(
        "--target-kind",
        required=True,
        choices=["workspace", "staged", "dirty", "head", "commit", "range"],
    )
    parser.add_argument("--revision")
    arguments = parser.parse_args()
    if os.environ.get(OPT_IN) != "1":
        raise SystemExit(f"{OPT_IN}=1 is required")

    workspace = arguments.workspace.resolve(strict=True)
    binary = arguments.binary.resolve(strict=True)
    expected_executable = executable_evidence(arguments.codex)
    before = workspace_fingerprint(workspace)
    command = [
        str(binary), "specialist", "review", "--workspace", str(workspace),
        "--profile", arguments.profile, "--target-kind", arguments.target_kind,
        "--format", "json",
    ]
    if arguments.revision is not None:
        command.extend(["--revision", arguments.revision])
    completed = run(command, cwd=workspace, env=os.environ.copy())
    after = workspace_fingerprint(workspace)
    if completed.returncode != 0:
        failure = json.loads(completed.stdout)
        raise ValueError(
            f"live scoped review failed with exit {completed.returncode}: "
            f"{json.dumps(failure.get('error'), sort_keys=True)}"
        )
    envelope = json.loads(completed.stdout)
    resources = registry()
    validate("dolgorae-machine-v2.schema.json", envelope, resources)
    result = envelope["data"]
    validate("dolgorae-specialist-review-tool-v2.schema.json", result, resources)
    if (result["reviewer"]["model"], result["reviewer"]["effort"]) != ("gpt-5.6-luna", "low"):
        raise ValueError("live review did not use gpt-5.6-luna / low")
    observed = result["reviewer"]["executable"]
    if observed["version"] != expected_executable["version"]:
        raise ValueError("live result bound the wrong Codex version")
    identity = observed["file_identity"]
    for field in ("resolved_path", "device", "inode", "sha256"):
        if identity[field] != expected_executable[field]:
            raise ValueError(f"live result bound the wrong executable {field}")
    if observed["sha256"] != expected_executable["sha256"]:
        raise ValueError("live result SHA-256 disagrees with the executable identity")
    if before != after or result["workflow_issued_source_mutation"] is not False:
        raise ValueError("live scoped review changed the source workspace")
    if result["settlement"]["state"] != "settled":
        raise ValueError("live scoped review did not settle its authoritative result")

    evidence = {
        "schema": "dolgorae-live-scoped-specialist-review-evidence/v1",
        "target_kind": arguments.target_kind,
        "requested_revision": arguments.revision,
        "codex": expected_executable,
        "capability_result": observed["capability_result"],
        "model": result["reviewer"]["model"],
        "effort": result["reviewer"]["effort"],
        "capture": {
            "manifest_digest": result["target"]["manifest_digest"],
            "whole_target_digest": result["target"]["whole_target_digest"],
            "integrity": result["target"]["capture_integrity"],
        },
        "finding_count": len(result["verdict"]["findings"]),
        "workspace_fingerprint": before,
        "workflow_issued_source_mutation": False,
        "settlement": "settled",
        "machine_output_digest": digest(completed.stdout),
    }
    print(json.dumps(evidence, separators=(",", ":"), sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
