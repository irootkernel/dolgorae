#!/usr/bin/env python3
"""Static barriers for the opt-in TASK-026 live provider campaign."""

from __future__ import annotations

import ast
import hashlib
import json
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path

import run_live_provider_acceptance as driver
from run_live_provider_acceptance import (
    MAXIMUM_RESULT_BYTES,
    MAXIMUM_TOOL_RESULT_ENVELOPE_BYTES,
    MAXIMUM_TOOL_RESULT_ROWS,
    RESULT_PAGE_BYTES,
    assert_primary_consumed_result,
    client,
    read_gateway_ready_line,
    stop_profile_server,
    tool_results,
)


ROOT = Path(__file__).resolve().parents[2]
DRIVER = ROOT / "tests/e2e/run_live_provider_acceptance.py"


def main() -> int:
    source = DRIVER.read_text(encoding="utf-8")
    ast.parse(source)
    required = (
        'OPT_IN = "DOLGORAE_RUN_LIVE_PROVIDER_ACCEPTANCE"',
        'PINNED_CODEX_VERSION = "codex-cli 0.158.0"',
        'CAMPAIGN_TEMP_ROOT = Path("/private/tmp")',
        "GATEWAY_READY_TIMEOUT_SECONDS = 15",
        "GATEWAY_READY_MAXIMUM_BYTES = 65_536",
        '"user_approval_required"',
        '"fully_delegated"',
        '"request_specialist_result"',
        '"assign_specialist_task_result"',
        '"read_specialist_result_result"',
        '"--minimum-result-bytes"',
        '"shared_profile_mutated": False',
        'source_auth = source_codex_home / "auth.json"',
        'controller_root.mkdir(parents=True, mode=0o700)',
        'controller_client_directory.mkdir(mode=0o700)',
        'controller_directory.mkdir(mode=0o700)',
        ").resolve(strict=True)",
    )
    missing = [fragment for fragment in required if fragment not in source]
    if missing:
        raise AssertionError(f"live provider driver lost required barriers: {missing!r}")
    original_campaign_root = driver.CAMPAIGN_TEMP_ROOT
    with tempfile.TemporaryDirectory() as temporary:
        driver.CAMPAIGN_TEMP_ROOT = Path(temporary)
        try:
            try:
                with driver.campaign_root("failure") as preserved:
                    credentials = (
                        preserved / "codex-home/auth.json",
                        preserved / "operator.json",
                        preserved / "home/.dolgorae/controller-carriers/task026/failure/controller.json",
                    )
                    for credential in credentials:
                        credential.parent.mkdir(parents=True, exist_ok=True)
                        credential.write_text("fixture credential", encoding="utf-8")
                    raise RuntimeError("fixture failure")
            except RuntimeError as error:
                if str(error) != "fixture failure":
                    raise
            else:
                raise AssertionError("failed campaign unexpectedly succeeded")
            if not preserved.is_dir() or any(path.exists() for path in credentials):
                raise AssertionError("failed campaign did not preserve only noncredential evidence")
        finally:
            driver.CAMPAIGN_TEMP_ROOT = original_campaign_root
    content = "provider-result"
    digest = hashlib.sha256(content.encode("utf-8")).hexdigest()
    page = {
        "operation": "read_specialist_result_result",
        "task_id": "fixture-task",
        "offset": 0,
        "length": len(content),
        "sha256": digest,
        "content": content,
        "truncated": False,
    }
    operations = [
        {"operation": operation}
        for operation in (
            "request_specialist_result",
            "await_specialist_operations_result",
            "list_specialists_result",
            "assign_specialist_task_result",
            "await_specialist_tasks_result",
            "collect_specialist_results_result",
        )
    ]
    assert_primary_consumed_result([*operations, page], 1)
    corrupted = dict(page, content="provider-resulU")
    try:
        assert_primary_consumed_result([*operations, corrupted], 1)
    except RuntimeError as error:
        if str(error) != "live Primary result page content failed digest verification":
            raise
    else:
        raise AssertionError("corrupted Primary-visible content passed digest verification")
    oversized_content = "A" * (RESULT_PAGE_BYTES + 1)
    oversized_page = dict(
        page,
        length=len(oversized_content),
        sha256=hashlib.sha256(oversized_content.encode("utf-8")).hexdigest(),
        content=oversized_content,
    )
    try:
        assert_primary_consumed_result([*operations, oversized_page], 1)
    except RuntimeError as error:
        if str(error) != "live Primary result page exceeded the requested bound":
            raise
    else:
        raise AssertionError("oversized Primary-visible page bypassed its bound")
    over_total = dict(page, length=MAXIMUM_RESULT_BYTES + 1)
    try:
        assert_primary_consumed_result([*operations, over_total], 1)
    except RuntimeError as error:
        if str(error) != "live Primary result exceeded the artifact bound":
            raise
    else:
        raise AssertionError("oversized Primary-visible result bypassed its bound")
    first_content = "A" * RESULT_PAGE_BYTES
    second_content = "B"
    paged_content = first_content + second_content
    paged_digest = hashlib.sha256(paged_content.encode("utf-8")).hexdigest()
    paged = [
        dict(
            page,
            offset=0,
            length=len(paged_content),
            sha256=paged_digest,
            content=first_content,
            truncated=True,
        ),
        dict(
            page,
            offset=RESULT_PAGE_BYTES,
            length=len(paged_content),
            sha256=paged_digest,
            content=second_content,
            truncated=False,
        ),
    ]
    accepted = assert_primary_consumed_result(
        [*operations, *paged], RESULT_PAGE_BYTES + 1
    )
    if accepted["pages"] != 2:
        raise AssertionError("valid above-bound result did not preserve both pages")
    repeated = assert_primary_consumed_result(
        [*operations, paged[0], dict(paged[0]), paged[1]], RESULT_PAGE_BYTES + 1
    )
    if repeated != accepted:
        raise AssertionError("identical repeated page changed the verified result")
    for conflicting in (
        dict(paged[0], content="C" * RESULT_PAGE_BYTES),
        dict(paged[0], length=len(paged_content) + 1),
        dict(paged[0], sha256="0" * 64),
        dict(paged[0], truncated=False),
        dict(paged[0], task_id="other-task"),
    ):
        try:
            assert_primary_consumed_result([*operations, *paged, conflicting], 1)
        except RuntimeError as error:
            if str(error) != "live Primary returned conflicting repeated result pages":
                raise
        else:
            raise AssertionError("conflicting repeated page passed verification")
    for invalid_pages in (
        [paged[1]],
        [paged[0], dict(paged[1], task_id="other-task")],
    ):
        try:
            assert_primary_consumed_result([*operations, *invalid_pages], 1)
        except RuntimeError as error:
            if str(error) != "live Primary result pages are not one contiguous immutable result":
                raise
        else:
            raise AssertionError("gapped or mixed-task result passed verification")
    try:
        client(
            Path(sys.executable),
            "-c",
            ["import time; time.sleep(2)"],
            [],
            cwd=ROOT,
            env={},
            timeout=0.05,
        )
    except RuntimeError as error:
        if str(error) != "generated client -c timed out":
            raise
    else:
        raise AssertionError("generated client timeout was not redacted")
    original_run = driver.run
    driver.run = lambda *args, **kwargs: subprocess.CompletedProcess(
        args=args, returncode=7, stdout='{"error":{"code":"PROFILE_SERVER_BUSY"}}'
    )
    try:
        try:
            stop_profile_server(
                Path("/private/tmp/dolgorae"),
                "profile",
                Path("/private/tmp/operator"),
                "server-key",
                workspace=ROOT,
                env={},
            )
        except RuntimeError as error:
            if str(error) != "Profile Server cleanup failed (PROFILE_SERVER_BUSY)":
                raise
        else:
            raise AssertionError("failed Profile Server cleanup was accepted")
    finally:
        driver.run = original_run
    with tempfile.TemporaryDirectory() as temporary:
        state_root = Path(temporary)
        database = state_root / "orchestration/orchestration.sqlite3"
        database.parent.mkdir()
        connection = sqlite3.connect(database)
        connection.execute("CREATE TABLE brokered_tool_results (response_json TEXT, created_at_ms INTEGER, source_tool_call_id TEXT)")
        envelope = json.dumps(
            {
                "schema_version": "dolgorae.brokered-tool-result/v1",
                "outcome": "ok",
                "value": {"operation": "bounded"},
            }
        )
        connection.executemany(
            "INSERT INTO brokered_tool_results VALUES (?, ?, ?)",
            [
                (envelope, index, f"call-{index}")
                for index in range(MAXIMUM_TOOL_RESULT_ROWS + 1)
            ],
        )
        connection.commit()
        connection.close()
        try:
            tool_results(state_root)
        except RuntimeError as error:
            if str(error) != "live Primary produced too many tool results":
                raise
        else:
            raise AssertionError("tool-result row bound was not enforced")
        connection = sqlite3.connect(database)
        connection.execute("DELETE FROM brokered_tool_results")
        connection.execute(
            "INSERT INTO brokered_tool_results VALUES (?, 0, 'oversized')",
            ("x" * (MAXIMUM_TOOL_RESULT_ENVELOPE_BYTES + 1),),
        )
        connection.commit()
        connection.close()
        try:
            tool_results(state_root)
        except RuntimeError as error:
            if str(error) != "live Primary tool result exceeded the envelope bound":
                raise
        else:
            raise AssertionError("tool-result envelope bound was not enforced")
    partial = subprocess.Popen(
        [
            sys.executable,
            "-c",
            "import sys,time;sys.stdout.write('{');sys.stdout.flush();time.sleep(2)",
        ],
        stdout=subprocess.PIPE,
        text=True,
    )
    try:
        try:
            read_gateway_ready_line(partial, timeout=0.05)
        except RuntimeError as error:
            if str(error) != "gateway readiness timed out":
                raise
        else:
            raise AssertionError("partial gateway readiness bypassed its deadline")
    finally:
        partial.kill()
        partial.wait(timeout=5)
    print("live provider acceptance driver contract passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
