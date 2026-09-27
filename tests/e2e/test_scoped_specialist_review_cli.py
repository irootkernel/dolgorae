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
    *,
    stdin: str | None = None,
) -> tuple[subprocess.CompletedProcess[str], dict[str, object]]:
    environment = os.environ.copy()
    environment["HOME"] = str(home)
    completed = subprocess.run(
        [str(binary), *arguments],
        check=False,
        capture_output=True,
        text=True,
        input=stdin,
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
    v3_schema = validator(protocol_root, "dolgorae-specialist-review-tool-v3.schema.json")
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
                if index < 3:
                    configuration = case_home / "config.toml"
                    settings = [
                        'model = "gpt-5.6-luna"\nmodel_reasoning_effort = "low"\n',
                        'model = "gpt-5.6-luna"\n',
                        'model_reasoning_effort = "low"\n',
                    ]
                    configuration.write_text(
                        settings[index],
                        encoding="utf-8",
                    )
                    configuration.chmod(0o600)
                fixture = json.loads(scenario.read_text(encoding="utf-8"))
                for step in fixture["steps"]:
                    if step.get("method") == "model/list":
                        models = step["respond"]["result"]["data"]
                        if index < 2:
                            models.append({
                                "model": "gpt-5.6-luna",
                                "isDefault": False,
                                "supportedReasoningEfforts": [
                                    {"reasoningEffort": "medium"},
                                    {"reasoningEffort": "low"},
                                    {"reasoningEffort": "high"},
                                ],
                            })
                        if index % 2:
                            models.reverse()
                        for model in models:
                            efforts = model["supportedReasoningEfforts"]
                            offset = index % len(efforts)
                            model["supportedReasoningEfforts"] = efforts[offset:] + efforts[:offset]
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
                expected_model = "gpt-5.6-luna" if index < 2 else "gpt-6-sol"
                expected_effort = "low" if index in (0, 2) else "high"
                turns = [message for message in messages if message.get("method") == "turn/start"]
                if (
                    result["reviewer"]["model"] != expected_model
                    or result["reviewer"]["effort"] != expected_effort
                    or start_params["model"] != expected_model
                    or len(turns) != 1
                    or turns[0]["params"]["effort"] != expected_effort
                ):
                    raise AssertionError("Reviewer changed an explicit setting or used provider ordering for an omitted default")
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

            for index, kind in enumerate(("staged", "head")):
                profile = f"task040-reviewer-{index}"
                profiles.append(profile)
                case_home = codex_home / f"v3-{index}"
                case_bin = bin_root / f"v3-{index}"
                case_home.mkdir(mode=0o700)
                case_bin.mkdir(mode=0o700)
                scenario = root / f"scenario-v3-{index}.json"
                scenario.write_text(
                    native_codex.scenario_path("scoped_specialist_review_v3.json")
                    .read_text(encoding="utf-8")
                    .replace("thread-scope-v3", f"thread-scope-v3-{index}"),
                    encoding="utf-8",
                )
                transcript = root / f"app-server-transcript-v3-{index}.jsonl"
                codex = case_bin / "codex"
                native_codex.create_native_codex(
                    codex,
                    scenario=scenario,
                    codex_home=case_home,
                    schema_source=schema_source,
                    transcript=transcript,
                )
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
                    raise AssertionError(f"v3 profile add failed: {added_envelope!r}")

                brief = "완료 기준을 검토하세요.\n\n```sh\nprintf '$HOME'; $(touch should-not-run)\n```"
                context_content = '문맥 첫 줄\r\n"quoted" 둘째 줄'
                request = {
                    "schema": "dolgorae-specialist-review-request/v3",
                    "operation": "review_target",
                    "target": {"kind": kind},
                    "purpose": "completion",
                    "brief": brief,
                    "contexts": [{
                        "id": "acceptance",
                        "content": context_content,
                        "provenance": "caller supplied requirements",
                    }],
                    "criteria": [
                        {
                            "id": "C-korean",
                            "statement": "캡처된 후보를 검토한다.",
                            "source_context_ids": [],
                        },
                        {
                            "id": "C-context",
                            "statement": "승인된 문맥의 한계를 명시한다.",
                            "source_context_ids": ["acceptance"],
                        },
                    ],
                    "expected_output": "structured_review_v3",
                    "deadline_seconds": 60,
                }
                completed, result_envelope = invoke(
                    binary,
                    home,
                    [
                        "specialist", "review", "--workspace", str(workspace),
                        "--profile", profile, "--request-stdin", "--format", "json",
                    ],
                    stdin=json.dumps(request, ensure_ascii=False),
                )
                if completed.returncode != 0:
                    raise AssertionError(f"v3 scoped review failed for {kind}: {result_envelope!r}")
                result = result_envelope["data"]
                assert_valid(result, v3_schema, f"{kind} v3 result")
                assert_valid(result_envelope, machine_schema, f"{kind} v3 Machine result")
                if (
                    result["schema"] != "dolgorae-specialist-review-result/v3"  # type: ignore[index]
                    or result["target"]["request"] != {"kind": kind}  # type: ignore[index]
                    or result["verdict"]["overall_assessment"] != "insufficient_evidence"  # type: ignore[index]
                    or [assessment["criterion_id"] for assessment in result["verdict"]["criterion_assessments"]]  # type: ignore[index]
                    != ["C-korean", "C-context"]
                ):
                    raise AssertionError("v3 result lost target or ordered criterion assessment")

                messages = [
                    json.loads(line)
                    for line in transcript.read_text(encoding="utf-8").splitlines()
                    if line.strip()
                ]
                starts = [message for message in messages if message.get("method") == "thread/start"]
                turns = [message for message in messages if message.get("method") == "turn/start"]
                if len(starts) != 1 or len(turns) != 1:
                    raise AssertionError("v3 review did not use exactly one fresh Reviewer Turn")
                if (
                    result["reviewer"]["model"] != "gpt-6-sol"
                    or result["reviewer"]["effort"] != "high"
                    or starts[0]["params"]["model"] != "gpt-6-sol"
                    or turns[0]["params"]["effort"] != "high"
                ):
                    raise AssertionError("v3 review did not use the independent one-shot defaults")
                prompt = "\n".join(
                    item["text"] for item in turns[0]["params"]["input"]
                    if item.get("type") == "text"
                )
                schemas = [
                    json.loads(line) for line in prompt.splitlines()
                    if line.startswith('{"additionalProperties":')
                ]
                if len(schemas) != 1:
                    raise AssertionError("v3 provider prompt omitted its complete output schema")
                contract = schemas[0]
                v1_contract = json.loads((protocol_root / "dolgorae-specialist-review-tool-v1.schema.json").read_text())
                v3_contract = json.loads((protocol_root / "dolgorae-specialist-review-tool-v3.schema.json").read_text())
                assessment = contract["properties"]["criterion_assessments"]["items"]
                if (
                    contract["required"] != v3_contract["$defs"]["verdict"]["required"]
                    or contract["properties"]["findings"]["items"] != v1_contract["$defs"]["finding"]
                    or assessment["required"] != v3_contract["$defs"]["criterion_assessment"]["required"]
                    or assessment["properties"]["evidence"]["items"] != v3_contract["$defs"]["assessment_evidence"]
                    or '"$ref"' in json.dumps(contract)
                ):
                    raise AssertionError("v3 provider prompt lost nested fields or retained unresolved schema dependencies")
                role_text = starts[0]["params"]["developerInstructions"]
                turn_text = json.dumps(turns[0]["params"]["input"], ensure_ascii=False)
                if any(marker in role_text for marker in (brief, context_content, "C-korean")):
                    raise AssertionError("accepted task leaked into stable Reviewer Role instructions")
                for marker in (
                    "완료 기준을 검토하세요.",
                    "$HOME",
                    "$(touch should-not-run)",
                    "문맥 첫 줄",
                    "quoted",
                    "둘째 줄",
                    "C-korean",
                    "C-context",
                ):
                    if marker not in turn_text:
                        raise AssertionError(f"v3 Turn lost accepted task marker: {marker!r}")
                if (workspace / "should-not-run").exists():
                    raise AssertionError("shell metacharacters in the task were executed")
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
