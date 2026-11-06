"""A compiled native Codex stand-in whose app-server is the ADR-014 fake.

SPEC-013 requires `argv[0]` to be a direct Codex executable: a `#!` script is
refused outright, because the kernel would exec an interpreter and the spawn
image could never be the configured one.  So the controllable behaviour lives
in embedded Python inside a compiled native image named `codex`, which
is what the registry validates and what `wait_for_app_identity` later observes.

The image answers three launches, and nothing else:

* `--version` reports the selected schema producer's actual Codex release.
* `app-server generate-json-schema` delegates to that same installed executable,
  which must meet the Profile's 0.158.0 minimum. The Profile retains exact bundle
  checks for every registered tested release and required-subset checks for all
  releases. Schema
  generation is offline setup and never answers a protocol call.
* `app-server --listen unix://…` runs the shared fake app-server on that
  socket.  Every `model/list` page a Run resolves against therefore comes from
  the independent fixture, never from the installed release.

The native image remains resident and keeps the original argv, so both the
process executable identity and the `app-server` command-line fingerprint
remain valid throughout profile lifecycle and workload-absence checks.
"""

from __future__ import annotations

import json
import os
import pathlib
import re
import shlex
import shutil
import subprocess
import sys
import sysconfig

MINIMUM_CODEX_VERSION = "0.158.0"
TEST_CODEX_ENV = "DOLGORAE_TEST_CODEX_BIN"

REPOSITORY = pathlib.Path(__file__).resolve().parents[2]
_COMPATIBILITY_MANIFEST = json.loads(
    (REPOSITORY / "docs/protocol/codex-0.157.0-required-subset.json").read_text(encoding="utf-8")
)
TESTED_CODEX_VERSIONS = {
    tuple(map(int, schema["version"].split(".")))
    for schema in [
        _COMPATIBILITY_MANIFEST["pinned_runtime_schema"],
        *_COMPATIBILITY_MANIFEST["verified_runtime_schemas"],
    ]
}
FAKE_APP_SERVER = REPOSITORY / "tools" / "fake_app_server"
SCENARIO_ROOT = FAKE_APP_SERVER / "scenarios"

SHIM_SOURCE = """
#include <Python.h>
#include <stdlib.h>

int main(int argc, char **argv) {
    char **next = malloc(sizeof(char *) * (size_t)(argc + 2));
    if (next == NULL) {
        return 127;
    }
    next[0] = (char *)DRIVER_INTERPRETER;
    next[1] = (char *)DRIVER_SCRIPT;
    for (int index = 1; index < argc; index++) {
        next[index + 1] = argv[index];
    }
    next[argc + 1] = NULL;
    int result = Py_BytesMain(argc + 1, next);
    free(next);
    return result;
}
"""

DRIVER_SOURCE = '''"""The behaviour hosted inside the resident native image."""

import pathlib
import subprocess
import sys

SCHEMA_SOURCE = {schema_source}
FAKE_APP_SERVER = {fake_app_server}
SCENARIO = {scenario}
CODEX_HOME = {codex_home}
TRANSCRIPT = {transcript}
TURN_GATE = {turn_gate}
LISTEN_PREFIX = "unix://"

arguments = sys.argv[1:]
if arguments == ["--version"]:
    print("codex-cli {version}")
    raise SystemExit(0)
if "generate-json-schema" in arguments:
    raise SystemExit(subprocess.run([SCHEMA_SOURCE, *arguments], check=False, timeout=60).returncode)
if "app-server" in arguments and "--listen" in arguments:
    target = arguments[arguments.index("--listen") + 1]
    if not target.startswith(LISTEN_PREFIX):
        raise SystemExit(2)
    sys.path.insert(0, FAKE_APP_SERVER)
    import scenario as scenario_module
    import server as server_module

    if TURN_GATE is not None:
        import time
        original_dispatch = server_module.FakeAppServer.dispatch

        def gated_dispatch(server, connection, message):
            if message.get("method") == "turn/start":
                reached, release = map(pathlib.Path, TURN_GATE)
                reached.write_text("turn/start received\\n", encoding="utf-8")
                deadline = time.monotonic() + 60
                while not release.exists():
                    if time.monotonic() >= deadline:
                        raise TimeoutError("fixture Turn gate was not released")
                    time.sleep(0.01)
            return original_dispatch(server, connection, message)

        server_module.FakeAppServer.dispatch = gated_dispatch

    fake = server_module.FakeAppServer(
        pathlib.Path(target[len(LISTEN_PREFIX) :]),
        scenario_module.Scenario.load(pathlib.Path(SCENARIO), {{"codex_home": CODEX_HOME}}),
        None,
        pathlib.Path(TRANSCRIPT) if TRANSCRIPT else None,
    )
    fake.bind()
    fake.serve_forever()
    raise SystemExit(0)
raise SystemExit(2)
'''


