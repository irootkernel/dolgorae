"""Run the fake app-server from the command line.

    python3 tools/fake_app_server --socket /path/server.sock \
        --scenario tools/fake_app_server/scenarios/multi_turn_read_only.json \
        --bind codex_home=/tmp/home --ready-fd 3
"""

from __future__ import annotations

import argparse
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

import scenario as scenario_module  # noqa: E402
import server as server_module  # noqa: E402


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="fake_app_server")
    parser.add_argument("--socket", type=pathlib.Path, required=True)
    parser.add_argument("--scenario", type=pathlib.Path, required=True)
    parser.add_argument("--ready-fd", type=int, default=None)
    parser.add_argument(
        "--transcript",
        type=pathlib.Path,
        default=None,
        help="append every client message to this file, one JSON line each",
    )
    parser.add_argument(
        "--bind",
        action="append",
        default=[],
        metavar="NAME=VALUE",
        help="bind a ${NAME} placeholder used by the scenario",
    )
    arguments = parser.parse_args(argv)
    overrides = {}
    for binding in arguments.bind:
        name, separator, value = binding.partition("=")
        if not separator:
            parser.error(f"--bind expects NAME=VALUE, got {binding!r}")
        overrides[name] = value
    scenario = scenario_module.Scenario.load(arguments.scenario, overrides)
    fake = server_module.FakeAppServer(
        arguments.socket, scenario, arguments.ready_fd, arguments.transcript
    )
    fake.bind()
    try:
        fake.serve_forever()
    except KeyboardInterrupt:
        return 0
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
