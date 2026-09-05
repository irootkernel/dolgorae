# Dolgorae Deferred Feedback

This index owns small, independent, actionable findings intentionally postponed
from current work. It is not a second roadmap or status authority.

## DF-001: Consolidate external-facade validation helpers

- Owner: External Specialist Facade maintainers
- Finding: bounds, required-null deserialization, role-reference validation,
  and interrupt/terminal-proof plumbing still have small equivalent helpers in
  the facade, engagement store, and semantic service.
- Reason for deferral: the current layers intentionally revalidate at their own
  trust boundaries, and consolidating those helpers is independent of
  EPIC-006 correctness. A premature shared module could erase those boundary
  checks rather than merely deduplicate their representation.
- Revisit condition: the next external-facade schema revision or any change to
  a duplicated bound or terminal-proof budget.

## DF-002: Centralize external-engagement state predicates

- Owner: External Specialist Facade maintainers
- Finding: the SQLite schema, partial indexes, transition guards, recovery
  queries, and projections repeat the same active-task and active-engagement
  state sets as SQL literals.
- Reason for deferral: those literals currently agree and are covered by schema,
  migration, transition, and lifecycle tests. Replacing them safely requires a
  deliberate query-builder or generated-schema boundary and is independent of
  EPIC-006 behavior.
- Revisit condition: adding or renaming an engagement, member, or task state, or
  introducing the next orchestration schema migration.

## DF-003: Attest external-facade caller identity

- Owner: External Specialist Facade maintainers
- Finding: nested-hire defense in depth resolves `CODEX_THREAD_ID` from the
  caller environment, which a same-uid subprocess can unset or replace. The
  aggregate-owner Controller credential and profile isolation remain the
  authoritative capability boundary, so this does not grant a Specialist new
  authority under the current contract.
- Reason for deferral: an independently authoritative caller identity requires
  a descriptor-passed or worker-attested channel across the host, CLI, and Run
  boundary. That trust-boundary change is independent of EPIC-006 correctness
  while the owner credential remains unavailable to Specialist Runs.
- Revisit condition: extending the threat model to hostile same-uid callers,
  exposing the External Specialist Facade to a Specialist Runtime Profile, or
  adding an attested caller-identity carrier.

Record a future entry only with a concrete finding, owner, reason for deferral,
and revisit condition. Promote epic-sized work to the
[TODO owner](../todo/README.md) or adopt it in the canonical
[roadmap](../roadmap/README.md).
