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
