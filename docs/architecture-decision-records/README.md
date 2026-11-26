# Architecture Decision Records

This directory records accepted architectural decisions, their rationale, and
rejected alternatives for Dolgorae v2.

## ADR-001: Dolgorae v2 Clean-Break Multi-Provider Actor Architecture

- **Status**: Accepted
- **Date**: 2026-11-26
- **Scope**: Entire Dolgorae v2 repository (`feat/v2-actor-core`; v0.1.4 preserved on `legacy/v0.1.4` at `b083146`)

### Context

Dolgorae v0.1.4 grew to ~316,000 lines of Rust tightly coupled to a single
provider (`codex app-server`), gRPC/Protobuf (`tonic`/`prost`/`buf`), Go
consumer bindings, and multi-generation ledger/profile compatibility layers.
Supporting multiple Tier-1 structured agent CLIs (`codex`, `claude`, `agy`)
plus unified hierarchical actor delegation inside that monolith would multiply
complexity and lock every consumer to Codex-specific assumptions.

### Decision

1. **Clean-Break Reset**: Archive v0.1.4 unmodified on `legacy/v0.1.4` (`b083146`) and rebuild Dolgorae v2 from scratch as an 8-crate Rust 2024 workspace with strict mechanical dependency guards and production line budgets (`<= 15,000` lines core/api/mcp/cli/spi, `<= 1,500` lines per driver).
2. **UDS HTTP/1.1 JSON + SSE**: Replace gRPC/Protobuf/Buf with a local Unix domain socket HTTP/1.1 JSON + SSE API (`axum`/`hyper`) authenticated via OS peer credentials (`LOCAL_PEERCRED` via safe `tokio::net::UnixStream::peer_cred()`).
3. **Pure Tier-1 Structured Drivers**: Implement only structured wire-protocol drivers behind `dolgorae-spi`:
   - `driver-codex-app` (`codex app-server --stdio`)
   - `driver-claude-wire` (`claude -p --verbose --input-format stream-json --output-format stream-json ...`)
   - `driver-agy-acp` (`agy acp --stdio`)
4. **Unified Session / Actor / Delegation / Mailbox (Model 1-A) + Scoped MCP**: Unify root sessions and delegated child actors under one Session model, injecting `dolgorae mcp --scoped` with a per-actor bearer token (`DOLGORAE_ACTOR_TOKEN`) into child agents.
5. **Independent Local Daemon (`dolgorae serve`)**: Run Dolgorae as a standalone same-user local daemon rather than embedding it inside a UI wrapper or per-consumer helper process.
6. **Tolerant Reader SPI**: All SPI structs ignore unknown fields, and SPI enums (`InteractionKind`, `DecisionKind`, `ToolActivityStatus`, `TurnTerminalStatus`, `NormalizedEvent`) deserialize unknown variants into explicit `Unknown` / `Unsupported` fallbacks so upstream CLI updates cannot break deserialization.
7. **Consumer Compatibility Boundary**: Gul H4 and other frozen v0.1.4 gRPC consumers remain on `legacy/v0.1.4` until their dedicated Stage 2 migration task.

### Rejected Alternatives

- **Incremental migration of v0.1.4 monolith**: Rejected. Retaining 316k LOC of Codex-specific profile/carrier/ledger/gRPC state machines would violate the 15k LOC maintainability budget and leak v0.1 vocabulary across providers.
- **Hybrid `herdr-core` / PTY scraping fallback**: Rejected. PTY screen parsing is brittle for structured approvals, tool calls, and deterministic `output_schema` extraction; Tier-1 structured stdio protocols exist for Codex, Claude, and Agy.
- **Embedding `zcode` or proprietary harness runtimes**: Rejected. Dolgorae must remain provider-neutral and depend only on documented local CLI protocols.
- **Gul wrapper daemon**: Rejected. Making Gul own Dolgorae's daemon lifecycle would couple CLI, Aquarium, and MCP users to a desktop host and leak Gul concepts (`Carrier`, `ControllerKind`, `Profile`) into the core control layer.
- **Retaining gRPC / Buf / Go toolchain**: Rejected. Local UDS HTTP/1.1 JSON + SSE removes Buf/protoc/Go build dependencies, simplifies debugging and SSE replay (`Last-Event-ID`), and keeps the toolchain to Rust 1.97.1 + Python 3 stdlib.

### Consequences

- Build and validation require only Rust 1.97.1 (`rustfmt`, `clippy`) and Python 3 stdlib (`make test`).
- Driver isolation and line budgets are enforced mechanically in CI and local gates.
- Stage 1 delivers the complete v2 daemon, core engine, 3 Tier-1 drivers, and dual-mode MCP; Stage 2 migrates Gul; Stage 3 integrates Haeryong and Aquarium.
