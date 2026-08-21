# Dolgorae

Dolgorae is a local, durable control layer for persistent Codex runs. It adds
stable run identity, controller authorization, workspace writer coordination,
recovery, and auditability while leaving conversation storage with Codex.

The implementation is written in Rust. The product contract is defined by the
[specification](docs/specs.md), [architecture](docs/architecture.md), accepted
[architecture decisions](docs/architecture-decisions.md), and checked
[protocol](docs/protocol/) artifacts. The [roadmap](docs/roadmap.md) is the sole
delivery-status authority.

## Build and verify

The supported toolchain is Rust 1.97.1, Buf 1.66.1, and Python 3 for small
document/schema checks and black-box CLI tests.

```sh
python3 -m venv .venv
.venv/bin/python -m pip install -r tools/validation/requirements.txt
PYTHON_BIN=.venv/bin/python tools/check.sh
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the validation layout and contribution
rules. Start with the [documentation index](docs/README.md) when changing a
contract.
