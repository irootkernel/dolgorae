# Dolgorae v2

Dolgorae v2 is a local, multi-provider, durable agent actor control layer written
in Rust (edition 2024, Rust `1.97.1`). It runs an independent local daemon
(`dolgorae serve`) over a same-user Unix domain socket (HTTP/1.1 JSON +
Server-Sent Events) and exposes a dual-mode stdio MCP server (`dolgorae mcp`)
for external hosts and scoped child actors.

> **Legacy v0.1.4 notice**: The prior Codex-only gRPC v0.1.4 monolith is
> preserved unmodified on branch `legacy/v0.1.4` (`b083146`). Consumers pinned
> to the frozen v0.1.4 gRPC surface (including Gul H4) remain on
> `legacy/v0.1.4` until Stage 2.

## Workspace Layout

```text
crates/
  dolgorae-spi/          Pure driver-only SPI traits, events, and tolerant serde types
  dolgorae-core/         Roles, unified Session/Actor/Mailbox engine, SQLite event log, artifacts
  dolgorae-api/          UDS HTTP/1.1 JSON + SSE gateway with LOCAL_PEERCRED auth
  dolgorae-mcp/          Stdio MCP server (External Host Mode + Scoped Actor Mode)
  dolgorae-cli/          `dolgorae` binary — sole composition root wiring drivers into core/API/MCP
  drivers/
    driver-codex-app/    Tier-1 structured driver for `codex app-server --stdio`
    driver-claude-wire/  Tier-1 structured driver for `claude -p --verbose ... stream-json`
    driver-agy-acp/      Tier-1 structured driver for `agy acp --stdio`
```

## Toolchain and Verification

Dolgorae v2 requires only **Rust 1.97.1** (pinned in `rust-toolchain.toml`,
with `rustfmt` and `clippy`) and **Python 3** (standard library only).

Run the full repository verification gate:

```sh
make test
```

Individual gate layers:

- `make format-check`: read-only `cargo fmt --check`.
- `make test-prepare`: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `python3 tools/check_v2_guards.py` (workspace dependency closure, production line budgets, and scoped banned-term guard).
- `make test-unit`: Rust library unit tests and Python guard self-tests.
- `make test-int`: Rust integration tests.
- `make test-e2e`: End-to-end verification layer.

## Documentation Authorities

See [docs/README.md](docs/README.md) for the canonical authority map:

- [docs/specs/README.md](docs/specs/README.md) — Product specification
- [docs/architecture/README.md](docs/architecture/README.md) — Workspace architecture and invariants
- [docs/architecture-decision-records/README.md](docs/architecture-decision-records/README.md) — Architecture decision records (ADR-001)
- [docs/protocol/openapi-v2.yaml](docs/protocol/openapi-v2.yaml) — UDS HTTP/1.1 JSON + SSE OpenAPI 3.1 contract
- [docs/protocol/mcp-v2.md](docs/protocol/mcp-v2.md) — External Host and Scoped Actor MCP contract
- [docs/protocol/drivers-v2.md](docs/protocol/drivers-v2.md) — Tier-1 CLI protocol mappings
- [docs/roadmap/README.md](docs/roadmap/README.md) — Stage 1–3 delivery roadmap
