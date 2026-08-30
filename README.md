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

## Release maturity

The release train begins with `v0.1.0`, an Integration Preview covering the
cumulative product scope through `EPIC-004`. Later `v0.1.x` previews advance at
the `MILESTONE-ES1`, `MILESTONE-BH1`, and `MILESTONE-BC1` boundaries. They do
not claim the complete target specification, Personal Alpha readiness, or
customer support.

`v0.2.0` is the planned first customer-supported release. It remains a Personal
Alpha and requires every currently planned product Epic from `EPIC-005` through
`EPIC-011`, including the complete `MILESTONE-PA1` acceptance campaign. See the
[release train](docs/roadmap/README.md#release-train) for the exact version and
milestone boundaries.

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
