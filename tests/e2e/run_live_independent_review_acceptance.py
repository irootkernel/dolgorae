#!/usr/bin/env python3
"""Opt-in TASK-060 v3 campaign against Codex 0.157.1."""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import time

import test_one_shot_recovery_cli as recovery
from run_live_primary_bridge import checked_path
from schema_support import assert_valid, validator
from test_independent_review_acceptance import checked_review, fill_pipe, request, source_identity

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "validators"))
from validate_agent_skills import validate_installed_resources

OPT_IN = "DOLGORAE_RUN_LIVE_INDEPENDENT_REVIEW"
MINIMUM = (0, 157, 1)


def digest(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def executable(path: pathlib.Path) -> dict[str, object]:
    selected = path.resolve(strict=True)
    version = subprocess.run(
        [str(selected), "--version"], check=True, capture_output=True, text=True, timeout=30,
    ).stdout.strip()
    match = re.fullmatch(r"codex-cli ([0-9]+)\.([0-9]+)\.([0-9]+)", version)
    if match is None:
        raise ValueError("selected executable did not report a supported codex-cli version")
    stat = selected.stat()
    return {
        "path": str(selected), "version": tuple(map(int, match.groups())),
        "device": stat.st_dev, "inode": stat.st_ino, "sha256": digest(selected),
    }


def environment(root: pathlib.Path) -> dict[str, str]:
    env = {
        key: value for key, value in os.environ.items()
        if key in {"PATH", "LANG", "LC_ALL", "TERM"}
    }
    env.update(
        HOME=str(root / "home"), TMPDIR=str(root / "tmp"),
        XDG_CONFIG_HOME=str(root / "config"), XDG_CACHE_HOME=str(root / "cache"),
    )
    return env


def run_machine(binary: pathlib.Path, env: dict[str, str], args: list[str],
                body: dict[str, object] | None = None, timeout: int = 60) -> tuple[int, dict[str, object]]:
    completed = subprocess.run(
        [str(binary), *args], input=json.dumps(body) if body is not None else None,
        capture_output=True, text=True, env=env, timeout=timeout,
    )
    if completed.stderr:
        raise RuntimeError(f"CLI wrote stderr for {args[:2]}: {completed.stderr[:500]}")
    return completed.returncode, json.loads(completed.stdout)


def campaign(binary: pathlib.Path, binary_hash: str, selected: dict[str, object],
             auth_file: pathlib.Path) -> dict[str, object]:
    with tempfile.TemporaryDirectory(prefix="dolgorae-independent-live-") as temporary:
        root = pathlib.Path(temporary).resolve()
        for name in ("home", "tmp", "config", "cache", "workspace"):
            (root / name).mkdir(mode=0o700)
        workspace = root / "workspace"
        env = environment(root)
        shared, owned = "live-shared", "live-temporary"
        codex_homes = {name: root / f"codex-home-{name}" for name in (shared, owned)}
        for codex_home in codex_homes.values():
            codex_home.mkdir(mode=0o700)
            with auth_file.open("rb") as source, os.fdopen(
                os.open(codex_home / "auth.json", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "wb"
            ) as destination:
                shutil.copyfileobj(source, destination)
        installed = root / "installed-skill"
        subprocess.run(
            [sys.executable, str(ROOT / "tools/validators/package_agent_skill.py"),
             "install", "--destination", str(installed)],
            cwd=root, check=True, capture_output=True, timeout=30, env=env,
        )
        validate_installed_resources(installed)
        protocol = installed / "resources" / "protocol"
        machine_schema = validator(protocol, "dolgorae-machine-v2.schema.json")
        observation_schema = validator(protocol, "dolgorae-one-shot-review-observation-v1.schema.json")
        review_schema = validator(protocol, "dolgorae-specialist-review-tool-v3.schema.json",
                                  "#/$defs/review_result")
        assert_valid(request(), validator(protocol, "dolgorae-specialist-review-tool-v3.schema.json",
                                          "#/$defs/review_request"), "live v3 request")

        def checked(args: list[str], body: dict[str, object] | None = None,
                    timeout: int = 60) -> tuple[int, dict[str, object]]:
            code, envelope = run_machine(binary, env, args, body, timeout)
            assert_valid(envelope, machine_schema, "live CLI envelope")
            return code, envelope

        def success(args: list[str], body: dict[str, object] | None = None,
                    timeout: int = 60) -> dict[str, object]:
            code, envelope = checked(args, body, timeout)
            if code != 0:
                error = envelope["error"]
                reason = error.get("details", {}).get("reason", error.get("details", error["message"]))
                raise RuntimeError(f"live command failed: {args[:2]} {error['code']}: {reason}")
            return envelope["data"]

        def inspect(reference: str) -> dict[str, object]:
            observed = success(recovery.recovery_arguments(workspace, reference))
            assert_valid(observed, observation_schema, "live public observation")
            return observed

        subprocess.run(["git", "init", "-b", "main", str(workspace)],
                       check=True, capture_output=True, text=True, env=env, timeout=30)
        subprocess.run(["git", "-C", str(workspace), "config", "user.name", "Dolgorae E2E"],
                       check=True, capture_output=True, env=env, timeout=30)
        subprocess.run(["git", "-C", str(workspace), "config", "user.email",
                        "dolgorae@example.invalid"], check=True, capture_output=True,
                       env=env, timeout=30)
        (workspace / "hello.py").write_text('print("Hello world!")\n')
        subprocess.run(["git", "-C", str(workspace), "add", "hello.py"],
                       check=True, capture_output=True, env=env, timeout=30)
        subprocess.run(["git", "-C", str(workspace), "commit", "-m", "defective hello"],
                       check=True, capture_output=True, env=env, timeout=30)

        operator = root / "operator.json"
        shared_key = None
        try:
            success(["init", str(workspace)])
            controller = root / "owner-controller"
            success(["controller", "credential", "create", "--kind", "automation",
                     "--instance-id", controller.name, "--output", str(controller)])
            wrong_controller = root / "wrong-controller"
            success(["controller", "credential", "create", "--kind", "automation",
                     "--instance-id", wrong_controller.name, "--output", str(wrong_controller)])
            success(["operator", "credential", "initialize", "--output", str(operator)])
            before = source_identity(workspace)
            for profile in (shared, owned):
                success([
                    "profile", "add", profile, "--codex-home", str(codex_homes[profile]),
                    "--native-subagents", "enabled",
                    "--env", f"PATH={checked_path(env.get('PATH', '/usr/bin:/bin'))}",
                    "--env", f"LANG={env.get('LANG', 'en_US.UTF-8')}",
                    "--env", f"LC_ALL={env.get('LC_ALL', 'en_US.UTF-8')}",
                    "--", str(selected["path"]),
                ], timeout=900)
            shared_start = success(["profile", "server", "start", shared], timeout=900)
            shared_key = shared_start["state"]["server_key"]
            shared_epoch = shared_start["state"]["server_epoch"]

            def check_result(result: dict[str, object]) -> None:
                try:
                    checked_review(result, review_schema)
                except AssertionError as error:
                    verdict = result.get("verdict", {})
                    assessments = verdict.get("criterion_assessments", [])
                    shape = {
                        "overall_assessment": verdict.get("overall_assessment"),
                        "criteria": [{
                            "id": item.get("criterion_id"), "status": item.get("status"),
                            "evidence": [{key: entry.get(key) for key in
                                          ("basis", "path", "line_start", "line_end")}
                                         for entry in item.get("evidence", [])],
                        } for item in assessments],
                    }
                    raise AssertionError(f"{error}: {json.dumps(shape, sort_keys=True)[:1500]}") from error
                actual = result["reviewer"]["executable"]
                identity = actual["file_identity"]
                if (
                    actual["version"] != ".".join(map(str, selected["version"]))
                    or actual["sha256"] != selected["sha256"]
                    or any(identity[key] != selected[key]
                           for key in ("device", "inode", "sha256"))
                    or pathlib.Path(identity["resolved_path"]) != pathlib.Path(selected["path"])
                ):
                    raise AssertionError("review result did not bind the selected Codex executable")

            def one_turn(result: dict[str, object]) -> None:
                run_id = result["reviewer"]["run_id"]
                command = [str(binary), "run", "events", run_id, "--workspace",
                           str(workspace), "--after", "0"]
                completed = subprocess.run(command, check=True, capture_output=True,
                                           text=True, env=env, timeout=60)
                pages = [json.loads(line) for line in completed.stdout.splitlines()]
                assert all(page["ok"] for page in pages)
                records = [page["data"]["record"] for page in pages
                           if page["data"]["kind"] == "event"]
                turns = {record["turn_id"] for record in records
                         if record["type"] == "turn.state_changed"}
                if len(turns) != 1 or None in turns:
                    raise AssertionError("public run events did not establish exactly one Turn")

            shared_ref = recovery.reference(610)
            shared_args = recovery.review_arguments(workspace, shared, shared_ref, controller)
            shared_args.remove("--temporary-server")
            normal = success(shared_args, request(), timeout=900)
            check_result(normal)
            one_turn(normal)
            normal_observation = inspect(shared_ref)
            if normal_observation["result"] != normal or normal_observation["server"]["status"] != "preexisting":
                raise AssertionError("normal review did not retain its result and shared server")

            lost_ref = recovery.reference(611)
            lost_args = recovery.review_arguments(workspace, owned, lost_ref, controller)
            read_fd, write_fd = os.pipe()
            process = None
            try:
                fill_pipe(write_fd)
                process = subprocess.Popen(
                    [str(binary), *lost_args], stdin=subprocess.PIPE, stdout=write_fd,
                    stderr=subprocess.PIPE, text=True, env=env,
                )
                os.close(write_fd)
                write_fd = -1
                process.stdin.write(json.dumps(request()))
                process.stdin.close()
                process.stdin = None
                deadline = time.monotonic() + 900
                while True:
                    if process.poll() is not None:
                        raise AssertionError("original CLI exited before response-loss injection")
                    observed = inspect(lost_ref)
                    if (
                        observed["outcome"] == "succeeded" and observed["result"] is not None
                        and observed["reviewer"]["state"] == "closed"
                        and observed["engagement"]["state"] == "closed"
                        and observed["capture"]["state"] == "settled"
                        and observed["server"]["status"] == "retired"
                    ):
                        break
                    if time.monotonic() >= deadline:
                        raise TimeoutError("live review did not finish before response loss")
                    time.sleep(0.25)
                process.kill()
                process.communicate(timeout=10)
                if process.returncode != -9:
                    raise AssertionError("original CLI was not the terminated process")
            finally:
                if process is not None and process.poll() is None:
                    process.kill()
                    process.communicate(timeout=10)
                os.close(read_fd)
                if write_fd != -1:
                    os.close(write_fd)
            restored = inspect(lost_ref)
            if restored != observed:
                raise AssertionError("fresh public lookup changed the original result")
            check_result(restored["result"])
            one_turn(restored["result"])
            if success(recovery.recovery_arguments(workspace, lost_ref, controller)) != restored:
                raise AssertionError("authorized cleanup changed the completed result")
            code, refused = checked(recovery.recovery_arguments(workspace, lost_ref, wrong_controller))
            if code != 4 or refused["error"]["code"] != "REVIEW_RECOVERY_BLOCKED":
                raise AssertionError("wrong Controller acquired recovery authority")
            unknown = inspect(recovery.reference(612))
            if unknown["observation"] != "unknown" or unknown["result"] is not None:
                raise AssertionError("unknown reference invented a completed review")
            if source_identity(workspace) != before:
                raise AssertionError("live review changed source or Git state")
            shared_status = success(["profile", "server", "status", shared])
            owned_status = success(["profile", "server", "status", owned])
            if (
                shared_status["lifecycle"] != "ready"
                or shared_status["state"] is None
                or shared_status["state"]["server_epoch"] != shared_epoch
                or owned_status["lifecycle"] != "stopped"
            ):
                raise AssertionError("temporary cleanup affected the shared server")
            if digest(binary) != binary_hash or digest(pathlib.Path(selected["path"])) != selected["sha256"]:
                raise AssertionError("candidate or Codex executable changed during campaign")
            return {
                "version": ".".join(map(str, selected["version"])),
                "codex_sha256": selected["sha256"],
                "capability_result": restored["result"]["reviewer"]["executable"]["capability_result"],
                "candidate_sha256": binary_hash,
                "normal_result_sha256": hashlib.sha256(json.dumps(normal, sort_keys=True).encode()).hexdigest(),
                "lost_result_sha256": hashlib.sha256(json.dumps(restored["result"], sort_keys=True).encode()).hexdigest(),
                "source_head": before[0], "source_sha256": before[2],
            }
        finally:
            if shared_key is not None:
                with contextlib.suppress(Exception):
                    success([
                        "profile", "server", "stop", shared, "--operator-file", str(operator),
                        "--interrupt", "--confirm-server-key", str(shared_key),
                    ], timeout=90)
            recovery.terminate_owned(root)
            for path in (
                *(home / "auth.json" for home in codex_homes.values()),
                operator, root / "owner-controller", root / "wrong-controller",
            ):
                with contextlib.suppress(FileNotFoundError):
                    path.unlink()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--codex-minimum", required=True, type=pathlib.Path)
    parser.add_argument("--auth-file", required=True, type=pathlib.Path)
    arguments = parser.parse_args()
    if os.environ.get(OPT_IN) != "1":
        parser.error(f"{OPT_IN}=1 is required")
    try:
        minimum = executable(arguments.codex_minimum)
    except (OSError, ValueError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
        parser.error(f"selected path or Codex version is invalid: {error}")
    if minimum["version"] != MINIMUM:
        parser.error("select Codex 0.157.1")
    try:
        binary = arguments.binary.resolve(strict=True)
        auth_file = arguments.auth_file.expanduser().resolve(strict=True)
    except OSError as error:
        parser.error(f"candidate binary or credential path is invalid: {error}")
    binary_hash = digest(binary)
    if not auth_file.is_file():
        parser.error("--auth-file must identify a regular file")
    print(json.dumps(campaign(binary, binary_hash, minimum, auth_file), sort_keys=True), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
