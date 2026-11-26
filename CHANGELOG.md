# Changelog

This file records notable user-facing and maintainer-facing changes to
Dolgorae. Releases receive a version and date only when the repository has
corresponding release authority; historical releases are not inferred from
development commits.

## [Unreleased] - v2.0.0

### Changed (Clean-Break Reset)

- **Clean-break v2 multi-provider actor architecture (`V2-T01`)**: Archived the
  Codex-only gRPC v0.1.4 monolith unmodified on branch `legacy/v0.1.4`
  (`b083146`) and reset the active repository to an 8-crate Rust 2024 workspace
  (`dolgorae-spi`, `dolgorae-core`, `dolgorae-api`, `dolgorae-mcp`,
  `dolgorae-cli`, `driver-codex-app`, `driver-claude-wire`, `driver-agy-acp`).
- **Transport & toolchain simplification**: Replaced gRPC/Protobuf/Buf/Go build
  dependencies with a local Unix domain socket HTTP/1.1 JSON + SSE contract
  (`docs/protocol/openapi-v2.yaml`) and dual-mode stdio MCP contract
  (`docs/protocol/mcp-v2.md`). Verification now requires only Rust 1.97.1 and
  Python 3 stdlib (`make test`).
- **Driver-only SPI & mechanical guards**: Implemented the complete Tolerant
  Reader `dolgorae-spi` crate and added offline static guards
  (`tools/check_v2_guards.py`) enforcing workspace dependency boundaries,
  anti-evasion production line budgets (`<= 15,000` core/api/mcp/cli/spi lines,
  `<= 1,500` lines per driver), and provider-term isolation.
- **Tier-1 structured CLI drivers (`V2-T02`)**: Implemented `driver-codex-app`,
  `driver-claude-wire`, and `driver-agy-acp` against `dolgorae-spi` (each
  within `<= 1,500` production lines and zero `unsafe`), with zero-argv-secret
  MCP injection, shared `evaluate_interaction` `ReadOnly` safety floor,
  deadlock-free PGID teardown, and 32 offline fake-stdio conformance tests.
- **Unified Actor Core Engine (`V2-T03`)**: Implemented `dolgorae-core` with a
  dedicated single-writer SQLite WAL thread, canonical decimal `u64`
  Cursor/Revision ledger, ephemeral live-only streaming deltas, retention
  pruning with `refresh_required` gap signaling, streaming SHA-256
  content-addressed artifacts (`256 KiB` inline / `64 MiB` cap), symlink-safe
  Markdown/YAML Roles, Invariant 4 depth/cycle/per-operation scoped-actor
  isolation, CSPRNG hashed actor tokens, unified root-tree mailbox waking
  `wait_sessions(any|all)`, and optional workspace single-writer leases.
- **Thin API, Dual-Mode MCP & CLI Composition Root (`V2-T04`, `V2-VAL`)**:
  Implemented `dolgorae-api` (exclusive `DaemonLock`, safe `LOCAL_PEERCRED`
  UID checks, all 19 OpenAPI v2 routes, no-silent-gap SSE lag recovery,
  UTF-8-safe byte-buffered `UdsClient`), `dolgorae-mcp` (15 External Host
  tools + 7 fail-closed Scoped Actor tools with real `>256 KiB` `ArtifactRef`
  spill and lossless `64 KiB` pagination), and `dolgorae-cli` (independent
  `serve` detaching active sessions to resumable `idle` on SIGTERM), verified
  offline via `make test` and live across `codex 0.172.0`, `claude 2.1.332`,
  and `agy 1.5.18` with zero orphan processes.
- **Consumer compatibility**: Frozen v0.1.4 gRPC consumers (including Gul H4)
  stay on `legacy/v0.1.4` until Stage 2 (`V2-GUL-01`).

## v0.1.4 (Archived on `legacy/v0.1.4` at `b083146`)

- Completed Codex `0.172.0` qualification, public-v2 Broker/Session extensions,
  durable research groups and structured findings, caller-workspace specialist
  collaboration, and independent Aquarium/Podway/Gul Gate C/D handoff
  provenance. Full commit-level v0.1.4 changelog history is preserved on
  `legacy/v0.1.4`.

## v0.1.3 - 2026-10-01

- Added 27-method public-v1 Unix-socket gRPC provider API, immutable
  Specialist Roles and policy snapshots, Orchestrated Sessions, Specialist
  Review v3, frozen `dolgorae.gul-consumer/v1` bindings, and source-distributed
  `use-dolgorae` skill.

## v0.1.2 - 2026-09-08

- Added user-global Codex Profiles, reusable External Specialist Engagements,
  per-worktree writer authority, reconnectable interactions, and Darwin process
  identity recovery guards.

## v0.1.1 - 2026-08-31

- Moved all per-user configuration and mutable authority to `~/.dolgorae`.

## v0.1.0 - 2026-08-30

- Initial portable Git/non-Git workspace identity, persistent Codex app-server
  profiles, durable Run lifecycle commands, and isolated one-shot specialist
  review.
