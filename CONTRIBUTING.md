# Contributing to Dolgorae

Read [docs/README.md](docs/README.md) before changing a product contract. Update
the owning specification or architecture document before derived protocol,
implementation, test, or roadmap changes.

## Development setup

Install Rust 1.97.1, Buf 1.66.1, and Python 3. Then create an isolated Python
environment for the small repository checks:

```sh
python3 -m venv .venv
.venv/bin/python -m pip install -r tools/validation/requirements.txt
```

Run the complete gate from the repository root:

```sh
PYTHON_BIN=.venv/bin/python tools/check.sh
```

The gate runs Rust formatting, linting, unit and integration tests, Buf checks,
JSON duplicate-key and schema meta-validation, Markdown-link validation, and
black-box CLI/workspace tests. Put product semantics in Rust and its tests. Keep
Python limited to small independent repository checks or black-box executable
tests.

Do not commit virtual environments, caches, generated review material, local
workflow state, credentials, or run/session identifiers.
