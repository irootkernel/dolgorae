#!/usr/bin/env python3
"""Write a reproducible H1 archive from one immutable committed selection."""

import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import stat
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[2]
LOCK = "docs/protocol/dolgorae-gul-consumer-v2.lock.json"
ORIGINAL_LOCK_SHA256 = "4af0ff5ee310d25564541004a4a45ffd600e4a1ca1c0bacfa986ba8cac80f049"


def git(*arguments):
    result = subprocess.run(["git", *arguments], cwd=ROOT, capture_output=True)
    if result.returncode:
        raise SystemExit("cannot read selected committed Git objects")
    return result.stdout


def checked_path(name):
    if not isinstance(name, str):
        raise SystemExit("unsafe locked artifact path")
    path = PurePosixPath(name)
    if (not path.parts or path.is_absolute() or path.as_posix() != name
            or ".." in path.parts or "\\" in name or "\0" in name):
        raise SystemExit("unsafe locked artifact path")
    return path


def committed_selection(revision):
    entries = {}
    for row in git("ls-tree", "-rz", "--full-tree", revision).split(b"\0"):
        if row:
            header, name = row.split(b"\t", 1)
            entries[name.decode()] = header.decode().split()

    def blob(name):
        checked_path(name)
        entry = entries.get(name)
        if entry is None or entry[0] not in ("100644", "100755") or entry[1] != "blob":
            raise SystemExit(f"selected artifact is missing or not a regular Git blob: {name}")
        return git("cat-file", "blob", entry[2])

    lock_bytes = blob(LOCK)
    lock = json.loads(lock_bytes)
    selected = {LOCK: lock_bytes}
    for item in lock["artifacts"] + lock["frozen_v1_artifacts"]:
        name = item["path"]
        checked_path(name)
        if name in (LOCK, "delivery.json"):
            raise SystemExit("recursive or reserved locked artifact path")
        content = blob(name)
        if hashlib.sha256(content).hexdigest() != item["sha256"]:
            raise SystemExit(f"stale artifact binding: {name}")
        selected[name] = content
    return lock, selected


def check_worktree(selected):
    for name, content in selected.items():
        path = ROOT
        parts = checked_path(name).parts
        for index, part in enumerate(parts):
            path /= part
            try:
                mode = path.lstat().st_mode
            except OSError:
                raise SystemExit(f"selected working-tree file is missing: {name}") from None
            regular = stat.S_ISREG(mode) if index == len(parts) - 1 else stat.S_ISDIR(mode)
            if not regular or stat.S_ISLNK(mode):
                raise SystemExit(f"selected working-tree path is not regular: {name}")
        if path.read_bytes() != content:
            raise SystemExit(f"selected working-tree bytes differ from HEAD: {name}")


def delivery(lock, lock_bytes, revision):
    lock_sha256 = hashlib.sha256(lock_bytes).hexdigest()
    # Preserve the exact historical six-key delivery and archive bytes only for
    # the original qualified lock. Corrected provenance is supplied by its lock.
    if lock_sha256 == ORIGINAL_LOCK_SHA256:
        return {"contract_id": "dolgorae.h1-delivery/v1", "source_commit": revision,
                "lock": LOCK, "lock_sha256": lock_sha256, "consumer": "Gul E15",
                "next_provider_work": "EPIC-019 only after completed accepted H2"}
    delta = lock["later_handoffs"]["H2"]["corrected_delta"]
    return {"contract_id": "dolgorae.h1-delivery/v1", "source_commit": revision,
            "lock": LOCK, "lock_sha256": lock_sha256, "consumer": delta["producer"],
            "next_provider_work": delta["consumer"], "producer": lock["producer"],
            "ancestry": lock["ancestry"], "consumer_acceptance": lock["later_handoffs"]["H2"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--revision", help="explicit committed package revision; otherwise checked HEAD")
    args = parser.parse_args()
    output = args.output
    if not output.is_absolute() or output.exists() or output.is_symlink():
        raise SystemExit("output must be a new absolute file path")
    revision = git("rev-parse", "--verify", "--end-of-options", ("HEAD" if args.revision is None else args.revision) + "^{commit}").decode().strip()
    lock, selected = committed_selection(revision)
    if args.revision is None:
        check_worktree(selected)
    selected["delivery.json"] = (json.dumps(delivery(lock, selected[LOCK], revision), indent=2) + "\n").encode()
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
