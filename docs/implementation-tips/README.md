# Dolgorae Implementation Tips

This document owns non-normative guidance for changing, testing, and preparing
the Dolgorae implementation. Read the [documentation authority map](../README.md)
before changing a product contract, and update the owning specification or
architecture document before derived protocol, implementation, test, or roadmap
changes.

## Development setup

Install Rust 1.97.1, Buf 1.66.1, and Python 3. Then create an isolated Python
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
  meta-validation, Markdown-link validation, and Git whitespace checks.
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
