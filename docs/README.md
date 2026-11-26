# Dolgorae Documentation

Profile: `single-scope`. Dolgorae v2 has one delivery scope and one canonical
roadmap. The root [README](../README.md) is the user-facing entrypoint; this
directory owns the authoritative specifications, architecture, decisions,
protocol contracts, roadmap, and deferred feedback.

## Authority Map

| Role | Canonical owner |
| --- | --- |
| Product specifications | [specs/README.md](specs/README.md) |
| Architecture | [architecture/README.md](architecture/README.md) |
| Architecture decision records | [architecture-decision-records/README.md](architecture-decision-records/README.md) |
| Checked protocol contracts | [protocol/](protocol/) |
| Roadmap | [roadmap/README.md](roadmap/README.md) |
| Deferred feedback | [deferred-feedback/README.md](deferred-feedback/README.md) |

Checked protocol artifacts in `docs/protocol/`:

- [openapi-v2.yaml](protocol/openapi-v2.yaml): UDS HTTP/1.1 JSON + SSE API contract.
- [mcp-v2.md](protocol/mcp-v2.md): External Host Mode and Scoped Actor Mode MCP tool contracts.
- [drivers-v2.md](protocol/drivers-v2.md): Verified Tier-1 CLI protocol mappings and runtime quirks.

## Precedence and Synchronization

Each role owner is authoritative only for its stated domain:

1. `docs/specs/README.md` owns externally observable behavior and semantic requirements.
2. `docs/architecture/README.md` owns workspace boundaries, state ownership, process topology, and line budgets.
3. `docs/architecture-decision-records/README.md` owns accepted architectural rationale and rejected alternatives.
4. `docs/protocol/` derives wire, MCP, and driver-mapping contracts from the specification.
5. `docs/roadmap/README.md` alone owns delivery identity, order, and delivery status.
6. `docs/deferred-feedback/README.md` owns independent remaining follow-ups that do not belong to the active task.

If canonical documents disagree, resolve the contradiction in the owning
document before updating derived protocol files, implementation, tests, or
roadmap entries.

## Historical v0.1.4 Preservation

The legacy v0.1.4 Codex-only gRPC monolith is preserved on branch
`legacy/v0.1.4` (commit `b083146`). Consumer integrations pinned to the frozen
v0.1.4 gRPC surface (such as Gul H4) remain on `legacy/v0.1.4` until their
scheduled Stage 2 v2 migration.
