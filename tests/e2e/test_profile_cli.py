#!/usr/bin/env python3
"""Black-box Runtime Profile and Codex compatibility validation."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import shutil
import stat
import subprocess
import sys
import tempfile

from schema_support import assert_valid, validator

# Mirrors src/profile.rs's PROFILE_CAPABILITY_NAMES: the closed set of
# profile-specific Codex capability names `profile doctor` and `profile show`
# must always report, proven or explicitly unverified.
CLOSED_CAPABILITY_NAMES = {
    "account_read",
    "app_server_initialize",
    "early_response_id",
    "model_list",
    "native_subagent_lifecycle",
    "thread_absence_error",
}


def run(
    binary: pathlib.Path,
    home: pathlib.Path,
    *arguments: str,
    extra_environment: dict[str, str] | None = None,
    pass_fds: tuple[int, ...] = (),
) -> subprocess.CompletedProcess[str]:
    environment = os.environ.copy()
    environment["HOME"] = str(home)
    if extra_environment:
        environment.update(extra_environment)
    return subprocess.run(
        [str(binary), *arguments],
        check=False,
        capture_output=True,
        text=True,
        env=environment,
        pass_fds=pass_fds,
    )


def envelope(completed: subprocess.CompletedProcess[str]) -> dict[str, object]:
    if completed.stderr:
        raise AssertionError(f"unexpected stderr: {completed.stderr!r}")
    return json.loads(completed.stdout)


# SPEC-013 requires argv[0] to be a direct Codex executable and rejects shell
# interpreters and arbitrary wrappers, so the fixture Codex can no longer be a
# `#!`-prefixed script: the kernel would exec an interpreter and the spawn
# image could never be the configured executable. The controllable behaviour
# still lives in Python, but it is reached through a compiled native image
# named `codex`, which is what the registry validates.
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


def create_fake_codex(path: pathlib.Path, real_codex: pathlib.Path) -> None:
    compiler = shutil.which("cc")
    if compiler is None:
        raise AssertionError(
            "a C compiler is required: the profile fixture Codex must be a native "
            "executable now that argv[0] rejects interpreter scripts"
        )
    driver = path.with_name("codex-driver.py")
    program = f"""import json
import os
import pathlib
import subprocess
import sys

args = sys.argv[1:]
control_path = pathlib.Path(__file__).with_name("codex-mode.json")
control = json.loads(control_path.read_text(encoding="utf-8")) if control_path.exists() else {{}}
if args == ["--version"]:
    print("codex-cli " + control.get("version", "0.149.0"))
    raise SystemExit(0)
if "generate-json-schema" in args:
    if control.get("schema") == "command-missing":
        raise SystemExit(2)
    completed = subprocess.run([{json.dumps(str(real_codex))}, *args], check=False)
    if completed.returncode:
        raise SystemExit(completed.returncode)
    if control.get("schema") == "missing-field":
        output = pathlib.Path(args[args.index("--out") + 1])
        target = output / "v2" / "ModelListResponse.json"
        value = json.loads(target.read_text(encoding="utf-8"))
        value["required"] = []
        target.write_text(json.dumps(value), encoding="utf-8")
    raise SystemExit(0)
