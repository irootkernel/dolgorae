# Dolgorae v2 Architecture

This document owns the workspace topology, crate dependency boundaries, line
budgets, state ownership, and explicit non-goals of Dolgorae v2.

## 1. Eight-Crate Cargo Workspace

Dolgorae v2 is organized as a Rust 2024 workspace (`rust-version = "1.97.1"`,
`resolver = "3"`) with workspace-wide `unsafe_code = "deny"` and
`clippy::all = "deny"`:

```text
crates/
  dolgorae-spi/          Pure driver-only SPI traits, events, and tolerant serde types
  dolgorae-core/         Roles, unified Session/Actor/Mailbox engine, SQLite event log, artifacts
  dolgorae-api/          UDS HTTP/1.1 JSON + SSE server (Axum/Hyper) and peer-cred auth
  dolgorae-mcp/          Stdio MCP adapter (External Host Mode + Scoped Actor Mode)
  dolgorae-cli/          Binary `dolgorae` — sole composition root wiring drivers into core/API/MCP
  drivers/
    driver-codex-app/    Tier-1 structured driver for `codex app-server --stdio`
    driver-claude-wire/  Tier-1 structured driver for `claude -p --verbose ... stream-json`
    driver-agy-acp/      Tier-1 structured driver for `agy acp --stdio --process-per-session`
```

## 2. Mechanical Dependency Rules

Enforced mechanically on every gate run by `tools/check_v2_guards.py` over the
full transitive closure of `cargo metadata --no-deps --format-version 1 --offline`
across all dependency kinds (`normal`, `dev`, and `build`):

1. **`dolgorae-spi`**:
   - Zero workspace sibling dependencies.
   - Zero forbidden heavy dependencies (`rusqlite`, `axum`, `hyper`, `clap`, `tonic`, `prost`).
   - Owns only driver traits, specs, capabilities, interaction types, normalized events, and `DriverError`. Must never contain cursors, revisions, SSE framing, HTTP 409 conflicts, or 256 KiB/64 MiB payload bounds.
2. **Core / API / MCP isolation**:
   - `dolgorae-spi`, `dolgorae-core`, `dolgorae-api`, and `dolgorae-mcp` MUST NOT reach any `driver-*` crate transitively.
   - Non-test production source in `spi`/`core`/`api`/`mcp` MUST NOT leak provider-specific wire vocabulary or legacy v0.1/consumer vocabulary (`codex`, `claude`, `agy`, `acp`, `app-server`, `thread/`, `session/prompt`, `Carrier`, `ControllerKind`, `zcode`, `herdr_core`, `libproc`).
3. **Driver isolation**:
   - Each `driver-*` crate may depend ONLY on `dolgorae-spi` among workspace crates (never `core`, `api`, `mcp`, `cli`, or another `driver-*` crate).
   - Each driver owns its native request ID mapping and native permission grant mapping privately, exposing only opaque `String` IDs across `dolgorae-spi`.
4. **Single composition root**:
   - `dolgorae-cli` is the ONLY crate permitted to depend on `driver-*` crates alongside `core`, `api`, and `mcp`.

## 3. Production Line Budgets

Enforced mechanically by `tools/check_v2_guards.py`:

| Scope | Budget (physical production lines) |
| --- | --- |
| `dolgorae-spi + dolgorae-core + dolgorae-api + dolgorae-mcp + dolgorae-cli` | `<= 15,000` lines |
| Each individual `driver-*` crate | `<= 1,500` lines |

Counting rules:
- Scans `src/**/*.rs`, `build.rs`, `examples/**/*.rs`, and `benches/**/*.rs` for each workspace crate.
- Excludes crate `tests/` directories (integration tests) and brace-matched `#[cfg(test)]` items.
- **Anti-evasion rule**: Any non-test item placed after a `#[cfg(test)] mod` block in the same source file fails the guard immediately. All unit test modules must appear at the end of their source file.

## 4. State Ownership and Process Topology

```text
External Client (CLI / Gul / Aquarium / Host MCP)
   │  UDS HTTP/1.1 JSON + SSE (`~/.dolgorae/run/dolgorae.sock`, LOCAL_PEERCRED)
   ▼
dolgorae serve (single user-scoped daemon)
   ├── dolgorae-api  (peer_cred auth, HTTP/1.1 routes, SSE replay & heartbeat)
   ├── dolgorae-core (SQLite WAL state/event/mailbox DB, 256KiB/64MiB artifact store, Role loader)
   └── Driver Supervisors (one child process per active driver session)
         ├── driver-codex-app   ──stdio──▶ codex app-server --stdio
         ├── driver-claude-wire ──stdio──▶ claude -p --verbose ... stream-json
         └── driver-agy-acp     ──stdio──▶ agy acp --stdio --process-per-session
                │
                └── (optional Scoped MCP) ──stdio──▶ dolgorae mcp --scoped
                                                        │ (DOLGORAE_ACTOR_TOKEN over UDS)
                                                        ▼
                                                  dolgorae serve
```

- SQLite (`rusqlite` bundled, WAL mode) under `~/.dolgorae/state.db` is the single durable authority for sessions, actors, revisions, event logs, pending interactions, mailbox messages, and artifact metadata.
- Artifact blobs live under `~/.dolgorae/artifacts/<sha256[0..2]>/<sha256>`.
- No gRPC (`tonic`/`prost`/`buf`), no `unsafe` Rust (`tokio::net::UnixStream::peer_cred()` handles macOS `LOCAL_PEERCRED` safely), and no `libproc` FFI.

## 5. Responsibility Transfer and Explicit Non-Goals

| Concern | Owner | Explicit Non-Goal in Dolgorae v2 |
| --- | --- | --- |
| Conversation history & LLM turns | Provider CLIs (`codex`, `claude`, `agy`) | Dolgorae does not re-implement LLM clients or raw HTTP provider APIs |
| Multi-provider actor sessions, delegation, mailbox, approvals, artifacts | **Dolgorae v2** | No PTY scraping or screen-buffer parsing (Tier-1 structured protocols only) |
| Desktop UI, mobile push, terminal tabs, user profile/carrier vocabulary | Consumers (e.g., Gul) | Gul vocabulary (`ControllerKind`, `Carrier`, `Profile`) never enters Dolgorae v2 API/SPI |
| Workflow state machines, git commits, release gates | Aquarium / Podway | Dolgorae does not embed Podway procedures or Aquarium workflow policy |
| Legacy v0.1.4 Codex gRPC protocol | `legacy/v0.1.4` branch | No v0.1.4 protobuf or migration compatibility shim in v2 |
