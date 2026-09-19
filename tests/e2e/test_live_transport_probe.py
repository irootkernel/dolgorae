#!/usr/bin/env python3
"""Offline probe-decision regressions and TASK-025 contract reference checks.

These checks do not start Codex or the production assignment/wait/result path.
Reader cases are an independent response oracle that TASK-051 can also use with
actual black-box responses.
"""

from __future__ import annotations

import copy
import hashlib
import json
import os
import subprocess
import sys
import unittest
from pathlib import Path

from jsonschema import Draft202012Validator, FormatChecker

import run_live_transport_probe as probe

ROOT = Path(__file__).resolve().parents[2]
PROTOCOL = ROOT / "docs" / "protocol"
SCRIPT = Path(probe.__file__).resolve()
SCHEMA = json.loads((PROTOCOL / "dolgorae-orchestration-tool-v1.schema.json").read_text())
VALIDATOR = Draft202012Validator(SCHEMA, format_checker=FormatChecker())
TASK_ID = "018f0000-0000-7000-8000-0000000000aa"


def request(offset: int, limit: int) -> dict:
    return {
        "operation": "read_specialist_result",
        "task_id": TASK_ID,
        "offset": offset,
        "limit": limit,
    }


def page(source: str, offset: int, content: str, truncated: bool) -> dict:
    data = source.encode("utf-8")
    return {
        "operation": "read_specialist_result_result",
        "task_id": TASK_ID,
        "length": len(data),
        "sha256": hashlib.sha256(data).hexdigest(),
        "offset": offset,
        "content": content,
        "truncated": truncated,
    }


def validate_page_fixture(source: str, req: dict, response: dict) -> None:
    """Validate supplied fixture/black-box output, never serve a product request."""
    VALIDATOR.validate(req)
    VALIDATOR.validate(response)
    data = source.encode("utf-8", "strict")
    start, limit = req["offset"], req["limit"]
    if start > len(data):
        raise ValueError("offset exceeds EOF")
    try:
        data[:start].decode("utf-8", "strict")
    except UnicodeDecodeError as error:
        raise ValueError("offset is not a character boundary") from error
    end = min(len(data), start + limit)
    while end > start:
        try:
            data[start:end].decode("utf-8", "strict")
            break
        except UnicodeDecodeError:
            end -= 1
    if end == start and start < len(data):
        raise ValueError("limit cannot fit the next complete character")
    content = response["content"].encode("utf-8", "strict")
    expected = {
        "task_id": req["task_id"],
        "offset": start,
        "length": len(data),
        "sha256": hashlib.sha256(data).hexdigest(),
        "truncated": end < len(data),
    }
    if any(response[key] != value for key, value in expected.items()):
        raise ValueError("response identity, metadata, offset, or continuation disagrees")
    if content != data[start:end]:
        raise ValueError("page is not the maximal exact bounded UTF-8 prefix")


class ProbeDecisionTests(unittest.TestCase):
    def test_opt_in_guard_starts_no_campaign(self) -> None:
        env = os.environ.copy()
        env.pop(probe.OPT_IN, None)
        completed = subprocess.run(
            [sys.executable, "-B", str(SCRIPT)],
            cwd=ROOT,
            env=env,
            capture_output=True,
            text=True,
            timeout=15,
            check=False,
        )
        self.assertEqual(completed.returncode, 2)
        self.assertIn(f"{probe.OPT_IN}=1 is required", completed.stderr)

    def test_disposition_records_local_campaign_pin(self) -> None:
        artifact = json.loads(
            (PROTOCOL / "dolgorae-live-transport-selection-v1.json").read_text()
        )
        schema = json.loads(
            (PROTOCOL / "dolgorae-live-transport-selection-v1.schema.json").read_text()
        )
        Draft202012Validator(schema).validate(artifact)
        self.assertEqual(artifact["codex_pin"], "0.155.1")
        self.assertEqual(artifact["status"], "selected")
        self.assertEqual(artifact["selected"], "native_run_bound")
        self.assertEqual(artifact["live_evidence"], "recorded")
        self.assertEqual(set(artifact["required_scenarios"]), set(probe.REQUIRED_SCENARIOS))


