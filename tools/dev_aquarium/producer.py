"""Build an exact Dolgorae executable for Aquarium's development channel."""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import signal
import stat
import subprocess
import sys
import tarfile
import time
from collections.abc import Callable, Sequence
from pathlib import Path

import tomllib

DESCRIPTION_SCHEMA = "aquarium-dev-producer-description/v1"
MANIFEST_SCHEMA = "aquarium-dev-artifact-manifest/v1"
PROJECT_ID = "dolgorae"
ARTIFACT_KIND = "executable"
ARTIFACT_PATH = "bin/dolgorae"
BUILD_TIMEOUT_SECONDS = 30 * 60
TERM_GRACE_SECONDS = 5
REPOSITORY = Path(__file__).resolve().parents[2]


class ContractError(Exception):
    """A caller or repository precondition rejected before build admission."""


class BuildError(Exception):
    """An admitted producer build failed."""


def compact(document: dict[str, str]) -> str:
    return json.dumps(document, separators=(",", ":"), ensure_ascii=True) + "\n"


def package_version(repository: Path) -> str:
    document = tomllib.loads((repository / "Cargo.toml").read_text(encoding="utf-8"))
    version = document.get("package", {}).get("version")
    if not isinstance(version, str) or not version:
        raise ContractError("Cargo.toml package.version is missing")
    return version


def description(repository: Path = REPOSITORY) -> dict[str, str]:
    return {
        "schema": DESCRIPTION_SCHEMA,
        "project_id": PROJECT_ID,
        "next_version": f"v{package_version(repository)}",
        "artifact_kind": ARTIFACT_KIND,
        "artifact_path": ARTIFACT_PATH,
    }


def git(repository: Path, *arguments: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(repository), *arguments],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise ContractError(result.stderr.strip() or "Git inspection failed")
    return result.stdout.strip()


def clean_main_sha(repository: Path) -> str:
    if Path(git(repository, "rev-parse", "--show-toplevel")).resolve() != repository.resolve():
        raise ContractError("repository is not the canonical Git root")
    if git(repository, "symbolic-ref", "--quiet", "--short", "HEAD") != "main":
        raise ContractError("repository must be on local main")
    head = git(repository, "rev-parse", "HEAD")
    if head != git(repository, "rev-parse", "refs/heads/main"):
        raise ContractError("HEAD must equal refs/heads/main")
    if len(head) != 40 or any(character not in "0123456789abcdef" for character in head):
        raise ContractError("HEAD is not a full lowercase Git SHA")
    if git(repository, "status", "--porcelain=v1", "--untracked-files=all"):
        raise ContractError("repository worktree and index must be clean")
    return head


