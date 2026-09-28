#!/usr/bin/env python3
"""Prove the live campaign refuses unsafe or incomplete admission without account use."""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[2]
RUNNER = ROOT / "tests/e2e/run_live_independent_review_acceptance.py"


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="dolgorae-live-admission-") as temporary:
        root = pathlib.Path(temporary)
        baseline = root / "baseline"
        other_baseline = root / "other-baseline"
        candidate = root / "candidate"
        candidate.write_bytes(b"candidate identity only")
        for path, version in ((baseline, "0.157.1"), (other_baseline, "0.157.0")):
            path.write_text(f"#!/bin/sh\nprintf 'codex-cli {version}\\n'\n")
            path.chmod(0o700)

        def invoke(environment: dict[str, str], minimum: pathlib.Path,
                   auth: pathlib.Path | None = None) -> subprocess.CompletedProcess[str]:
            return subprocess.run(
                [sys.executable, str(RUNNER), "--binary", str(candidate),
                 "--codex-minimum", str(minimum),
                 "--auth-file", str(auth or root / "missing-auth")],
                capture_output=True, text=True, env=environment, timeout=15,
            )

        environment = {key: value for key, value in os.environ.items()
                       if key != "DOLGORAE_RUN_LIVE_INDEPENDENT_REVIEW"}
        refused = invoke(environment, root / "missing-baseline")
        if refused.returncode != 2 or "DOLGORAE_RUN_LIVE_INDEPENDENT_REVIEW=1 is required" not in refused.stderr:
            raise AssertionError("live runner touched selected inputs without opt-in")

        environment["DOLGORAE_RUN_LIVE_INDEPENDENT_REVIEW"] = "1"
        wrong_baseline = invoke(environment, other_baseline)
        if wrong_baseline.returncode != 2 or "Codex 0.157.1" not in wrong_baseline.stderr:
            raise AssertionError("live runner admitted the wrong baseline version")
        missing_auth = invoke(environment, baseline)
        if missing_auth.returncode != 2 or "missing-auth" not in missing_auth.stderr:
            raise AssertionError("live runner did not refuse a missing credential before campaign")
        non_file_auth = invoke(environment, baseline, root)
        if non_file_auth.returncode != 2 or "regular file" not in non_file_auth.stderr:
            raise AssertionError("live runner admitted a directory as the credential file")
    print("Live independent review admission refused missing opt-in, version, and credentials")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
