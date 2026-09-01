#!/usr/bin/env python3
"""Validate source-distributed agent skill structure and basic metadata."""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SKILL_ROOT = ROOT / "skills" / "use-dolgorae"
EXPECTED_FILES = {
    "SKILL.md",
    "references/configuration.md",
    "references/lifecycle.md",
    "references/recovery.md",
}
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


def main() -> int:
    try:
        if not SKILL_ROOT.is_dir():
            fail("skills/use-dolgorae: missing skill directory")
        validate_path(SKILL_ROOT)

        actual_files = source_files()
        if actual_files != EXPECTED_FILES:
            fail(
                "skills/use-dolgorae: file set differs from the contract: "
                f"expected {sorted(EXPECTED_FILES)}, got {sorted(actual_files)}"
            )

        entrypoint = (SKILL_ROOT / "SKILL.md").read_text(encoding="utf-8")
        validate_frontmatter(entrypoint)
        for reference in sorted(EXPECTED_FILES - {"SKILL.md"}):
            target = reference.removeprefix("references/")
            if f"](references/{target})" not in entrypoint:
                fail(f"skills/use-dolgorae/SKILL.md: unlinked reference {reference}")

        for relative in sorted(EXPECTED_FILES):
            content = (SKILL_ROOT / relative).read_text(encoding="utf-8")
            if HANGUL_RE.search(content):
                fail(f"skills/use-dolgorae/{relative}: repository guidance must be English")
    except (OSError, UnicodeError, SkillValidationError) as error:
        print(f"Agent skill validation failed: {error}", file=sys.stderr)
        return 1

    print(f"Agent skill validation passed: {len(EXPECTED_FILES)} files")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
