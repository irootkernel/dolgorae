#!/usr/bin/env python3
"""Black-box Runtime Profile and Codex compatibility validation."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import stat
import subprocess
import sys
import tempfile

import native_codex
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


def create_fake_codex(
    path: pathlib.Path, real_codex: pathlib.Path, codex_home: pathlib.Path
) -> None:
    # Only schema generation uses the pinned local Codex. Bootstrap RPCs use
    # the independent fixture and must not depend on a real account login.
    scenario = path.with_name("profile-probe.json")
    scenario.write_text(
        json.dumps({
            "schema_version": 1,
            "name": "profile-probe",
            "steps": [
                {"method": "initialize", "respond": {"result": {
                    "codexHome": "${codex_home}",
                    "userAgent": "fake-app-server/1",
                    "capabilities": {"experimentalApi": False},
                }}},
                {"method": "account/read", "respond": {"result": {
                    "requiresOpenaiAuth": False,
                }}},
                {"method": "model/list", "respond": {"result": {
                    "data": [{
                        "model": "gpt-5.6", "isDefault": True,
                        "supportedReasoningEfforts": [{"reasoningEffort": "medium"}],
                    }],
                    "nextCursor": None,
                }}},
                {"method": "thread/read", "respond": {"error": {
                    "code": -32600, "message": "thread not found",
                }}},
            ],
        }),
        encoding="utf-8",
    )
    driver = path.with_name("codex-driver.py")
    program = f"""import json
import pathlib
import subprocess
import sys

args = sys.argv[1:]
control_path = pathlib.Path(__file__).with_name("codex-mode.json")
control = json.loads(control_path.read_text(encoding="utf-8")) if control_path.exists() else {{}}
if args == ["--version"]:
    print("codex-cli " + control.get("version", "0.158.0"))
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
    if control.get("schema") == "digest-mismatch":
        output = pathlib.Path(args[args.index("--out") + 1])
        target = output / "v2" / "ModelListResponse.json"
        value = json.loads(target.read_text(encoding="utf-8"))
        value["x-test-digest-mismatch"] = True
        target.write_text(json.dumps(value), encoding="utf-8")
    if control.get("schema") == "experimental-digest-mismatch" and "--experimental" in args:
        output = pathlib.Path(args[args.index("--out") + 1])
        target = output / "v2" / "ModelListResponse.json"
        value = json.loads(target.read_text(encoding="utf-8"))
        value["x-test-experimental-digest-mismatch"] = True
        target.write_text(json.dumps(value), encoding="utf-8")
    raise SystemExit(0)
if "app-server" in args and "--listen" in args:
    target = args[args.index("--listen") + 1]
    if not target.startswith("unix://"):
        raise SystemExit(2)
    sys.path.insert(0, {json.dumps(str(native_codex.FAKE_APP_SERVER))})
    from scenario import Scenario
    from server import FakeAppServer
    fake = FakeAppServer(
        pathlib.Path(target[len("unix://"):]),
        Scenario.load(pathlib.Path({json.dumps(str(scenario))}),
                      {{"codex_home": {json.dumps(str(codex_home.resolve()))}}}),
        None,
        None,
    )
    fake.bind()
    print("Profile probe fixture ready", flush=True)
    fake.serve_forever()
    raise SystemExit(0)
