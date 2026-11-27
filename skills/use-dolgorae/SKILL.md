---
name: use-dolgorae
description: "Use Dolgorae v2 safely through its local CLI, UDS HTTP/1.1 JSON+SSE daemon (`dolgorae serve`), or stdio MCP server (`dolgorae mcp`) when explicitly asked to inspect, configure, operate, diagnose, or recover Dolgorae state. Ground every action in the exact binary and advertised runtime capabilities; do not activate for generic code review or merely because a repository contains Dolgorae files."
---

# Use Dolgorae (v2)

Dolgorae v2 is a local, multi-provider, durable agent actor control layer
supporting Tier-1 structured CLI drivers (`codex-app`, `claude-wire`,
`agy-acp`).

> **Legacy v0.1.4 notice**: The prior Codex-only gRPC v0.1.4 CLI and protocol
> moved to branch `legacy/v0.1.4` (`b083146`). Do not invoke removed v0.1.4
> subcommands (`workspace inspect`, `profile`, `run`, `review`, `engagement`)
> against a v2 binary.

## Current Stage Status

Dolgorae v2 Stage 1 (`EPIC-V2-001`: `V2-T01` through `V2-T04` and `V2-VAL`) is
implemented across all 8 crates. `dolgorae serve` (independent UDS HTTP/1.1
JSON+SSE daemon with `LOCAL_PEERCRED` auth), `dolgorae mcp [--scoped]`
(External Host 15-tool mode and Scoped Actor 7-tool mode), and human-readable
or `--json` CLI subcommands (`version`, `driver list|doctor`, `role list|show`,
`session ...`, `workspace lease|transfer|release`, `artifact read`) are
operational across `codex-app`, `claude-wire`, and `agy-acp`. Expanded
ecosystem workflows (Haeryong/Aquarium playbooks) remain scoped to `V2-ECO-01`.

## Safety and Authorization Boundaries

1. Confirm that the user explicitly requested a Dolgorae operation. Presence of
   this skill or a `.dolgorae/` directory never authorizes starting a daemon,
   launching external CLI sessions, using live accounts, mutating credentials,
   or cancelling active work.
2. Always resolve and verify the exact executable before acting:

   ```sh
   command -v dolgorae
   dolgorae version --json
   dolgorae drivers
   ```

3. Consult the canonical v2 contracts in the repository before calling HTTP/SSE
   or MCP interfaces:
   - `docs/specs/README.md`
   - `docs/protocol/openapi-v2.yaml`
   - `docs/protocol/mcp-v2.md`
   - `docs/protocol/drivers-v2.md`
4. Treat `dolgorae serve` UDS connections as same-user `LOCAL_PEERCRED`
   authenticated and treat `dolgorae mcp --scoped` as strictly bound to its
   `DOLGORAE_ACTOR_TOKEN` actor scope.
