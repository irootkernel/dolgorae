#!/usr/bin/env python3
"""Fault injection at the native-host / prototype stdio MCP boundary.

Native request frames remain in memory. The selected prototype is the real
backend executable; this proxy never invents a positive native-host call.
"""

from concurrent.futures import ThreadPoolExecutor
import copy
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import time


PRIVATE = "_dolgorae_host_binding"
LIMIT = 1024 * 1024


class Failure(Exception):
    pass


def require(condition: bool, code: str) -> None:
    if not condition:
        raise Failure(code)


def rpc(prototype: str, state: Path, route: str, request: dict, discard: bool = False) -> dict | None:
    initialize = {"jsonrpc": "2.0", "id": "fault-proxy-initialize", "method": "initialize",
                  "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                             "clientInfo": {"name": "bounded-fault-proxy", "version": "1"}}}
    frames = json.dumps(initialize) + "\n" + json.dumps(request) + "\n"
    result = subprocess.run([prototype, "host-mcp", str(state), route], input=frames,
                            capture_output=True, text=True, timeout=8)
    require(result.returncode == 0, "backend_process_failed")
    if discard:
        # Transport intentionally loses the reply before native-host delivery.
        return None
    responses = [json.loads(line) for line in result.stdout.splitlines()]
    require(len(responses) == 2 and responses[-1].get("id") == request.get("id"), "backend_response_mismatch")
    return responses[-1]


def receipt(response: dict) -> dict:
    require("error" not in response, "backend_trusted_admission_refused")
    content = response.get("result", {}).get("content", [])
    require(len(content) == 1, "backend_receipt_shape_invalid")
    value = json.loads(content[0]["text"])
    require(value.get("schema") == "dolgorae.host-probe-receipt/v1", "backend_receipt_schema_invalid")
    return value


def lookup(prototype: str, state: Path, route: str, reference: str) -> tuple[int, dict | None]:
    result = subprocess.run([prototype, "lookup", str(state), route, reference],
                            capture_output=True, text=True, timeout=8)
    return result.returncode, json.loads(result.stdout) if result.returncode == 0 else None


def state_rows(state: Path) -> list:
    with sqlite3.connect(state / "invocations.sqlite") as db:
        return db.execute("SELECT native_key,request_ref,status,receipt FROM calls ORDER BY rowid").fetchall()


def write_summary(path: Path, value: dict) -> None:
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(descriptor, "w") as output:
        json.dump(value, output, sort_keys=True)


def negative_state(prototype: str, state: Path, route: str, request: dict, parent: Path) -> dict:
    rows = state_rows(state)
    key = json.loads(rows[0][0])
    event = {"hook_event_name": "PreToolUse", "session_id": key[1], "turn_id": key[2],
             "tool_use_id": key[3], "tool_name": "mcp__dolgorae__dolgorae_identity_probe",
             "mcp_server": {"name": "dolgorae", "source": "dynamic"},
             "tool_input": {k: v for k, v in request["params"]["arguments"].items() if k != PRIVATE}}
    observations = {}
    for label in ("missing", "corrupted"):
        target = parent / ("negative-state-" + label)
        target.mkdir(mode=0o700)
        database = target / "invocations.sqlite"
        if label == "corrupted":
            with sqlite3.connect(state / "invocations.sqlite") as source, sqlite3.connect(database) as dest:
                source.backup(dest)
            os.chmod(database, 0o600)
            database.write_bytes(b"intentionally-corrupted-contract-fixture")
        before = hashlib.sha256(database.read_bytes()).hexdigest() if database.exists() else None
        hook = subprocess.run([prototype, "host-hook", str(target), route], input=json.dumps(event),
                              capture_output=True, text=True, timeout=8)
        denied = json.loads(hook.stdout).get("hookSpecificOutput", {}).get("permissionDecision") == "deny"
        rejected = rpc(prototype, target, route, request)
        code, _ = lookup(prototype, target, route, rows[0][1])
        after = hashlib.sha256(database.read_bytes()).hexdigest() if database.exists() else None
        require(denied and "error" in rejected and code != 0 and before == after,
                label + "_state_did_not_refuse_without_replacement")
        observations[label + "_state"] = "refused_without_new_identity"
    observations["lookup_failure"] = "refused_without_redispatch"
    return observations


def matrix(prototype: str, state: Path, route: str, request: dict, parent: Path) -> tuple[dict, dict]:
    rows = state_rows(state)
    require(len(rows) == 1 and rows[0][2] == "prepared", "initial_native_request_not_prepared")
    reference = rows[0][1]
    # Start a real replacement backend but lose delivery before sending tools/call.
    process = subprocess.Popen([prototype, "host-mcp", str(state), route], stdin=subprocess.PIPE,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    process.terminate()
    process.wait(timeout=5)
    code, unresolved = lookup(prototype, state, route, reference)
    require(code == 0 and unresolved == {"status": "unresolved_effect"}, "pre_acceptance_loss_not_conservative")
    require(state_rows(state) == rows, "pre_acceptance_loss_changed_identity")

    # Commit acceptance+receipt atomically, then drop the actual stdio reply.
    rpc(prototype, state, route, request, discard=True)
    code, accepted = lookup(prototype, state, route, reference)
    require(code == 0 and accepted and accepted.get("acceptances") == 1, "lost_acceptance_reply_not_recoverable")
    require(accepted.get("external_request_ref") == reference, "lookup_changed_reference")
    first = rpc(prototype, state, route, request)
    require(receipt(first) == accepted, "exact_replay_changed_receipt")
    with ThreadPoolExecutor(max_workers=8) as pool:
        replayed = list(pool.map(lambda _: rpc(prototype, state, route, request), range(8)))
    require(all(receipt(value) == accepted for value in replayed), "concurrent_replay_changed_receipt")
    changed = copy.deepcopy(request)
    changed["params"]["arguments"]["payload"] = "changed-input-conflict"
    require("error" in rpc(prototype, state, route, changed), "input_conflict_not_rejected")
    require(len(state_rows(state)) == 1, "replay_or_conflict_created_identity")

    # A further lost committed-receipt replay is resolved by the same lookup.
    rpc(prototype, state, route, request, discard=True)
    require(lookup(prototype, state, route, reference) == (0, accepted), "committed_receipt_loss_changed_lookup")
    require(receipt(rpc(prototype, state, route, request)) == accepted, "replacement_stdio_process_changed_receipt")
    failures = negative_state(prototype, state, route, request, parent)
    observations = {
        "initial_native_request": "passed", "exact_replay": "same_receipt",
        "changed_input_conflict": "refused", "concurrent_replay_8": "same_receipt_one_identity",
        "backend_stdio_reconnect": "same_receipt", "backend_mcp_process_restart": "same_receipt",
        "loss_before_acceptance": "retained_prepared_identity_lookup_unresolved",
        "loss_after_acceptance_before_host_ack": "same_receipt_by_lookup",
        "loss_after_receipt_commit": "same_receipt_by_lookup_and_replay",
        "acceptance_receipt_commit": "one_atomic_transaction", **failures,
        "pending_native_call_replay_after_host_resume": "not_tested",
        "tested_layer": "native_host_produced_request_to_actual_prototype_stdio_backend",
        "native_request_frames_persisted": False,
    }
    return first, observations


def main() -> int:
    prototype, state_arg, route, summary_arg = sys.argv[1:5]
    crash_case = sys.argv[5:] == ["crash-before-ack"]
    state, summary = Path(state_arg), Path(summary_arg)
    calls = 0
    observations = json.loads(summary.read_text()) if summary.exists() else {}
    starts = observations.get("outer_stdio_process_starts", 0) + 1
    observations["outer_stdio_process_starts"] = starts
    try:
        for raw in sys.stdin.buffer:
            require(len(raw) <= LIMIT, "native_frame_outside_bound")
            request = json.loads(raw)
            if "id" not in request:
                continue
            if request.get("method") == "tools/call":
                calls += 1
                require(calls <= (2 if starts == 1 else 1), "native_call_budget_exceeded")
                if crash_case and starts == 1:
                    rpc(prototype, state, route, request, discard=True)
                    values = state_rows(state)
                    require(len(values) == 1 and values[0][2] == "accepted", "crash_point_receipt_not_committed")
                    observations.update({"crash_after_receipt_commit_before_native_ack": True,
                                         "native_reply_withheld": True, "native_calls_observed": 1})
                    write_summary(summary, observations)
                    write_summary(summary.parent / "crash-ready.local", {"ready": True})
                    # The parent harness kills the real native host and this
                    # entire MCP process group before a result can be delivered.
                    while True:
                        time.sleep(1)
                elif crash_case:
                    response = rpc(prototype, state, route, request)
                    values = state_rows(state)
                    require(len(values) == 2 and values[0][1] != values[1][1], "crash_resume_new_call_not_distinct")
                    require(receipt(response).get("external_request_ref") == values[-1][1], "crash_resume_receipt_mismatch")
                    observations.update({"native_calls_observed": 2,
                                         "outer_native_mcp_transport_restart": "fresh_proxy_process_accepted_new_native_call"})
                elif calls == 1 and starts == 1:
                    response, observations = matrix(prototype, state, route, request, summary.parent)
                    observations["outer_stdio_process_starts"] = starts
                else:
                    response = rpc(prototype, state, route, request)
                    values = state_rows(state)
                    expected = 2 if starts == 1 else 3
                    require(len(values) == expected and len({row[1] for row in values}) == expected,
                            "distinct_native_call_was_deduplicated")
                    require(receipt(response).get("external_request_ref") == values[-1][1], "later_native_receipt_mismatch")
                    observations["distinct_native_calls_identical_input"] = "distinct_identities"
                    observations["native_calls_observed"] = expected
                    if starts > 1:
                        observations["outer_native_mcp_transport_restart"] = "fresh_proxy_process_accepted_new_native_call"
                write_summary(summary, observations)
            else:
                response = rpc(prototype, state, route, request)
            print(json.dumps(response), flush=True)
        return 0
    except (Failure, OSError, ValueError, sqlite3.Error, subprocess.SubprocessError) as error:
        observations["failure"] = str(error) if isinstance(error, Failure) else "fault_proxy_failed"
        write_summary(summary, observations)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
