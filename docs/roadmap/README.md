# Dolgorae v2 Roadmap

This file is the sole delivery-order and delivery-status authority for Dolgorae v2.
Historical v0.1.4 delivery records remain on branch `legacy/v0.1.4` (`b083146`).

## Stage 1 — Dolgorae v2 Actor Core (`EPIC-V2-001`)

| Task ID | Scope | Status |
| --- | --- | --- |
| `V2-T01` | Clean-break reset, 8-crate Rust 2024 workspace, complete driver-only `dolgorae-spi`, canonical v2 docs & OpenAPI/MCP/driver contracts, hardened offline static guards & `Makefile` | `COMPLETED` |
| `V2-T02` | Tier-1 structured drivers (`driver-codex-app`, `driver-claude-wire`, `driver-agy-acp`), scoped MCP injection, PGID teardown, and shared fake-stdio conformance suite | `COMPLETED` |
| `V2-T03` | `dolgorae-core`: Role/Persona loader, SQLite WAL event log & mailbox, decimal u64 cursor/revision ledger, optimistic concurrency checks, 256 KiB / 64 MiB content-addressed artifact store, delegation depth/cycle enforcement, writer lease, and actor token issuance | `COMPLETED` |
| `V2-T04` | `dolgorae-api` (UDS HTTP/1.1 JSON + SSE, `LOCAL_PEERCRED`, `Last-Event-ID` replay, `refresh_required`, heartbeat), `dolgorae-mcp` (External Host + Scoped Actor Mode), and `dolgorae-cli` (`serve`, `mcp`, `drivers`, session/role commands) | `COMPLETED` |
| `V2-VAL` | Cold end-to-end verification of Stage 1 across all 8 crates, static guards, mock-driver E2E tests, and bounded opt-in live CLI smoke verification | `PENDING` |

## Stage 2 — Gul v2 Consumer Migration (`EPIC-V2-002`)

| Task ID | Scope | Status |
| --- | --- | --- |
| `V2-GUL-01` | Migrate Gul consumer from `legacy/v0.1.4` gRPC to Dolgorae v2 UDS HTTP/1.1 JSON + SSE (`Cursor`/`Revision`, `highest_cursor`, `refresh_required`, `409` optimistic concurrency, generic interaction cards) | `PLANNED` |

> Note: Gul H4 stays pinned to `legacy/v0.1.4` until `V2-GUL-01` is completed and qualified.

## Stage 3 — Haeryong & Aquarium Integration (`EPIC-V2-003`)

| Task ID | Scope | Status |
| --- | --- | --- |
| `V2-ECO-01` | Integrate Haeryong and Aquarium workflows with Dolgorae v2 External Host MCP, Scoped Actor delegation, and `skills/use-dolgorae/SKILL.md` v2 procedures | `PLANNED` |