raise SystemExit(2)
"""
    driver.write_text(program, encoding="utf-8")
    driver.chmod(0o644)
    native_codex.compile_native_driver(path, driver)


def create_script_codex(path: pathlib.Path) -> None:
    """The interpreter-script argv[0] the direct-executable contract rejects."""
    path.write_text(f"#!{sys.executable}\nraise SystemExit(0)\n", encoding="utf-8")
    path.chmod(0o755)


def set_mode(path: pathlib.Path, *, version: str = "0.158.0", schema: str = "ok") -> None:
    path.with_name("codex-mode.json").write_text(
        json.dumps({"version": version, "schema": schema}), encoding="utf-8"
    )


def add_arguments(codex_home: pathlib.Path, executable: pathlib.Path) -> list[str]:
    return [
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
    profile_root: pathlib.Path,
    kind: str,
    workspace_id: str,
    run_id: str,
    observed_epoch: int = 2,
) -> int:
    """Append one canonical global membership-v2 record and index."""
    journal = profile_root / "membership.jsonl"
    records = [
        json.loads(line)
        for line in journal.read_text(encoding="utf-8").splitlines()
        if line
    ] if journal.exists() else []
    revision = records[-1]["revision"] + 1 if records else 1
    previous = records[-1]["record_sha256"] if records else "0" * 64
    disposition = "released" if kind in ("run_closed", "tombstone_orphan") else "active"
    body = {
        "schema_version": 2,
        "revision": revision,
        "workspace_id": workspace_id,
        "run_id": run_id,
        "disposition": disposition,
        "controller_id": None,
        "worker_generation": None,
        "thread_id": None,
        "connection_id": None,
        "lifecycle": kind,
        "writer": False,
        "observed_epoch": observed_epoch,
        "runtime_locator": None,
        "previous_sha256": previous,
    }
    record = dict(body, record_sha256=canonical_sha256(body))
    with journal.open("a", encoding="utf-8") as sink:
        sink.write(json.dumps(record, separators=(",", ":")) + "\n")
    journal.chmod(0o600)
    records.append(record)

    members: dict[str, dict[str, object]] = {}
    for entry in records:
        members[f"{entry['workspace_id']}:{entry['run_id']}"] = entry
    index_path = profile_root / "members.json"
    index_path.write_text(
        json.dumps({
            "schema_version": 2,
            "server_key": profile_root.name,
            "revision": revision,
            "journal_sha256": hashlib.sha256(journal.read_bytes()).hexdigest(),
            "members": members,
        }),
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


def write_bound_manifest(
    home: pathlib.Path, profile_root: pathlib.Path, workspace_id: str, run_id: str
) -> None:
    """Create the minimal immutable identity proof checked by membership repair."""
    run_root = home / ".dolgorae" / "workspaces" / workspace_id / "runs" / run_id
    run_root.mkdir(parents=True, mode=0o700, exist_ok=True)
    manifest = run_root / "manifest.json"
    manifest.write_text(
        json.dumps(
            {
                "schema_version": 2,
                "workspace_id": workspace_id,
                "run_id": run_id,
                "global_profile_binding": {
                    "schema_version": 2,
                    "server_key": profile_root.name,
                },
            },
            sort_keys=True,
            separators=(",", ":"),
        ),
        encoding="utf-8",
    )
    manifest.chmod(0o600)


def validate(binary: pathlib.Path, protocol_root: pathlib.Path) -> None:
    machine = validator(protocol_root, "dolgorae-machine-v2.schema.json")
    real_codex = native_codex.installed_codex()

    with tempfile.TemporaryDirectory(prefix="dolgorae-task005-") as temporary:
        root = pathlib.Path(temporary)
        home = root / "home"
        workspace = root / "workspace"
        codex_home = root / "codex-home"
        bin_root = root / "bin"
        for directory in (home, workspace, codex_home, bin_root):
            directory.mkdir(mode=0o700)
        subprocess.run(["git", "-C", str(workspace), "init", "-b", "main"], check=True, capture_output=True)

        uninitialized_workspace_scoped = run(
            binary, home, "profile", "list", "--workspace", str(workspace)
        )
        if (
            uninitialized_workspace_scoped.returncode != 2
            or envelope(uninitialized_workspace_scoped)["error"]["code"]
            != "INVALID_ARGUMENT"
        ):
            raise AssertionError(
                "removed workspace scope was not rejected before the home gate: "
                f"{uninitialized_workspace_scoped.stdout}"
            )
        if (home / ".dolgorae").exists():
            raise AssertionError("rejected Profile syntax initialized the Dolgorae home")

        initialized = run(binary, home, "init", str(workspace))
        if initialized.returncode != 0:
            raise AssertionError(f"workspace init failed: {initialized.stdout}")
        workspace_id = envelope(initialized)["data"]["workspace_id"]

        workspace_scoped = run(
            binary, home, "profile", "list", "--workspace", str(workspace)
        )
        if (
            workspace_scoped.returncode != 2
            or envelope(workspace_scoped)["error"]["code"] != "INVALID_ARGUMENT"
        ):
            raise AssertionError(
                f"profile command accepted removed workspace scope: {workspace_scoped.stdout}"
            )

        fake = bin_root / "codex"
        create_fake_codex(fake, real_codex, codex_home)
        set_mode(fake)
        for invalid_name in ("Default", "with space", "with:colon", "-leading", "a" * 129):
            invalid_add = run(
                binary,
                home,
                "profile",
                "add",
                invalid_name,
                *add_arguments(codex_home, fake),
            )
            invalid_envelope = envelope(invalid_add)
            if (
                invalid_add.returncode != 3
                or invalid_envelope["error"]["code"] != "PROFILE_CONFIG_INVALID"
            ):
                raise AssertionError(
                    f"invalid profile name was accepted: {invalid_name!r}: {invalid_add.stdout}"
                )
            assert_valid(invalid_envelope, machine, "invalid-profile-name Machine envelope")
        if envelope(run(binary, home, "profile", "list"))["data"]["profiles"]:
            raise AssertionError("failed profile-name validation changed the registry")

        added = run(binary, home, "profile", "add", "default", *add_arguments(codex_home, fake))
        added_envelope = envelope(added)
        if added.returncode != 0 or added_envelope["data"]["name"] != "default":
            raise AssertionError(f"profile add failed: {added.stdout}")
        assert_valid(added_envelope, machine, "profile-add Machine envelope")
        registry_path = home / ".dolgorae" / "profiles.yaml"
        if stat.S_IMODE(registry_path.stat().st_mode) != 0o600:
            raise AssertionError("profile registry mode is not 0600")

        # `profile show` never executes the profile, so its closed capability
        # snapshot must always be the explicit-unverified baseline: present,
        # complete, and never fabricated as supported.
        shown = envelope(run(binary, home, "profile", "show", "default"))
        assert_valid(shown, machine, "profile-show Machine envelope")
        shown_capabilities = shown["data"]["capabilities"]
        if set(shown_capabilities) != CLOSED_CAPABILITY_NAMES:
            raise AssertionError(f"profile show capability snapshot is not closed: {shown_capabilities}")
        if any(state != "unverified" for state in shown_capabilities.values()):
            raise AssertionError(f"profile show fabricated a non-unverified capability: {shown_capabilities}")

        duplicate = run(binary, home, "profile", "add", "default", *add_arguments(codex_home, fake))
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
            binary, home, "profile", "add", "scripted", *add_arguments(codex_home, scripted)
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
            *add_arguments(codex_home, wrapper_root / "codex"),
        )
        if (
            wrapper_add.returncode != 3
            or envelope(wrapper_add)["error"]["code"] != "PROFILE_CONFIG_INVALID"
        ):
            raise AssertionError(f"a wrapper argv[0] was accepted: {wrapper_add.stdout}")

        reserved = add_arguments(codex_home, fake)
        reserved.extend(["--enable", "multi_agent"])
        rejected = run(binary, home, "profile", "add", "reserved", *reserved)
        if rejected.returncode != 3 or envelope(rejected)["error"]["code"] != "PROFILE_CONFIG_INVALID":
            raise AssertionError(f"reserved native flag was not rejected: {rejected.stdout}")
        listed = envelope(run(binary, home, "profile", "list"))
        if [profile["name"] for profile in listed["data"]["profiles"]] != ["default"]:
            raise AssertionError("failed profile add changed the registry")

        exact = run(binary, home, "profile", "doctor", "default")
        exact_envelope = envelope(exact)
        if exact.returncode != 0:
            raise AssertionError(f"exact compatibility failed: {exact.stdout}")
        exact_data = exact_envelope["data"]
        assert_valid(exact_envelope, machine, "profile-doctor Machine envelope")
        if exact_data["compatibility"] != "tested" or exact_data["codex_version"] != "0.158.0":
            raise AssertionError(f"exact compatibility returned wrong facts: {exact.stdout}")
        if exact_data["diagnostics"]:
            raise AssertionError(f"tested minimum reported compatibility warnings: {exact.stdout}")
        never_started_root = home / ".dolgorae" / "profiles" / exact_data["server_key"]
        never_started_verify = run(
            binary, home, "profile", "membership", "verify", "default"
        )
        never_started_data = envelope(never_started_verify)["data"]
        if (
            never_started_verify.returncode != 0
            or never_started_data["complete"] is not True
            or never_started_data["revision"] != 0
            or never_started_data["records"] != 0
            or never_started_data["members"]
            or never_started_data["orphans"]
            or never_started_root.exists()
        ):
            raise AssertionError(
                f"never-started membership verification was not empty and read-only: {never_started_verify.stdout}"
            )
        assert_valid(
            envelope(never_started_verify),
            machine,
            "never-started-membership-verify Machine envelope",
        )
        # Bare doctor never starts a singleton, so with none already running
        # its capability snapshot must be the closed, all-unverified baseline
        # rather than the empty map it used to report.
        bare_capabilities = exact_data["capabilities"]
        if set(bare_capabilities) != CLOSED_CAPABILITY_NAMES:
            raise AssertionError(f"bare doctor capability snapshot is not closed: {bare_capabilities}")
        if any(state != "unverified" for state in bare_capabilities.values()):
            raise AssertionError(f"bare doctor fabricated a non-unverified capability: {bare_capabilities}")

        set_mode(fake, version="0.158.1", schema="digest-mismatch")
        newer = run(
            binary,
            home,
            "profile",
            "doctor",
            "default",
        )
        if newer.returncode != 0 or envelope(newer)["data"]["compatibility"] != "unverified":
            raise AssertionError(f"newer compatible version failed: {newer.stdout}")
        if envelope(newer)["data"]["diagnostics"][0]["code"] != "CODEX_VERSION_UNVERIFIED":
            raise AssertionError(f"newer version lost its qualification warning: {newer.stdout}")

        set_mode(fake, version="0.157.1")
        below_minimum = run(binary, home, "profile", "doctor", "default")
        if (
            below_minimum.returncode != 0
            or envelope(below_minimum)["data"]["compatibility"] != "rejected"
        ):
            raise AssertionError(f"pre-minimum version was not rejected: {below_minimum.stdout}")

        set_mode(fake, version="0.153.4")
        older = run(
            binary,
            home,
            "profile",
            "doctor",
            "default",
        )
        if older.returncode != 0 or envelope(older)["data"]["compatibility"] != "rejected":
            raise AssertionError(f"older version was not rejected: {older.stdout}")

        set_mode(fake, schema="digest-mismatch")
        wrong_digest = run(binary, home, "profile", "doctor", "default")
        if (
            wrong_digest.returncode != 0
            or envelope(wrong_digest)["data"]["compatibility"] != "rejected"
        ):
            raise AssertionError(f"exact version with wrong schema digest was not rejected: {wrong_digest.stdout}")

        set_mode(fake, version="0.158.00", schema="digest-mismatch")
        equivalent_version = run(binary, home, "profile", "doctor", "default")
        if (
            equivalent_version.returncode != 0
            or envelope(equivalent_version)["data"]["compatibility"] != "rejected"
        ):
            raise AssertionError(f"equivalent minimum version bypassed schema digest pin: {equivalent_version.stdout}")

        set_mode(fake, schema="experimental-digest-mismatch")
        wrong_experimental = run(binary, home, "profile", "doctor", "default")
        if (
            wrong_experimental.returncode != 0
            or envelope(wrong_experimental)["data"]["compatibility"] != "rejected"
        ):
            raise AssertionError(f"exact version with wrong experimental digest was not rejected: {wrong_experimental.stdout}")

        set_mode(fake, schema="missing-field")
        missing = run(
            binary,
            home,
            "profile",
            "doctor",
            "default",
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
            / ".dolgorae"
            / "profiles"
            / exact_data["server_key"]
        )
        server_log = profile_root / "server.log"
        if not server_log.is_file() or stat.S_IMODE(server_log.stat().st_mode) != 0o600:
            raise AssertionError("profile log drainer did not create a private server log")
        if server_log.stat().st_size > 1024 * 1024:
            raise AssertionError("profile server log exceeded its rotation bound")
        if "Profile probe fixture ready" not in server_log.read_text(encoding="utf-8"):
            raise AssertionError("profile log drainer lost the fixture diagnostic")
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

        alias_added = run(binary, home, "profile", "add", "alias", *add_arguments(codex_home, fake))
        if alias_added.returncode != 0:
            raise AssertionError(f"same-contract profile alias was not preserved: {alias_added.stdout}")
        alias_exact = run(binary, home, "profile", "doctor", "alias")
        alias_data = envelope(alias_exact)["data"] if alias_exact.returncode == 0 else {}
        if alias_exact.returncode != 0 or alias_data.get("server_key") != exact_data["server_key"]:
            raise AssertionError(f"same-contract profile alias did not share the server key: {alias_exact.stdout}")

        started = run(binary, home, "profile", "server", "start", "default")
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

        status = envelope(run(binary, home, "profile", "server", "status", "default"))
        if status["data"]["lifecycle"] != "ready":
            raise AssertionError("server status did not reconnect to the singleton")
        assert_valid(status, machine, "profile-server-status Machine envelope")

        # The journal-derived index and the running server state form one
        # consistency boundary. Verification must reject a stale state
        # revision instead of reporting a clean membership snapshot.
        state_path = profile_root / "state.json"
        current_state = json.loads(state_path.read_text(encoding="utf-8"))
        current_state["membership_revision"] += 1
        state_path.write_text(json.dumps(current_state), encoding="utf-8")
        state_path.chmod(0o600)
        revision_mismatch = run(
            binary, home, "profile", "membership", "verify", "default"
        )
        revision_mismatch_envelope = envelope(revision_mismatch)
        if (
            revision_mismatch.returncode == 0
            or revision_mismatch_envelope["error"]["code"]
            != "PROFILE_MEMBERSHIP_INCOMPLETE"
        ):
            raise AssertionError(
                f"membership revision mismatch was accepted: {revision_mismatch.stdout}"
            )
        assert_valid(
            revision_mismatch_envelope,
            machine,
            "membership-revision-mismatch Machine envelope",
        )
        current_state["membership_revision"] -= 1
        state_path.write_text(json.dumps(current_state), encoding="utf-8")
        state_path.chmod(0o600)

        # Verification classifies each manifest failure without mutating the
        # journal. An operator-confirmed tombstone then releases only the exact
        # orphan identity after rechecking it under the server lock.
        orphan_ids = {
            "manifest_missing": "01a071ba-0000-7000-8000-000000000003",
            "manifest_malformed": "01a071ba-0000-7000-8000-000000000004",
            "profile_binding_mismatch": "01a071ba-0000-7000-8000-000000000005",
        }
        for orphan_id in orphan_ids.values():
            append_membership(profile_root, "run_registered", workspace_id, orphan_id)
        malformed_root = (
            home / ".dolgorae" / "workspaces" / workspace_id / "runs"
            / orphan_ids["manifest_malformed"]
        )
        malformed_root.mkdir(parents=True, mode=0o700, exist_ok=True)
        malformed_manifest = malformed_root / "manifest.json"
        malformed_manifest.write_text("{", encoding="utf-8")
        malformed_manifest.chmod(0o600)
        write_bound_manifest(
            home,
            pathlib.Path("b" * 64),
            workspace_id,
            orphan_ids["profile_binding_mismatch"],
        )
        orphan_verification = envelope(
            run(binary, home, "profile", "membership", "verify", "default")
        )
        if orphan_verification["data"]["complete"] is not False:
            raise AssertionError("membership verification accepted orphan manifests")
        reasons = {item["reason"] for item in orphan_verification["data"]["orphans"]}
        if reasons != set(orphan_ids):
            raise AssertionError(f"membership orphan reasons were incomplete: {reasons}")
        assert_valid(orphan_verification, machine, "membership-orphans Machine envelope")
        for orphan_id in orphan_ids.values():
            tombstoned = run(
                binary,
                home,
                "profile",
                "membership",
                "tombstone-orphan",
                "default",
                "--operator-file",
                str(operator),
                "--confirm-server-key",
                exact_data["server_key"],
                "--confirm-workspace-id",
                workspace_id,
                "--confirm-run-id",
                orphan_id,
            )
            if tombstoned.returncode != 0 or envelope(tombstoned)["data"]["tombstoned"] is not True:
                raise AssertionError(f"orphan tombstone failed: {tombstoned.stdout}")
            assert_valid(envelope(tombstoned), machine, "membership-tombstone Machine envelope")

        # A Run registered as a live member of this server must not be
        # interrupted by an ordinary stop. Until registration existed the live
        # member set was always empty and every membership gate was vacuous.
        live_member_id = "01a071ba-0000-7000-8000-000000000001"
        write_bound_manifest(home, profile_root, workspace_id, live_member_id)
        append_membership(profile_root, "run_registered", workspace_id, live_member_id)
        gated_remove = run(binary, home, "profile", "remove", "default")
        gated_remove_envelope = envelope(gated_remove)
        if (
            gated_remove.returncode != 4
            or gated_remove_envelope["error"]["code"] != "PROFILE_SERVER_BUSY"
            or gated_remove_envelope["error"]["retryable"] is not True
        ):
            raise AssertionError(f"a remove with a live member was not gated: {gated_remove.stdout}")
        assert_valid(gated_remove_envelope, machine, "live-member remove gate Machine envelope")
        gated_stop = run(
            binary,
            home,
            "profile",
            "server",
            "stop",
            "alias",
            "--operator-file",
            str(operator),
        )
        gated_stop_envelope = envelope(gated_stop)
        if (
            gated_stop.returncode != 4
            or gated_stop_envelope["error"]["code"] != "PROFILE_SERVER_BUSY"
        ):
            raise AssertionError(f"a stop with a live member was not gated: {gated_stop.stdout}")
        assert_valid(gated_stop_envelope, machine, "live-member stop gate Machine envelope")

        # The guard fixture above is deliberately only a minimal manifest. A
        # real forced stop must write into a conformant Run ledger, so release
        # the fixture member and allocate a genuine threadless Run for the
        # operator-override path.
        append_membership(profile_root, "run_closed", workspace_id, live_member_id)
        controller = root / "profile-stop-controller"
        created_controller = run(
            binary,
            home,
            "controller",
            "credential",
            "create",
            "--kind",
            "human-cli",
            "--instance-id",
            "profile-stop-test",
            "--output",
            str(controller),
        )
        if created_controller.returncode != 0:
            raise AssertionError(
                f"profile stop controller creation failed: {created_controller.stdout}"
            )
        started_member = run(
            binary,
            home,
            "run",
            "start",
            "--workspace",
            str(workspace),
            "--profile",
            "alias",
            "--controller-file",
            str(controller),
            "--control-mode",
            "direct-interactive",
            "--execution-lane",
            "shared-readonly",
            "--required-assurance",
            "best-effort-personal-alpha",
            "--purpose",
            "implementation",
            "--instructions",
            "validate Profile operator interruption",
            "--idempotency-key",
            "profile-stop-member",
        )
        if started_member.returncode != 0:
            raise AssertionError(f"profile stop Run allocation failed: {started_member.stdout}")
        live_member_id = envelope(started_member)["data"]["run_id"]

        # A confirmed interrupt is the explicit evidence that authorizes it,
        # and the interrupt is recorded with the members it cost.
        interrupted = run(
            binary,
            home,
            "profile",
            "server",
            "stop",
            "default",
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
        if [member["run_id"] for member in interrupted_members] != [live_member_id]:
            raise AssertionError(
                f"the interrupt diagnostic did not name the members it cost: {evidence[-1]}"
            )
        run_audit = (
            home
            / ".dolgorae"
            / "workspaces"
            / workspace_id
            / "runs"
            / live_member_id
            / "audit.jsonl"
        )
        overrides = [
            record
            for record in (
                json.loads(line)
                for line in run_audit.read_text(encoding="utf-8").splitlines()
                if line
            )
            if record["kind"] == "profile_observed"
            and record["payload"].get("observation") == "operator_override"
        ]
        if (
            len(overrides) != 1
            or overrides[0]["payload"]["profile"] != "default"
            or overrides[0]["payload"]["server_key"] != exact_data["server_key"]
            or overrides[0]["payload"]["outcome"] != "no_active_turn"
        ):
            raise AssertionError(
                f"the forced stop did not append one Run-local operator override: {overrides}"
            )
        repeated_interrupt = run(
            binary,
            home,
            "profile",
            "server",
            "stop",
            "default",
            "--operator-file",
            str(operator),
            "--interrupt",
            "--confirm-server-key",
            exact_data["server_key"],
        )
        if (
            repeated_interrupt.returncode != 0
            or envelope(repeated_interrupt)["data"]["stopped"] is not False
            or sum(
                1
                for line in run_audit.read_text(encoding="utf-8").splitlines()
                if line
                and json.loads(line)["kind"] == "profile_observed"
                and json.loads(line)["payload"].get("observation")
                == "operator_override"
            )
            != 1
        ):
            raise AssertionError(
                f"a completed forced-stop retry was not idempotent: {repeated_interrupt.stdout}"
            )
        released_index = json.loads(
            (profile_root / "members.json").read_text(encoding="utf-8")
        )
        released_member = released_index["members"][f"{workspace_id}:{live_member_id}"]
        if (
            released_member["disposition"] != "released"
            or released_member["lifecycle"] != "operator_interrupt_quiescent"
        ):
            raise AssertionError(
                f"the confirmed quiescent result was overwritten during release: {released_member}"
            )

        # Exercise the same stop path against a real hidden worker with a live
        # Turn. The fixture settles that Turn only after shutdown sends
        # turn/interrupt, so terminal_observed cannot be inferred from an idle
        # projection or from the Profile Server process exiting.
        live_codex_home = root / "live-codex-home"
        live_codex_home.mkdir(mode=0o700)
        live_bin = bin_root / "live"
        live_bin.mkdir(mode=0o700)
        live_codex = live_bin / "codex"
        live_transcript = root / "profile-live-interrupt.jsonl"
        native_codex.create_native_codex(
            live_codex,
            scenario=native_codex.scenario_path("profile_live_interrupt.json"),
            codex_home=live_codex_home,
            schema_source=real_codex,
            transcript=live_transcript,
        )
        live_added = run(
            binary,
            home,
            "profile",
            "add",
            "live",
            *add_arguments(live_codex_home, live_codex),
        )
        if live_added.returncode != 0:
            raise AssertionError(f"live-worker profile add failed: {live_added.stdout}")
        live_started = run(binary, home, "profile", "server", "start", "live")
        if live_started.returncode != 0:
            raise AssertionError(f"live-worker profile start failed: {live_started.stdout}")
        live_server_key = envelope(live_started)["data"]["state"]["server_key"]
        live_member = run(
            binary,
            home,
            "run",
            "start",
            "--workspace",
            str(workspace),
            "--profile",
            "live",
            "--controller-file",
            str(controller),
            "--control-mode",
            "direct-interactive",
            "--execution-lane",
            "shared-readonly",
            "--required-assurance",
            "best-effort-personal-alpha",
            "--purpose",
            "implementation",
            "--instructions",
            "validate live Profile interruption",
            "--idempotency-key",
            "profile-live-stop-member",
        )
        if live_member.returncode != 0:
            raise AssertionError(f"live-worker Run allocation failed: {live_member.stdout}")
        live_run_id = envelope(live_member)["data"]["run_id"]
        submitted = run(
            binary,
            home,
            "run",
            "--controller-file",
            str(controller),
            "submit",
            live_run_id,
            "--workspace",
            str(workspace),
            "--message",
            "remain live until Profile stop",
            "--idempotency-key",
            "profile-live-turn",
        )
        if (
            submitted.returncode != 0
            or envelope(submitted)["data"]["status"] != "accepted"
        ):
            raise AssertionError(f"live-worker Turn did not start: {submitted.stdout}")
        live_stopped = run(
            binary,
            home,
            "profile",
            "server",
            "stop",
            "live",
            "--operator-file",
            str(operator),
            "--interrupt",
            "--confirm-server-key",
            live_server_key,
        )
        if live_stopped.returncode != 0 or envelope(live_stopped)["data"]["stopped"] is not True:
            raise AssertionError(f"live-worker Profile stop failed: {live_stopped.stdout}")
        live_run_root = (
            home / ".dolgorae" / "workspaces" / workspace_id / "runs" / live_run_id
        )
        live_overrides = [
            json.loads(line)["payload"]
            for line in (live_run_root / "audit.jsonl").read_text(encoding="utf-8").splitlines()
            if line
            and json.loads(line)["kind"] == "profile_observed"
            and json.loads(line)["payload"].get("observation") == "operator_override"
        ]
        if len(live_overrides) != 1 or live_overrides[0]["outcome"] != "terminal_observed":
            raise AssertionError(
                f"live worker did not durably record its terminal before Profile stop: {live_overrides}"
            )
        live_profile_root = home / ".dolgorae" / "profiles" / live_server_key
        live_index = json.loads(
            (live_profile_root / "members.json").read_text(encoding="utf-8")
        )
        live_record = live_index["members"][f"{workspace_id}:{live_run_id}"]
        if (
            live_record["disposition"] != "released"
            or live_record["lifecycle"] != "operator_interrupt_terminal"
        ):
            raise AssertionError(f"live-worker membership outcome drifted: {live_record}")
        live_calls = [
            json.loads(line)
            for line in live_transcript.read_text(encoding="utf-8").splitlines()
            if line
        ]
        if sum(call.get("method") == "turn/interrupt" for call in live_calls) != 1:
            raise AssertionError("Profile stop did not interrupt the live worker exactly once")
        live_removed = run(binary, home, "profile", "remove", "live")
        if live_removed.returncode != 0:
            raise AssertionError(f"live-worker profile removal failed: {live_removed.stdout}")

        # Exercise the uncertain branch with a worker whose interrupted Turn
        # never publishes terminal evidence. The stop must retain the observed
        # Turn identity and durably quarantine the Run as outcome_unknown.
        unknown_codex_home = root / "unknown-codex-home"
        unknown_codex_home.mkdir(mode=0o700)
        unknown_bin = bin_root / "unknown"
        unknown_bin.mkdir(mode=0o700)
        unknown_codex = unknown_bin / "codex"
        unknown_transcript = root / "profile-interrupt-unknown.jsonl"
        native_codex.create_native_codex(
            unknown_codex,
            scenario=native_codex.scenario_path("profile_interrupt_unknown.json"),
            codex_home=unknown_codex_home,
            schema_source=real_codex,
            transcript=unknown_transcript,
        )
        unknown_added = run(
            binary,
            home,
            "profile",
            "add",
            "unknown",
            *add_arguments(unknown_codex_home, unknown_codex),
        )
        if unknown_added.returncode != 0:
            raise AssertionError(f"unknown-worker profile add failed: {unknown_added.stdout}")
        unknown_started = run(binary, home, "profile", "server", "start", "unknown")
        if unknown_started.returncode != 0:
            raise AssertionError(f"unknown-worker profile start failed: {unknown_started.stdout}")
        unknown_server_key = envelope(unknown_started)["data"]["state"]["server_key"]
        unknown_member = run(
            binary,
            home,
            "run",
            "start",
            "--workspace",
            str(workspace),
            "--profile",
            "unknown",
            "--controller-file",
            str(controller),
            "--control-mode",
            "direct-interactive",
            "--execution-lane",
            "shared-readonly",
            "--required-assurance",
            "best-effort-personal-alpha",
            "--purpose",
            "implementation",
            "--instructions",
            "validate uncertain Profile interruption",
            "--idempotency-key",
            "profile-unknown-stop-member",
        )
        if unknown_member.returncode != 0:
            raise AssertionError(f"unknown-worker Run allocation failed: {unknown_member.stdout}")
        unknown_run_id = envelope(unknown_member)["data"]["run_id"]
        unknown_submit = run(
            binary,
            home,
            "run",
            "--controller-file",
            str(controller),
            "submit",
            unknown_run_id,
            "--workspace",
            str(workspace),
            "--message",
            "never publish a terminal",
            "--idempotency-key",
            "profile-unknown-turn",
        )
        if unknown_submit.returncode != 0:
            raise AssertionError(f"unknown-worker Turn did not start: {unknown_submit.stdout}")
        unknown_stopped = run(
            binary,
            home,
            "profile",
            "server",
            "stop",
            "unknown",
            "--operator-file",
            str(operator),
            "--interrupt",
            "--confirm-server-key",
            unknown_server_key,
        )
        if unknown_stopped.returncode != 0:
            raise AssertionError(f"unknown-worker Profile stop failed: {unknown_stopped.stdout}")
        unknown_run_root = (
            home / ".dolgorae" / "workspaces" / workspace_id / "runs" / unknown_run_id
        )
        unknown_overrides = [
            json.loads(line)["payload"]
            for line in (unknown_run_root / "audit.jsonl").read_text(encoding="utf-8").splitlines()
            if line
            and json.loads(line)["kind"] == "profile_observed"
            and json.loads(line)["payload"].get("observation") == "operator_override"
        ]
        if (
            len(unknown_overrides) != 1
            or unknown_overrides[0]["outcome"] != "outcome_unknown"
            or unknown_overrides[0]["turn_id"] != "turn-profile-unknown"
        ):
            raise AssertionError(
                f"uncertain Profile stop lost its durable Turn outcome: {unknown_overrides}"
            )
        unknown_index = json.loads(
            (
                home
                / ".dolgorae"
                / "profiles"
                / unknown_server_key
                / "members.json"
            ).read_text(encoding="utf-8")
        )
        unknown_record = unknown_index["members"][f"{workspace_id}:{unknown_run_id}"]
        if (
            unknown_record["disposition"] != "released"
            or unknown_record["lifecycle"] != "interrupted_unknown"
        ):
            raise AssertionError(f"uncertain membership outcome drifted: {unknown_record}")
        unknown_calls = [
            json.loads(line)
            for line in unknown_transcript.read_text(encoding="utf-8").splitlines()
            if line
        ]
        if sum(call.get("method") == "turn/interrupt" for call in unknown_calls) != 1:
            raise AssertionError("uncertain Profile stop did not interrupt exactly once")
        unknown_removed = run(binary, home, "profile", "remove", "unknown")
        if unknown_removed.returncode != 0:
            raise AssertionError(
                f"unknown-worker profile removal failed: {unknown_removed.stdout}"
            )

        # Bring the singleton back for the rest of the matrix, which needs a
        # running old server to migrate away from.
        restarted = run(
            binary, home, "profile", "server", "start", "default"
        )
        if restarted.returncode != 0 or envelope(restarted)["data"]["state"]["lifecycle"] != "ready":
            raise AssertionError(f"server restart after the interrupt failed: {restarted.stdout}")
        assert_valid(envelope(restarted), machine, "profile-server-restart Machine envelope")
        ungated_members = envelope(
            run(binary, home, "profile", "membership", "verify", "default")
        )["data"]
        if ungated_members["complete"] is not True:
            raise AssertionError(
                f"releasing the member left the journal incomplete: {ungated_members}"
            )

        conflicting_arguments = add_arguments(codex_home, fake)
        conflicting_arguments[conflicting_arguments.index("LANG=en_US.UTF-8")] = "LANG=C"
        conflicting_arguments[conflicting_arguments.index("LC_ALL=en_US.UTF-8")] = "LC_ALL=C"
        conflicting_add = run(binary, home, "profile", "add", "conflict", *conflicting_arguments)
        if conflicting_add.returncode != 0:
            raise AssertionError(f"same-home conflict profile add failed: {conflicting_add.stdout}")

        # A diagnostic launch probe may start and stop only its own exact
        # contract. It must never migrate an unrelated, already-running
        # singleton and then tear the replacement down as probe cleanup.
        old_state = envelope(
            run(binary, home, "profile", "server", "status", "default")
        )["data"]["state"]
        conflicting_probe = run(
            binary,
            home,
            "profile",
            "doctor",
            "conflict",
            "--launch-probe",
        )
        if (
            conflicting_probe.returncode != 4
            or envelope(conflicting_probe)["error"]["code"] != "PROFILE_LAUNCH_CONFLICT"
        ):
            raise AssertionError(f"diagnostic probe crossed the active contract: {conflicting_probe.stdout}")
        probe_preserved = envelope(
            run(binary, home, "profile", "server", "status", "default")
        )["data"]["state"]
        if probe_preserved["pid"] != old_state["pid"]:
            raise AssertionError("diagnostic launch probe replaced the existing singleton")

        # A different launch contract cannot auto-roll a server that still
        # owns a live Run. The failed attempt must leave the old process and
        # active contract untouched.
        rollover_member_id = "01a071ba-0000-7000-8000-000000000002"
        write_bound_manifest(home, profile_root, workspace_id, rollover_member_id)
        append_membership(profile_root, "run_registered", workspace_id, rollover_member_id)
        conflicting_start = run(
            binary, home, "profile", "server", "start", "conflict"
        )
        if (
            conflicting_start.returncode != 4
            or envelope(conflicting_start)["error"]["code"] != "PROFILE_SERVER_BUSY"
        ):
            raise AssertionError(f"live same-home singleton was not preserved: {conflicting_start.stdout}")
        preserved = envelope(
            run(binary, home, "profile", "server", "status", "default")
        )["data"]["state"]
        if preserved["pid"] != old_state["pid"] or preserved["server_key"] != old_state["server_key"]:
            raise AssertionError("failed automatic rollover changed the live source server")

        # Once the source membership is empty, starting the new contract
        # retires only the Dolgorae-managed singleton and publishes the new
        # generation without an operator credential.
        append_membership(profile_root, "run_closed", workspace_id, rollover_member_id)
        conflicting_start = run(
            binary, home, "profile", "server", "start", "conflict"
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
            binary, home, "profile", "server", "start", "default"
        )
        returned_data = envelope(returned)["data"] if returned.returncode == 0 else {}
        if returned.returncode != 0 or returned_data["state"]["server_key"] != exact_data["server_key"]:
            raise AssertionError(f"rollover back to the original contract failed: {returned.stdout}")
        assert_valid(envelope(returned), machine, "quiescent-rollover-return Machine envelope")

        conflicting_remove = run(
            binary, home, "profile", "remove", "conflict"
        )
        if conflicting_remove.returncode != 0:
            raise AssertionError(f"conflict profile cleanup failed: {conflicting_remove.stdout}")
        assert_valid(envelope(conflicting_remove), machine, "profile-remove Machine envelope")

        set_mode(fake, version="0.158.1")
        migrated_snapshot = envelope(
            run(binary, home, "profile", "doctor", "default")
        )["data"]
        migrated = run(
            binary,
            home,
            "profile",
            "server",
            "migrate",
            "default",
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
                "--operator-fd",
                str(capability.fileno()),
                pass_fds=(capability.fileno(),),
            )
        if stopped.returncode != 0 or envelope(stopped)["data"]["stopped"] is not True:
            raise AssertionError(f"server stop failed: {stopped.stdout}")
        assert_valid(envelope(stopped), machine, "profile-server-final-stop Machine envelope")
        stopped_status = envelope(
            run(binary, home, "profile", "server", "status", "default")
        )
        if stopped_status["data"]["lifecycle"] != "stopped":
            raise AssertionError("stopped server status did not report stopped")
        assert_valid(stopped_status, machine, "stopped profile-server-status Machine envelope")
        set_mode(fake)
        membership = envelope(run(binary, home, "profile", "membership", "verify", "default"))
        if (
            membership["data"]["complete"] is not True
            or membership["data"]["records"] != membership["data"]["revision"]
            or membership["data"]["orphans"]
        ):
            raise AssertionError("canonical global membership did not verify cleanly")
        assert_valid(membership, machine, "profile-membership-verify Machine envelope")

        state = envelope(run(binary, home, "profile", "diagnostics", "list", "default"))
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

        home_hash = hashlib.sha256(
            b"dolgorae-home-v1\0" + exact_data["expected_codex_home"].encode("utf-8")
        ).hexdigest()
        home_dir = home / ".dolgorae" / "homes" / home_hash
        home_dir.mkdir(parents=True, exist_ok=True)
        home_dir.chmod(0o700)

        # Recreate the durable residue of a crash after interrupt-stop APPLY:
        # the physical lifetime is gone, while state, the stopping reservation,
        # and an attached Run remain. State reset must release that exact epoch
        # as interrupted_unknown and clear both lifecycle records.
        stranded_state = returned_data["state"]
        state_path = profile_root / "state.json"
        state_path.write_text(json.dumps(stranded_state), encoding="utf-8")
        state_path.chmod(0o600)
        stranded_member_id = "01a071ba-0000-7000-8000-000000000007"
        write_bound_manifest(home, profile_root, workspace_id, stranded_member_id)
        append_membership(
            profile_root,
            "run_registered",
            workspace_id,
            stranded_member_id,
            stranded_state["server_epoch"],
        )
        active_path = home_dir / "active.json"
        active_path.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "canonical_codex_home": exact_data["expected_codex_home"],
                    "server_key": exact_data["server_key"],
                    "server_epoch": stranded_state["server_epoch"],
                    "lifecycle": "stopping",
                    "pid": stranded_state["pid"],
                    "transition_token": "00000000-0000-7000-8000-000000000001",
                }
            ),
            encoding="utf-8",
        )
        active_path.chmod(0o600)
        set_mode(fake, version="0.158.1")
        changed_contract = run(binary, home, "profile", "doctor", "default")
        if (
            changed_contract.returncode != 0
            or envelope(changed_contract)["data"]["server_key"] == exact_data["server_key"]
        ):
            raise AssertionError(f"version change did not produce a new contract: {changed_contract.stdout}")
        recovered_stop = run(
            binary,
            home,
            "profile",
            "state",
            "reset",
            "default",
            "--operator-file",
            str(operator),
            "--confirm-server-key",
            exact_data["server_key"],
            "--require-server-absence",
        )
        if recovered_stop.returncode != 0:
            raise AssertionError(
                f"state reset did not recover a stranded interrupt stop: {recovered_stop.stdout}"
            )
        assert_valid(
            envelope(recovered_stop), machine, "stranded-stop-state-reset Machine envelope"
        )
        set_mode(fake)
        if state_path.exists() or active_path.exists():
            raise AssertionError("state reset left a stranded lifecycle record")
        recovered_membership = envelope(
            run(binary, home, "profile", "membership", "verify", "default")
        )
        recovered_member = next(
            member
            for member in recovered_membership["data"]["members"]
            if member["workspace_id"] == workspace_id
            and member["run_id"] == stranded_member_id
        )
        if (
            recovered_member["disposition"] != "released"
            or recovered_member["lifecycle"] != "interrupted_unknown"
        ):
            raise AssertionError(
                f"state reset did not classify the stranded member: {recovered_member}"
            )
        assert_valid(
            recovered_membership, machine, "recovered-stop-membership Machine envelope"
        )

        # Without state, a dangling rendezvous has no recorded process or
        # inode identity. Reset must report the collision instead of claiming
        # success and leaving the next start blocked.
        unrecorded_socket = pathlib.Path(stranded_state["socket_path"])
        unrecorded_socket.symlink_to(unrecorded_socket.with_name("missing.sock"))
        blocked_reset = run(
            binary,
            home,
            "profile",
            "state",
            "reset",
            "default",
            "--operator-file",
            str(operator),
            "--confirm-server-key",
            exact_data["server_key"],
            "--require-server-absence",
        )
        blocked_reset_envelope = envelope(blocked_reset)
        if (
            blocked_reset.returncode != 4
            or blocked_reset_envelope["error"]["code"] != "PROFILE_SERVER_BUSY"
            or not unrecorded_socket.is_symlink()
        ):
            raise AssertionError(
                f"state reset accepted an unrecorded dangling socket: {blocked_reset.stdout}"
            )
        unrecorded_socket.unlink()

        # Fabricate a blocked migration fence naming this now-absent server
        # key and a never-started key. State reset proves both lifetimes absent
        # and repairs the fence as a best-effort side effect.
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

        removed = run(binary, home, "profile", "remove", "default")
        if removed.returncode != 0 or envelope(removed)["data"]["removed"] is not True:
            raise AssertionError(f"profile remove failed: {removed.stdout}")
        assert_valid(envelope(removed), machine, "final profile-remove Machine envelope")


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
