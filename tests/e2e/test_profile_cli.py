#!/usr/bin/env python3
"""Black-box Runtime Profile and Codex compatibility validation."""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import shutil
import stat
import subprocess
import sys
import tempfile


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


def create_fake_codex(path: pathlib.Path, real_codex: pathlib.Path) -> None:
    program = f"""#!{sys.executable}
import json
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
    path.write_text(program, encoding="utf-8")
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


def validate(binary: pathlib.Path) -> None:
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
        if added.returncode != 0 or envelope(added)["data"]["name"] != "default":
            raise AssertionError(f"profile add failed: {added.stdout}")
        registry_path = next((home / "Library" / "Application Support" / "Dolgorae" / "workspaces").glob("*/local.yaml"))
        if stat.S_IMODE(registry_path.stat().st_mode) != 0o600:
            raise AssertionError("profile registry mode is not 0600")

        duplicate = run(binary, home, "profile", "add", "default", *add_arguments(workspace, codex_home, fake))
        if duplicate.returncode != 4 or envelope(duplicate)["error"]["code"] != "PROFILE_ALREADY_EXISTS":
            raise AssertionError(f"duplicate profile was not rejected: {duplicate.stdout}")

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
        if exact_data["compatibility"] != "tested" or exact_data["codex_version"] != "0.149.0":
            raise AssertionError(f"exact compatibility returned wrong facts: {exact.stdout}")

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
        required_probe_capabilities = {
            "account_read",
            "app_server_initialize",
            "early_response_id",
            "model_list",
            "thread_absence_error",
        }
        if not required_probe_capabilities.issubset(launched_data["capabilities"]):
            raise AssertionError("launch probe did not publish its profile capability snapshot")
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

        operator = root / "operator"
        operator.write_text("test-only-operator\n", encoding="utf-8")
        operator.chmod(0o600)
        started = run(binary, home, "profile", "server", "start", "default", "--workspace", str(workspace))
        started_data = envelope(started)["data"]
        if started.returncode != 0 or started_data["state"]["lifecycle"] != "ready":
            raise AssertionError(f"server start failed: {started.stdout}")
        status = envelope(run(binary, home, "profile", "server", "status", "default", "--workspace", str(workspace)))
        if status["data"]["lifecycle"] != "ready":
            raise AssertionError("server status did not reconnect to the singleton")

        conflicting_arguments = add_arguments(workspace, codex_home, fake)
        conflicting_arguments[conflicting_arguments.index("LANG=en_US.UTF-8")] = "LANG=C"
        conflicting_arguments[conflicting_arguments.index("LC_ALL=en_US.UTF-8")] = "LC_ALL=C"
        conflicting_add = run(binary, home, "profile", "add", "conflict", *conflicting_arguments)
        if conflicting_add.returncode != 0:
            raise AssertionError(f"same-home conflict profile add failed: {conflicting_add.stdout}")
        conflicting_start = run(
            binary, home, "profile", "server", "start", "conflict", "--workspace", str(workspace)
        )
        if (
            conflicting_start.returncode != 4
            or envelope(conflicting_start)["error"]["code"] != "PROFILE_LAUNCH_CONFLICT"
        ):
            raise AssertionError(f"same-home singleton conflict was not rejected: {conflicting_start.stdout}")
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
        set_mode(fake)
        membership = envelope(run(binary, home, "profile", "membership", "verify", "default", "--workspace", str(workspace)))
        if membership["data"]["complete"] is not True or membership["data"]["revision"] < 2:
            raise AssertionError("membership journal did not retain both generations")

        state = envelope(run(binary, home, "profile", "diagnostics", "list", "default", "--workspace", str(workspace)))
        if not state["data"]["items"]:
            raise AssertionError("profile diagnostics are empty")

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
        removed = run(binary, home, "profile", "remove", "default", "--workspace", str(workspace))
        if removed.returncode != 0 or envelope(removed)["data"]["removed"] is not True:
            raise AssertionError(f"profile remove failed: {removed.stdout}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    arguments = parser.parse_args()
    validate(arguments.binary.resolve())
    print("Profile CLI validation passed: registry, compatibility, singleton, membership")
    return 0


if __name__ == "__main__":
    sys.exit(main())
