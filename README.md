# Dolgorae

Dolgorae is a local, durable control layer for persistent Codex runs. It adds
stable run identity, controller authorization, workspace writer coordination,
recovery, and auditability while leaving conversation storage with Codex.

The implementation is written in Rust. The product contract is defined by the
[specification](docs/specs/README.md),
[architecture](docs/architecture/README.md), accepted
[architecture decisions](docs/architecture-decision-records/README.md), and
checked [protocol](docs/protocol/) artifacts. The
[roadmap](docs/roadmap/README.md) is the sole delivery-status authority.

## Build and use from source

Build Dolgorae with the supported Rust 1.97.1 toolchain, then inspect the
currently available command surface:

```sh
cargo build --locked
./target/debug/dolgorae --human --help
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for development and validation guidance.
Maintainers should start with the [documentation index](docs/README.md) before
changing a contract. Release history is recorded in the
[changelog](CHANGELOG.md).
