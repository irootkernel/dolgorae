# EPIC-005: Access, Interaction, and Recovery Safety

Roadmap Epic: [`EPIC-005`](../roadmap/README.md#epic-005-access-interaction-and-recovery-safety)

This temporary dossier owns the implementation detail needed while EPIC-005 is
active. The canonical roadmap remains the sole authority for identifiers,
ordering, dependencies, lifecycle vocabulary, and status.

## Goal and purpose

Enforce one durable writer authority per canonical worktree, require the active
Controller for interactions and lifecycle changes, and fail closed whenever
process identity, background execution, history, or delivery outcomes cannot be
proved.

The Epic hardens the existing read-only Specialist Review Preview. It does not
delay or redefine `MILESTONE-SR1`, generalize external Specialist engagements,
or introduce the later Brokered Hierarchy control plane.

## Scope and approach

- Preserve the fixed `~/.dolgorae` configuration and mutable-authority root and
  complete the remaining durable writer-authority and handoff behavior.
- Add durable, generation-qualified pending interactions and Controller-owned
  responses without public session-scoped approval state.
- Make pause, close, process cleanup, history reconciliation, and fork recovery
  conservative across crashes, restarts, identity drift, and unknown outcomes.
- Keep specifications, architecture, accepted decisions, checked protocol
  artifacts, implementation, tests, public usage, and operations guidance
  synchronized with the behavior each authority owns.

## Task objectives

### Durable writer authority and cross-profile handoff

Build on the read-only Specialist and ordinary-reader baseline. Retain the
canonical fixed-home contract, implement one revisioned writer-authority state
machine per worktree, and serialize its transitions with short BSD record locks
without holding file locks across external waits. Support explicit acquisition,
release, idle same-controller cross-profile handoff, dedicated write
continuations, and the operator-authorized repair of a proved writer-free
`blocked_unknown` state. Fail closed on stale generations, identity ambiguity,
background-execution uncertainty, or incomplete retirement proof.

### Pending requests and approvals

Normalize supported command, file, and pinned user-input interactions into
durable generation- and server-epoch-qualified pending requests. Persist before
delivery, enforce Controller ownership and first-valid-response semantics, and
retain reconnectable non-secret state without replaying a request after restart.
Reader mode auto-declines through the configured `approvalPolicy:"never"`.
Recognized unsupported permission and elicitation methods return method-not-found
without creating pending lifecycle state.

### Pause, close, and lifecycle shutdown

Implement idle and interrupting pause or close, immutable closure, access
instruction replacement, terminal sealing, and verified cleanup of the worker,
connection, and owned Dedicated Run Server descendants. Preserve the shared
Profile Server. Keep live control-socket cleanup on TASK-004's fail-closed
primitive; TASK-020 owns self-heal after establishing four-verdict process
identity.

### Process identity and group recovery

Establish four-verdict worker and Dedicated Run Server identity with boot-session
proof, provisional identity, kqueue continuity, persisted member snapshots, and
complete process-group, parent, and session census. Recovery must detect PID,
group, session, boot, and inode reuse; signal no unrelated process; and continue
cleanup only from verified identities and complete empty censuses. Add live
control-socket self-heal only when that proof authorizes replacement of the
existing pathname.

### History reconciliation, outcome unknown, and fork

Reconcile persisted thread history only across a proved-absent old epoch and a
compatible new epoch. Represent uncertain completion as `outcome_unknown`,
permit non-authoritative read-only reconciliation to paused, prohibit replay of
unknown input, and support only manifest-defined fork boundaries plus explicit
`fork --fresh` escape while preserving instruction provenance.

## Required constraints

- Preserve exactly one writer authority per canonical worktree across profiles,
  client origins, parent shapes, generations, and linked Git worktrees.
- Keep `effective_policy` and `writer_authority` independent and bind every
  PREPARE, APPLY, COMMIT, cancel, response, repair, and recovery action to its
  current revision and Controller authority.
- Never infer absence, completion, release, cleanup, reconciliation, or replay
  safety from elapsed time, process identifiers alone, or incomplete census.
- Keep shared-readonly Runs read-only; a write request creates a lineage-linked
  dedicated continuation with fixed workspace, profile, control mode, thread
  residency, and same-principal Controller semantics.
- Preserve secret-bearing user input only as an opaque successful receipt; do
  not retain its content or a content-derived digest.
- Keep the shared Profile Server and unrelated operating-system processes
  outside Dedicated Run cleanup ownership.

## Prohibited shortcuts and non-goals

- Do not add alternate state-root discovery, migration, fallback, or
  compatibility behavior around `~/.dolgorae`.
- Do not promote a shared-readonly Run in place, transfer authority while a Run
  is active or waiting, or release authority before background absence is
  proved.
- Do not treat `CommandExecution.processId`, activity signals, PID/PGID
  continuity, or a live socket pathname as sufficient process identity.
- Do not replay an interaction or turn whose delivery or outcome is unknown.
- Do not claim ownership of shared App Server descendants or implement the
  reusable external engagement, Gul gateway, Brokered Hierarchy, collaboration,
  operator UI, or release work owned by later Epics.
- Do not push, release, install, perform a live rollout, or mutate another
  repository as part of this Epic.

## Acceptance and closeout

Every member Task must satisfy the roadmap's ordinary completion gate with its
specified deterministic crash, concurrency, identity, protocol, and recovery
coverage. Specifications, architecture, accepted decisions, checked contracts,
implementation, tests, user and operator guidance, and independent review must
agree that writer authority, Controller-authorized interaction, pause and close,
process identity, and outcome-unknown reconciliation fail safely.

Before marking the Epic complete, classify every remaining dossier statement as
durable information to promote to its canonical owner or temporary delivery
context to remove. Then delete this file and its TODO index entry, replace the
roadmap's `Detailed SOT` link with existing `Canonical Outcomes` links, and
perform the Epic lifecycle transition in one approved closeout change. Keep no
archive or tombstone copy of this dossier.
