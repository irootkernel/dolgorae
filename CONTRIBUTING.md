# Contributing to Dolgorae

Read [AGENTS.md](AGENTS.md) and the [documentation authority map](docs/README.md)
before changing product contracts, crate boundaries, or driver protocols.

Run `make test` before proposing changes; it enforces formatting, Clippy,
mechanical workspace dependency and line-budget guards, unit tests, integration
tests, and E2E checks using only Rust 1.97.1 and Python 3 stdlib.
