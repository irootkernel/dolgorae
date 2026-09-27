#!/usr/bin/env python3
"""Validate source skill metadata and independently installed contract resources."""

from __future__ import annotations

import hashlib
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path, PurePosixPath
from urllib.parse import unquote

from jsonschema import Draft202012Validator
from jsonschema.exceptions import SchemaError, ValidationError
from referencing import Registry, Resource
from referencing.exceptions import NoSuchResource, Unresolvable

from validate_markdown import LINK_RE


ROOT = Path(__file__).resolve().parents[2]
from package_agent_skill import (
    MARKDOWN_FILES,
    checked_package,
    references,
    regular_files,
    resource_bundle,
    schema_index,
)

SKILL_ROOT = ROOT / "skills" / "use-dolgorae"
HANGUL_RE = re.compile(r"[\u1100-\u11ff\u3130-\u318f\uac00-\ud7af]")


class SkillValidationError(RuntimeError):
    pass


def fail(message: str) -> None:
    raise SkillValidationError(message)


def validate_path(path: Path) -> None:
    current = ROOT
    for part in path.relative_to(ROOT).parts:
        current /= part
        if current.is_symlink():
            fail(f"skill path must not be a symlink: {current.relative_to(ROOT)}")


def validate_frontmatter(content: str) -> None:
    if not content.startswith("---\n"):
        fail("skills/use-dolgorae/SKILL.md: missing YAML frontmatter")
    _, separator, remainder = content.partition("\n---\n")
    if not separator or not remainder.strip():
        fail("skills/use-dolgorae/SKILL.md: incomplete YAML frontmatter")
    frontmatter = content[4 : content.index("\n---\n")]
    lines = frontmatter.splitlines()
    if not lines or lines[0] != "name: use-dolgorae":
        fail("skills/use-dolgorae/SKILL.md: invalid skill name")
    descriptions = [line for line in lines if line.startswith("description: ")]
    if len(descriptions) != 1:
        fail("skills/use-dolgorae/SKILL.md: invalid description")
    encoded_description = descriptions[0].removeprefix("description: ")
    try:
        description = json.loads(encoded_description)
    except json.JSONDecodeError:
        fail("skills/use-dolgorae/SKILL.md: description must be double-quoted")
    if not isinstance(description, str) or not description.strip():
        fail("skills/use-dolgorae/SKILL.md: invalid description")
    if len(lines) != 2:
        fail("skills/use-dolgorae/SKILL.md: unexpected frontmatter fields")


def source_files() -> set[str]:
    try:
        listed = subprocess.run(
            [
                "git",
                "ls-files",
                "--cached",
                "--others",
                "--exclude-standard",
                "-z",
                "--",
                "skills/use-dolgorae",
            ],
            cwd=ROOT,
            check=True,
            capture_output=True,
        ).stdout
    except subprocess.CalledProcessError as error:
        raise SkillValidationError("cannot enumerate source-distributed skill files") from error

    prefix = "skills/use-dolgorae/"
    actual_files: set[str] = set()
    for encoded in listed.split(b"\0"):
        if not encoded:
            continue
        repository_path = encoded.decode("utf-8")
        if not repository_path.startswith(prefix):
            fail(f"unexpected skill path from Git: {repository_path}")
        relative = repository_path.removeprefix(prefix)
        path = ROOT / repository_path
        validate_path(path)
        if path.is_file():
            actual_files.add(relative)
    return actual_files