class ProviderContractTests(unittest.TestCase):
    def test_assign_is_an_acceptance_receipt_not_terminal_observation(self) -> None:
        accepted = {
            "operation": "assign_specialist_task_result",
            "task_id": TASK_ID,
            "target_run_id": TASK_ID,
            "state": "accepted",
        }
        VALIDATOR.validate(accepted)
        for state in ("completed", "failed", "cancelled", "expired", "interrupted_unknown"):
            with self.subTest(state=state):
                self.assertFalse(VALIDATOR.is_valid(dict(accepted, state=state)))
        self.assertEqual(SCHEMA["x-waitSemantics"]["blocking_budget_seconds"], 60)
        self.assertNotIn("completed", SCHEMA["x-waitSemantics"]["task_non_terminal_states"])
        self.assertIn("completed", SCHEMA["x-waitSemantics"]["task_terminal_states"])

    def test_unicode_pages_concatenate_without_loss(self) -> None:
        source = "가🙂\r\nZ"
        cases = [
            (0, 4, "가", True),
            (3, 4, "🙂", True),
            (7, 2, "\r\n", True),
            (9, 1, "Z", False),
        ]
        combined = b""
        cursor = 0
        for offset, limit, content, truncated in cases:
            self.assertEqual(offset, cursor)
            validate_page_fixture(
                source, request(offset, limit), page(source, offset, content, truncated)
            )
            encoded = content.encode("utf-8")
            self.assertGreater(len(encoded), 0)
            combined += encoded
            cursor += len(encoded)
        self.assertEqual(combined, source.encode("utf-8"))
        validate_page_fixture(source, request(cursor, 1), page(source, cursor, "", False))

    def test_empty_artifact_and_exact_eof_are_valid(self) -> None:
        validate_page_fixture("", request(0, 1), page("", 0, "", False))
        validate_page_fixture("가", request(3, 1), page("가", 3, "", False))

    def test_invalid_boundaries_and_small_limits_cannot_return_success(self) -> None:
        source = "가🙂\r\nZ"
        for offset, limit in ((0, 1), (0, 2), (1, 10), (3, 3), (4, 8), (11, 1)):
            with self.subTest(offset=offset, limit=limit):
                with self.assertRaises(ValueError):
                    validate_page_fixture(
                        source, request(offset, limit), page(source, offset, "", False)
                    )

    def test_empty_nonprogress_page_is_rejected(self) -> None:
        source = "가🙂"
        self.assertFalse(VALIDATOR.is_valid(page(source, 0, "", True)))
        with self.assertRaises(ValueError):
            validate_page_fixture(source, request(0, 4), page(source, 0, "", False))

    def test_no_lossy_normalization_or_over_limit_output(self) -> None:
        source = "가🙂\r\nZ"
        for offset, limit, content in (
            (0, 3, "�"),
            (0, 1, "가"),
            (7, 2, "\n"),
            (0, 10, "가🙂\nZ"),
        ):
            with self.subTest(content=content):
                with self.assertRaises(ValueError):
                    validate_page_fixture(
                        source, request(offset, limit), page(source, offset, content, True)
                    )

    def test_metadata_digest_and_offset_cannot_drift(self) -> None:
        source = "가🙂"
        good = page(source, 0, "가", True)
        for field, value in (("sha256", "0" * 64), ("length", 100), ("offset", 3), ("truncated", False)):
            with self.subTest(field=field):
                bad = copy.deepcopy(good)
                bad[field] = value
                with self.assertRaises(ValueError):
                    validate_page_fixture(source, request(0, 3), bad)

    def test_read_bounds_and_errors_are_checked(self) -> None:
        for offset, limit in ((-1, 1), (0, 0), (0, 65537), (2**53, 1)):
            self.assertFalse(VALIDATOR.is_valid(request(offset, limit)))
        error = {
            "operation": "orchestration_error",
            "code": "SPECIALIST_RESULT_UNREADABLE",
            "message": "Result page is unavailable for this request.",
            "retryable": False,
        }
        VALIDATOR.validate(error)

    def test_existing_ascii_fixture_has_exact_digest(self) -> None:
        fixture = json.loads(
            (PROTOCOL / "examples/orchestration-read-result-page.valid.json").read_text()
        )
        validate_page_fixture("hello result", request(0, 65536), fixture)


if __name__ == "__main__":
    unittest.main()
