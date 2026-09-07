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

## DF-008: Keep global Profile digest predicates consistent

- Owner: Global Profile runtime maintainers
- Finding: registry digest validation and runtime server-key validation repeat
  the same lowercase SHA-256 predicate.
- Reason for deferral: their current accepted formats agree; centralization is
  independent of current binding and membership correctness.
- Revisit condition: changing a digest format or its accepted representation.

## DF-009: Centralize quiescence refusal construction

- Owner: Global Profile membership maintainers
- Finding: the two quiescence lock-entry paths construct the same busy error.
- Reason for deferral: current codes, details, and retryability agree and both
  paths reject live members; consolidation addresses future contract drift.
- Revisit condition: changing the Profile busy error or quiescence diagnostics.

## DF-010: Exercise concurrent initial home creation across processes

- Owner: Global Profile home maintainers
- Finding: registry writes and membership appends have concurrency coverage,
  while initial generation creation is covered by sequential lifecycle tests.
- Reason for deferral: initial creation holds the parent-directory lock and
  rechecks the generation under that lock; an additional process-race campaign
  strengthens evidence without correcting a known current race.
- Revisit condition: changing initialization locking, staging recovery, or the
  atomic home-publication sequence.

## DF-011: Define recovery of abandoned atomic-write temporaries

- Owner: Global Profile persistence maintainers
- Finding: a crash can leave private atomic-write temporary files beside the
  live registry or membership state.
- Reason for deferral: those mode-0600 files are not read as live authority;
  automatic cleanup needs its own safe ownership and active-writer rules.
- Revisit condition: adding storage maintenance or investigating repeated
  crash-related accumulation in the global home.

## DF-012: Construct reviewer configurations at their final version boundary

- Owner: Specialist planning and semantic-service maintainers
- Finding: the reviewer planner produces a historical configuration that its
  production caller converts to the current version and binding digest.
- Reason for deferral: the sole production caller performs both conversions
  before publishing the configuration; direct current-version construction
  would make future consumers less dependent on that caller convention.
- Revisit condition: adding a reviewer-plan consumer or a configuration version.

## DF-013: Make snapshot binding persistence explicit to future callers

- Owner: Global Profile launch maintainers
- Finding: snapshot preparation also records the name-to-launch binding history.
- Reason for deferral: current callers authorize preparation and its durable
  binding write; an explicit return-and-record boundary would help future
  read-only consumers avoid accidentally adopting a mutating helper.
- Revisit condition: introducing a read-only snapshot consumer or changing launch
  preparation ownership.

## DF-014: Consolidate compatibility checks while preserving entry contracts

- Owner: Run and External Specialist semantic-service maintainers
- Finding: Run admission and prepared Specialist admission repeat model, effort,
  and capability checks with entry-specific error construction.
- Reason for deferral: both paths reject incompatible inputs under their current
  contracts; sharing validation facts is independent of that behavior and must
  retain each public error surface and its boundary revalidation.
- Revisit condition: changing compatibility requirements or adding a consumer.

## DF-015: Add a checked Run manifest successor fixture

- Owner: Run persistence and protocol-validation maintainers
- Finding: the v2 manifest has Rust publication and recovery coverage but no
  positive fixture routed through the checked JSON Schema example validator.
- Reason for deferral: no manifest/schema divergence has been established;
  a retained representative fixture would supplement existing semantic tests
  with direct schema-instance coverage.
- Revisit condition: changing the manifest schema or expanding checked examples.

## DF-016: Define a shared Run manifest size budget

- Owner: Run persistence and Global Profile membership maintainers
- Finding: Run recovery and membership orphan verification read complete
  manifests without a shared byte ceiling, so an oversized user-owned manifest
  can consume excess memory during inspection.
- Reason for deferral: the current publisher has no total-manifest byte budget;
  a limit only in orphan verification could reject a valid published Run.
  Establish matching publication, recovery, and inspection limits together,
  including compatibility with existing immutable snapshots. Hostile same-uid
  mutation is outside the current threat model.
- Revisit condition: adding a persisted Run size budget, changing snapshot
  bounds, or extending the local-state threat model.

## DF-017: Distinguish binding-history failures in home diagnostics

- Owner: Global Profile home and Machine error-contract maintainers
- Finding: malformed, oversized, or inconsistent binding history is reported
  under the existing `malformed_marker` classification, which does not identify
  the failing artifact precisely.
- Reason for deferral: the current classification correctly blocks admission;
  a more specific classification needs coordinated error-contract and operator
  runbook changes rather than silently altering a checked response.
- Revisit condition: introducing a home-diagnostic contract successor or the
  journal-maintenance candidate in the TODO index.

## DF-018: Extend defensive home and Machine projection regressions

- Owner: Global Profile and CLI validation maintainers
- Finding: explicit negative fixtures could additionally pin non-v1 registry
  removal and symlinked workspace-entry rejection; schema assertions could
  also be extended to remaining non-engagement Machine test helpers.
- Reason for deferral: existing production guards reject those filesystem and
  schema states, and current command/schema tests establish no divergence.
  These cases are independent regression expansion, beyond the exercised
  marker encoding/size, registry limits, and home-admission classifications.
- Revisit condition: changing registry removal, workspace-entry traversal, or
  the affected Machine test helpers.

Record a future entry only with a concrete finding, owner, reason for deferral,
and revisit condition. Promote epic-sized work to the
[TODO owner](../todo/README.md) or adopt it in the canonical
[roadmap](../roadmap/README.md).
