"""Prepare synthetic historical records with a test-only Rust harness.

The product CLI keeps its cutover guards. All profile executables and homes
must be inside the caller-owned fixture; this helper cannot use a real account.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def test_binary() -> Path:
    manifest = Path(os.environ.get("CARGO_TARGET_DIR", str(ROOT / "target"))) / "accepted-fixture-artifacts.json"
    for line in manifest.read_text().splitlines():
        value = json.loads(line)
        if value.get("reason") == "compiler-artifact" and value.get("executable") \
                and value.get("target", {}).get("name") == "accepted_legacy_cli":
            return Path(value["executable"]).resolve(strict=True)
    raise RuntimeError("historical fixture test binary was not prepared")


def prepare(binary: Path, home: Path, arguments: list[str], *, input: str | None = None,
            pass_fds: tuple[int, ...] = (), env: dict | None = None,
            timeout: float | None = None) -> subprocess.CompletedProcess[str]:
    argv, environment, request = command(home, arguments, env)
    try:
        completed = subprocess.run(argv, env=environment, input=input,
                                   pass_fds=pass_fds, capture_output=True, text=True, timeout=timeout)
        return subprocess.CompletedProcess([str(binary), *arguments], completed.returncode,
                                           completed.stdout, completed.stderr)

    finally:
        request.unlink(missing_ok=True)


def command(home: Path, arguments: list[str], env: dict | None = None) -> tuple[list[str], dict, Path]:
    """Return a direct fixture process for disconnect and signal tests.

    The request remains within its disposable fixture until the test cleans it.
    """
    root = home.resolve(strict=True).parent
    executable = root / "accepted-legacy-dolgorae"
    if not executable.exists():
        temporary_fd, temporary_name = tempfile.mkstemp(prefix=".accepted-binary-", dir=root)
        temporary = Path(temporary_name)
        try:
            with os.fdopen(temporary_fd, "wb") as output, test_binary().open("rb") as source:
                shutil.copyfileobj(source, output)
            temporary.chmod(0o700)
            try:
                os.link(temporary, executable)
            except FileExistsError:
                pass
        finally:
            temporary.unlink(missing_ok=True)
    descriptor, filename = tempfile.mkstemp(prefix="accepted-legacy-", suffix=".json", dir=root)
    request = Path(filename)
    with os.fdopen(descriptor, "w") as file:
        json.dump({"root": str(root), "arguments": arguments}, file)
    environment = dict(os.environ) if env is None else dict(env)
    environment.update(HOME=str(home), DOLGORAE_ACCEPTED_FIXTURE_REQUEST=str(request))
    return [str(executable), *arguments], environment, request


def creates_historical_record(arguments: list[str]) -> bool:
    return (arguments[:2] == ["review-target", "capture"]
            or arguments[:2] == ["specialist", "review"]
            or arguments[:3] in (["specialist", "policy", "add"], ["specialist", "policy", "validate"]))


if __name__ == "__main__":
    import argparse
    import sys

    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--home", type=Path, required=True)
    parser.add_argument("arguments", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    arguments = args.arguments[1:] if args.arguments[:1] == ["--"] else args.arguments
    completed = prepare(args.binary, args.home, arguments,
                        input=sys.stdin.read() if not sys.stdin.isatty() else None)
    print(completed.stdout, end="")
    raise SystemExit(completed.returncode)
