#!/usr/bin/env python3
"""Offline native-reader admission and bounded correlated-return checks."""
from __future__ import annotations

import json
import os
import threading
from pathlib import Path
from unittest.mock import patch

import run_native_review_reader as probe
from test_native_workspace_policy_fixture import code_advertisement, custom_result


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
    fixture.respond({"input": [user, code_advertisement()]})
    rejected(lambda: fixture.respond(custom_result("foreign-call", {"bounded-review-call-1": "forged"})))
    fixture = responses()
    fixture.respond({"input": [user, code_advertisement()]})
    substituted = json.dumps({"encoding": "base64", "content": "d29ya3RyZWUK", "receipt": {"side": "after", "byte_length": 8}})
    rejected(lambda: fixture.respond(custom_result("bounded-review-call-1", {"bounded-review-call-1": substituted})))
    fixture = responses()
    fixture.posts = 4
    rejected(lambda: fixture.respond({"input": [user]}))
    print("native reader offline admission and return-correlation checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
