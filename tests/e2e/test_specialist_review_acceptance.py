#!/usr/bin/env python3
"""Deterministic fake-adapter checks for the TASK-013 live runner."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import stat
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tests" / "e2e" / "run_specialist_review_acceptance.py"
SPEC = importlib.util.spec_from_file_location("review_acceptance", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def executable(path: Path, source: str) -> None:
    path.write_text(source, encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="dolgorae-review-acceptance-") as raw:
        root = Path(raw)
        old_home = os.environ.get("HOME")
        old_thread = os.environ.get("CODEX_THREAD_ID")
        os.environ["HOME"] = str(root / "home")
        os.environ["CODEX_THREAD_ID"] = "018f0000-0000-7000-8000-000000006099"
        (root / "home").mkdir()
        workspace = root / "workspace"
        workspace.mkdir()
        os.system(f"git -C {workspace!s} init -q")
        (workspace / "change.txt").write_text("nontrivial working-tree change\n", encoding="utf-8")
        codex = root / "codex"
        executable(codex, "#!/bin/sh\nprintf 'codex-cli 0.149.0\\n'\n")
        fake = root / "dolgorae"
        success = {
            "schema_version": 1,
            "ok": True,
            "command": "specialist.review",
            "invocation_id": "018f0000-0000-7000-8000-000000006010",
            "data": {
                "operation": "review_working_tree_result",
                "review_id": "018f0000-0000-7000-8000-000000006001",
                "reviewer_run_id": "018f0000-0000-7000-8000-000000006002",
                "state": "completed",
                "summary": "one finding",
                "findings": [{
                    "severity": "P2",
                    "title": "fix me",
                    "description": "the fake change needs a deterministic fix",
                    "path": "change.txt",
                    "line_start": 1,
                    "line_end": 1,
                    "recommendation": "apply the deterministic correction",
                    "confidence": "high",
                }],
                "result_artifact_ref": "018f0000-0000-7000-8000-000000006003",
                "workspace_write_observed": False,
            },
        }
        failure = {
            "schema_version": 1,
            "ok": False,
            "command": "specialist.review",
            "invocation_id": "018f0000-0000-7000-8000-000000006011",
            "error": {"code": "PROFILE_NOT_FOUND", "message": "unavailable", "retryable": False, "details": {}},
        }
        executable(
            fake,
            "#!/usr/bin/env python3\n"
            "import json,sys\n"
            f"success={success!r}\n"
            f"failure={failure!r}\n"
            "missing='__task013_missing_profile__' in sys.argv\n"
            "json.dump(failure if missing else success,sys.stdout,separators=(',',':'))\n"
            "raise SystemExit(5 if missing else 0)\n",
        )
        reviewer_state = (
            root
            / "home"
            / "Library"
            / "Application Support"
            / "Dolgorae"
            / "workspaces"
            / "fake"
            / "runs"
            / success["data"]["reviewer_run_id"]
        )
        reviewer_state.mkdir(parents=True)
        (reviewer_state / "state.json").write_text(
            json.dumps({"thread_id": "018f0000-0000-7000-8000-000000006098"}),
            encoding="utf-8",
        )
        (reviewer_state / "manifest.json").write_text(
            json.dumps({"profile": {"process_static_configuration": {"mcp_servers": {}}}}),
            encoding="utf-8",
        )
        canary = "parent-only-canary"
        good = MODULE.success_evidence(fake, workspace, "reviewer", codex, canary)
        assert good["finding_count"] == 1
        assert good["workspace_fingerprint_before"] == good["workspace_fingerprint_after"]
        assert all(good["observable_scan"].values())
        failed = MODULE.failure_evidence(fake, workspace, codex, canary)
        assert failed["error_code"] == "PROFILE_NOT_FOUND"
        assert failed["retryable"] is False

        leaking = json.loads(json.dumps(success))
        leaking["data"]["summary"] = canary
        executable(
            fake,
            "#!/usr/bin/env python3\n"
            f"import json; json.dump({leaking!r},__import__('sys').stdout,separators=(',',':'))\n",
        )
        try:
            MODULE.success_evidence(fake, workspace, "reviewer", codex, canary)
        except ValueError as error:
            assert "canary" in str(error)
        else:
            raise AssertionError("host-context canary leak was accepted")

        mutating = json.loads(json.dumps(success))
        executable(
            fake,
            "#!/usr/bin/env python3\n"
            "from pathlib import Path\n"
            "import json,sys\n"
            "Path('change.txt').write_text('mutated\\n')\n"
            f"json.dump({mutating!r},sys.stdout,separators=(',',':'))\n",
        )
        try:
            MODULE.success_evidence(fake, workspace, "reviewer", codex, canary)
        except ValueError as error:
            assert "isolation" in str(error)
        else:
            raise AssertionError("workspace mutation was accepted")

        (workspace / ".gitignore").write_text("ignored.txt\n", encoding="utf-8")
        (workspace / "ignored.txt").write_text("before\n", encoding="utf-8")
        executable(
            fake,
            "#!/usr/bin/env python3\n"
            "from pathlib import Path\n"
            "import json,sys\n"
            "Path('ignored.txt').write_text('after!\\n')\n"
            f"json.dump({success!r},sys.stdout,separators=(',',':'))\n",
        )
        try:
            MODULE.success_evidence(fake, workspace, "reviewer", codex, canary)
        except ValueError as error:
            assert "isolation" in str(error)
        else:
            raise AssertionError("ignored workspace mutation was accepted")

        state = root / "home" / "Library" / "Application Support" / "Dolgorae"
        state.mkdir(parents=True, exist_ok=True)
        (state / "event.jsonl").write_text(canary, encoding="utf-8")
        assert MODULE.canary_absent_from_state(state, canary) is False

        executable(
            fake,
            "#!/usr/bin/env python3\n"
            "import sys; sys.stdout.write('x' * 1048577)\n",
        )
        try:
            MODULE.success_evidence(fake, workspace, "reviewer", codex, canary)
        except ValueError as error:
            assert "1 MiB" in str(error)
        else:
            raise AssertionError("oversized streaming output was accepted")
        assert hashlib.sha256(SCRIPT.read_bytes()).hexdigest()
        registry = MODULE.Registry()
        protocol = ROOT / "docs" / "protocol"
        for path in sorted(protocol.glob("*.schema.json")):
            schema = json.loads(path.read_text(encoding="utf-8"))
            registry = registry.with_resource(
                schema["$id"], MODULE.Resource.from_contents(schema)
            )
        acceptance_schema = json.loads(
            (protocol / "dolgorae-specialist-review-acceptance-v1.schema.json").read_text(encoding="utf-8")
        )
        acceptance = json.loads(
            (protocol / "dolgorae-specialist-review-acceptance-v1.json").read_text(encoding="utf-8")
        )
        MODULE.Draft202012Validator(
            acceptance_schema,
            registry=registry,
            format_checker=MODULE.FormatChecker(),
        ).validate(acceptance)
        if old_home is None:
            del os.environ["HOME"]
        else:
            os.environ["HOME"] = old_home
        if old_thread is None:
            del os.environ["CODEX_THREAD_ID"]
        else:
            os.environ["CODEX_THREAD_ID"] = old_thread
    print("specialist review acceptance fake-adapter tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
