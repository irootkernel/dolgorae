# Dolgorae TODO Candidates and Dossiers

This index owns future epic-sized candidates and temporary dossiers for adopted
Epics awaiting closeout, including those still `PLANNED`. It does not own
roadmap identity, order, status, or dependencies.

## Future candidates

### Read-only Podway observation

- Scope: post-v0.1.3, without an assigned release or active Task slot. Inspect
  Podway's actual contract before assigning implementation IDs; do not invent
  a source API from this requirements record.
- Authority: Podway owns FSM definition and actual execution; Dolgorae publishes
  safe bound observations; Gul renders them without direct Podway state access.
- Data: full pinned graph/version/digest, execution identity distinct from the
  Session, active node set, node states and stable execution identities/counts,
  per-loop iterations including nested loops, revision and freshness.
- Definition binding: display the complete definition used by that execution,
  not a newly edited definition file. Preserve definition and execution identity
  through snapshot/update recovery, parallel nodes and nested loops.
- Counts: initial execution is the first occurrence. A genuinely new execution
  attempt increments its node count; skipped nodes do not count as executed.
  Loop iteration and node execution count are independent source facts, never
  derived by incrementing on notification arrival.
- Recovery: duplicates, reconnect and resume of the same execution do not
  increment counts. Multiple executions remain distinct. Missing source evidence
  is stale/unavailable, never fabricated zero counts or completion.
- Prompt correlation may later link a workflow execution to its initiating user
  request, but must not delay or become a dependency of v0.1.3 history.
- Read-only boundary: no Gul UI/backend/API FSM editing, jump, skip, force-complete,
  retry-node or reset-count action. Zoom/selection/details affect presentation only.
  Changes or jumps are ordinary prompts judged by the executing LLM under Podway
  rules. Only actual Podway state changes update the diagram, not an LLM promise.
- Compatibility: a later optional observation surface must not block existing
  chat, prompt history, approvals, result reads or session close. No speculative
  Podway wire is frozen in TASK-053 and no implementation enters v0.1.3.
- Adoption: coordinate later Dolgorae observation and Gul visualization Epics
  after source-contract inspection. These do not become provider release blockers.

## Adopted Epic dossiers

The term adopted here does not imply that the roadmap state is `ACTIVE`.

