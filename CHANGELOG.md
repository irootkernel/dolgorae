# Changelog

This file records notable user-facing and maintainer-facing changes to
Dolgorae. Releases receive a version and date only when the repository has
corresponding release authority; historical releases are not inferred from
development commits.

## v0.1.2 - Unreleased

### Added

- User-global Codex Profiles with explicit per-Run selection, immutable Run
  bindings, cross-workspace server membership, and fail-closed legacy-home
  detection.
- Reusable externally planned Specialist Engagements with multiple durable
  members, sequential tasks, reconnect-safe idempotency, result redelivery,
  isolated-change artifacts, and Writer-authorized canonical work.
- Added the optional source-distributed `use-dolgorae` agent skill for
  capability-aware workspace, profile, immutable-target, and one-shot review
  workflows.
- Durable per-worktree writer authority, explicit access transitions, write
  continuations, and safe cross-profile handoff.
- Reconnectable Controller-authorized command, file-change, and user-input
  interactions.
- Conservative pause, resume, close, and terminal cleanup for persistent runs.
- Outcome-unknown reconciliation and provenance-preserving run forks with an
  explicit fresh escape.

### Changed

- Align External Specialist v2 error projections with the Machine error contract,
  including missing global Profiles and unsupported home generations.
- Reject insecure existing server locks before Profile membership or lifecycle
  operations without changing their permissions.
- Workspace initialization is account-neutral, Profile commands no longer
  accept workspace scope, and both Specialist entry paths use the selected
  global Profile through versioned machine-readable contracts.
- Changed `version`, `--version`, and `-V` to compact `dolgorae v<version>`
  output and added exact two-field JSON through `version --json`.
- Run and profile recovery now require four-verdict Darwin process identity
  before cleanup or live-socket repair.
- Writer transitions now fail closed after uncertain worker outcomes, orphaned
  dedicated servers are retired only after identity-proven worker absence, and
  interaction-response receipts survive worker restart.
- Writer recovery now preserves known pre-effect state and releases closed
  engagement Writers without respawning workers.
- External Specialist facade failures now conform to the checked error-result
  contract.
- Immutable worktree-target capture now rejects symlinks in every path
  component and bounds Git object output before buffering it.

### Fixed

- Reject duplicate Profile and environment keys before reading or changing the global registry.
- Reject inconsistent membership history before Profile lifecycle operations or membership updates.
- Validate workspace compatibility before creating global home state during initialization.

## v0.1.1 - 2026-08-31

### Changed

- Moved all per-user configuration and mutable authority to the fixed
  `~/.dolgorae` root.

## v0.1.0 - 2026-08-30

### Added

- Established portable Git and explicit non-Git workspaces with durable local
  identity, permission-safe state, and machine-readable command envelopes.
- Added persistent Codex app-server profiles, worker and controller authority,
  durable Run lifecycle commands, reconnectable event history, recovery, and
  audit records.
- Added isolated one-shot specialist review with bounded structured findings
  and explicit failure, timeout, and settlement behavior.
- Added immutable workspace, staged, dirty, HEAD, commit, and range review
  targets with safety checks, source-drift detection, and idempotent settlement.

### Changed

- Migrated canonical documentation into explicit single-scope role directories
  and established the missing implementation, operations, TODO, and deferred
  feedback owners.
