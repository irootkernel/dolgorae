#!/usr/bin/env python3
"""Offline native-reader admission and bounded correlated-return checks."""
from __future__ import annotations

import json
import os
import threading
from pathlib import Path
from unittest.mock import patch

import run_native_review_reader as probe


def responses() -> probe.ReaderResponses:
    fixture = object.__new__(probe.ReaderResponses)
    fixture.lock = threading.Lock()
    fixture.posts = 0
    fixture.steps = 0
    fixture.failure = None
    return fixture


def rejected(operation) -> None:
    try:
        operation()
    except (ValueError, RuntimeError):
        return
    raise AssertionError("unsafe native fixture input was admitted")


def main() -> int:
    with patch.dict(os.environ, {}, clear=True), patch.object(probe, "executable_identity") as identity:
        assert probe.main() == 2
        rejected(lambda: probe.campaign(Path("unused-driver"), Path("unused-codex")))
        identity.assert_not_called()
    with patch.dict(os.environ, {probe.OPT_IN: "1"}), patch.object(
        probe, "executable_identity", return_value={"sha256": "0" * 64}
    ), patch.object(probe.tempfile, "mkdtemp") as allocation:
        rejected(lambda: probe.campaign(Path("unused-driver"), Path("unused-codex")))
        allocation.assert_not_called()
    fixture = responses()
    user = {"type": "message", "role": "user", "content": [
        {"type": "input_text", "text": "fixture-step-1: read the index candidate once"}]}
    fixture.respond({"input": [user]})
    wrong = {"type": "function_call_output", "call_id": "foreign-call", "output": "forged"}
    rejected(lambda: fixture.respond({"input": [wrong]}))
    fixture = responses()
    fixture.respond({"input": [user]})
    substituted = {"type": "function_call_output", "call_id": "bounded-review-call-1",
                   "output": json.dumps({"encoding": "base64", "content": "d29ya3RyZWUK",
                                         "receipt": {"side": "after", "byte_length": 8}})}
    rejected(lambda: fixture.respond({"input": [substituted]}))
    fixture = responses()
    fixture.posts = 4
    rejected(lambda: fixture.respond({"input": [user]}))
    print("native reader offline admission and return-correlation checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
