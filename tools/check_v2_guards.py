#!/usr/bin/env python3
"""Static architecture guards for Dolgorae v2.

Enforces three mechanical invariants using Python 3 stdlib only:
1. Workspace dependency boundaries over the full transitive closure of
   `cargo metadata --no-deps --format-version 1 --offline` across normal,
   dev, and build dependencies.
2. Evasion-proof physical production line budgets (core+spi+api+mcp+cli <= 15,000,
   each driver-* <= 1,500), rejecting any non-test item placed after a
   `#[cfg(test)] mod` block.
3. Scoped banned-term isolation on non-test production code in
   `dolgorae-spi`, `dolgorae-core`, `dolgorae-api`, and `dolgorae-mcp`.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from collections import deque
from dataclasses import dataclass
from pathlib import Path

CORE_GROUP_BUDGET = 15_000
DRIVER_BUDGET = 1_500

CORE_GROUP_CRATES = frozenset(
    {
        "dolgorae-spi",
        "dolgorae-core",
        "dolgorae-api",
        "dolgorae-mcp",
        "dolgorae-cli",
    }
)

EXPECTED_DRIVER_CRATES = frozenset(
    {
        "driver-codex-app",
        "driver-claude-wire",
        "driver-agy-acp",
    }
)

EXPECTED_WORKSPACE_CRATES = CORE_GROUP_CRATES | EXPECTED_DRIVER_CRATES

ISOLATED_FROM_DRIVERS = frozenset(
    {
        "dolgorae-spi",
        "dolgorae-core",
        "dolgorae-api",
        "dolgorae-mcp",
    }
)

SPI_FORBIDDEN_EXTERNAL_DEPS = frozenset(
    {
        "rusqlite",
        "axum",
        "hyper",
        "clap",
        "tonic",
        "prost",
    }
)

BANNED_TERM_PATTERNS: tuple[tuple[str, re.Pattern[str]], ...] = (
    ("codex", re.compile(r"codex", re.IGNORECASE)),
    ("claude", re.compile(r"claude", re.IGNORECASE)),
    ("agy", re.compile(r"(?:(?<![A-Za-z0-9])agy(?![a-z])|(?<![a-z])Agy(?![a-z]))")),
    ("acp", re.compile(r"(?:(?<![A-Za-z0-9])acp(?![a-z])|(?<![a-z])Acp(?![a-z]))")),
    ("app-server", re.compile(r"app-server", re.IGNORECASE)),
    ("thread/", re.compile(r"thread/")),
    ("session/prompt", re.compile(r"session/prompt")),
    ("Carrier", re.compile(r"\bCarrier\b")),
    ("ControllerKind", re.compile(r"\bControllerKind\b")),
    ("zcode", re.compile(r"zcode", re.IGNORECASE)),
    ("herdr_core", re.compile(r"herdr[_-]core", re.IGNORECASE)),
    ("libproc", re.compile(r"libproc", re.IGNORECASE)),
)


@dataclass(frozen=True)
class Token:
    kind: str  # "ident", "punct", "str", "char"
    text: str
    start_line: int
    end_line: int


@dataclass
class FileScanResult:
    path: Path
    total_lines: int
    prod_lines: int
    prod_text_by_line: list[tuple[int, str]]
    errors: list[str]


@dataclass
class GuardReport:
    crate_prod_lines: dict[str, int]
    core_group_lines: int
    driver_lines: dict[str, int]
    violations: list[str]


def tokenize_rust(source: str) -> list[Token]:
    """Tokenize Rust source while skipping comments and preserving string/char boundaries."""
    tokens: list[Token] = []
    n = len(source)
    i = 0
    line = 1

    while i < n:
        ch = source[i]

        # Whitespace
        if ch in " \t\r\n":
            if ch == "\n":
                line += 1
            i += 1
            continue

        # Line comment
        if ch == "/" and i + 1 < n and source[i + 1] == "/":
            i += 2
            while i < n and source[i] != "\n":
                i += 1
            continue

        # Nested block comment
        if ch == "/" and i + 1 < n and source[i + 1] == "*":
            depth = 1
            i += 2
            while i < n and depth > 0:
                if source[i] == "/" and i + 1 < n and source[i + 1] == "*":
                    depth += 1
                    i += 2
                elif source[i] == "*" and i + 1 < n and source[i + 1] == "/":
                    depth -= 1
                    i += 2
                else:
                    if source[i] == "\n":
                        line += 1
                    i += 1
            continue

        # Raw string literal: r"...", r#"..."#, br#"..."#, cr#"..."#
        raw_prefix = 0
        if ch == "r":
            raw_prefix = 1
        elif ch in ("b", "c") and i + 1 < n and source[i + 1] == "r":
            raw_prefix = 2

        if raw_prefix:
            j = i + raw_prefix
            hashes = 0
            while j < n and source[j] == "#":
                hashes += 1
                j += 1
            if j < n and source[j] == '"':
                start_line = line
                j += 1
                closing = '"' + ("#" * hashes)
                end_idx = source.find(closing, j)
                if end_idx == -1:
                    end_idx = n
                    next_i = n
                else:
                    next_i = end_idx + len(closing)
                line += source[i:next_i].count("\n")
                tokens.append(Token("str", source[i:next_i], start_line, line))
                i = next_i
                continue

        # Byte/C string literal: b"...", c"..."
        if ch in ("b", "c") and i + 1 < n and source[i + 1] == '"':
            start_line = line
            j = i + 2
            while j < n:
                if source[j] == "\\":
                    if j + 1 < n and source[j + 1] == "\n":
                        line += 1
                    j += 2
                    continue
                if source[j] == '"':
                    j += 1
                    break
                if source[j] == "\n":
                    line += 1
                j += 1
            tokens.append(Token("str", source[i:j], start_line, line))
            i = j
            continue

        # Normal string literal
        if ch == '"':
            start_line = line
            j = i + 1
            while j < n:
                if source[j] == "\\":
                    if j + 1 < n and source[j + 1] == "\n":
                        line += 1
                    j += 2
                    continue
                if source[j] == '"':
                    j += 1
                    break
                if source[j] == "\n":
                    line += 1
                j += 1
            tokens.append(Token("str", source[i:j], start_line, line))
            i = j
            continue

        # Byte char literal: b'x' or b'\x..'
        if ch == "b" and i + 1 < n and source[i + 1] == "'":
            start_line = line
            j = i + 2
            while j < n:
                if source[j] == "\\":
                    j += 2
                    continue
                if source[j] == "'":
                    j += 1
                    break
                if source[j] == "\n":
                    line += 1
                j += 1
            tokens.append(Token("char", source[i:j], start_line, line))
            i = j
            continue

        # Char literal vs lifetime ('a)
        if ch == "'":
            start_line = line
            if i + 1 < n and source[i + 1] == "\\":
                j = i + 2
                while j < n and source[j] != "'":
                    if source[j] == "\n":
                        line += 1
                    j += 1
                if j < n:
                    j += 1
                tokens.append(Token("char", source[i:j], start_line, line))
                i = j
                continue
            if i + 2 < n and source[i + 2] == "'" and source[i + 1] != "\n":
                tokens.append(Token("char", source[i : i + 3], start_line, line))
                i += 3
                continue
            # Lifetime token
            j = i + 1
            while j < n and (source[j].isalnum() or source[j] == "_"):
                j += 1
            tokens.append(Token("ident", source[i:j], start_line, line))
            i = j
            continue

        # Identifier or keyword
        if ch.isalpha() or ch == "_":
            j = i + 1
            while j < n and (source[j].isalnum() or source[j] == "_"):
                j += 1
            tokens.append(Token("ident", source[i:j], line, line))
            i = j
            continue

        # Single punctuation character
        tokens.append(Token("punct", ch, line, line))
        i += 1

    return tokens


def _match_delim(tokens: list[Token], idx: int, open_ch: str, close_ch: str) -> int:
    """Return index of matching closing delimiter starting at tokens[idx] == open_ch."""
    depth = 0
    i = idx
    while i < len(tokens):
        if tokens[i].kind == "punct":
            if tokens[i].text == open_ch:
                depth += 1
            elif tokens[i].text == close_ch:
                depth -= 1
                if depth == 0:
                    return i
        i += 1
    return len(tokens) - 1


def _is_cfg_test_attribute(tokens: list[Token], hash_idx: int) -> tuple[bool, int]:
    """Check if tokens[hash_idx..] is an outer `#[cfg(test)]` attribute.

    Returns (is_cfg_test, end_bracket_idx).
    """
    n = len(tokens)
    if hash_idx + 1 >= n or tokens[hash_idx].text != "#":
        return (False, hash_idx)
    if tokens[hash_idx + 1].text != "[":
        return (False, hash_idx)
    end_idx = _match_delim(tokens, hash_idx + 1, "[", "]")
    inner = [t.text for t in tokens[hash_idx + 2 : end_idx]]
    # Exact `#[cfg(test)]` or `#[cfg(all(..., test, ...))]` (simple forms)
    if inner == ["cfg", "(", "test", ")"]:
        return (True, end_idx)
    return (False, end_idx)


def _skip_outer_attributes(tokens: list[Token], idx: int) -> int:
    n = len(tokens)
    while idx + 1 < n and tokens[idx].text == "#" and tokens[idx + 1].text == "[":
        idx = _match_delim(tokens, idx + 1, "[", "]") + 1
    return idx


def _skip_item(tokens: list[Token], idx: int) -> tuple[bool, int]:
    """Advance over one Rust item starting at idx.

    Returns (is_mod_item, end_token_idx_inclusive).
    """
    n = len(tokens)
    is_mod = False
    i = idx

    # Optional visibility / qualifiers (`pub`, `pub(...)`, `unsafe`, `async`, `const`, `extern "..."`)
    while i < n:
        tok = tokens[i]
        if tok.kind == "ident" and tok.text == "pub":
            i += 1
            if i < n and tokens[i].text == "(":
                i = _match_delim(tokens, i, "(", ")") + 1
            continue
        if tok.kind == "ident" and tok.text in ("unsafe", "async", "const", "default"):
            i += 1
            continue
        if tok.kind == "ident" and tok.text == "extern":
            i += 1
            if i < n and tokens[i].kind == "str":
                i += 1
            continue
        break

    if i < n and tokens[i].kind == "ident" and tokens[i].text == "mod":
        is_mod = True

    # Walk until the top-level `;` or `{ ... }` block completing this item.
    paren_depth = 0
    bracket_depth = 0
    while i < n:
        tok = tokens[i]
        if tok.kind == "punct":
            if tok.text == "(":
                paren_depth += 1
            elif tok.text == ")":
                paren_depth = max(0, paren_depth - 1)
            elif tok.text == "[":
                bracket_depth += 1
            elif tok.text == "]":
                bracket_depth = max(0, bracket_depth - 1)
            elif paren_depth == 0 and bracket_depth == 0:
                if tok.text == ";":
                    return (is_mod, i)
                if tok.text == "{":
                    end_brace = _match_delim(tokens, i, "{", "}")
                    return (is_mod, end_brace)
        i += 1
    return (is_mod, max(idx, n - 1))


def scan_rust_file(path: Path) -> FileScanResult:
    """Count physical production lines, exclude brace-matched #[cfg(test)] items,
    and fail if any non-test item appears after a top-level `#[cfg(test)] mod`."""
    source = path.read_text(encoding="utf-8")
    raw_lines = source.splitlines()
    total_lines = len(raw_lines)
    tokens = tokenize_rust(source)

    excluded_lines: set[int] = set()
    errors: list[str] = []

    brace_depth = 0
    seen_cfg_test_mod = False
    first_cfg_test_mod_line = 0
    i = 0
    n = len(tokens)

    while i < n:
        tok = tokens[i]

        # Track nested braces for non-test top-level items
        if brace_depth > 0:
            if tok.kind == "punct":
                if tok.text == "{":
                    brace_depth += 1
                elif tok.text == "}":
                    brace_depth -= 1
            i += 1
            continue

        # Top-level (`brace_depth == 0`)
        # Inner attribute `#![...]`
        if tok.text == "#" and i + 1 < n and tokens[i + 1].text == "!":
            if seen_cfg_test_mod:
                errors.append(
                    f"{path}:{tok.start_line}: non-test item appears after #[cfg(test)] mod "
                    f"(started at line {first_cfg_test_mod_line})"
                )
            if i + 2 < n and tokens[i + 2].text == "[":
                i = _match_delim(tokens, i + 2, "[", "]") + 1
            else:
                i += 1
            continue

        # Outer attribute block(s)
        if tok.text == "#" and i + 1 < n and tokens[i + 1].text == "[":
            attr_start_idx = i
            has_cfg_test = False
            j = i
            while j + 1 < n and tokens[j].text == "#" and tokens[j + 1].text == "[":
                is_test_attr, end_attr = _is_cfg_test_attribute(tokens, j)
                if is_test_attr:
                    has_cfg_test = True
                j = end_attr + 1

            if has_cfg_test:
                item_start_line = tokens[attr_start_idx].start_line
                is_mod, item_end_idx = _skip_item(tokens, j)
                item_end_line = tokens[item_end_idx].end_line
                for ln in range(item_start_line, item_end_line + 1):
                    excluded_lines.add(ln)
                if is_mod:
                    if not seen_cfg_test_mod:
                        first_cfg_test_mod_line = item_start_line
                    seen_cfg_test_mod = True
                i = item_end_idx + 1
                continue

            if seen_cfg_test_mod:
                errors.append(
                    f"{path}:{tok.start_line}: non-test item appears after #[cfg(test)] mod "
                    f"(started at line {first_cfg_test_mod_line})"
                )
            i = j
            continue

        # Non-attribute top-level token
        if seen_cfg_test_mod:
            errors.append(
                f"{path}:{tok.start_line}: non-test item appears after #[cfg(test)] mod "
                f"(started at line {first_cfg_test_mod_line})"
            )
            # Advance past this offending item so we don't spam per-token errors.
            _, end_idx = _skip_item(tokens, i)
            i = end_idx + 1
            continue

        if tok.kind == "punct" and tok.text == "{":
            brace_depth += 1
        i += 1

    prod_text_by_line = [
        (idx + 1, text)
        for idx, text in enumerate(raw_lines)
        if (idx + 1) not in excluded_lines
    ]
    return FileScanResult(
        path=path,
        total_lines=total_lines,
        prod_lines=len(prod_text_by_line),
        prod_text_by_line=prod_text_by_line,
        errors=errors,
    )


def collect_crate_prod_files(crate_dir: Path) -> list[Path]:
    """Collect production Rust source files for a crate, strictly excluding tests/."""
    files: list[Path] = []
    for subdir in ("src", "examples", "benches"):
        base = crate_dir / subdir
        if base.is_dir():
            files.extend(sorted(base.rglob("*.rs")))
    build_rs = crate_dir / "build.rs"
    if build_rs.is_file():
        files.append(build_rs)
    return sorted(files)


def load_cargo_metadata(manifest_path: Path, offline: bool = True) -> dict:
    cmd = [
        "cargo",
        "metadata",
        "--no-deps",
        "--format-version",
        "1",
        "--manifest-path",
        str(manifest_path),
    ]
    if offline:
        cmd.append("--offline")
    proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if proc.returncode != 0:
        raise RuntimeError(
            f"cargo metadata failed (exit {proc.returncode}): {proc.stderr.strip()}"
        )
    return json.loads(proc.stdout)


def compute_transitive_closure(
    graph: dict[str, set[str]], start: str
) -> dict[str, list[str]]:
    """Return shortest path from `start` to each reachable node in `graph`."""
    visited: dict[str, list[str]] = {}
    queue: deque[tuple[str, list[str]]] = deque()
    for neighbor in sorted(graph.get(start, ())):
        visited[neighbor] = [start, neighbor]
        queue.append((neighbor, [start, neighbor]))
    while queue:
        node, path = queue.popleft()
        for nxt in sorted(graph.get(node, ())):
            if nxt not in visited:
                visited[nxt] = path + [nxt]
                queue.append((nxt, path + [nxt]))
    return visited


def check_workspace_guards(
    manifest_path: Path,
    *,
    offline: bool = True,
    core_budget: int = CORE_GROUP_BUDGET,
    driver_budget: int = DRIVER_BUDGET,
) -> GuardReport:
    metadata = load_cargo_metadata(manifest_path, offline=offline)
    member_ids = set(metadata.get("workspace_members", []))
    packages = [p for p in metadata.get("packages", []) if p["id"] in member_ids]

    by_name: dict[str, dict] = {}
    by_dir: dict[Path, str] = {}
    for pkg in packages:
        name = pkg["name"]
        pkg_dir = Path(pkg["manifest_path"]).parent.resolve()
        by_name[name] = pkg
        by_dir[pkg_dir] = name

    violations: list[str] = []

    # 0. Enforce exact 8-crate workspace membership (prevents unbudgeted/unscanned extra crates)
    actual_crates = set(by_name.keys())
    extra_crates = sorted(actual_crates - EXPECTED_WORKSPACE_CRATES)
    missing_crates = sorted(EXPECTED_WORKSPACE_CRATES - actual_crates)
    if extra_crates:
        violations.append(
            f"workspace membership: unexpected extra workspace crate(s): {extra_crates}"
        )
    if missing_crates:
        violations.append(
            f"workspace membership: missing expected workspace crate(s): {missing_crates}"
        )

    # 1. Build workspace dependency graph across ALL dep kinds (normal, dev, build)
    ws_graph: dict[str, set[str]] = {name: set() for name in by_name}
    direct_all_deps: dict[str, set[str]] = {name: set() for name in by_name}

    for name, pkg in by_name.items():
        for dep in pkg.get("dependencies", []):
            dep_name = dep["name"]
            direct_all_deps[name].add(dep_name)
            dep_path = dep.get("path")
            target_ws_name: str | None = None
            if dep_path:
                resolved = Path(dep_path).resolve()
                if resolved in by_dir:
                    target_ws_name = by_dir[resolved]
            if target_ws_name is None and dep_name in by_name:
                target_ws_name = dep_name
            if target_ws_name is not None:
                ws_graph[name].add(target_ws_name)

    # Rule 1a: dolgorae-spi has zero workspace sibling deps and zero forbidden heavy deps
    if "dolgorae-spi" in by_name:
        spi_ws_deps = sorted(ws_graph["dolgorae-spi"])
        if spi_ws_deps:
            violations.append(
                f"dependency boundary: dolgorae-spi must have zero workspace sibling deps, found {spi_ws_deps}"
            )
        spi_forbidden = sorted(direct_all_deps["dolgorae-spi"] & SPI_FORBIDDEN_EXTERNAL_DEPS)
        if spi_forbidden:
            violations.append(
                f"dependency boundary: dolgorae-spi has forbidden heavy deps {spi_forbidden}"
            )

    # Rule 1b: spi/core/api/mcp cannot reach any driver-* crate transitively
    driver_crates = sorted(name for name in by_name if name.startswith("driver-"))
    for crate in sorted(ISOLATED_FROM_DRIVERS):
        if crate not in by_name:
            continue
        reachable = compute_transitive_closure(ws_graph, crate)
        for drv in driver_crates:
            if drv in reachable:
                chain = " -> ".join(reachable[drv])
                violations.append(
                    f"dependency boundary: {crate} must not reach driver crate {drv} (path: {chain})"
                )

    # Rule 1c: driver-* crates can depend ONLY on dolgorae-spi among workspace crates
    for drv in driver_crates:
        reachable = compute_transitive_closure(ws_graph, drv)
        illegal = sorted(target for target in reachable if target != "dolgorae-spi")
        if illegal:
            chains = [f"{t} ({' -> '.join(reachable[t])})" for t in illegal]
            violations.append(
                f"dependency boundary: {drv} may depend only on dolgorae-spi among workspace crates, "
                f"found {', '.join(chains)}"
            )

    # 2. Evasion-proof physical production line counting + scoped banned-term checks
    crate_prod_lines: dict[str, int] = {}
    for name, pkg in sorted(by_name.items()):
        crate_dir = Path(pkg["manifest_path"]).parent.resolve()
        prod_total = 0
        for rs_file in collect_crate_prod_files(crate_dir):
            scan = scan_rust_file(rs_file)
            prod_total += scan.prod_lines
            violations.extend(scan.errors)

            if name in ISOLATED_FROM_DRIVERS:
                for line_no, line_text in scan.prod_text_by_line:
                    for term, pattern in BANNED_TERM_PATTERNS:
                        if pattern.search(line_text):
                            violations.append(
                                f"banned term '{term}' in {name} non-test code at "
                                f"{rs_file}:{line_no}: {line_text.strip()}"
                            )
        crate_prod_lines[name] = prod_total

    core_group_lines = sum(
        crate_prod_lines.get(c, 0) for c in CORE_GROUP_CRATES if c in crate_prod_lines
    )
    if core_group_lines > core_budget:
        violations.append(
            f"line budget exceeded: core/spi/api/mcp/cli = {core_group_lines} > {core_budget} prod lines"
        )

    driver_lines = {drv: crate_prod_lines.get(drv, 0) for drv in driver_crates}
    for drv, count in sorted(driver_lines.items()):
        if count > driver_budget:
            violations.append(
                f"line budget exceeded: {drv} = {count} > {driver_budget} prod lines"
            )

    return GuardReport(
        crate_prod_lines=crate_prod_lines,
        core_group_lines=core_group_lines,
        driver_lines=driver_lines,
        violations=violations,
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Dolgorae v2 static architecture guards")
    parser.add_argument(
        "--manifest-path",
        type=Path,
        default=Path(__file__).resolve().parent.parent / "Cargo.toml",
        help="Path to workspace Cargo.toml",
    )
    parser.add_argument(
        "--allow-online",
        action="store_true",
        help="Allow cargo metadata without --offline (disabled by default)",
    )
    args = parser.parse_args(argv)

    try:
        report = check_workspace_guards(
            args.manifest_path.resolve(),
            offline=not args.allow_online,
        )
    except Exception as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 2

    print("Dolgorae v2 production line counts:")
    for crate, count in sorted(report.crate_prod_lines.items()):
        print(f"  {crate:22s} {count:6d}")
    print(
        f"  {'CORE GROUP TOTAL':22s} {report.core_group_lines:6d} / {CORE_GROUP_BUDGET}"
    )
    for drv, count in sorted(report.driver_lines.items()):
        print(f"  {drv + ' (budget)':22s} {count:6d} / {DRIVER_BUDGET}")

    if report.violations:
        print("\nGuard violations:", file=sys.stderr)
        for v in report.violations:
            print(f"  - {v}", file=sys.stderr)
        return 1

    print("All Dolgorae v2 guards passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
