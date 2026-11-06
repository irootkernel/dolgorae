#!/usr/bin/env python3
"""Validate the immutable H1 selection without binding it to current Cargo bytes."""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import subprocess
import sys
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[2]
SOURCE = "0927cbf87b0a3ba37c8509016c99f14c3f9f3c29"
LOCK = "docs/protocol/dolgorae-gul-consumer-v2.lock.json"
LOCK_SHA256 = "5900393481ed200c6ef4eadab96b034d679f3e4cc6b0d42b764c4211c94a862a"
ARCHIVE_SHA256 = "9b8427b6958b6ef9323dfcea1a512ee3ba070a9e594076613abfc98849012e3c"
SCRIPTS = (
    "tools/validators/validate_h1_contract.py",
    "tests/e2e/test_h1_selected_registry.py",
    "tests/e2e/test_h1_package.py",
)


def main() -> int:
    try:
        with tempfile.TemporaryDirectory(prefix="dolgorae-frozen-h1-", dir="/tmp") as temporary:
            root = Path(temporary).resolve()
            archive = root / "h1.tgz"
            subprocess.run(
                [sys.executable, "-B", str(ROOT / "tools/validators/package_h1.py"),
                 "--revision", SOURCE, "--output", str(archive)],
                cwd=ROOT, check=True,
            )
            files = {}
            with tarfile.open(archive) as selected:
                for member in selected:
                    path = PurePosixPath(member.name)
                    if (not member.isfile() or path.is_absolute() or ".." in path.parts
                            or path.as_posix() != member.name or len(path.parts) < 2
                            or path.parts[0] != "dolgorae-h1"):
                        raise ValueError(f"unsafe H1 member: {member.name}")
                    name = PurePosixPath(*path.parts[1:]).as_posix()
                    if name in files:
                        raise ValueError(f"duplicate H1 member: {name}")
                    with selected.extractfile(member) as content:
                        files[name] = content.read()
            if hashlib.sha256(files[LOCK]).hexdigest() != LOCK_SHA256:
                raise ValueError("immutable H1 lock identity differs")
            lock = json.loads(files[LOCK])
            entries = lock["artifacts"] + lock["frozen_v1_artifacts"]
            expected = {entry["path"] for entry in entries} | {LOCK, "delivery.json"}
            if files.keys() != expected:
                raise ValueError("immutable H1 member inventory differs")
            for entry in entries:
                if hashlib.sha256(files[entry["path"]]).hexdigest() != entry["sha256"]:
                    raise ValueError(f"immutable H1 member bytes differ: {entry['path']}")
            print(f"H1 lock and {len(files)} selected members match", flush=True)
            if hashlib.sha256(archive.read_bytes()).hexdigest() != ARCHIVE_SHA256:
                raise ValueError("H1 archive encoding identity differs after lock/member equality; check compressor/toolchain")
            checkout = root / "selected"
            for name, content in files.items():
                path = checkout / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(content)
            environment = dict(os.environ, TMPDIR=str(root), PYTHONDONTWRITEBYTECODE="1")
            for script in SCRIPTS:
                print(f"[frozen H1] {script}", flush=True)
                subprocess.run([sys.executable, "-B", str(checkout / script)],
                               cwd=checkout, env=environment, check=True)
    except (OSError, ValueError, KeyError, tarfile.TarError, subprocess.CalledProcessError) as error:
        print(f"Frozen H1 validation failed: {error}", file=sys.stderr)
        return 1
    print("Frozen H1 archive and all three unchanged selected scripts passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
