# Fake app-server fixture

An independent Codex app-server stand-in: a Python subprocess that binds one
Unix socket, completes the WebSocket upgrade, and answers JSON-RPC from a
manifest-validated declarative scenario.

ADR-014 requires this independence. A fake that shares the production ingest
path can certify the same mistake twice, so nothing here imports the Rust crate
and nothing here uses Python's `json` module for inbound protocol text — that
module keeps the last of a set of duplicate members and discards number lexemes,
which is exactly the behaviour the product refuses. `jsonlite.py` is a strict
reader written for this fixture: it rejects duplicate object members and keeps
every number as its source lexeme.

## Running it

```sh
python3 tools/fake_app_server \
    --socket "$TMPDIR/app-server.sock" \
    --scenario tools/fake_app_server/scenarios/multi_turn_read_only.json \
    --bind codex_home=/tmp/codex-home \
    --ready-fd 3
```

`--ready-fd` writes one `ready\n` line after the socket is bound and permissions
are set, so a test never races the listener. `--bind NAME=VALUE` fills a
`${NAME}` placeholder used by the scenario. `--transcript PATH` appends every
client message verbatim, one JSON line each, so a case can assert about what the
client did *not* send — a reply into a Thread it does not own, for instance.

## Scenarios

`manifest.json` is the schema; `scenario.py` validates against it before the
socket is bound, so a malformed scenario is a startup failure rather than a
silent no-answer. A scenario is a list of steps keyed by inbound method:

| Member | Meaning |
| --- | --- |
| `method` | inbound JSON-RPC method this step answers |
| `occurrence` | match only the *n*-th call of that method |
| `respond.result` | result value for a request |
| `respond.error` | error object for a request |
| `respond.generate` | build a reply too large to hold in one message |
| `respond.silent` | accept the request and answer nothing |
| `emit` | notifications, server requests, or a close, sent after the reply |
| `emit[].await_reply` | hold this emission, and every one after it in the step, until the client answers the request just sent |

`await_reply` is what keeps an approval honest: a real app-server does not
finish a Turn while it is still waiting on one, so a scenario that emitted the
approval and its Turn's completion together would certify a client that answers
an interaction the Run has already left behind.

`fragment_bytes` splits every outbound message across WebSocket continuation
frames, including across UTF-8 sequence boundaries, so a client that validates
text per frame instead of per message is caught.

| Scenario | Proves |
| --- | --- |
| `multi_turn_read_only.json` | one Thread across several Turns, an approval interaction, and an interrupt |
| `streamed_thread_read.json` | a terminal Turn read back from a Thread history larger than any single-message bound |
| `foreign_thread_request.json` | a shared server asking this connection to decide for another Thread |
| `shutdown_interrupt.json` | a Turn that runs until the worker interrupts it on shutdown |
| `model_list_paginated.json` | a `model/list` catalogue whose wanted model appears only on the last page |
| `model_list_shape_faults.json` | five intact `model/list` replies that disagree with the pinned Codex 0.149 shapes, one per call |
| `model_list_unbounded_pagination.json` | a `model/list` that hands out one more cursor forever |
| `run_start_model_list.json` | every `model/list` walk one `dolgorae run … start` performs — the profile probe's, and one effort resolution per start — plus a worker attach refused once |

The four `model_list`/`run_start` scenarios are what makes model resolution
provable without a Codex on the machine: the walk, its bound, and every shape
the pinned required subset fixes are decided against this fixture instead of
against whatever release happens to be installed.

`run_start_model_list.json` is the one a black-box argv case drives. It is
reached through a compiled native `codex` image the Runtime Profile launches
itself (`tests/e2e/native_codex.py`), because SPEC-013 refuses an interpreter
script as `argv[0]`; the installed Codex is then used for nothing but
generating the pinned JSON Schema bundle.
