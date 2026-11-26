#!/usr/bin/env python3
"""Offline unit tests for tools/check_v2_guards.py.

All negative fixtures are created exclusively inside tempfile.TemporaryDirectory()
outside the repository tree and require zero network access.
"""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

TOOLS_DIR = Path(__file__).resolve().parent
REPO_ROOT = TOOLS_DIR.parent
if str(TOOLS_DIR) not in sys.path:
    sys.path.insert(0, str(TOOLS_DIR))

import check_v2_guards as guards  # noqa: E402


def _write_minimal_workspace(
    root: Path,
    *,
    spi_deps: str = "",
    core_deps: str = 'dolgorae-spi = { path = "../dolgorae-spi" }\n',
    core_dev_deps: str = "",
    api_deps: str = 'dolgorae-core = { path = "../dolgorae-core" }\n',
    driver_deps: str = 'dolgorae-spi = { path = "../../dolgorae-spi" }\n',
    core_src: str = "pub fn ok() -> u32 { 1 }\n",
    driver_src: str = 'pub fn id() -> &\'static str { "mock" }\n',
    extra_crates: dict[str, tuple[str, str]] | None = None,
    omit_crates: set[str] | None = None,
) -> Path:
    """Create a self-contained offline 8-crate Cargo workspace in a temp directory."""
    omit = omit_crates or set()
    crates: dict[str, tuple[str, str]] = {
        "crates/dolgorae-spi": (
            f'[package]\nname = "dolgorae-spi"\nversion = "0.1.0"\nedition = "2024"\n\n[dependencies]\n{spi_deps}',
            "pub struct Spec;\n",
        ),
        "crates/dolgorae-core": (
            f'[package]\nname = "dolgorae-core"\nversion = "0.1.0"\nedition = "2024"\n\n[dependencies]\n{core_deps}\n[dev-dependencies]\n{core_dev_deps}',
            core_src,
        ),
        "crates/dolgorae-api": (
            f'[package]\nname = "dolgorae-api"\nversion = "0.1.0"\nedition = "2024"\n\n[dependencies]\n{api_deps}',
            "pub fn route() {}\n",
        ),
        "crates/dolgorae-mcp": (
            '[package]\nname = "dolgorae-mcp"\nversion = "0.1.0"\nedition = "2024"\n\n[dependencies]\ndolgorae-core = { path = "../dolgorae-core" }\n',
            "pub fn mcp() {}\n",
        ),
        "crates/dolgorae-cli": (
            '[package]\nname = "dolgorae-cli"\nversion = "0.1.0"\nedition = "2024"\n\n[dependencies]\ndolgorae-core = { path = "../dolgorae-core" }\ndriver-codex-app = { path = "../drivers/driver-codex-app" }\n',
            "pub fn cli() {}\n",
        ),
        "crates/drivers/driver-codex-app": (
            f'[package]\nname = "driver-codex-app"\nversion = "0.1.0"\nedition = "2024"\n\n[dependencies]\n{driver_deps}',
            driver_src,
        ),
        "crates/drivers/driver-claude-wire": (
            '[package]\nname = "driver-claude-wire"\nversion = "0.1.0"\nedition = "2024"\n\n[dependencies]\ndolgorae-spi = { path = "../../dolgorae-spi" }\n',
            'pub fn id() -> &\'static str { "claude-wire" }\n',
        ),
        "crates/drivers/driver-agy-acp": (
            '[package]\nname = "driver-agy-acp"\nversion = "0.1.0"\nedition = "2024"\n\n[dependencies]\ndolgorae-spi = { path = "../../dolgorae-spi" }\n',
            'pub fn id() -> &\'static str { "agy-acp" }\n',
        ),
    }

    for omitted in omit:
        crates.pop(omitted, None)
    if extra_crates:
        crates.update(extra_crates)

    members_toml = ",\n".join(f'    "{rel}"' for rel in crates)
    (root / "Cargo.toml").write_text(
        f'[workspace]\nresolver = "3"\nmembers = [\n{members_toml},\n]\n',
        encoding="utf-8",
    )

    for rel, (manifest, src) in crates.items():
        pkg_dir = root / rel
        (pkg_dir / "src").mkdir(parents=True, exist_ok=True)
        (pkg_dir / "Cargo.toml").write_text(manifest, encoding="utf-8")
        (pkg_dir / "src" / "lib.rs").write_text(src, encoding="utf-8")

    return root / "Cargo.toml"


