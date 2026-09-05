#!/usr/bin/env python3
"""Verify the checked public-v1 descriptor and Profile request reservations."""

from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path

from google.protobuf.descriptor_pb2 import FileDescriptorSet

ROOT = Path(__file__).resolve().parents[2]
PROTOCOL = ROOT / "docs" / "protocol"


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> int:
    manifest_path = PROTOCOL / "dolgorae-public-v1.descriptor.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    source = PROTOCOL / manifest["source"]
    descriptor = PROTOCOL / manifest["descriptor"]
    errors: list[str] = []
    if digest(source) != manifest["source_sha256"]:
        errors.append("source digest does not match the descriptor manifest")
    if digest(descriptor) != manifest["descriptor_sha256"]:
        errors.append("descriptor digest does not match the descriptor manifest")

    file_set = FileDescriptorSet.FromString(descriptor.read_bytes())
    public = next(
        (item for item in file_set.file if item.name == manifest["source"]), None
    )
    if public is None:
        errors.append("public proto is absent from the descriptor set")
    else:
        messages = {item.name: item for item in public.message_type}
        for name in (
            "ListProfilesRequest",
            "GetProfileRequest",
            "ListProfileDiagnosticsRequest",
        ):
            message = messages.get(name)
            if message is None:
                errors.append(f"{name} is absent")
                continue
            if any(field.name == "workspace" or field.number == 2 for field in message.field):
                errors.append(f"{name} still exposes workspace field 2")
            if "workspace" not in message.reserved_name:
                errors.append(f"{name} does not reserve workspace")
            if not any(item.start == 2 and item.end == 3 for item in message.reserved_range):
                errors.append(f"{name} does not reserve field 2")
        start = messages.get("StartRunRequest")
        start_fields = {} if start is None else {field.name: field.number for field in start.field}
        if start_fields.get("workspace") != 2 or start_fields.get("profile_name") != 5:
            errors.append("StartRunRequest does not preserve workspace=2 and profile_name=5")

    if errors:
        print("Public descriptor validation failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print("Public descriptor validation passed: Profile request reservations are exact")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
