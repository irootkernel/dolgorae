# Changelog

This file records notable user-facing and maintainer-facing changes to
Dolgorae. Releases receive a version and date only when the repository has
corresponding release authority; historical releases are not inferred from
development commits.

## v0.1.3 - Unreleased

### Added

- Freeze the checked `dolgorae.gul-consumer/v1` wire contract with two
  Controller-authorized orchestration queries, a 27-method required profile,
  whole-session close correlation, generated clients, and immutable fixtures.
- Select native Codex `item/tool/call` as the private live Primary transport on the TASK-025 isolated-home campaign pin (locally installed Codex CLI 0.155.1; the product compatibility baseline remains 0.153.4). Freeze assignment receipts and `blocking` waits, lossless UTF-8 `read_specialist_result` pages, and the internal live-Primary boundaries for submission versus completion, Worker/Broker routing, business-rejection replay, and layered authorization.
- Add verified orphan inspection and cleanup for detached Dolgorae processes, including disposable E2E owners.
- Add task-aware Specialist Review v3 for one-shot and reusable assignments,
  with exact accepted briefs, inline contexts, ordered completion criteria, and criterion-complete structured results.
- Foreground local gRPC gateway with same-user Unix socket ownership, bounded
  event subscriptions, and the 24-method Brokered Hierarchy bootstrap surface.
- Shared Run, Controller, Writer, interaction, and artifact observations for Machine CLI and gRPC,
  with durable event revisions and immutable Turn acceptance replay across gateway and worker replacement.
- Checked common and project Specialist Role sources, create-exclusive
  Specialist Policy Registry operations, and immutable global-Profile v2 policy
  snapshots.
- Prepared Dolgorae-Orchestrated Session bootstrap and a durable Brokered
  Hierarchy core with approval-aware Specialist provisioning, broker-owned
  Controller capabilities, accepted task/result delivery, writer sequencing,
  recovery, and a transport-independent Primary tool dispatcher.

### Changed

- Bound transient Specialist result-capture recovery by the durable task
  deadline, restrict ledger fallback to fail-closed quiescent restart recovery,
  anchor one-shot v3 waits to durable acceptance, preserve legacy review failure
  meanings, validate checked scoped-review failures and v3 await/collect task
  results, bind terminal facade result construction to the accepted request
  digest, classify corrupt accepted v3 tasks as integrity failures, and
  preserve structured-output validation errors across exact retry.
- Set the Codex compatibility baseline to 0.153.4 while retaining completed-only forks.
- Honor explicit Profile model and reasoning-effort settings for one-shot
  Specialist Review, rejecting unavailable selections without substitution.

## v0.1.2 - 2026-09-08

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

- Release the Operator lock explicitly after fork inheritance and retry interrupted
  lock operations without misclassifying contention.
- Bound global Profile home and registry reads before rejecting oversized files.
- Bound membership replay reads and stream journal checksums without changing
  membership limits or persisted hashes.
- Reject null or mixed-version Profile fields and missing required fields in
  immutable Agent Configuration snapshots.
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

- Reject changed live worker executables and restore missing control sockets;
  persist explicit recovery requirements when safe restoration fails.
- Preserve interaction quarantine and terminal staleness, and durably accept
  pinned Codex file-change approvals.
- Report current Writer activity and destination server epochs, preserve
  handoff-cancellation retries, and stabilize write-continuation retries while
  requiring evidence for access-transition failure reasons.
- Reject insecure Profile state and membership authority files, including
  dangling symlinks, and refuse logged-out Profiles that require authentication.
- Align immutable review-target failures with the checked machine error contract.
- Reject FIFO files during worktree capture without blocking.
- Reject unterminated membership journals before reading or appending authority.

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