class TestCheckV2Guards(unittest.TestCase):
    def test_real_repository_passes_offline(self) -> None:
        report = guards.check_workspace_guards(REPO_ROOT / "Cargo.toml", offline=True)
        self.assertEqual(report.violations, [], f"unexpected violations: {report.violations}")
        self.assertGreater(report.crate_prod_lines["dolgorae-spi"], 100)
        self.assertLessEqual(report.core_group_lines, guards.CORE_GROUP_BUDGET)
        for drv in ("driver-codex-app", "driver-claude-wire", "driver-agy-acp"):
            self.assertIn(drv, report.driver_lines)
            self.assertLessEqual(report.driver_lines[drv], guards.DRIVER_BUDGET)

    def test_rejects_unknown_extra_workspace_crate(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            manifest = _write_minimal_workspace(
                Path(tmp),
                extra_crates={
                    "crates/dolgorae-util": (
                        '[package]\nname = "dolgorae-util"\nversion = "0.1.0"\nedition = "2024"\n',
                        "pub fn hidden() {}\n",
                    )
                },
            )
            report = guards.check_workspace_guards(manifest, offline=True)
            joined = "\n".join(report.violations)
            self.assertIn("unexpected extra workspace crate(s): ['dolgorae-util']", joined)

    def test_rejects_missing_expected_workspace_crate(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            manifest = _write_minimal_workspace(
                Path(tmp),
                omit_crates={"crates/drivers/driver-agy-acp"},
            )
            report = guards.check_workspace_guards(manifest, offline=True)
            joined = "\n".join(report.violations)
            self.assertIn("missing expected workspace crate(s): ['driver-agy-acp']", joined)

    def test_rejects_core_to_driver_dependency_and_transitive_reachability(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            manifest = _write_minimal_workspace(
                Path(tmp),
                core_dev_deps='driver-codex-app = { path = "../drivers/driver-codex-app" }\n',
            )
            report = guards.check_workspace_guards(manifest, offline=True)
            joined = "\n".join(report.violations)
            self.assertIn("dolgorae-core must not reach driver crate driver-codex-app", joined)
            # api depends on core, so transitive closure must also catch dolgorae-api -> driver
            self.assertIn("dolgorae-api must not reach driver crate driver-codex-app", joined)

    def test_rejects_driver_depending_on_core(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            manifest = _write_minimal_workspace(
                Path(tmp),
                driver_deps=(
                    'dolgorae-spi = { path = "../../dolgorae-spi" }\n'
                    'dolgorae-core = { path = "../../dolgorae-core" }\n'
                ),
            )
            report = guards.check_workspace_guards(manifest, offline=True)
            joined = "\n".join(report.violations)
            self.assertIn("driver-codex-app may depend only on dolgorae-spi", joined)

    def test_rejects_spi_sibling_and_forbidden_heavy_dep(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            manifest = _write_minimal_workspace(
                Path(tmp),
                spi_deps='dolgorae-core = { path = "../dolgorae-core" }\nrusqlite = "0.37"\n',
                core_deps="",
            )
            report = guards.check_workspace_guards(manifest, offline=True)
            joined = "\n".join(report.violations)
            self.assertIn("dolgorae-spi must have zero workspace sibling deps", joined)
            self.assertIn("dolgorae-spi has forbidden heavy deps ['rusqlite']", joined)

    def test_rejects_non_test_code_after_cfg_test_mod(self) -> None:
        evasive_src = """pub fn before() -> u32 { 1 }

#[cfg(test)]
mod tests {
    #[test]
    fn inside() {
        assert_eq!(1, 1);
    }
}

pub fn snuck_after_tests() -> u32 { 2 }
"""
        with tempfile.TemporaryDirectory() as tmp:
            manifest = _write_minimal_workspace(Path(tmp), core_src=evasive_src)
            report = guards.check_workspace_guards(manifest, offline=True)
            joined = "\n".join(report.violations)
            self.assertIn("non-test item appears after #[cfg(test)] mod", joined)

    def test_rejects_line_budget_overflow(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            oversized_driver = "\n".join(f"// line {i}" for i in range(1_505)) + "\n"
            manifest = _write_minimal_workspace(Path(tmp), driver_src=oversized_driver)
            report = guards.check_workspace_guards(manifest, offline=True)
            joined = "\n".join(report.violations)
            self.assertIn("line budget exceeded: driver-codex-app", joined)

        with tempfile.TemporaryDirectory() as tmp:
            oversized_core = "\n".join(f"// line {i}" for i in range(50)) + "\n"
            manifest = _write_minimal_workspace(Path(tmp), core_src=oversized_core)
            report = guards.check_workspace_guards(manifest, offline=True, core_budget=20)
            joined = "\n".join(report.violations)
            self.assertIn("line budget exceeded: core/spi/api/mcp/cli", joined)

    def test_rejects_banned_terms_in_non_test_code_only(self) -> None:
        leaking_src = """pub fn leak() -> &'static str {
    "thread/start via ControllerKind"
}
"""
        with tempfile.TemporaryDirectory() as tmp:
            manifest = _write_minimal_workspace(Path(tmp), core_src=leaking_src)
            report = guards.check_workspace_guards(manifest, offline=True)
            joined = "\n".join(report.violations)
            self.assertIn("banned term 'thread/'", joined)
            self.assertIn("banned term 'ControllerKind'", joined)

        # Terms inside trailing #[cfg(test)] mod must not trigger false positives
        clean_prod_with_test_ref = """pub fn clean() -> u32 { 42 }

#[cfg(test)]
mod tests {
    #[test]
    fn mentions_wire_example() {
        let _ = "codex thread/start";
    }
}
"""
        with tempfile.TemporaryDirectory() as tmp:
            manifest = _write_minimal_workspace(Path(tmp), core_src=clean_prod_with_test_ref)
            report = guards.check_workspace_guards(manifest, offline=True)
            self.assertEqual(report.violations, [], f"unexpected violations: {report.violations}")


if __name__ == "__main__":
    unittest.main()
