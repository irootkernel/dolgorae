# Deferred Feedback

This file is the canonical owner for independent, non-blocking follow-up items
deferred during Dolgorae v2 development.

- Do not defer current correctness, security, or acceptance requirements here.
- Promote epic-sized follow-ups to `docs/roadmap/README.md`.

## Active Items

- **DF-V2-001 (Driver teardown & `setsid` caveat)**: Child processes spawned inside provider sandboxes that invoke `setsid()` escape the driver's `process_group(0)` PGID boundary; additionally align `driver-claude-wire` and `driver-agy-acp` `Drop` reapers to always use a dedicated OS reaper thread (matching `driver-codex-app`) rather than `Handle::spawn` so teardown remains resilient during Tokio runtime shutdown.
- **DF-V2-002 (Agy user-level permission settings under `ReadOnly`)**: `agy 1.5.18` mode `default` enforces *"Ask before edits and commands, per your permission settings."* User-global Agy settings that pre-allow edits could bypass ACP permission prompts before `driver-agy-acp`'s `evaluate_interaction` hard-deny runs. Evaluate Agy `--sandbox` / explicit non-interactive flags if future ACP releases expose stricter read-only enforcement.
- **DF-V2-003 (Driver crate line-budget headroom)**: After `V2-T02`, all three drivers pass under `1,500` production lines (`1,467`, `1,422`, `1,432`). Before adding new driver features in later tasks, deduplicate local JSON-RPC/NDJSON and PGID helper boilerplate within each driver to restore headroom.
- **DF-V2-004 (`dolgorae-core` `store.rs` modular split & minor edge hardening)**: Split `crates/dolgorae-core/src/store.rs` into aggregate submodules (`sessions`, `events`, `interactions`, `mailbox`, `tokens_leases`) in a future cleanup pass; also harden active-session `resume_session` failure rollback (preserve idle/closed status cleanly if driver resume fails) and tighten `complete_turn` placeholder clearing to match the specific turn's reservation rather than `LIKE 'pending-%'`.
