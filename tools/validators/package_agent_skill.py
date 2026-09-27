#!/usr/bin/env python3
"""Derive and install the source-distributed Dolgorae skill package."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
from pathlib import Path
from urllib.parse import urldefrag, urljoin

ROOT = Path(__file__).resolve().parents[2]
SKILL_ROOT = ROOT / "skills" / "use-dolgorae"
MARKDOWN_FILES = {
    "SKILL.md",
    "references/configuration.md",
    "references/lifecycle.md",
    "references/provider.md",
    "references/recovery.md",
}
SCHEMA_ROOTS = (
    "dolgorae-specialist-review-tool-v3.schema.json",
    "dolgorae-external-specialist-facade-v3.schema.json",
    "dolgorae-one-shot-review-observation-v1.schema.json",
    "dolgorae-machine-v2.schema.json",
)
EXAMPLES = (
    ("external-engagement-v2-hire.valid.json", "dolgorae-external-specialist-facade-v2.schema.json", "#/$defs/hire_request"),
    ("external-engagement-v3-assign.valid.json", "dolgorae-external-specialist-facade-v3.schema.json", "#/$defs/assign_request"),
    ("specialist-review-v3-request.valid.json", "dolgorae-specialist-review-tool-v3.schema.json", "#/$defs/review_request"),
    *((f"one-shot-review-observation-v1-{state}.valid.json", "dolgorae-one-shot-review-observation-v1.schema.json", "")
      for state in ("known", "unknown", "blocked", "response-lost")),
)


class PackageError(ValueError):
    pass


def references(node: object, base: str):
    """Yield resolved reference URIs, retaining fragments for local validation."""
    if isinstance(node, dict):
        base = urljoin(base, node.get("$id", ""))
        for key, value in node.items():
            if key in ("$ref", "$dynamicRef"):
                yield urljoin(base, value)
            else:
                yield from references(value, base)
    elif isinstance(node, list):
        for value in node:
            yield from references(value, base)


def schema_index(protocol: Path) -> dict[str, tuple[Path, dict]]:
    result = {}
    for path in sorted(protocol.glob("*.json")):
        document = json.loads(path.read_bytes())
        if isinstance(document, dict) and "$id" in document:
            identifier = document["$id"]
            if identifier in result:
                raise PackageError(f"duplicate schema ID: {identifier}")
            result[identifier] = (path, document)
    return result


def resource_bundle() -> dict[str, bytes]:
    protocol = ROOT / "docs" / "protocol"
    indexed = schema_index(protocol)
    pending = [json.loads((protocol / name).read_bytes())["$id"] for name in SCHEMA_ROOTS]
    selected = set()
    files = {}
    while pending:
        identifier = pending.pop()
        if identifier in selected:
            continue
        if identifier not in indexed:
            raise PackageError(f"canonical schema dependency is missing: {identifier}")
        selected.add(identifier)
        path, document = indexed[identifier]
        files[f"protocol/{path.name}"] = path.read_bytes()
        pending.extend(urldefrag(uri)[0] for uri in references(document, identifier))
    for name, schema, _ in EXAMPLES:
        if f"protocol/{schema}" not in files:
            raise PackageError(f"example schema is outside the resource closure: {schema}")
        files[f"protocol/examples/{name}"] = (protocol / "examples" / name).read_bytes()
    manifest = {
        "schema": "dolgorae-agent-skill-resources/v1",
        "roots": [f"protocol/{name}" for name in SCHEMA_ROOTS],
        "examples": [
            {"path": f"protocol/examples/{name}", "schema": f"protocol/{schema}", "fragment": fragment}
            for name, schema, fragment in EXAMPLES
        ],
        "files": {name: hashlib.sha256(content).hexdigest() for name, content in sorted(files.items())},
    }
    files["manifest.json"] = (json.dumps(manifest, indent=2) + "\n").encode()
    return files


def regular_files(root: Path) -> set[str]:
    if root.is_symlink() or not root.is_dir():
        raise PackageError(f"package root must be a regular directory: {root}")
    files = set()
    for path in root.rglob("*"):
        if path.is_symlink() or not (path.is_file() or path.is_dir()):
            raise PackageError(f"package contains a non-regular entry: {path}")
        if path.is_file():
            files.add(path.relative_to(root).as_posix())
    return files


def checked_package() -> dict[str, bytes]:
    resources = {f"resources/{name}": content for name, content in resource_bundle().items()}
    expected = MARKDOWN_FILES | resources.keys()
    actual = regular_files(SKILL_ROOT)
    if actual != expected:
        raise PackageError(f"package file set differs: missing={sorted(expected - actual)}, extra={sorted(actual - expected)}")
    for name, content in resources.items():
        if (SKILL_ROOT / name).read_bytes() != content:
            raise PackageError(f"package resource differs from canonical source: {name}")
    return {**resources, **{name: (SKILL_ROOT / name).read_bytes() for name in MARKDOWN_FILES}}


def install(destination: Path) -> None:
    files = checked_package()
    destination.parent.mkdir(parents=True, exist_ok=True)
    # mkdir is exclusive: even an empty existing directory must survive unchanged.
    destination.mkdir(mode=0o700)
    try:
        for name, content in sorted(files.items()):
            path = destination / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
    except BaseException:
        shutil.rmtree(destination)
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("sync", help="derive source package resources from canonical protocol files")
    installer = subparsers.add_parser("install", help="install a checked package to a new directory")
    installer.add_argument("--destination", required=True, type=Path)
    args = parser.parse_args()
    try:
        if args.command == "sync":
            resources = SKILL_ROOT / "resources"
            expected = resource_bundle()
            if resources.exists():
                extras = regular_files(resources) - expected.keys()
                if extras:
                    raise PackageError(f"remove obsolete resource files explicitly before syncing: {sorted(extras)}")
            for name, content in expected.items():
                path = resources / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(content)
            print(f"Derived {len(expected)} skill resource files")
        else:
            install(args.destination)
            print(f"Installed use-dolgorae at {args.destination}")
    except (OSError, ValueError) as error:
        parser.exit(1, f"Skill package error: {error}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
