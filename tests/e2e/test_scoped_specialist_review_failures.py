#!/usr/bin/env python3
"""Failure, preservation, drift, tamper, and cancellation checks for TASK-015."""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import shutil
import signal
import sqlite3
import subprocess
import tempfile
import time

import native_codex
from schema_support import assert_valid, validator


def machine(binary: pathlib.Path, home: pathlib.Path, arguments: list[str]) -> tuple[int, dict[str, object]]:
    environment = os.environ.copy()
    environment["HOME"] = str(home)
    completed = subprocess.run(
        [str(binary), *arguments], check=False, capture_output=True, text=True, env=environment
    )
    if completed.stderr:
        raise AssertionError(f"unexpected stderr for {arguments}: {completed.stderr!r}")
    return completed.returncode, json.loads(completed.stdout)


def start(binary: pathlib.Path, home: pathlib.Path, arguments: list[str]) -> subprocess.Popen[str]:
    environment = os.environ.copy()
    environment["HOME"] = str(home)
    return subprocess.Popen(
        [str(binary), *arguments],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        env=environment,
    )


def git(repository: pathlib.Path, *arguments: str) -> str:
    return subprocess.run(
        ["git", "-C", str(repository), *arguments],
        check=True, capture_output=True, text=True,
    ).stdout.strip()


def wait_for(path_root: pathlib.Path, predicate, process: subprocess.Popen[str]) -> pathlib.Path:
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if process.poll() is not None:
            stdout, stderr = process.communicate()
            raise AssertionError(f"review exited before fault injection: {stdout!r} {stderr!r}")
        if path_root.exists():
            for candidate in path_root.iterdir():
                if predicate(candidate):
                    return candidate
        time.sleep(0.001)
    process.kill()
    raise AssertionError("timed out waiting for the review fault-injection boundary")


def finish(process: subprocess.Popen[str]) -> tuple[int, dict[str, object]]:
    stdout, stderr = process.communicate(timeout=30)
    if stderr:
        raise AssertionError(f"unexpected review stderr: {stderr!r}")
    return process.returncode, json.loads(stdout)


def add_profile(
    binary: pathlib.Path,
    home: pathlib.Path,
    workspace: pathlib.Path,
    root: pathlib.Path,
    schema_source: pathlib.Path,
    name: str,
    scenario_name: str,
) -> pathlib.Path:
    codex_home = root / f"codex-home-{name}"
    bin_root = root / f"bin-{name}"
    codex_home.mkdir(mode=0o700)
    bin_root.mkdir(mode=0o700)
    codex = bin_root / "codex"
    scenario_source = native_codex.scenario_path(scenario_name)
    scenario = root / f"scenario-{name}.json"
    scenario.write_text(
        scenario_source.read_text(encoding="utf-8").replace(
            "thread-timeout", f"thread-{name}"
        ),
        encoding="utf-8",
    )
    native_codex.create_native_codex(
        codex,
        scenario=scenario,
        codex_home=codex_home,
        schema_source=schema_source,
        transcript=root / f"transcript-{name}.jsonl",
    )
    code, envelope = machine(
        binary,
        home,
        [
            "profile", "add", name,
            "--codex-home", str(codex_home), "--native-subagents", "enabled",
            "--env", "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
            "--env", "LANG=en_US.UTF-8", "--env", "LC_ALL=en_US.UTF-8",
            "--", str(codex),
        ],
    )
    if code != 0:
        raise AssertionError(f"profile add failed: {envelope!r}")
    return codex


def review_args(workspace: pathlib.Path, profile: str, deadline: int = 600) -> list[str]:
    return [
        "specialist", "review", "--workspace", str(workspace), "--profile", profile,
        "--target-kind", "workspace", "--deadline-seconds", str(deadline), "--format", "json",
    ]


