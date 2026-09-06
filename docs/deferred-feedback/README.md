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

## DF-004: Keep membership checksum fields in one representation

- Owner: Global Profile membership maintainers
- Finding: append and replay construct equivalent checksum bodies separately.
- Reason for deferral: the fields currently agree and journal replay tests
  verify their compatibility; consolidation is independent of current behavior.
- Revisit condition: the next membership schema revision or checksum-field change.

## DF-005: Enforce complete launch-snapshot projection during schema evolution

- Owner: Global Profile runtime maintainers
- Finding: the name-neutral launch snapshot explicitly copies the named
  snapshot's fields, so a future field needs a matching projection change.
- Reason for deferral: all current launch fields are present and binding tests
  cover current equality; this is a future schema-maintenance constraint.
- Revisit condition: adding a Profile snapshot field or changing launch identity.

## DF-006: Centralize the Profile lifecycle home path

- Owner: Global Profile lifecycle maintainers
- Finding: lifecycle operations derive the home from the server root at several
  call sites instead of retaining it in their shared path context.
- Reason for deferral: the fixed current layout is consistent at those sites;
  changing the path representation is independent of current lifecycle safety.
- Revisit condition: changing the Profile server directory layout.

## DF-007: Coordinate orphan verification with Run manifest successors

- Owner: Global Profile and Run persistence maintainers
- Finding: the Profile layer's bounded orphan check explicitly reads v2 Run
  identity fields to preserve the approved dependency boundary.
- Reason for deferral: current v2 fields agree with the Run owner; a shared
  verifier requires a separate boundary design when the contract evolves.
- Revisit condition: introducing a Run manifest successor or changing its
  workspace, Run, or global Profile identity fields.

Record a future entry only with a concrete finding, owner, reason for deferral,
and revisit condition. Promote epic-sized work to the
[TODO owner](../todo/README.md) or adopt it in the canonical
[roadmap](../roadmap/README.md).
