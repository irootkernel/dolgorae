# Dolgorae Implementation Tips

This document owns non-normative guidance for changing, testing, and preparing
the Dolgorae implementation. Read the [documentation authority map](../README.md)
before changing a product contract, and update the owning specification or
architecture document before derived protocol, implementation, test, or roadmap
changes.

## Development setup

Install Rust 1.97.1, Buf 1.69.0, and Python 3. Then create an isolated Python
environment for the small repository checks:

```sh
python3 -m venv .venv
.venv/bin/python -m pip install -r tools/validation/requirements.txt
```

Run the complete gate from the repository root:

```sh
make PYTHON_BIN=.venv/bin/python test
```

The Make targets define four ordered validation layers:

- `make test-prepare` applies `cargo fmt`, then runs Clippy, `cargo check`,
  architecture guardrails, Buf checks, JSON duplicate-key and schema
  meta-validation, schema-example validation, Markdown-link validation,
  source-distributed agent-skill validation, Aquarium development-channel
  producer tests, and Git whitespace checks.
- `make test-unit` runs Rust library and binary unit tests.
- `make test-int` runs the explicitly selected Rust integration suites using
  fakes, stubs, and isolated filesystem fixtures. It does not invoke the
  Dolgorae executable, Git, a database, the network, or another external
  system.
- `make test-e2e` builds Dolgorae and runs Python black-box scenarios in an
  isolated temporary home. E2E uses a real Git installation and fails when its
  required external prerequisites are unavailable.

`make test` runs those targets in that order. Because `test-prepare` applies
formatting, it may modify Rust source files without changing their meaning. Use
`make format-check` when a read-only formatting check is required.

Put product semantics in Rust and its tests. Keep Python limited to small
independent repository checks or black-box executable tests. Any future E2E
database or external service must use test-only credentials and namespaces;
production state and the user's real home must never be reused.

Do not commit virtual environments, caches, generated review material, local
workflow state, credentials, or run/session identifiers.

For task-aware Specialist work, keep reusable Role behavior in the Agent
Configuration and executable brief/context/criteria in `src/task_request.rs`.
Exercise one-shot composition in `src/review.rs` and reusable facade behavior in
`src/external_engagement.rs`; both must persist the accepted JCS request before
dispatch and validate `structured_review_v3` before artifact commit. Test
Korean, CRLF, quotes, Markdown, and shell metacharacters as data, plus every
count and byte boundary. Fake-runtime E2E must assert that task bytes appear in
the Turn input but not `developerInstructions`, and must not be described as a
live-provider acceptance result.

## Orchestration core verification

Keep Role and policy semantics in `src/specialist_policy.rs` and durable
aggregate behavior in `src/orchestration.rs`. The orchestration adapter is the
effect boundary: tests should inject deterministic failures immediately before
and after SQLite commit, Primary intent publication, child reservation, Worker
publication, thread creation, task dispatch, result append, and delivery
receipt. A retry may resume only when durable state proves that no external
effect was accepted; ambiguous publication or Turn acceptance must remain
`recovery_required` or `interrupted_unknown`.

Use isolated temporary state roots for broker tests and check both SQLite and
the protected credential carrier. Raw capabilities must not appear in the
database, events, tool results, or diagnostics; the carrier must be mode 0600
and disappear only after authoritative Specialist release. Schema examples and
the fake adapter prove the transport-independent core. They do not prove the
later live model-facing tool transport, and the opt-in live Codex targets remain
outside the complete repository gate.
