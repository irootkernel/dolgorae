# Dolgorae TODO Candidates and Dossiers

This index owns future epic-sized candidates and temporary dossiers for adopted
Epics awaiting closeout, including those still `PLANNED`. It does not own
roadmap identity, order, status, or dependencies.

## Future candidates

### Separate Specialist launch preparation from the external facade

- Owner: Shared semantic service and Specialist workspace maintainers
- Problem: brokered Run provisioning in `semantic` calls launch-root and
  sandbox helpers in `external_engagement`, which already depends on `semantic`.
  The resulting module cycle couples the two Specialist facades.
- Candidate: move the shared launch preparation and cleanup helpers to the
  workspace module, then update the architecture dependency checks.
- Revisit condition: the next change to Specialist launch-root or sandbox
  preparation, or an intentional extraction of either facade.

### Global Profile journal capacity and operator maintenance

- Owner: Global Profile persistence and operator-recovery maintainers
- Problem: binding history and membership journals have finite read ceilings.
  Sustained growth can reach those ceilings and block ordinary admission or
  lifecycle repair; existing recovery never compacts or deletes history.
- Candidate: define capacity diagnostics and an explicitly authorized
  maintenance operation that preserves immutable Run bindings, active or
  unknown membership, revision/checksum continuity, and auditability. Specify
  locking, interrupted-maintenance recovery, and how over-cap state is admitted
  before introducing pruning, compaction, or a persisted-contract successor.
- Reason for separate design: maintenance adds operator authority and retention
  semantics beyond the current fail-closed storage contract.
- Revisit condition: sustained deployment approaches the 1 MiB binding-history
  or 8 MiB membership-journal ceiling, or storage maintenance is adopted.

### Actual Gul consumer acceptance

- Owner: Gul integration maintainers, with Dolgorae provider maintainers
- Prerequisite: Gul has a runnable client pinned to the TASK-053 consumer contract,
  and an exact Dolgorae v0.1.3 artifact is released after MILESTONE-BH1-P and
  separate RC QA. Contract-ready permits mock/UI development, not actual integration.
- Outcome: verify actual Gul startup/supervision, protected Controller carriers,
  Orchestrated Session creation, approval UX, Specialist result rendering,
  public result discovery and artifact metadata/chunks/digest, complete original
  prompt history through pages/restart/close, sequential human input without
  queue/steering, whole-session close/recovery and event reconnect against exact
  supported client/provider revisions.
- Evidence: `real_gul_harness`, meaning the actual Gul client and its integration
  campaign, not a Gul-shaped test client or provider-only tests.
- Milestone: only this separately adopted consumer campaign can establish
  `MILESTONE-BH1`. Provider conformance never implies it has passed.
- Delivery boundary: not a prerequisite for EPIC-008, v0.1.3 provider release
  eligibility, or EPIC-009 provider implementation. Do not create an ACTIVE or
  BLOCKED placeholder that consumes the roadmap's sequential Task slot.
- Revisit condition: Gul is ready for integration; adopt a bounded acceptance
  item then, with concrete revision/build inputs and normal completion gates.

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

- [Independent review readiness](TODO-independent-review-readiness.md): adopted
  as [EPIC-016](../roadmap/README.md#epic-016-reliable-independent-specialist-review).
  The dossier records v3 output and durable failure diagnostics, public one-shot
  recovery and temporary-server cleanup, installed-skill checks, and live
  acceptance. Delivery status and order remain in the roadmap.

The term adopted here does not imply that the roadmap state is `ACTIVE`.

An unadopted `TODO-*.md` file has no roadmap identity. On adoption, retain its
dossier from planning through execution and review until Epic closeout, list it
here, and link it from the [roadmap](../roadmap/README.md) with `Detailed SOT`.
This follows the [documentation lifecycle](../README.md#roadmap-identity-and-dossier-lifecycle)
and does not depend on the Epic being `ACTIVE`. Closeout promotes durable
content to its canonical owners, removes the dossier and this index entry, and
replaces the roadmap link with `Canonical Outcomes`.