An unadopted `TODO-*.md` file has no roadmap identity. On adoption, retain its
dossier from planning through execution and review until Epic closeout, list it
here, and link it from the [roadmap](../roadmap/README.md) with `Detailed SOT`.
This follows the [documentation lifecycle](../README.md#roadmap-identity-and-dossier-lifecycle)
and does not depend on the Epic being `ACTIVE`. Closeout promotes durable
content to its canonical owners, removes the dossier and this index entry, and
replaces the roadmap link with `Canonical Outcomes`.

### v0.1.4 release alignment

- Dossier: [Dolgorae v0.1.4 release alignment](TODO-DOLGORAE-V014-RELEASE-ALIGNMENT.md).
- Correction dossier: [Delegation release proof](TODO-DOLGORAE-RELEASE-PROOF-CORRECTION.md),
  owned by COMPLETE EPIC-023 and TASK-090..092. It preserves original H1 and
  requires a new package plus Gul delta H2 and independent Gate A before
  EPIC-020 resumes; runtime implementation remains in TASK-076/077.
- Post-Gate-D upgrade-resilience expansion:
  [Runtime upgrade resilience and setup-bound host admission](TODO-RUNTIME-UPGRADE-RESILIENCE.md),
  owned by ACTIVE EPIC-024 and TASK-095..098 within unreleased v0.1.4. It
  preserves historical EPIC-022 provenance on H3 `8fb6a3578ae3` while gating
  v0.1.4 publication on a superseding H3/H4 pair and refreshed Gate C/D.
- Consumers: EPIC-018, EPIC-019, EPIC-023, EPIC-020, EPIC-009, EPIC-021 and EPIC-022 (plus EPIC-024 via its dedicated dossier).
- Work: modern public/Gul contracts, reviewed corrections, durable delegation,
  complete internal collaboration, findings/groups, and mandatory Aquarium,
  Podway and Gul acceptance on the exact Dolgorae v0.1.4 artifact. The
  [consumer matrix](../specs/dolgorae-consumer-acceptance-v014.md) owns requirements,
  evidence and external prerequisites.
- Execution: five phases and four Dolgorae/Gul whole-Epic handoffs. EPIC-018
  contains TASK-067/068; TASK-069 opens EPIC-019 after Gul contract Epic closeout.
  Phase 3 includes EPIC-023's bounded contract exchange before EPIC-020 resumes
  and closes EPIC-019/023/020/009/021 with a runnable candidate; Phase 4 closes
  Gul product qualification; Phase 5 runs EPIC-022 independently.
- Lifecycle: all new work remains PLANNED in the roadmap. Each consumer links
  the same dossier through Detailed SOT. Retain it through earlier closeouts until
  the last consumer closes and all references are settled.

### Global Profile journal capacity and operator maintenance

- Adopted owner: EPIC-019, TASK-072 and TASK-073, with storage/read cost in
  TASK-075.
- The former candidate is now part of v0.1.4: capacity diagnostics and explicitly
  authorized maintenance preserve immutable bindings, active or unknown
  membership, revision/checksum continuity and auditability.
- The shared dossier defines locking, interrupted maintenance, over-cap
  admission and retention requirements before persisted-contract changes.
  Adoption is not evidence that compaction or capacity recovery is implemented.

### Actual Gul consumer acceptance

- Adopted owner: Gul's completed Phase 4 qualification produces campaign
  evidence; EPIC-022 TASK-088 independently verifies it in Phase 5. TASK-069
  records contract Gate A at the beginning of EPIC-019 from completed Gul H2.
- The v0.1.4 campaign uses the fixed Phase 3 provider and intended Gul v0.1.0
  artifact. It covers startup, carriers, creation, approvals, results, history,
  reconnect, closure and recovery. Gul closes its own Epic before TASK-088;
  publication waits for both Gate C results and TASK-089's three-consumer Gate D.
- Only actual consumer qualification establishes MILESTONE-BH1. Provider-only
  tests and contract artifacts do not establish it.
- Historical EPIC-008/v0.1.3 provider eligibility stays unchanged. The new
  candidate-first joint release boundary is adopted for v0.1.4 and must be
  reconciled in its successor contract by TASK-067.

### Runtime upgrade resilience and setup-bound host admission

- Dossier: [Runtime upgrade resilience and setup-bound host admission](TODO-RUNTIME-UPGRADE-RESILIENCE.md).
- Adopted owner: ACTIVE EPIC-024 and TASK-095 through TASK-098, assigned to
  unreleased v0.1.4 (and cumulative v0.2.0 ahead of EPIC-010).
- Work: deterministic domain-separated SHA-256 `process_static` capture
  (`_dolgorae_unclassified_sha256`, refused for checked one-shot review) and
  key-name-only diagnostics for unclassified non-inclusion `CODEX_HOME/config.toml`
  keys, SemVer §11 version precedence (with H1 checked-review pre-release
  refusal), removal of brittle `absent: true` `TurnStartParams` subset checks,
  side-effect-free no-op `thread/settings/updated` tolerance, dedicated-entrypoint
  zero-member quiescent same-or-newer upgrade rollover and review-ownership
  propagation for Profile Servers, caller-scoped (`External` and caller-scoped
  `Managed`) minimum-version hardened non-executing setup binding plus
  split-digest `schema_version: 2` online `owner-executable-refresh` and
  `v1`/`v2` compatible `adapter_version` continuity in `trusted_host`, and
  superseding H3/H4 three-consumer Gate C/D requalification.
- Lifecycle: all four Tasks start PLANNED. Keep historical EPIC-022 Gate C/D
  evidence intact while gating v0.1.4 publication on TASK-098.
