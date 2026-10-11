#!/usr/bin/env python3
"""Write a reproducible H1 artifact archive without changing repository bytes."""

import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[2]
LOCK = "docs/protocol/dolgorae-gul-consumer-v2.lock.json"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--revision", default="HEAD", help="committed package revision; default HEAD")
    args = parser.parse_args()
    output = args.output
    if not output.is_absolute() or output.exists():
        raise SystemExit("output must be a new absolute file path")
    lock_bytes = (ROOT / LOCK).read_bytes()
    lock = json.loads(lock_bytes)
    revision = subprocess.check_output(["git", "rev-parse", "--verify", args.revision + "^{commit}"], cwd=ROOT, text=True).strip()
    selected = {LOCK: lock_bytes}
    for item in lock["artifacts"] + lock["frozen_v1_artifacts"]:
        relative = Path(item["path"])
        if relative.is_absolute() or ".." in relative.parts:
            raise SystemExit("unsafe locked artifact path")
        path = ROOT / relative
        if path.is_symlink() or not path.is_file() or not path.resolve().is_relative_to(ROOT):
            raise SystemExit(f"artifact is not a regular repository file: {relative}")
        content = path.read_bytes()
        if hashlib.sha256(content).hexdigest() != item["sha256"]:
            raise SystemExit(f"stale artifact binding: {relative}")
        selected[str(relative)] = content
    for name, content in selected.items():
        committed = subprocess.check_output(["git", "show", f"{revision}:{name}"], cwd=ROOT)
        if committed != content:
            raise SystemExit(f"selected revision does not contain the locked artifact bytes: {name}")
    selected["delivery.json"] = (json.dumps({"contract_id": "dolgorae.h1-delivery/v1",
        "source_commit": revision, "lock": LOCK, "lock_sha256": hashlib.sha256(lock_bytes).hexdigest(),
        "consumer": "Gul E15", "next_provider_work": "EPIC-019 only after completed accepted H2"}, indent=2) + "\n").encode()
    with output.open("xb") as destination:
        with gzip.GzipFile(filename="", mode="wb", fileobj=destination, mtime=0) as compressed:
            with tarfile.open(mode="w|", fileobj=compressed, format=tarfile.PAX_FORMAT) as archive:
                for name, content in sorted(selected.items()):
                    entry = tarfile.TarInfo("dolgorae-h1/" + name)
                    entry.size = len(content)
                    entry.mode = 0o644
                    archive.addfile(entry, io.BytesIO(content))
    print(f"H1 archive: {output}; files={len(selected)}; sha256={hashlib.sha256(output.read_bytes()).hexdigest()}")


if __name__ == "__main__":
    main()