def validate_output(repository: Path, raw_output: str | None) -> tuple[Path, int, os.stat_result]:
    if not raw_output:
        raise ContractError("AQUARIUM_DEV_OUTPUT is required")
    output = Path(raw_output)
    if not output.is_absolute():
        raise ContractError("AQUARIUM_DEV_OUTPUT must be absolute")
    if output.is_symlink():
        raise ContractError("AQUARIUM_DEV_OUTPUT must not be a symbolic link")
    if not output.is_dir():
        raise ContractError("AQUARIUM_DEV_OUTPUT must be an existing directory")
    resolved = output.resolve(strict=True)
    repository_resolved = repository.resolve(strict=True)
    if resolved == repository_resolved or repository_resolved in resolved.parents:
        raise ContractError("AQUARIUM_DEV_OUTPUT must be outside the repository")
    output = resolved
    descriptor = os.open(output, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    identity = os.fstat(descriptor)
    if os.listdir(descriptor):
        os.close(descriptor)
        raise ContractError("AQUARIUM_DEV_OUTPUT must be empty")
    return output, descriptor, identity


def same_output_identity(output: Path, identity: os.stat_result) -> bool:
    current = os.stat(output, follow_symlinks=False)
    return stat.S_ISDIR(current.st_mode) and (current.st_dev, current.st_ino) == (
        identity.st_dev,
        identity.st_ino,
    )


def stop_process_group(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    deadline = time.monotonic() + TERM_GRACE_SECONDS
    while process.poll() is None and time.monotonic() < deadline:
        time.sleep(0.05)
    if process.poll() is None:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    process.wait()


def run_build(
    source: Path,
    build_root: Path,
    *,
    timeout: float = BUILD_TIMEOUT_SECONDS,
    command: Sequence[str] | None = None,
) -> None:
    environment = os.environ.copy()
    environment["CARGO_HOME"] = str(build_root / "cargo-home")
    environment["CARGO_TARGET_DIR"] = str(build_root / "target")
    process = subprocess.Popen(
        list(command or ["cargo", "build", "--locked", "--release", "--bin", PROJECT_ID]),
        cwd=source,
        env=environment,
        start_new_session=True,
    )
    try:
        return_code = process.wait(timeout=timeout)
    except (subprocess.TimeoutExpired, KeyboardInterrupt):
        stop_process_group(process)
        raise BuildError("release build timed out or was interrupted") from None
    if return_code != 0:
        raise BuildError(f"release build failed with exit {return_code}")


def export_source(repository: Path, git_sha: str, build_root: Path) -> Path:
    archive = build_root / "source.tar"
    result = subprocess.run(
        ["git", "-C", str(repository), "archive", "--format=tar", "--output", str(archive), git_sha],
        check=False,
    )
    if result.returncode != 0:
        raise BuildError("Git archive failed")
    source = build_root / "source"
    source.mkdir(mode=0o700)
    with tarfile.open(archive, "r:") as document:
        document.extractall(source, filter="data")
    archive.unlink()
    return source


def fsync_file(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def remove_at(parent_fd: int, name: str) -> None:
    """Remove one producer-owned entry without resolving the root pathname."""
    try:
        metadata = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
    except FileNotFoundError:
        return
    if stat.S_ISDIR(metadata.st_mode):
        child_fd = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent_fd)
        try:
            for child in os.listdir(child_fd):
                remove_at(child_fd, child)
        finally:
            os.close(child_fd)
        os.rmdir(name, dir_fd=parent_fd)
    else:
        os.unlink(name, dir_fd=parent_fd)


def build(
    repository: Path = REPOSITORY,
    raw_output: str | None = None,
    *,
    timeout: float = BUILD_TIMEOUT_SECONDS,
    command: Sequence[str] | None = None,
    phase_hook: Callable[[str], None] | None = None,
) -> dict[str, str]:
    git_sha = clean_main_sha(repository)
    output, output_fd, identity = validate_output(repository, raw_output)
    build_root = output / ".build"
    manifest_tmp = output / ".manifest.tmp"
    try:
        build_root.mkdir(mode=0o700)
        source = export_source(repository, git_sha, build_root)
        if phase_hook:
            phase_hook("source-exported")
        run_build(source, build_root, timeout=timeout, command=command)
        built = build_root / "target" / "release" / PROJECT_ID
        if built.is_symlink() or not built.is_file() or not os.access(built, os.X_OK):
            raise BuildError("release artifact is missing, invalid, or not executable")
        bin_directory = output / "bin"
        bin_directory.mkdir(mode=0o755)
        artifact = bin_directory / PROJECT_ID
        shutil.copyfile(built, artifact)
        os.chmod(artifact, 0o755)
        fsync_file(artifact)
        fsync_file(bin_directory)
        digest = hashlib.sha256(artifact.read_bytes()).hexdigest()
        manifest = {
            "schema": MANIFEST_SCHEMA,
            "project_id": PROJECT_ID,
            "git_sha": git_sha,
            "development_version": f"v{package_version(source)}-dev.{git_sha[:12]}",
            "artifact_kind": ARTIFACT_KIND,
            "artifact_path": ARTIFACT_PATH,
            "sha256": f"sha256:{digest}",
        }
        shutil.rmtree(build_root)
        if phase_hook:
            phase_hook("artifact-ready")
        if not same_output_identity(output, identity):
            raise BuildError("AQUARIUM_DEV_OUTPUT identity changed during build")
        with manifest_tmp.open("x", encoding="utf-8", newline="\n") as stream:
            stream.write(compact(manifest))
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(manifest_tmp, output / "manifest.json")
        os.fsync(output_fd)
        if sorted(path.relative_to(output).as_posix() for path in output.rglob("*") if path.is_file()) != [
            ARTIFACT_PATH,
            "manifest.json",
        ]:
            raise BuildError("producer output inventory is invalid")
        return manifest
    except Exception:
        for entry in (".manifest.tmp", ".build", "manifest.json", "bin"):
            remove_at(output_fd, entry)
        raise
    finally:
        os.close(output_fd)


def main(arguments: Sequence[str] | None = None, repository: Path = REPOSITORY) -> int:
    command = list(arguments if arguments is not None else sys.argv[1:])
    try:
        if command == ["describe"]:
            sys.stdout.write(compact(description(repository)))
            return 0
        if command == ["build"]:
            manifest = build(repository, os.environ.get("AQUARIUM_DEV_OUTPUT"))
            sys.stdout.write(compact(manifest))
            return 0
        raise ContractError("expected describe or build")
    except ContractError as error:
        print(f"dolgorae development producer: {error}", file=sys.stderr)
        return 2
    except (BuildError, OSError, subprocess.SubprocessError, tarfile.TarError) as error:
        print(f"dolgorae development producer: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
