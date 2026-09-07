# Dolgorae TODO Candidates and Dossiers

This index owns future epic-sized candidates and temporary dossiers for adopted
active Epics. It does not own roadmap identity, order, status, or dependencies.

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

## Adopted active-Epic dossiers

None.

An unadopted `TODO-*.md` file has no roadmap identity. On adoption, retain its
dossier only while the Epic is active, list it here, and link it from the
[roadmap](../roadmap/README.md) with `Detailed SOT`. Closeout promotes durable
content to its canonical owners, removes the dossier and this index entry, and
replaces the roadmap link with `Canonical Outcomes`.
