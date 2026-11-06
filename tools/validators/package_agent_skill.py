#!/usr/bin/env python3
"""Derive and install the source-distributed Dolgorae skill package."""

from __future__ import annotations

import argparse
import hashlib
import json
import posixpath
import shutil
from pathlib import Path
from urllib.parse import unquote, urldefrag, urljoin

from validate_markdown import LINK_RE

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
    "dolgorae-public-resolution-v2.schema.json",
    "dolgorae-machine-v6.schema.json",
    "dolgorae-interaction-decision-v2.schema.json",
    "dolgorae-specialist-review-tool-v4.schema.json",
    "dolgorae-external-specialist-facade-v4.schema.json",
    "dolgorae-machine-v4.schema.json",
    "dolgorae-instruction-migration-v1.schema.json",
    "dolgorae-specialist-review-tool-v3.schema.json",
    "dolgorae-external-specialist-facade-v3.schema.json",
    "dolgorae-one-shot-review-observation-v1.schema.json",
    "dolgorae-machine-v2.schema.json",
)
EXAMPLES = (
    ("external-engagement-v4-hire.valid.json", "dolgorae-external-specialist-facade-v4.schema.json", "#/$defs/hire_request"),
    ("external-engagement-v4-assign.valid.json", "dolgorae-external-specialist-facade-v4.schema.json", "#/$defs/assign_request"),
    ("specialist-review-v4-request.valid.json", "dolgorae-specialist-review-tool-v4.schema.json", "#/$defs/review_request"),
    ("instruction-migration-request.valid.json", "dolgorae-instruction-migration-v1.schema.json", "#/$defs/request"),
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
    # Embedded policy resources have their own IDs, but ship in their owner's
    # canonical file. Keep that file intact so offline registries can crawl it.
    owners = {}

    def collect_ids(node: object, base: str, owner: str) -> None:
        if isinstance(node, dict):
            base = urljoin(base, node.get("$id", ""))
            if "$id" in node:
                previous = owners.setdefault(base, owner)
                if previous != owner:
                    raise PackageError(f"duplicate embedded schema ID: {base}")
            for value in node.values():
                collect_ids(value, base, owner)
        elif isinstance(node, list):
            for value in node:
                collect_ids(value, base, owner)

    for identifier, (_, document) in indexed.items():
        collect_ids(document, identifier, identifier)
    pending = [json.loads((protocol / name).read_bytes())["$id"] for name in SCHEMA_ROOTS]
    selected = set()
    files = {}
    while pending:
        identifier = pending.pop()
        identifier = owners.get(identifier, identifier)
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


def source_package() -> dict[str, bytes]:
    resources = {f"resources/{name}": content for name, content in resource_bundle().items()}
    actual = regular_files(SKILL_ROOT)
    if actual != MARKDOWN_FILES:
        raise PackageError(f"source skill file set differs: missing={sorted(MARKDOWN_FILES - actual)}, extra={sorted(actual - MARKDOWN_FILES)}")
    files = dict(resources)
    protocol = (ROOT / "docs/protocol").resolve()
    for name in sorted(MARKDOWN_FILES):
        markdown = SKILL_ROOT / name

        def project(match):
            target = match.group(1)
            local, separator, fragment = target.partition("#")
            if not local or local.startswith(("https://", "http://", "mailto:")):
                return match.group(0)
            canonical = (markdown.parent / unquote(local)).resolve()
            if canonical.is_relative_to(protocol):
                installed = "resources/protocol/" + canonical.relative_to(protocol).as_posix()
                if installed not in resources:
                    raise PackageError(f"guidance resource is outside the selected closure: {target}")
                projected = posixpath.relpath(installed, posixpath.dirname(name) or ".")
                return match.group(0).replace(f"({target})", f"({projected}{separator}{fragment})")
            if not canonical.is_relative_to(SKILL_ROOT.resolve()) or canonical.relative_to(SKILL_ROOT.resolve()).as_posix() not in MARKDOWN_FILES:
                raise PackageError(f"guidance link is outside the source package: {target}")
            return match.group(0)

        files[name] = LINK_RE.sub(project, markdown.read_text(encoding="utf-8")).encode()
    return files


def check_installed_package(root: Path) -> None:
    expected = source_package()
    actual = regular_files(root)
    if actual != expected.keys():
        raise PackageError(f"package file set differs: missing={sorted(expected.keys() - actual)}, extra={sorted(actual - expected.keys())}")
    for name, content in expected.items():
        if (root / name).read_bytes() != content:
            raise PackageError(f"package resource differs from canonical source: {name}")


def install(destination: Path) -> None:
    files = source_package()
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
    installer = subparsers.add_parser("install", help="install a checked package to a new directory")
    installer.add_argument("--destination", required=True, type=Path)
    args = parser.parse_args()
    try:
        install(args.destination)
        print(f"Installed use-dolgorae at {args.destination}")
    except (OSError, ValueError) as error:
        parser.exit(1, f"Skill package error: {error}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
