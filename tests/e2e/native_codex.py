"""A compiled native Codex stand-in whose app-server is the ADR-014 fake.

SPEC-013 requires `argv[0]` to be a direct Codex executable: a `#!` script is
refused outright, because the kernel would exec an interpreter and the spawn
image could never be the configured one.  So the controllable behaviour lives
in Python but is reached through a compiled native image named `codex`, which
is what the registry validates and what `wait_for_app_identity` later observes.

The image answers three launches, and nothing else:

* `--version` reports the pinned Codex release, so profile compatibility is a
  fixed fact rather than whatever the machine happens to have.
* `app-server generate-json-schema` delegates to the locally installed exact
  Codex 0.149.0.  There is no checked schema bundle in-tree — only its digest —
  so the bundle has to come from the real release; this is setup, and it never
  answers a protocol call.
* `app-server --listen unix://…` runs the shared fake app-server on that
  socket.  Every `model/list` page a Run resolves against therefore comes from
  the independent fixture, never from the installed release.

The exec shim keeps the original argv, so the launched process still carries
`app-server` in its command line: the profile lifecycle waits for exactly that
fingerprint before it believes a server is up.
"""

from __future__ import annotations

import os
import pathlib
import shutil
import subprocess
import sys

PINNED_CODEX_VERSION = "0.149.0"
PINNED_CODEX_ENV = "DOLGORAE_TEST_CODEX_BIN"

REPOSITORY = pathlib.Path(__file__).resolve().parents[2]
FAKE_APP_SERVER = REPOSITORY / "tools" / "fake_app_server"
SCENARIO_ROOT = FAKE_APP_SERVER / "scenarios"

SHIM_SOURCE = """
#include <stdlib.h>
#include <unistd.h>

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
    execv(DRIVER_INTERPRETER, next);
    return 127;
}
"""

DRIVER_SOURCE = '''"""The behaviour behind the native image, reached through execv."""

import pathlib
import subprocess
import sys

SCHEMA_SOURCE = {schema_source}
FAKE_APP_SERVER = {fake_app_server}
SCENARIO = {scenario}
CODEX_HOME = {codex_home}
TRANSCRIPT = {transcript}
LISTEN_PREFIX = "unix://"

arguments = sys.argv[1:]
if arguments == ["--version"]:
    print("codex-cli {version}")
    raise SystemExit(0)
if "generate-json-schema" in arguments:
    raise SystemExit(subprocess.run([SCHEMA_SOURCE, *arguments], check=False).returncode)
if "app-server" in arguments and "--listen" in arguments:
    target = arguments[arguments.index("--listen") + 1]
    if not target.startswith(LISTEN_PREFIX):
        raise SystemExit(2)
    sys.path.insert(0, FAKE_APP_SERVER)
    import scenario as scenario_module
    import server as server_module

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


def installed_codex() -> pathlib.Path:
    """The locally installed exact Codex, or a failure that names why not.

    Schema generation is the one thing this fixture cannot fake: the profile
    contract pins the *digest* of the 0.149.0 bundle, not its contents, so a
    bundle from any other release — or a hand-written one — is refused. The
    prerequisite is therefore hard, and absent it the case fails rather than
    quietly proving something weaker.
    """
    override = os.environ.get(PINNED_CODEX_ENV)
    resolved = override or shutil.which("codex")
    if not resolved:
        raise AssertionError(
            f"Codex {PINNED_CODEX_VERSION} is required to generate the pinned "
            "app-server schema bundle; the live app-server is faked, the bundle "
            f"cannot be; set {PINNED_CODEX_ENV} to the exact executable"
        )
    codex = pathlib.Path(resolved).resolve()

    def reported_version(candidate: pathlib.Path) -> str:
        if not candidate.is_file():
            return "missing"
        completed = subprocess.run(
            [str(candidate), "--version"], check=False, capture_output=True, text=True
        )
        if completed.returncode != 0:
            return f"exit {completed.returncode}"
        return completed.stdout.strip()

    expected = f"codex-cli {PINNED_CODEX_VERSION}"
    reported = reported_version(codex)
    if reported == expected:
        return codex
    if override:
        raise AssertionError(
            f"{PINNED_CODEX_ENV} must report {expected!r}, got {reported!r} from {codex}"
        )

    # Codex standalone installations keep versioned releases beside the
    # `current` target. A host upgrade may advance PATH while the test's pinned
    # schema generator remains installed, so resolve that sibling explicitly
    # instead of asking users to downgrade their ordinary Codex CLI.
    release_root = codex.parent.parent.parent
    if release_root.name == "releases":
        pinned = sorted(
            release_root.glob(f"{PINNED_CODEX_VERSION}-*/bin/codex")
        )
        for candidate in pinned:
            candidate = candidate.resolve()
            if reported_version(candidate) == expected:
                return candidate

    raise AssertionError(
        f"expected {expected} for schema generation, got {reported!r} from {codex}; "
        f"install the pinned standalone release or set {PINNED_CODEX_ENV}"
    )


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
) -> None:
    """Compile a native `codex` at `path` that serves `scenario` when listening.

    `transcript`, when given, is where the fake appends every client message it
    received, one JSON line each. That is what lets a case assert which
    conversation actually reached the fixture rather than inferring it from the
    answers alone.
    """
    compiler = shutil.which("cc")
    if compiler is None:
        raise AssertionError(
            "a C compiler is required: argv[0] rejects interpreter scripts, so the "
            "fixture Codex must be a native executable"
        )
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
            version=PINNED_CODEX_VERSION,
        ),
        encoding="utf-8",
    )
    driver.chmod(0o644)
    source = path.with_name(f"{path.name}-shim.c")
    source.write_text(SHIM_SOURCE, encoding="utf-8")
    compiled = subprocess.run(
        [
            compiler,
            "-O0",
            f'-DDRIVER_INTERPRETER="{sys.executable}"',
            f'-DDRIVER_SCRIPT="{driver}"',
            "-o",
            str(path),
            str(source),
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    if compiled.returncode != 0:
        raise AssertionError(f"fixture Codex shim failed to compile: {compiled.stderr}")
    path.chmod(0o755)