def assert_failure(
    envelope: dict[str, object],
    code: str,
    settlement: str,
    details_schema,
    *,
    engagement_state: str | None = None,
    required_action: str | None = None,
) -> str:
    error = envelope["error"]  # type: ignore[index]
    if error["code"] != code:  # type: ignore[index]
        raise AssertionError(f"expected {code}, got {error!r}")
    details = error["details"]  # type: ignore[index]
    assert_valid(details, details_schema, f"{code} v2 details")
    if details["settlement_state"] != settlement:  # type: ignore[index]
        raise AssertionError(f"unexpected settlement state: {details!r}")
    if engagement_state is not None and details["engagement_state"] != engagement_state:  # type: ignore[index]
        raise AssertionError(f"unexpected engagement state: {details!r}")
    if required_action is not None and details["required_action"] != required_action:  # type: ignore[index]
        raise AssertionError(f"unexpected required action: {details!r}")
    if "cause_details" not in details:  # type: ignore[operator]
        raise AssertionError(f"failure lost its causal details: {details!r}")
    return str(details["capture_ref"])  # type: ignore[index]


def terminate_owned(root: pathlib.Path) -> None:
    listing = subprocess.run(["ps", "-axo", "pid=,command="], check=True, capture_output=True, text=True)
    own_pid = os.getpid()
    for line in listing.stdout.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        pid_text, _, command = stripped.partition(" ")
        if str(root) not in command:
            continue
        pid = int(pid_text)
        if pid != own_pid:
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass


