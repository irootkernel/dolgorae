# Changelog

This file records notable user-facing and maintainer-facing changes to
Dolgorae. Releases receive a version and date only when the repository has
corresponding release authority; historical releases are not inferred from
development commits.

## v0.1.2 - Unreleased

### Added

- Added the optional source-distributed `use-dolgorae` agent skill for
  capability-aware workspace, profile, immutable-target, and one-shot review
  workflows.

### Changed

- Changed `version`, `--version`, and `-V` to compact `dolgorae v<version>`
  output and added exact two-field JSON through `version --json`.

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