def _quoted(value: object) -> str:
    """A Python string literal for a path this fixture controls."""
    text = str(value)
    if "\\" in text or '"' in text or "\n" in text:
        raise AssertionError(f"fixture path is not embeddable: {text!r}")
    return f'"{text}"'


def codex_version(executable: pathlib.Path) -> str:
    """Read the schema producer's actual release, without starting an app-server."""
    if not executable.is_file():
        raise AssertionError(f"schema generator is unavailable: {executable}")
    completed = subprocess.run(
        [str(executable), "--version"], check=False, capture_output=True, text=True, timeout=10
    )
    reported = completed.stdout.strip()
    version = re.fullmatch(r"codex-cli ([0-9]+\.[0-9]+\.[0-9]+)", reported)
    if completed.returncode != 0 or version is None:
        raise AssertionError(
            f"invalid Codex --version from {executable}: "
            f"exit {completed.returncode}, {reported!r}"
        )
    return version.group(1)


def installed_codex() -> pathlib.Path:
    """Select the current schema producer under the Profile's minimum-version rule."""
    resolved = os.environ.get(TEST_CODEX_ENV) or shutil.which("codex")
    if not resolved:
        raise AssertionError(
            f"Codex >= {MINIMUM_CODEX_VERSION} is required for offline schema "
            f"generation; install Codex or set {TEST_CODEX_ENV}"
        )
    codex = pathlib.Path(resolved).resolve()
    version = codex_version(codex)
    if tuple(map(int, version.split("."))) < tuple(map(int, MINIMUM_CODEX_VERSION.split("."))):
        raise AssertionError(
            f"Codex >= {MINIMUM_CODEX_VERSION} is required for offline schema "
            f"generation, got {version!r} from {codex}"
        )
    return codex


def scenario_path(name: str) -> pathlib.Path:
    path = SCENARIO_ROOT / name
    if not path.is_file():
        raise AssertionError(f"the fake app-server scenario is missing: {path}")
    return path


def create_native_codex(
    path: pathlib.Path,
    *,
    scenario: pathlib.Path,
    codex_home: pathlib.Path,
    schema_source: pathlib.Path,
    transcript: pathlib.Path | None = None,
    turn_gate: tuple[pathlib.Path, pathlib.Path] | None = None,
) -> None:
    """Compile a native `codex` at `path` that serves `scenario` when listening.

    `transcript`, when given, is where the fake appends every client message it
    received, one JSON line each. That is what lets a case assert which
    conversation actually reached the fixture rather than inferring it from the
    answers alone.

    `turn_gate` optionally names reached/release files for a bounded test barrier
    immediately before the fake answers `turn/start`.
    """
    driver = path.with_name(f"{path.name}-driver.py")
    driver.write_text(
        DRIVER_SOURCE.format(
            schema_source=_quoted(schema_source),
            fake_app_server=_quoted(FAKE_APP_SERVER),
            scenario=_quoted(scenario),
            # The account home the fixture reports is the canonical one: the
            # profile contract pins `canonical_codex_home`, and a server that
            # answered with the caller's unresolved spelling would be refused
            # as a different account home.
            codex_home=_quoted(codex_home.resolve()),
            transcript="None" if transcript is None else _quoted(transcript),
            turn_gate="None" if turn_gate is None else f"({_quoted(turn_gate[0])}, {_quoted(turn_gate[1])})",
            version=codex_version(schema_source),
        ),
        encoding="utf-8",
    )
    driver.chmod(0o644)
    compile_native_driver(path, driver)


def compile_native_driver(path: pathlib.Path, driver: pathlib.Path) -> None:
    """Keep a fixture Python driver inside its configured native process image."""
    # Historical fixture admission compares the embedded path with the resolved
    # executable's sibling driver, including when TMPDIR has a symlink parent.
    driver = driver.resolve(strict=True)
    compiler = shutil.which("cc")
    if compiler is None:
        raise AssertionError(
            "a C compiler is required: argv[0] rejects interpreter scripts, so the "
            "fixture Codex must be a native executable"
        )
    source = path.with_name(f"{path.name}-shim.c")
    source.write_text(SHIM_SOURCE, encoding="utf-8")
    python_config = pathlib.Path(sysconfig.get_config_var("BINDIR")) / (
        f"python{sysconfig.get_config_var('VERSION')}-config"
    )
    embedding_flags = shlex.split(
        subprocess.check_output(
            [str(python_config), "--embed", "--cflags", "--ldflags"], text=True, timeout=10
        )
    )
    compiled = subprocess.run(
        [
            compiler,
            "-O0",
            f'-DDRIVER_INTERPRETER="{sys.executable}"',
            f'-DDRIVER_SCRIPT="{driver}"',
            "-o",
            str(path),
            str(source),
            *embedding_flags,
        ],
        check=False,
        capture_output=True,
        text=True,
        timeout=60,
    )
    if compiled.returncode != 0:
        raise AssertionError(f"fixture Codex shim failed to compile: {compiled.stderr}")
    path.chmod(0o755)