def validate_installed_resources(package: Path) -> None:
    """Resolve only installed resources; the registry cannot fetch missing IDs."""
    resources = package / "resources"
    manifest = json.loads((resources / "manifest.json").read_bytes())
    if manifest["schema"] != "dolgorae-agent-skill-resources/v1":
        fail("unsupported installed resource manifest")
    expected = set(manifest["files"]) | {"manifest.json"}
    for name in expected:
        path = PurePosixPath(name)
        if path.is_absolute() or ".." in path.parts or path.as_posix() != name:
            fail(f"resource path must stay inside the package: {name}")
    if regular_files(resources) != expected:
        fail("installed resource inventory is incomplete or contains extra files")
    for name, digest in manifest["files"].items():
        if hashlib.sha256((resources / name).read_bytes()).hexdigest() != digest:
            fail(f"installed resource digest differs: {name}")

    def deny_remote(uri: str):
        raise NoSuchResource(ref=uri)

    indexed = schema_index(resources / "protocol")
    registry = Registry(retrieve=deny_remote)
    for identifier, (_, document) in indexed.items():
        registry = registry.with_resource(identifier, Resource.from_contents(document))
    for identifier, (_, document) in indexed.items():
        for uri in references(document, identifier):
            registry.resolver().lookup(uri)
        Draft202012Validator.check_schema(document)
    for name in manifest["roots"]:
        if name not in manifest["files"]:
            fail(f"schema root is outside the installed inventory: {name}")
        document = json.loads((resources / name).read_bytes())
        if document["$id"] not in indexed:
            fail(f"installed schema root is missing: {name}")
    for example in manifest["examples"]:
        if any(example[key] not in manifest["files"] for key in ("path", "schema")):
            fail("example references a file outside the installed inventory")
        document = json.loads((resources / example["schema"]).read_bytes())
        checked = Draft202012Validator(
            {"$ref": document["$id"] + example["fragment"]}, registry=registry
        )
        checked.validate(json.loads((resources / example["path"]).read_bytes()))

    for name in MARKDOWN_FILES:
        path = package / name
        for match in LINK_RE.finditer(path.read_text(encoding="utf-8")):
            target = match.group(1).split("#", 1)[0]
            if not target or target.startswith(("https://", "http://", "mailto:")):
                continue
            resolved = (path.parent / unquote(target)).resolve()
            if not resolved.is_relative_to(package.resolve()) or not resolved.is_file():
                fail(f"installed guidance link is outside or absent from package: {name}: {target}")


def main() -> int:
    try:
        if not SKILL_ROOT.is_dir():
            fail("skills/use-dolgorae: missing skill directory")
        validate_path(SKILL_ROOT)

        expected_files = MARKDOWN_FILES | {f"resources/{name}" for name in resource_bundle()}
        actual_files = source_files()
        if actual_files != expected_files:
            fail(
                "skills/use-dolgorae: file set differs from the contract: "
                f"expected {sorted(expected_files)}, got {sorted(actual_files)}"
            )
        checked_package()

        entrypoint = (SKILL_ROOT / "SKILL.md").read_text(encoding="utf-8")
        validate_frontmatter(entrypoint)
        for reference in sorted(MARKDOWN_FILES - {"SKILL.md"}):
            target = reference.removeprefix("references/")
            if f"](references/{target})" not in entrypoint:
                fail(f"skills/use-dolgorae/SKILL.md: unlinked reference {reference}")

        for relative in sorted(MARKDOWN_FILES):
            content = (SKILL_ROOT / relative).read_text(encoding="utf-8")
            if HANGUL_RE.search(content):
                fail(f"skills/use-dolgorae/{relative}: repository guidance must be English")
        with tempfile.TemporaryDirectory(prefix="dolgorae-installed-skill-") as temporary:
            installed = Path(temporary) / "use-dolgorae"
            subprocess.run(
                [sys.executable, str(ROOT / "tools/validators/package_agent_skill.py"), "install", "--destination", str(installed)],
                cwd=temporary, check=True, capture_output=True,
            )
            validate_installed_resources(installed)
    except (OSError, UnicodeError, ValueError, SkillValidationError,
            SchemaError, ValidationError, Unresolvable, subprocess.CalledProcessError) as error:
        print(f"Agent skill validation failed: {error}", file=sys.stderr)
        return 1

    print(f"Agent skill validation passed: {len(expected_files)} files; isolated installed resources resolved")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