def validate(binary: pathlib.Path, protocol_root: pathlib.Path) -> None:
    details_schema = validator(protocol_root, "dolgorae-specialist-review-tool-v2.schema.json")
    schema_source = native_codex.installed_codex()
    with tempfile.TemporaryDirectory(prefix="dolgorae-task015-failures-") as temporary:
        root = pathlib.Path(temporary)
        home = root / "home"
        workspace = root / "workspace"
        home.mkdir(mode=0o700)
        workspace.mkdir(mode=0o700)
        git(workspace, "init", "-b", "main")
        git(workspace, "config", "user.name", "Dolgorae E2E")
        git(workspace, "config", "user.email", "dolgorae@example.invalid")
        (workspace / "root.txt").write_text("root\n", encoding="utf-8")
        git(workspace, "add", "root.txt")
        git(workspace, "commit", "-m", "root")
        code, initialized = machine(binary, home, ["init", str(workspace)])
        if code != 0:
            raise AssertionError(f"init failed: {initialized!r}")
        workspace_id = str(initialized["data"]["workspace_id"])  # type: ignore[index]
        targets = home / ".dolgorae" / "workspaces" / workspace_id / "review-targets"
        try:
            drift_codex = add_profile(
                binary, home, workspace, root, schema_source, "drift-reviewer", "scoped_specialist_review.json"
            )
            drift = start(binary, home, review_args(workspace, "drift-reviewer"))
            wait_for(targets, lambda path: path.name.startswith(".verify-"), drift)
            old_codex = drift_codex.with_name("codex.old")
            drift_codex.rename(old_codex)
            shutil.copy2(schema_source, drift_codex)
            drift_codex.chmod(0o755)
            returncode, envelope = finish(drift)
            if returncode == 0:
                raise AssertionError("executable drift unexpectedly succeeded")
            capture_ref = assert_failure(envelope, "REVIEW_EXECUTABLE_DRIFT", "settled", details_schema)
            if (targets / capture_ref / "source").exists():
                raise AssertionError("authoritative pre-launch drift retained source bytes")

            slow_tree = workspace / "post-drift-window"
            slow_tree.mkdir()
            for index in range(1_000):
                (slow_tree / f"{index:04d}.txt").write_text(f"{index}\n", encoding="utf-8")
            post_drift_codex = add_profile(
                binary,
                home,
                workspace,
                root,
                schema_source,
                "post-drift-reviewer",
                "scoped_specialist_review.json",
            )
            post_drift = start(binary, home, review_args(workspace, "post-drift-reviewer"))
            database = targets.parent / "orchestration" / "orchestration.sqlite3"
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                if database.is_file():
                    with sqlite3.connect(database) as connection:
                        state = connection.execute(
                            "SELECT state FROM engagements ORDER BY rowid DESC LIMIT 1"
                        ).fetchone()
                    if state is not None and state[0] == "result_ready":
                        os.kill(post_drift.pid, signal.SIGSTOP)
                        break
                if post_drift.poll() is not None:
                    raise AssertionError("post-turn drift review exited before fault injection")
                time.sleep(0.001)
            else:
                raise AssertionError("post-turn drift review never reached result_ready")
            old_post_drift = post_drift_codex.with_name("codex.old")
            post_drift_codex.rename(old_post_drift)
            shutil.copy2(schema_source, post_drift_codex)
            post_drift_codex.chmod(0o755)
            os.kill(post_drift.pid, signal.SIGCONT)
            returncode, envelope = finish(post_drift)
            if returncode == 0:
                raise AssertionError("post-turn executable drift unexpectedly succeeded")
            capture_ref = assert_failure(
                envelope, "REVIEW_EXECUTABLE_DRIFT", "settled", details_schema
            )
            if (targets / capture_ref / "source").exists():
                raise AssertionError("authoritative post-turn drift retained source bytes")
            shutil.rmtree(slow_tree)

            add_profile(
                binary, home, workspace, root, schema_source, "tamper-reviewer", "scoped_specialist_review.json"
            )
            tamper = start(binary, home, review_args(workspace, "tamper-reviewer"))
            capture_root = wait_for(
                targets,
                lambda path: not path.name.startswith(".") and (path / "source" / "current" / "root.txt").is_file(),
                tamper,
            )
            target_file = capture_root / "source" / "current" / "root.txt"
            target_file.chmod(0o644)
            target_file.write_text("tampered\n", encoding="utf-8")
            returncode, envelope = finish(tamper)
            if returncode == 0:
                raise AssertionError("capture tampering unexpectedly succeeded")
            capture_ref = assert_failure(
                envelope,
                "REVIEW_TARGET_MUTATED",
                "preserved",
                details_schema,
                engagement_state="result_ready",
                required_action="inspect_authority",
            )
            if not (targets / capture_ref / "source").exists():
                raise AssertionError("tampered capture recovery evidence was removed")

            add_profile(
                binary, home, workspace, root, schema_source, "timeout-reviewer", "scoped_specialist_review_timeout.json"
            )
            code, envelope = machine(binary, home, review_args(workspace, "timeout-reviewer", 1))
            if code == 0:
                raise AssertionError("unknown timeout unexpectedly succeeded")
            capture_ref = assert_failure(
                envelope,
                "REVIEW_TIMEOUT",
                "preserved",
                details_schema,
                engagement_state="interrupted_unknown",
                required_action="inspect_authority",
            )
            if not (targets / capture_ref / "source").exists():
                raise AssertionError("unknown timeout removed recovery bytes")

            add_profile(
                binary, home, workspace, root, schema_source, "cancel-reviewer", "scoped_specialist_review_timeout.json"
            )
            known_targets = set(targets.iterdir())
            cancelled = start(binary, home, review_args(workspace, "cancel-reviewer", 30))
            wait_for(
                targets,
                lambda path: path not in known_targets
                and not path.name.startswith(".")
                and (path / "source").is_dir(),
                cancelled,
            )
            database = targets.parent / "orchestration" / "orchestration.sqlite3"
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                if database.is_file():
                    with sqlite3.connect(database) as connection:
                        state = connection.execute(
                            "SELECT state FROM engagements ORDER BY rowid DESC LIMIT 1"
                        ).fetchone()
                    if state is not None and state[0] == "executing":
                        break
                if cancelled.poll() is not None:
                    raise AssertionError("cancellation review exited before task acceptance")
                time.sleep(0.01)
            else:
                raise AssertionError("cancellation review never reached executing authority")
            cancelled.send_signal(signal.SIGINT)
            returncode, envelope = finish(cancelled)
            if returncode == 0:
                raise AssertionError("explicit cancellation unexpectedly succeeded")
            capture_ref = assert_failure(
                envelope,
                "REVIEW_CANCELLED",
                "preserved",
                details_schema,
                engagement_state="interrupted_unknown",
                required_action="inspect_authority",
            )
            if not (targets / capture_ref / "source").exists():
                raise AssertionError("cancellation removed unknown recovery bytes")
        finally:
            terminate_owned(root)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=pathlib.Path)
    parser.add_argument("--protocol-root", default="docs/protocol", type=pathlib.Path)
    arguments = parser.parse_args()
    validate(arguments.binary.resolve(), arguments.protocol_root.resolve())
    print("scoped specialist review failure tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
