#!/usr/bin/env python3
"""Validate local Markdown links and balanced fenced code blocks."""

from __future__ import annotations

import re
import sys
from pathlib import Path
from urllib.parse import unquote

ROOT = Path(__file__).resolve().parents[2]
LINK_RE = re.compile(r"(?<!!)\[[^\]]*\]\(([^)]+)\)")


def main() -> int:
    errors: list[str] = []
    checked_links = 0
    checked_files = 0
    for path in sorted((ROOT / "docs").rglob("*.md")):
        checked_files += 1
        text = path.read_text(encoding="utf-8")
        fence: tuple[str, int] | None = None
        for lineno, line in enumerate(text.splitlines(), 1):
            stripped = line.lstrip()
            match = re.match(r"(`{3,}|~{3,})", stripped)
            if match:
                marker = match.group(1)
                family = marker[0]
                if fence is None:
                    fence = (family, len(marker))
                elif family == fence[0] and len(marker) >= fence[1]:
                    fence = None
        if fence is not None:
            errors.append(f"{path.relative_to(ROOT)}: unclosed fenced code block")

        for match in LINK_RE.finditer(text):
            raw_target = match.group(1).strip()
            if raw_target.startswith("<") and raw_target.endswith(">"):
                raw_target = raw_target[1:-1]
            target = raw_target.split(maxsplit=1)[0]
            if not target or target.startswith(("#", "http://", "https://", "mailto:")):
                continue
            target = unquote(target.split("#", 1)[0].split("?", 1)[0])
            if not target:
                continue
            checked_links += 1
            resolved = (path.parent / target).resolve()
            try:
                resolved.relative_to(ROOT.resolve())
            except ValueError:
                errors.append(f"{path.relative_to(ROOT)}: link escapes package root: {raw_target}")
                continue
            if not resolved.exists():
                line = text.count("\n", 0, match.start()) + 1
                errors.append(f"{path.relative_to(ROOT)}:{line}: unresolved local link: {raw_target}")

    if errors:
        print("Markdown validation failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print(f"Markdown validation passed: {checked_files} files, {checked_links} local links")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
