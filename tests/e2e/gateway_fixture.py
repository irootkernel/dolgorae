#!/usr/bin/env python3
"""Prepare an isolated native fake-Codex account for public gateway tests."""
from __future__ import annotations

import argparse
import json
import os
import pathlib
import subprocess

import native_codex


def prepare(binary: pathlib.Path, root: pathlib.Path, scenario_name: str) -> None:
    root = root.resolve()
    environment = dict(os.environ, HOME=str(root / "home"))
    for name in ("home", "workspace", "codex-home", "bin", "tmp", "config", "cache"):
        path = root / name
        path.mkdir(mode=0o700, exist_ok=True)
        path.chmod(0o700)
    environment.update(TMPDIR=str(root / "tmp"), XDG_CONFIG_HOME=str(root / "config"), XDG_CACHE_HOME=str(root / "cache"))

    def cli(*args: str) -> dict:
        result = subprocess.run([str(binary), *args], env=environment, capture_output=True, text=True, timeout=90)
        if result.returncode:
            raise AssertionError(f"fixture command failed {args}: {result.stdout} {result.stderr}")
        envelope = json.loads(result.stdout)
        if not envelope["ok"]:
            raise AssertionError(f"fixture command rejected {args}: {envelope}")
        return envelope["data"]

    workspace = cli("init", str(root / "workspace"), "--non-git")
    scenario = pathlib.Path(scenario_name)
    if not scenario.is_file():
        variant = scenario_name in ("gateway_active", "gateway_pressure")
        scenario = native_codex.scenario_path("run_start_model_list.json" if variant else scenario_name)
        if variant:
            document = json.loads(scenario.read_text())
            for step in document["steps"]:
                if step["method"] == "turn/start":
                    step.pop("emit", None)
            if scenario_name == "gateway_pressure":
                document["steps"] = [step for step in document["steps"] if step["method"] != "turn/start"]
                for index in range(1, 8):
                    step = {"method": "turn/start", "occurrence": index, "respond": {"result": {"turn": {"id": f"turn-{index}"}}}}
                    if index <= 6:
                        step["emit"] = [{"kind": "notification", "method": "turn/completed", "params": {"threadId": "${thread_id}", "turn": {"id": f"turn-{index}", "status": "completed", "items": [{"type": "agentMessage", "phase": "final_answer", "status": "completed", "text": "x" * 900_000}]}}}]
                    document["steps"].append(step)
            document["steps"].append({"method": "turn/interrupt", "respond": {"result": {"interrupted": True}}, "emit": [{"kind": "notification", "method": "turn/completed", "params": {"threadId": "${thread_id}", "turn": {"id": "${turn_id}", "status": "interrupted", "items": []}}}]})
            scenario = root / "scenario.json"
            scenario.write_text(json.dumps(document))
    native_codex.create_native_codex(root / "bin" / "codex", scenario=scenario.resolve(), codex_home=root / "codex-home", schema_source=native_codex.installed_codex(), transcript=root / "transcript.jsonl")
    cli("profile", "add", "default", "--codex-home", str(root / "codex-home"), "--native-subagents", "enabled", "--env", "PATH=/usr/bin:/bin:/usr/sbin:/sbin", "--env", "LANG=en_US.UTF-8", "--env", "LC_ALL=en_US.UTF-8", "--", str(root / "bin" / "codex"))
    cli("operator", "credential", "initialize", "--output", str(root / "operator.json"))
    carrier_root = root / "home" / ".dolgorae" / "controller-carriers"
    carrier_parent = carrier_root / "gateway-native" / "test-installation"
    for directory in (carrier_root, carrier_parent.parent, carrier_parent):
        directory.mkdir(mode=0o700, exist_ok=True)
        directory.chmod(0o700)
    controller = cli("controller", "credential", "create", "--kind", "automation", "--instance-id", "gateway-native-test", "--output", str(carrier_parent / "controller.json"))
    server = cli("profile", "server", "start", "default")["state"]
    configuration = {"workspace_id": workspace["workspace_id"], "controller_id": controller["controller"]["controller_id"], "server_key": server["server_key"]}
    output = root / "fixture.json"
    output.write_text(json.dumps(configuration))
    output.chmod(0o600)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument("--root", type=pathlib.Path, required=True)
    parser.add_argument("--scenario", required=True)
    args = parser.parse_args()
    prepare(args.binary.resolve(), args.root, args.scenario)


if __name__ == "__main__":
    main()
