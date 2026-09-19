# Dolgorae TODO Candidates and Dossiers

This index owns future epic-sized candidates and temporary dossiers for adopted
Epics awaiting closeout, including those still `PLANNED`. It does not own
roadmap identity, order, status, or dependencies.

## Future candidates

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
- Prerequisite: Gul has a supported runnable client and Dolgorae has completed
  `MILESTONE-BH1-P` provider acceptance.
- Outcome: verify actual Gul startup/supervision, protected Controller carriers,
  Orchestrated Session creation, approval UX, Specialist result rendering,
  artifact metadata/chunks and digest, event reconnect, and provider restart
  against exact supported client/provider revisions.
- Evidence: `real_gul_harness`, meaning the actual Gul client and its integration
  campaign, not a Gul-shaped test client or provider-only tests.
- Milestone: only this separately adopted consumer campaign can establish
  `MILESTONE-BH1`. Provider conformance never implies it has passed.
- Delivery boundary: not a prerequisite for EPIC-008, v0.1.3 provider release
  eligibility, or EPIC-009 provider implementation. Do not create an ACTIVE or
  BLOCKED placeholder that consumes the roadmap's sequential Task slot.
- Revisit condition: Gul is ready for integration; adopt a bounded acceptance
  item then, with concrete revision/build inputs and normal completion gates.

## Adopted Epic dossiers

- [EPIC-008: Live Dolgorae Provider](EPIC-008-live-provider.md) is adopted for
  implementation. The roadmap alone owns its current `PLANNED` status and the
  eight-Task execution order. Documentation adoption does not start a runtime,
  commit changes, or authorize live credentials.

The term adopted here does not imply that the roadmap state is `ACTIVE`.

An unadopted `TODO-*.md` file has no roadmap identity. On adoption, retain its
dossier from planning through execution and review until Epic closeout, list it
here, and link it from the [roadmap](../roadmap/README.md) with `Detailed SOT`.
This follows the [documentation lifecycle](../README.md#roadmap-identity-and-dossier-lifecycle)
and does not depend on the Epic being `ACTIVE`. Closeout promotes durable
content to its canonical owners, removes the dossier and this index entry, and
replaces the roadmap link with `Canonical Outcomes`.
