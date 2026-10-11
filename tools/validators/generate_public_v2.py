#!/usr/bin/env python3
"""Reproduce only the checked H1 public-v2 descriptor and Go bindings."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]
PROTOCOL = ROOT / "docs/protocol"
SOURCE = "dolgorae/public/v2/dolgorae.proto"
MODULE = "github.com/rootkernel/dolgorae/contracts/gul-consumer-v2"
GO_ROOT = PROTOCOL / "generated/gul-consumer-v2/go"
OUTPUTS = {
    "dolgorae-public-v2.descriptor.pb": PROTOCOL / "dolgorae-public-v2.descriptor.pb",
    "dolgorae/public/v2/dolgorae.pb.go": GO_ROOT / "dolgorae/public/v2/dolgorae.pb.go",
    "dolgorae/public/v2/dolgorae_grpc.pb.go": GO_ROOT / "dolgorae/public/v2/dolgorae_grpc.pb.go",
}


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def checked_output(command: list[str]) -> str:
    return subprocess.check_output(command, text=True, cwd=ROOT).strip()


def scratch_parent() -> Path | None:
    volume = Path("/Volumes/RootKernel")
    if os.path.ismount(volume) and os.access(volume, os.W_OK):
        parent = volume / "tmp"
        parent.mkdir(exist_ok=True)
        return parent
    return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help="replace v2 generated outputs; otherwise compare")
    args = parser.parse_args()
    tools = {
        "protoc": ("protoc", "libprotoc 36.2"),
        "buf": ("buf", "1.69.0"),
        "protoc-gen-go": ("protoc-gen-go", "protoc-gen-go v1.36.11"),
        "protoc-gen-go-grpc": ("protoc-gen-go-grpc", "protoc-gen-go-grpc 1.6.1"),
    }
    versions = {}
    for name, (command, expected) in tools.items():
        actual = checked_output([command, "--version"])
        if actual != expected:
            raise SystemExit(f"{name}: expected {expected!r}, got {actual!r}")
        versions[name] = actual
    executable = Path(shutil.which("protoc") or "").resolve()
    include = executable.parent.parent / "include"
    imported = include / "google/protobuf/timestamp.proto"
    if not imported.is_file():
        raise SystemExit("the selected protoc installation does not supply timestamp.proto")
    # The recorded command uses placeholders for disposable roots, never machine paths.
    command = [
        "protoc", "--proto_path=docs/protocol", "--proto_path=<protoc-include>",
        "--include_imports", "--include_source_info",
        "--descriptor_set_out=<output>/dolgorae-public-v2.descriptor.pb",
        "--go_out=<output>", "--go_opt=paths=source_relative",
        "--go-grpc_out=<output>", "--go-grpc_opt=paths=source_relative", SOURCE,
    ]
    with tempfile.TemporaryDirectory(prefix="dolgorae-public-v2-", dir=scratch_parent()) as temporary:
        actual_command = [part.replace("<protoc-include>", str(include)).replace("<output>", temporary) for part in command]
        subprocess.run(actual_command, cwd=ROOT, check=True)
        generated = {name: (Path(temporary) / name).read_bytes() for name in OUTPUTS}
    metadata = {
        "schema_version": 2, "package": "dolgorae.public.v2", "protocol_version": 2,
        "minimum_client_protocol_version": 2, "maximum_client_protocol_version": 2,
        "source": SOURCE, "source_sha256": sha256((PROTOCOL / SOURCE).read_bytes()),
        "descriptor": "dolgorae-public-v2.descriptor.pb",
        "descriptor_sha256": sha256(generated["dolgorae-public-v2.descriptor.pb"]),
        "include_imports": True, "include_source_info": True,
        "runtime_availability": "not_activated_by_publication",
        "compatibility": "separate_public_v2_package_frozen_public_v1_unchanged",
        "generator": {
            "versions": versions, "command": command,
            "script": "tools/validators/generate_public_v2.py",
            "script_sha256": sha256(Path(__file__).read_bytes()),
            "inputs": [{"path": SOURCE, "sha256": sha256((PROTOCOL / SOURCE).read_bytes())},
                       {"path": "google/protobuf/timestamp.proto", "sha256": sha256(imported.read_bytes())}],
            "go_module": MODULE,
            "outputs": [{"path": str(OUTPUTS[name].relative_to(PROTOCOL)), "sha256": sha256(data)}
                        for name, data in sorted(generated.items())],
        },
    }
    generated["dolgorae-public-v2.descriptor.json"] = (json.dumps(metadata, indent=2) + "\n").encode()
    destinations = dict(OUTPUTS, **{"dolgorae-public-v2.descriptor.json": PROTOCOL / "dolgorae-public-v2.descriptor.json"})
    for name, data in generated.items():
        destination = destinations[name]
        if args.write:
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(data)
        elif not destination.is_file() or destination.read_bytes() != data:
            raise SystemExit(f"generated output differs: {destination.relative_to(ROOT)}")
    print("public-v2 descriptor and Go bindings reproduced" + (" and written" if args.write else " byte-identically"))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
