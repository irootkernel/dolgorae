#!/usr/bin/env python3
"""Black-box TASK-015 coverage for immutable-target Specialist Review v2."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import subprocess
import tempfile
import threading
import time

import native_codex
from schema_support import assert_valid, validator


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


def git(repository: pathlib.Path, *arguments: str) -> str:
    return subprocess.run(
        ["git", "-C", str(repository), *arguments],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def validate(binary: pathlib.Path, protocol_root: pathlib.Path) -> None:
    machine_schema = validator(protocol_root, "dolgorae-machine-v2.schema.json")
    v2_schema = validator(protocol_root, "dolgorae-specialist-review-tool-v2.schema.json")
    schema_source = native_codex.installed_codex()
    with tempfile.TemporaryDirectory(prefix="dolgorae-task015-") as temporary:
        root = pathlib.Path(temporary).resolve()
        home = root / "home"
        workspace = root / "workspace"
        codex_home = root / "codex-home"
        bin_root = root / "bin"
        for directory in (home, workspace, codex_home, bin_root):
            directory.mkdir(mode=0o700)
        git(workspace, "init", "-b", "main")
        git(workspace, "config", "user.name", "Dolgorae E2E")
        git(workspace, "config", "user.email", "dolgorae@example.invalid")
        (workspace / "root.txt").write_text("root\n", encoding="utf-8")
        git(workspace, "add", "root.txt")
        git(workspace, "commit", "-m", "root")
        root_commit = git(workspace, "rev-parse", "HEAD")
        (workspace / "second.txt").write_text("second\n", encoding="utf-8")
        git(workspace, "add", "second.txt")
        git(workspace, "commit", "-m", "second")
        head_commit = git(workspace, "rev-parse", "HEAD")
        (workspace / "root.txt").write_text("staged\n", encoding="utf-8")
        git(workspace, "add", "root.txt")
        (workspace / "root.txt").write_text("dirty\n", encoding="utf-8")
        (workspace / "untracked.txt").write_text("untracked\n", encoding="utf-8")

        initialized, envelope = invoke(binary, home, ["init", str(workspace)])
        if initialized.returncode != 0:
            raise AssertionError(f"workspace initialization failed: {envelope!r}")
        workspace_id = str(envelope["data"]["workspace_id"])  # type: ignore[index]
        state_root = home / ".dolgorae" / "workspaces" / workspace_id
        operator = root / "operator"
        initialized_operator, operator_envelope = invoke(
            binary, home, ["operator", "credential", "initialize", "--output", str(operator)]
        )
        if initialized_operator.returncode != 0:
            raise AssertionError(f"operator initialization failed: {operator_envelope!r}")

        cases = [
            ("workspace", None),
            ("staged", None),
            ("dirty", None),
            ("head", None),
            ("commit", root_commit),
            ("range", f"{root_commit}..{head_commit}"),
            ("range", f"{root_commit}...{head_commit}"),
        ]
        review_ids: set[str] = set()
        run_ids: set[str] = set()
        profiles: list[str] = []
        transcripts: list[pathlib.Path] = []
        try:
            for index, (kind, revision) in enumerate(cases):
                profile = f"task015-reviewer-{index}"
                profiles.append(profile)
                case_home = codex_home / str(index)
                case_bin = bin_root / str(index)
                case_home.mkdir(mode=0o700)
                case_bin.mkdir(mode=0o700)
                scenario = root / f"scenario-{index}.json"
                scenario.write_text(
                    native_codex.scenario_path("scoped_specialist_review.json")
                    .read_text(encoding="utf-8")
                    .replace("thread-scope-1", f"thread-case-{index}"),
                    encoding="utf-8",
                )
                if index == 0:
                    configuration = case_home / "config.toml"
                    configuration.write_text(
                        'model = "gpt-5.6-luna"\nmodel_reasoning_effort = "low"\n',
                        encoding="utf-8",
                    )
                    configuration.chmod(0o600)
                    fixture = json.loads(scenario.read_text(encoding="utf-8"))
                    for step in fixture["steps"]:
                        if step.get("method") == "model/list":
                            step["respond"]["result"]["data"].append({
                                "model": "gpt-5.6-luna",
                                "isDefault": False,
                                "supportedReasoningEfforts": [
                                    {"reasoningEffort": "medium"},
                                    {"reasoningEffort": "low"},
                                ],
                            })
                    scenario.write_text(json.dumps(fixture), encoding="utf-8")
                transcript = root / f"app-server-transcript-{index}.jsonl"
                transcripts.append(transcript)
                codex = case_bin / "codex"
                native_codex.create_native_codex(
                    codex,
                    scenario=scenario,
                    codex_home=case_home,
                    schema_source=schema_source,
                    transcript=transcript,
                )
                codex_stat = codex.stat()
                codex_sha256 = hashlib.sha256(codex.read_bytes()).hexdigest()
                added, added_envelope = invoke(
                    binary,
                    home,
                    [
                        "profile", "add", profile,
                        "--codex-home", str(case_home), "--native-subagents", "enabled",
                        "--env", "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
                        "--env", "LANG=en_US.UTF-8", "--env", "LC_ALL=en_US.UTF-8",
                        "--", str(codex),
                    ],
                )
                if added.returncode != 0:
                    raise AssertionError(f"profile add failed: {added_envelope!r}")
                arguments = [
                    "specialist", "review", "--workspace", str(workspace),
                    "--profile", profile, "--target-kind", kind,
                    "--format", "json",
                ]
                if revision is not None:
                    arguments.extend(["--revision", revision])

                changed = threading.Event()
                stop = threading.Event()

                def mutate_after_capture() -> None:
                    targets = state_root / "review-targets"
                    known = set(targets.iterdir()) if targets.exists() else set()
                    while not stop.is_set():
                        if targets.exists():
                            for candidate in targets.iterdir():
                                if candidate not in known and (candidate / "source").is_dir():
                                    (workspace / "later-source-change.txt").write_text(
                                        f"changed after capture {index}\n", encoding="utf-8"
                                    )
                                    changed.set()
                                    return
                        time.sleep(0.001)

                watcher = threading.Thread(target=mutate_after_capture, daemon=True)
                watcher.start()
                completed, result_envelope = invoke(binary, home, arguments)
                stop.set()
                watcher.join(timeout=1)
                if completed.returncode != 0:
                    raise AssertionError(f"scoped review failed for {kind}: {result_envelope!r}")
                result = result_envelope["data"]
                assert_valid(result, v2_schema, f"{kind} v2 result")
                assert_valid(result_envelope, machine_schema, f"{kind} Machine result")
                if result["target"]["request"]["kind"] != kind:  # type: ignore[index]
                    raise AssertionError("result lost the exact target kind")
                if result["workflow_issued_source_mutation"] is not False:  # type: ignore[index]
                    raise AssertionError("workflow claimed a source mutation")
                if result["settlement"]["state"] != "settled":  # type: ignore[index]
                    raise AssertionError("authoritative result was not settled")
                if result["engagement"]["id"] != result["review_id"]:  # type: ignore[index]
                    raise AssertionError("result lost its engagement binding")
                review_ids.add(str(result["review_id"]))  # type: ignore[index]
                run_ids.add(str(result["reviewer"]["run_id"]))  # type: ignore[index]
                capture_ref = str(result["target"]["capture_ref"])  # type: ignore[index]
                capture_root = state_root / "review-targets" / capture_ref
                target_result = result["target"]  # type: ignore[index]
                source_identity = result["capture_time_source_identity"]  # type: ignore[index]
                for field in (
                    "resolved_base", "resolved_head", "manifest_digest", "whole_target_digest"
                ):
                    if target_result[field] != source_identity[field]:  # type: ignore[index]
                        raise AssertionError(f"result lost capture identity field {field}")
                settlement = result["settlement"]  # type: ignore[index]
                if (
                    settlement["capture_ref"] != capture_ref  # type: ignore[index]
                    or settlement["capture_revision"] != 2  # type: ignore[index]
                    or settlement["source_bytes_removed"] is not True  # type: ignore[index]
                    or settlement["replay"] is not False  # type: ignore[index]
                ):
                    raise AssertionError("settlement is not bound to the first authoritative capture")
                executable = result["reviewer"]["executable"]  # type: ignore[index]
                identity = executable["file_identity"]  # type: ignore[index]
                if (
                    pathlib.Path(identity["resolved_path"]) != codex.resolve()  # type: ignore[index]
                    or identity["device"] != codex_stat.st_dev  # type: ignore[index]
                    or identity["inode"] != codex_stat.st_ino  # type: ignore[index]
                    or identity["sha256"] != codex_sha256  # type: ignore[index]
                    or executable["sha256"] != codex_sha256  # type: ignore[index]
                ):
                    raise AssertionError("success result is not bound to the exact Reviewer executable")
                messages = [
                    json.loads(line)
                    for line in transcript.read_text(encoding="utf-8").splitlines()
                    if line.strip()
                ]
                starts = [message for message in messages if message.get("method") == "thread/start"]
                if len(starts) != 1:
                    raise AssertionError("Reviewer did not start exactly one thread")
                start_params = starts[0]["params"]
                if index == 0:
                    turns = [message for message in messages if message.get("method") == "turn/start"]
                    if (
                        result["reviewer"]["model"] != "gpt-5.6-luna"
                        or result["reviewer"]["effort"] != "low"
                        or start_params["model"] != "gpt-5.6-luna"
                        or not turns
                        or any(turn["params"]["effort"] != "low" for turn in turns)
                    ):
                        raise AssertionError("Reviewer substituted the default model or effort")
                if (
                    pathlib.Path(start_params["cwd"]) != capture_root / "source"
                    or start_params["sandbox"] != "read-only"
                    or start_params["approvalPolicy"] != "never"
                ):
                    raise AssertionError("Reviewer launch did not bind the immutable read-only target")
                if (capture_root / "source").exists() or (capture_root / ".settlement-source").exists():
                    raise AssertionError("settled capture retained provider-visible bytes")
                if index == 0 and not changed.is_set():
                    raise AssertionError("the later-source-change campaign missed its capture window")
            if len(review_ids) != len(cases) or len(run_ids) != len(cases):
                raise AssertionError("scoped reviews reused an engagement or Reviewer Run")
            transcript_text = "\n".join(
                transcript.read_text(encoding="utf-8") for transcript in transcripts
            )
            if str(workspace) in transcript_text:
                raise AssertionError("Reviewer-visible app-server traffic disclosed the source path")
            if transcript_text.count('"method":"thread/start"') != len(cases):
                raise AssertionError("the fake app-server did not observe one fresh thread per scope")
        finally:
            for profile in profiles:
                invoke(
                    binary,
                    home,
                    [
                        "profile", "server", "stop", profile,
                        "--operator-file", str(operator),
                    ],
                )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--protocol-root", default="docs/protocol", type=pathlib.Path)
    arguments = parser.parse_args()
    validate(arguments.binary.resolve(), arguments.protocol_root.resolve())
    print("scoped specialist review CLI tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
