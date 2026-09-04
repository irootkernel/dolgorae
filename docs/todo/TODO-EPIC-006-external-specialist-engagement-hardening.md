# EPIC-006: External Specialist Engagement Hardening

Roadmap Epic: [`EPIC-006`](../roadmap/README.md#epic-006-external-specialist-engagement-hardening)

This temporary dossier owns the implementation detail needed while EPIC-006 is
active. The canonical roadmap remains the sole authority for identifiers,
ordering, dependencies, lifecycle vocabulary, and status.

## Goal and purpose

Generalize the one-shot read-only Specialist Review Preview into a durable,
reusable External Specialist Engagement while the external AI or host remains
the only semantic control plane.

The Epic enables an external control plane to keep, recover, and explicitly
coordinate multiple Dolgorae-managed Specialists across sequential tasks and
restarts. It does not turn the engagement into a Brokered Hierarchy or give
Dolgorae ownership of the external plan or task graph.

## Scope and approach

- Build on the completed one-shot Specialist Review facade and aggregate model
  without weakening the access, lifecycle, process-identity, or outcome rules
  established by EPIC-005.
- Support multiple independently hired, long-lived Specialist members and
  repeated sequential tasks per member through explicit engagement operations.
- Make engagement, member, accepted-task, result-delivery, and idempotency state
  durable and recoverable across host disconnects and Dolgorae restarts.
- Permit `isolated_write` only through a separate isolated workspace or
  worktree. Keep canonical writes subject to the existing writer-authority
  contract and explicit external-host quiescence.
- Keep specifications, architecture, accepted decisions, checked protocol
  artifacts, implementation, tests, public usage, and operations guidance
  synchronized with the behavior each authority owns.

## Task objective

### Reusable external Specialist engagements

Remove the preview's one-shot lifecycle restriction while preserving its
trusted facade and aggregate ownership. Support explicit inspection, hiring,
assignment, waiting, collection, cancellation, release, completion, and abort;
safe host reconnect; completed-result redelivery; and exact aggregate-scoped
idempotency across restarts. Reconcile every accepted task through the durable
pause, close, process-identity, history, and outcome-unknown rules owned by
TASK-019 through TASK-021.

## Required constraints

- Keep the external AI or host as the sole semantic planner. Dolgorae stores
  operational engagement state but does not infer, persist, or execute an
  external task graph.
- Retain at most one active Turn per Specialist and never preempt implicitly.
  The external control plane explicitly waits, retries, hires another member,
  cancels work, or releases the member.
- Bind engagement operations to the aggregate owner and each Specialist Run to
  its own Controller authority. Parent projections and external provenance do
  not grant mutation authority.
- Preserve accepted-task identity and completed-not-delivered results without
  replaying a target Turn. Ambiguous accepted or running work becomes
  `interrupted_unknown` and is never silently replayed.
- Keep engagement, member, task, delivery, and lifecycle transitions exact-key
  idempotent and fail closed on same-key input drift.
- Apply the same canonical workspace writer authority regardless of whether a
  Run originated from an external engagement or another supported use case.

## Prohibited shortcuts and non-goals

- Do not create another semantic control plane, convert an External Specialist
  Engagement into a Brokered Hierarchy, or infer scheduling decisions for the
  external host.
- Do not allow an external Specialist to hire a nested first-class Specialist
  or use the Brokered Collaboration Plane.
- Do not permit concurrent Turns for one Specialist, implicit preemption,
  automatic retry of unknown input, or in-place attachment of an existing Run.
- Do not treat a canonical workspace as isolated-write storage or bypass the
  TASK-017 writer-authority protocol when the external host may be writing.
- Do not implement the later Gul gateway, Dolgorae Primary control plane,
  Brokered Hierarchy, lateral collaboration, operator UI, or release work.
- Do not push, release, install, perform a live rollout, or mutate another
  repository as part of this Epic.

## Acceptance and closeout

TASK-022 must satisfy the roadmap's ordinary completion gate and its complete
restart, multi-member, sequential-task, redelivery, lifecycle, write-isolation,
authorization, and denial matrix. Specifications, architecture, checked
contracts, implementation, tests, user and operator guidance, and independent
review must agree that external hosts can durably keep, reuse, recover, and
explicitly coordinate Specialist Engagements. Epic completion then unlocks
`MILESTONE-ES1`.

Before marking the Epic complete, classify every remaining dossier statement as
durable information to promote to its canonical owner or temporary delivery
context to remove. Then delete this file and its TODO index entry, replace the
roadmap's `Detailed SOT` link with existing `Canonical Outcomes` links, and
perform the Epic lifecycle transition in one approved closeout change. Keep no
archive or tombstone copy of this dossier.