os.execve({json.dumps(str(real_codex))}, [{json.dumps(str(real_codex))}, *args], os.environ)
"""
    driver.write_text(program, encoding="utf-8")
    driver.chmod(0o644)
    source = path.with_name("codex-shim.c")
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


def create_script_codex(path: pathlib.Path) -> None:
    """The interpreter-script argv[0] the direct-executable contract rejects."""
    path.write_text(f"#!{sys.executable}\nraise SystemExit(0)\n", encoding="utf-8")
    path.chmod(0o755)


def set_mode(path: pathlib.Path, *, version: str = "0.149.0", schema: str = "ok") -> None:
    path.with_name("codex-mode.json").write_text(
        json.dumps({"version": version, "schema": schema}), encoding="utf-8"
    )


def add_arguments(
    workspace: pathlib.Path, codex_home: pathlib.Path, executable: pathlib.Path
) -> list[str]:
    return [
        "--workspace",
        str(workspace),
        "--codex-home",
        str(codex_home),
        "--native-subagents",
        "enabled",
        "--env",
        "PATH=/usr/bin:/bin:/usr/sbin:/sbin",
        "--env",
        "LANG=en_US.UTF-8",
        "--env",
        "LC_ALL=en_US.UTF-8",
        "--",
        str(executable),
    ]


def canonical_sha256(value: dict[str, object]) -> str:
    """The JCS digest src/profile.rs chains membership records with."""
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def append_membership(
    profile_root: pathlib.Path, kind: str, workspace_id: str, run_id: str | None
) -> int:
    """Append one chained membership record and rewrite the derived index.

    A Run joins a profile server through `profile::register_run_member`, a Rust
    API with no CLI surface, so the black-box test writes the journal record
    that call would have written. Doing it here keeps the gate under test the
    product's own replay and index derivation rather than a stub.
    """
    journal = profile_root / "membership.jsonl"
    records = [
        json.loads(line)
        for line in journal.read_text(encoding="utf-8").splitlines()
        if line
    ]
    revision = records[-1]["revision"] + 1 if records else 1
    previous = records[-1]["record_sha256"] if records else "0" * 64
    body = {
        "schema_version": 1,
        "revision": revision,
        "kind": kind,
        "workspace_id": workspace_id,
        "run_id": run_id,
        "previous_sha256": previous,
    }
    record = dict(body, record_sha256=canonical_sha256(body))
    with journal.open("a", encoding="utf-8") as sink:
        sink.write(json.dumps(record, separators=(",", ":")) + "\n")
    records.append(record)

    active: dict[str, dict[str, object]] = {}
    for entry in records:
        if entry["run_id"] is None:
            continue
        identity = f"{entry['workspace_id']}:{entry['run_id']}"
        if entry["kind"] in ("run_registered", "run_state_changed"):
            active[identity] = entry
        elif entry["kind"] in ("run_removed", "run_closed", "tombstone_orphan", "run_migrated"):
            active.pop(identity, None)
    index_path = profile_root / "members.json"
    index_path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "revision": revision,
                "journal_sha256": hashlib.sha256(journal.read_bytes()).hexdigest(),
                "active_members": active,
            }
        ),
        encoding="utf-8",
    )
    index_path.chmod(0o600)

    state_path = profile_root / "state.json"
    if state_path.is_file():
        state = json.loads(state_path.read_text(encoding="utf-8"))
        state["membership_revision"] = revision
        state_path.write_text(json.dumps(state), encoding="utf-8")
        state_path.chmod(0o600)
    return revision


def validate(binary: pathlib.Path, protocol_root: pathlib.Path) -> None:
    machine = validator(protocol_root, "dolgorae-machine-v1.schema.json")
    real_codex_name = shutil.which("codex")
    if real_codex_name is None:
        raise AssertionError("Codex 0.149.0 is required for the profile matrix")
    real_codex = pathlib.Path(real_codex_name).resolve()
    version = subprocess.run(
        [str(real_codex), "--version"], check=True, capture_output=True, text=True
    ).stdout.strip()
    if version != "codex-cli 0.149.0":
        raise AssertionError(f"expected codex-cli 0.149.0, got {version!r}")

    with tempfile.TemporaryDirectory(prefix="dolgorae-task005-") as temporary:
        root = pathlib.Path(temporary)
        home = root / "home"
        workspace = root / "workspace"
        codex_home = root / "codex-home"
        bin_root = root / "bin"
        for directory in (home, workspace, codex_home, bin_root):
            directory.mkdir(mode=0o700)
        (home / "Library" / "Application Support").mkdir(parents=True, mode=0o700)
        subprocess.run(["git", "-C", str(workspace), "init", "-b", "main"], check=True, capture_output=True)
        initialized = run(binary, home, "init", str(workspace))
        if initialized.returncode != 0:
            raise AssertionError(f"workspace init failed: {initialized.stdout}")

        fake = bin_root / "codex"
        create_fake_codex(fake, real_codex)
        set_mode(fake)
        added = run(binary, home, "profile", "add", "default", *add_arguments(workspace, codex_home, fake))
        added_envelope = envelope(added)
        if added.returncode != 0 or added_envelope["data"]["name"] != "default":
            raise AssertionError(f"profile add failed: {added.stdout}")
        assert_valid(added_envelope, machine, "profile-add Machine envelope")
        registry_path = next((home / "Library" / "Application Support" / "Dolgorae" / "workspaces").glob("*/local.yaml"))
        if stat.S_IMODE(registry_path.stat().st_mode) != 0o600:
            raise AssertionError("profile registry mode is not 0600")

        # `profile show` never executes the profile, so its closed capability
        # snapshot must always be the explicit-unverified baseline: present,
        # complete, and never fabricated as supported.
        shown = envelope(run(binary, home, "profile", "show", "default", "--workspace", str(workspace)))
        assert_valid(shown, machine, "profile-show Machine envelope")
        shown_capabilities = shown["data"]["capabilities"]
        if set(shown_capabilities) != CLOSED_CAPABILITY_NAMES:
            raise AssertionError(f"profile show capability snapshot is not closed: {shown_capabilities}")
        if any(state != "unverified" for state in shown_capabilities.values()):
            raise AssertionError(f"profile show fabricated a non-unverified capability: {shown_capabilities}")

        duplicate = run(binary, home, "profile", "add", "default", *add_arguments(workspace, codex_home, fake))
        if duplicate.returncode != 4 or envelope(duplicate)["error"]["code"] != "PROFILE_ALREADY_EXISTS":
            raise AssertionError(f"duplicate profile was not rejected: {duplicate.stdout}")

        # SPEC-013 rejects shell interpreters and arbitrary wrappers as
        # argv[0]. A `#!` script named `codex` used to satisfy every check the
        # registry made (it exists, it is executable, it is named codex) and
        # would have been launched through an interpreter the launch contract
        # never recorded.
        script_root = bin_root / "script"
        script_root.mkdir(mode=0o700)
        scripted = script_root / "codex"
        create_script_codex(scripted)
        script_add = run(
            binary, home, "profile", "add", "scripted", *add_arguments(workspace, codex_home, scripted)
        )
        script_envelope = envelope(script_add)
        if script_add.returncode != 3 or script_envelope["error"]["code"] != "PROFILE_CONFIG_INVALID":
            raise AssertionError(f"an interpreter-script argv[0] was accepted: {script_add.stdout}")
        if "interpreter script" not in script_envelope["error"]["details"]["reason"]:
            raise AssertionError(f"argv[0] rejection did not name the reason: {script_add.stdout}")
        assert_valid(script_envelope, machine, "scripted-argv0 Machine envelope")

        # The same hole through a symlink: named `codex`, resolving to an
        # arbitrary program.
        wrapper_root = bin_root / "wrapper"
        wrapper_root.mkdir(mode=0o700)
        (wrapper_root / "codex").symlink_to("/bin/echo")
        wrapper_add = run(
            binary,
            home,
            "profile",
            "add",
            "wrapped",
            *add_arguments(workspace, codex_home, wrapper_root / "codex"),
        )
        if (
            wrapper_add.returncode != 3
            or envelope(wrapper_add)["error"]["code"] != "PROFILE_CONFIG_INVALID"
        ):
            raise AssertionError(f"a wrapper argv[0] was accepted: {wrapper_add.stdout}")

        reserved = add_arguments(workspace, codex_home, fake)
        reserved.extend(["--enable", "multi_agent"])
        rejected = run(binary, home, "profile", "add", "reserved", *reserved)
        if rejected.returncode != 3 or envelope(rejected)["error"]["code"] != "PROFILE_CONFIG_INVALID":
            raise AssertionError(f"reserved native flag was not rejected: {rejected.stdout}")
        listed = envelope(run(binary, home, "profile", "list", "--workspace", str(workspace)))
        if [profile["name"] for profile in listed["data"]["profiles"]] != ["default"]:
            raise AssertionError("failed profile add changed the registry")

        exact = run(binary, home, "profile", "doctor", "default", "--workspace", str(workspace))
        exact_envelope = envelope(exact)
        if exact.returncode != 0:
            raise AssertionError(f"exact compatibility failed: {exact.stdout}")
        exact_data = exact_envelope["data"]
        assert_valid(exact_envelope, machine, "profile-doctor Machine envelope")
        if exact_data["compatibility"] != "tested" or exact_data["codex_version"] != "0.149.0":
            raise AssertionError(f"exact compatibility returned wrong facts: {exact.stdout}")
        # Bare doctor never starts a singleton, so with none already running
        # its capability snapshot must be the closed, all-unverified baseline
        # rather than the empty map it used to report.
        bare_capabilities = exact_data["capabilities"]
        if set(bare_capabilities) != CLOSED_CAPABILITY_NAMES:
            raise AssertionError(f"bare doctor capability snapshot is not closed: {bare_capabilities}")
        if any(state != "unverified" for state in bare_capabilities.values()):
            raise AssertionError(f"bare doctor fabricated a non-unverified capability: {bare_capabilities}")

        set_mode(fake, version="0.150.0")
        newer = run(
            binary,
            home,
            "profile",
            "doctor",
            "default",
            "--workspace",
            str(workspace),
        )
        if newer.returncode != 0 or envelope(newer)["data"]["compatibility"] != "unverified":
            raise AssertionError(f"newer compatible version failed: {newer.stdout}")

        set_mode(fake, version="0.148.0")
        older = run(
            binary,
            home,
            "profile",
            "doctor",
            "default",
            "--workspace",
            str(workspace),
        )
        if older.returncode != 0 or envelope(older)["data"]["compatibility"] != "rejected":
            raise AssertionError(f"older version was not rejected: {older.stdout}")

        set_mode(fake, schema="missing-field")
        missing = run(
            binary,
            home,
            "profile",
            "doctor",
            "default",
            "--workspace",
            str(workspace),
        )
        if missing.returncode != 0 or envelope(missing)["data"]["compatibility"] != "rejected":
            raise AssertionError(f"missing schema field was not rejected: {missing.stdout}")

        set_mode(fake)
        launched = run(
            binary,
            home,
            "profile",
            "doctor",
            "default",
            "--workspace",
            str(workspace),
            "--launch-probe",
        )
        launched_envelope = envelope(launched)
        if "data" not in launched_envelope:
            raise AssertionError(f"launch probe failed: {launched.stdout}")
        launched_data = launched_envelope["data"]
        if launched.returncode != 0 or launched_data["server_started"] is not False:
            raise AssertionError(f"launch probe failed: {launched.stdout}")
        probed_capabilities = launched_data["capabilities"]
        if set(probed_capabilities) != CLOSED_CAPABILITY_NAMES:
            raise AssertionError(f"launch probe capability snapshot is not closed: {probed_capabilities}")
        genuinely_proven = {"account_read", "app_server_initialize", "model_list", "thread_absence_error"}
        for name in genuinely_proven:
            if probed_capabilities[name] != "supported":
                raise AssertionError(f"{name} was not proven supported: {probed_capabilities}")
        for name in CLOSED_CAPABILITY_NAMES - genuinely_proven:
            # early_response_id and native_subagent_lifecycle cannot be
            # genuinely proven from a bootstrap probe against no real thread;
            # claiming them supported without proof was the bug this guards.
            if probed_capabilities[name] != "unverified":
                raise AssertionError(
                    f"{name} must stay unverified until genuinely proven, got {probed_capabilities[name]!r}"
                )
        profile_root = (
            home
            / "Library"
            / "Application Support"
            / "Dolgorae"
            / "profiles"
            / exact_data["server_key"]
        )
        server_log = profile_root / "server.log"
        if not server_log.is_file() or stat.S_IMODE(server_log.stat().st_mode) != 0o600:
            raise AssertionError("profile log drainer did not create a private server log")
        if server_log.stat().st_size > 1024 * 1024:
            raise AssertionError("profile server log exceeded its rotation bound")
        # An app-server writes plain diagnostic text, not JSON. Treating "not
        # JSON" as "could not be redacted" turned every captured line into a
        # drop marker and left the operator with a log of nothing. The marker
        # is reserved for a line redaction genuinely failed on.
        if "[DOLGORAE_LOG_LINE_DROPPED]" in server_log.read_text(
            encoding="utf-8", errors="replace"
        ):
            raise AssertionError(
                "the profile log drainer dropped a line it was able to redact"
            )

        operator = root / "operator"
        initialized_operator = run(
            binary,
            home,
            "operator",
            "credential",
            "initialize",
            "--output",
            str(operator),
        )
        if initialized_operator.returncode != 0:
            raise AssertionError(
                f"operator initialization failed: {initialized_operator.stdout}"
            )

        # Profile commands skip cli.rs's generic argument-contract validation
        # (main.rs dispatches them before it runs), so profile.rs's own
        # parsing must independently preserve INVALID_ARGUMENT for mutually
        # exclusive operator carriers instead of masking it as a profile
        # operator error.
        conflicting_carriers = run(
            binary,
            home,
            "profile",
            "state",
            "reset",
            "default",
            "--workspace",
            str(workspace),
            "--operator-file",
            str(operator),
            "--operator-fd",
            "3",
        )
        conflicting_carriers_envelope = envelope(conflicting_carriers)
        if (
            conflicting_carriers.returncode != 2
            or conflicting_carriers_envelope["error"]["code"] != "INVALID_ARGUMENT"
        ):
            raise AssertionError(
                f"mutually exclusive operator carriers were not rejected: {conflicting_carriers.stdout}"
            )
        assert_valid(conflicting_carriers_envelope, machine, "conflicting-operator-carrier Machine envelope")

        started = run(binary, home, "profile", "server", "start", "default", "--workspace", str(workspace))
        started_data = envelope(started)["data"]
        if started.returncode != 0 or started_data["state"]["lifecycle"] != "ready":
            raise AssertionError(f"server start failed: {started.stdout}")
        assert_valid(envelope(started), machine, "profile-server-start Machine envelope")

        # A running singleton's process, drainer, and socket absence cannot
        # be proven, so state reset must refuse with the retryable
        # PROFILE_SERVER_BUSY rather than the unregistered code it used to.
        busy_reset = run(
            binary,
            home,
            "profile",
            "state",
            "reset",
            "default",
            "--workspace",
            str(workspace),
            "--operator-file",
            str(operator),
            "--confirm-server-key",
            exact_data["server_key"],
            "--require-server-absence",
        )
        busy_reset_envelope = envelope(busy_reset)
        if (
            busy_reset.returncode != 4
            or busy_reset_envelope["error"]["code"] != "PROFILE_SERVER_BUSY"
            or busy_reset_envelope["error"]["retryable"] is not True
        ):
            raise AssertionError(f"state reset of a running server was not rejected: {busy_reset.stdout}")
        assert_valid(busy_reset_envelope, machine, "state-reset-while-running Machine envelope")

        status = envelope(run(binary, home, "profile", "server", "status", "default", "--workspace", str(workspace)))
        if status["data"]["lifecycle"] != "ready":
            raise AssertionError("server status did not reconnect to the singleton")
        assert_valid(status, machine, "profile-server-status Machine envelope")

        # A Run registered as a live member of this server must not be
        # interrupted by an ordinary stop. Until registration existed the live
        # member set was always empty and every membership gate was vacuous.
        workspace_id = registry_path.parent.name
        append_membership(profile_root, "run_registered", workspace_id, "run-live-member")
        gated_stop = run(
            binary,
            home,
            "profile",
            "server",
            "stop",
            "default",
            "--workspace",
            str(workspace),
            "--operator-file",
            str(operator),
        )
        gated_stop_envelope = envelope(gated_stop)
        if (
            gated_stop.returncode != 4
            or gated_stop_envelope["error"]["code"] != "PROFILE_MEMBERSHIP_INCOMPLETE"
        ):
            raise AssertionError(f"a stop with a live member was not gated: {gated_stop.stdout}")
        if "--interrupt" not in gated_stop_envelope["error"]["details"]["reason"]:
            raise AssertionError(
                f"the live-member gate did not name its interrupt evidence: {gated_stop.stdout}"
            )
        assert_valid(gated_stop_envelope, machine, "live-member stop gate Machine envelope")

        # A confirmed interrupt is the explicit evidence that authorizes it,
        # and the interrupt is recorded with the members it cost.
        interrupted = run(
            binary,
            home,
            "profile",
            "server",
            "stop",
            "default",
            "--workspace",
            str(workspace),
            "--operator-file",
            str(operator),
            "--interrupt",
            "--confirm-server-key",
            exact_data["server_key"],
        )
        if interrupted.returncode != 0 or envelope(interrupted)["data"]["stopped"] is not True:
            raise AssertionError(f"a confirmed interrupt stop failed: {interrupted.stdout}")
        assert_valid(envelope(interrupted), machine, "profile-server-stop Machine envelope")
        interrupt_records = envelope(
            run(
                binary,
                home,
                "profile",
                "diagnostics",
                "list",
                "default",
                "--workspace",
                str(workspace),
                "--projection",
                "operational",
                "--operator-file",
                str(operator),
            )
        )["data"]["items"]
        evidence = [
            item for item in interrupt_records if item.get("kind") == "operator_interrupt"
        ]
        if not evidence:
            raise AssertionError("the interrupt was not recorded as a diagnostic")
        interrupted_members = evidence[-1]["details"]["interrupted_members"]
        if [member["run_id"] for member in interrupted_members] != ["run-live-member"]:
            raise AssertionError(
                f"the interrupt diagnostic did not name the members it cost: {evidence[-1]}"
            )

        # Release the member and bring the singleton back for the rest of the
        # matrix, which needs a running old server to migrate away from.
        append_membership(profile_root, "run_closed", workspace_id, "run-live-member")
        restarted = run(
            binary, home, "profile", "server", "start", "default", "--workspace", str(workspace)
        )
        if restarted.returncode != 0 or envelope(restarted)["data"]["state"]["lifecycle"] != "ready":
            raise AssertionError(f"server restart after the interrupt failed: {restarted.stdout}")
        assert_valid(envelope(restarted), machine, "profile-server-restart Machine envelope")
        ungated_members = envelope(
            run(binary, home, "profile", "membership", "verify", "default", "--workspace", str(workspace))
        )["data"]
        if ungated_members["complete"] is not True:
            raise AssertionError(
                f"releasing the member left the journal incomplete: {ungated_members}"
            )

        conflicting_arguments = add_arguments(workspace, codex_home, fake)
        conflicting_arguments[conflicting_arguments.index("LANG=en_US.UTF-8")] = "LANG=C"
        conflicting_arguments[conflicting_arguments.index("LC_ALL=en_US.UTF-8")] = "LC_ALL=C"
        conflicting_add = run(binary, home, "profile", "add", "conflict", *conflicting_arguments)
        if conflicting_add.returncode != 0:
            raise AssertionError(f"same-home conflict profile add failed: {conflicting_add.stdout}")

        # A diagnostic launch probe may start and stop only its own exact
        # contract. It must never migrate an unrelated, already-running
        # singleton and then tear the replacement down as probe cleanup.
        old_state = envelope(
            run(binary, home, "profile", "server", "status", "default", "--workspace", str(workspace))
        )["data"]["state"]
        conflicting_probe = run(
            binary,
            home,
            "profile",
            "doctor",
            "conflict",
            "--workspace",
            str(workspace),
            "--launch-probe",
        )
        if (
            conflicting_probe.returncode != 4
            or envelope(conflicting_probe)["error"]["code"] != "PROFILE_LAUNCH_CONFLICT"
        ):
            raise AssertionError(f"diagnostic probe crossed the active contract: {conflicting_probe.stdout}")
        probe_preserved = envelope(
            run(binary, home, "profile", "server", "status", "default", "--workspace", str(workspace))
        )["data"]["state"]
        if probe_preserved["pid"] != old_state["pid"]:
            raise AssertionError("diagnostic launch probe replaced the existing singleton")

        # A different launch contract cannot auto-roll a server that still
        # owns a live Run. The failed attempt must leave the old process and
        # active contract untouched.
        append_membership(profile_root, "run_registered", workspace_id, "run-rollover-blocker")
        conflicting_start = run(
            binary, home, "profile", "server", "start", "conflict", "--workspace", str(workspace)
        )
        if (
            conflicting_start.returncode != 4
            or envelope(conflicting_start)["error"]["code"] != "PROFILE_MEMBERSHIP_INCOMPLETE"
        ):
            raise AssertionError(f"live same-home singleton was not preserved: {conflicting_start.stdout}")
        preserved = envelope(
            run(binary, home, "profile", "server", "status", "default", "--workspace", str(workspace))
        )["data"]["state"]
        if preserved["pid"] != old_state["pid"] or preserved["server_key"] != old_state["server_key"]:
            raise AssertionError("failed automatic rollover changed the live source server")

        # Once the source membership is empty, starting the new contract
        # retires only the Dolgorae-managed singleton and publishes the new
        # generation without an operator credential.
        append_membership(profile_root, "run_closed", workspace_id, "run-rollover-blocker")
        conflicting_start = run(
            binary, home, "profile", "server", "start", "conflict", "--workspace", str(workspace)
        )
        conflicting_data = envelope(conflicting_start)["data"] if conflicting_start.returncode == 0 else {}
        if conflicting_start.returncode != 0 or conflicting_data["state"]["lifecycle"] != "ready":
            raise AssertionError(f"quiescent same-home rollover failed: {conflicting_start.stdout}")
        if conflicting_data["state"]["server_key"] == old_state["server_key"]:
            raise AssertionError("automatic rollover retained the old launch contract")
        assert_valid(envelope(conflicting_start), machine, "quiescent-rollover Machine envelope")

        # Returning to the original profile exercises a second automatic
        # rollover and proves the committed migration fence is reusable.
        returned = run(
            binary, home, "profile", "server", "start", "default", "--workspace", str(workspace)
        )
        returned_data = envelope(returned)["data"] if returned.returncode == 0 else {}
        if returned.returncode != 0 or returned_data["state"]["server_key"] != exact_data["server_key"]:
            raise AssertionError(f"rollover back to the original contract failed: {returned.stdout}")
        assert_valid(envelope(returned), machine, "quiescent-rollover-return Machine envelope")

        conflicting_remove = run(
            binary, home, "profile", "remove", "conflict", "--workspace", str(workspace)
        )
        if conflicting_remove.returncode != 0:
            raise AssertionError(f"conflict profile cleanup failed: {conflicting_remove.stdout}")

        set_mode(fake, version="0.150.0")
        migrated_snapshot = envelope(
            run(binary, home, "profile", "doctor", "default", "--workspace", str(workspace))
        )["data"]
        migrated = run(
            binary,
            home,
            "profile",
            "server",
            "migrate",
            "default",
            "--workspace",
            str(workspace),
            "--operator-file",
            str(operator),
            "--confirm-old-server-key",
            exact_data["server_key"],
            "--confirm-new-server-key",
            migrated_snapshot["server_key"],
        )
        if migrated.returncode != 0 or envelope(migrated)["data"]["migrated"] is not True:
            raise AssertionError(f"operator migration failed: {migrated.stdout}")
        assert_valid(envelope(migrated), machine, "profile-server-migrate Machine envelope")
        with operator.open("rb") as capability:
            stopped = run(
                binary,
                home,
                "profile",
                "server",
                "stop",
                "default",
                "--workspace",
                str(workspace),
                "--operator-fd",
                str(capability.fileno()),
                pass_fds=(capability.fileno(),),
            )
        if stopped.returncode != 0 or envelope(stopped)["data"]["stopped"] is not True:
            raise AssertionError(f"server stop failed: {stopped.stdout}")
        assert_valid(envelope(stopped), machine, "profile-server-final-stop Machine envelope")
        set_mode(fake)
        membership = envelope(run(binary, home, "profile", "membership", "verify", "default", "--workspace", str(workspace)))
        if membership["data"]["complete"] is not True or membership["data"]["revision"] < 2:
            raise AssertionError("membership journal did not retain both generations")
        assert_valid(membership, machine, "profile-membership-verify Machine envelope")

        state = envelope(run(binary, home, "profile", "diagnostics", "list", "default", "--workspace", str(workspace)))
        if not state["data"]["items"]:
            raise AssertionError("profile diagnostics are empty")
        assert_valid(state, machine, "profile-diagnostics-list Machine envelope")

        # A confirmation that names the wrong server key is a profile
        # identity mismatch against the recorded contract, not a bare
        # "profile" detail blob.
        mismatched_confirm = run(
            binary,
            home,
            "profile",
            "state",
            "reset",
            "default",
            "--workspace",
            str(workspace),
            "--operator-file",
            str(operator),
            "--confirm-server-key",
            "0" * 64,
            "--require-server-absence",
        )
        mismatched_confirm_envelope = envelope(mismatched_confirm)
        mismatched_error = mismatched_confirm_envelope["error"]
        if mismatched_confirm.returncode != 5 or mismatched_error["code"] != "PROFILE_MISMATCH":
            raise AssertionError(f"wrong confirm-server-key was not rejected: {mismatched_confirm.stdout}")
        if (
            mismatched_error["details"]["field"] != "confirm_server_key"
            or mismatched_error["details"]["expected"] != exact_data["server_key"]
            or mismatched_error["details"]["actual"] != "0" * 64
        ):
            raise AssertionError(f"PROFILE_MISMATCH details were not exact: {mismatched_confirm.stdout}")
        assert_valid(mismatched_confirm_envelope, machine, "state-reset-confirm-mismatch Machine envelope")

        # Fabricate a migration fence stuck in "prepared" phase, as a crash
        # between PREPARE and COMMIT would leave it, naming this profile's
        # own (now-stopped, so provably absent) server key as one side and a
        # never-started server key as the other. `profile state reset`
        # proving both recorded lifetimes absent must repair a blocked fence as a
        # best-effort side effect rather than leaving the CODEX_HOME fenced
        # forever with no operator recovery path.
        home_hash = hashlib.sha256(
            b"dolgorae-home-v1\0" + exact_data["expected_codex_home"].encode("utf-8")
        ).hexdigest()
        home_dir = home / "Library" / "Application Support" / "Dolgorae" / "homes" / home_hash
        home_dir.mkdir(parents=True, exist_ok=True)
        home_dir.chmod(0o700)
        migration_path = home_dir / "migration.json"
        never_started_server_key = "f" * 64
        migration_path.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "migration_id": "00000000-0000-7000-8000-000000000000",
                    "old_server_key": exact_data["server_key"],
                    "new_server_key": never_started_server_key,
                    "phase": "migration_blocked",
                }
            ),
            encoding="utf-8",
        )
        migration_path.chmod(0o600)

        reset = run(
            binary,
            home,
            "profile",
            "state",
            "reset",
            "default",
            "--workspace",
            str(workspace),
            "--operator-file",
            str(operator),
            "--confirm-server-key",
            exact_data["server_key"],
            "--require-server-absence",
        )
        if reset.returncode != 0:
            raise AssertionError(f"profile state reset failed: {reset.stdout}")
        if envelope(reset)["data"]["migration_repaired"] is not True:
            raise AssertionError(f"stale migration fence was not repaired: {reset.stdout}")
        repaired_migration = json.loads(migration_path.read_text(encoding="utf-8"))
        if repaired_migration["phase"] != "rolled_back":
            raise AssertionError(f"stale migration fence phase was not rolled back: {repaired_migration}")

        removed = run(binary, home, "profile", "remove", "default", "--workspace", str(workspace))
        if removed.returncode != 0 or envelope(removed)["data"]["removed"] is not True:
            raise AssertionError(f"profile remove failed: {removed.stdout}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument(
        "--protocol-root", type=pathlib.Path, default=pathlib.Path("docs/protocol")
    )
    arguments = parser.parse_args()
    validate(arguments.binary.resolve(), arguments.protocol_root.resolve())
    print("Profile CLI validation passed: registry, compatibility, singleton, membership")
    return 0


if __name__ == "__main__":
    sys.exit(main())
