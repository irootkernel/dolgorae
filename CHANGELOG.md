# Changelog

This file records notable user-facing and maintainer-facing changes to
Dolgorae. Releases receive a version and date only when the repository has
corresponding release authority; historical releases are not inferred from
development commits.

## v0.1.4 - Unreleased

## v0.1.3 - 2026-10-01

### Added

- Serve the 27-method public-v1 provider API over a protected same-user Unix socket,
  with bounded subscriptions and durable Machine CLI/gRPC observations.
- Add immutable Specialist Roles and policy snapshots, create-exclusive policy
  installation, and atomic Orchestrated Session bootstrap.
- Connect trusted Run-bound Primary calls to managed Specialist provisioning,
  durable approval routing, generation fencing, and deterministic idle reuse.
- Dispatch accepted Specialist tasks with immutable artifact context, exact replay,
  busy rejection, and fail-closed Writer and uncertain-dispatch boundaries.
- Add acceptance-anchored deadlines, bounded `any`/`all` waits, and cancellation
  that remains `interrupt_requested` until terminal Turn proof.
- Publish verified Primary-owned Specialist results with exact redelivery and
  bounded UTF-8 artifact pages without exposing child Controller authority.
- Expose complete Controller-authorized Run timelines with accepted prompts,
  safe image metadata, large-input artifacts, interactions, and exact replay.
- Expose coherent Orchestrated Session snapshots and captured-head result
  discovery that survives private collection and gateway restart.
- Close complete Orchestrated Sessions through one durable operation, fencing new
  work and preserving close correlation, restart recovery, and unknown outcomes.
- Freeze `dolgorae.gul-consumer/v1` generated clients, descriptor baselines, and
  schema digests while preserving pre-extension public-v1 consumers.
- Add task-aware Specialist Review v3 with immutable accepted briefs, inline
  context, ordered criteria, and criterion-complete results alongside legacy v1/v2.
- Retain one-shot review references for read-only inspection and authorized cleanup
  after CLI loss, retiring only explicitly owned temporary servers.
- Package `use-dolgorae` with local checked schemas, examples, and operating
  guidance; reinstall from v0.1.3 to update the optional skill.
- Add identity-verified orphan inspection and cleanup for detached processes
  owned by explicitly selected retired homes.
- Provide opt-in compatibility, provider, and independent-review acceptance
  campaigns alongside isolated default E2E coverage.

### Changed

- Require Codex 0.158.0 or newer for Profile runtimes and pin its schema bundles;
  report the qualified minimum as `tested` and newer compatible versions as `unverified`.
- Bind endpoint overrides to launch state, verify protected socket rendezvous,
  guard Profile reset, and retain completed-only history-copying forks.
- Honor explicit one-shot Profile model and reasoning effort without substitution;
  omitted settings use `gpt-6-sol` and `high`.
- Require public `CloseRun` for Orchestrated roots; Machine CLI `run close`
  refuses them.
- Keep accepted review results and safe structured-output diagnostics stable
  across exact retry, restart, await, and collection.
- Bound transient result-capture recovery by the durable task deadline and
  restrict ledger fallback to quiescent restart recovery.
- Require Go 1.26.6 and Buf 1.69.0 for repository validation alongside Rust 1.97.1
  and the documented Python validation dependencies.

- Update Aquarium Procedures with explicit review-route evidence and separate
  implementation, commit, and publication approvals.

### Fixed

- Reject native Codex `--profile` launch arguments before Profile registration
  because the qualified app-server does not support them.
- Accept native absolute file-change and move paths within the canonical
  workspace while refusing paths that escape it.
- Report durable policy epochs and thread generations in Run snapshots; resume
  retained threads after idle-worker loss and retire absent-generation Writer
  authority before starting a successor.
- Complete prepared write admissions after a definite `WRITER_BUSY` refusal.
- Reject duplicate Broker approval JSON members and fresh Primary Turns after
  Session close begins; preserve the checked close error and exact replay.
- Record Reviewer readiness before the first Turn and retain resolved v3
  execution metadata in capture failures.
- Align Specialist Policy v2 success and rejection details with the checked
  machine contracts.
- Keep resumed Run snapshots available by rebinding the retained thread and
  policy to the current native server before reporting idle, without submitting a Turn.
- Reuse compatible Profile servers after Codex updates runtime trust settings.
- Retain `RUN_MANIFEST_INVALID` as a checked one-shot failure across inspection
  and exact retry, with exit status 5.
- Allow External Specialists to be released before their first task. Retire
  closed Workers and global Profile membership on release or abort so Profile
  servers can stop normally; preserve terminal ledger seals when interrupting
  stale closed attachments.
- Expire overdue accepted Specialist tasks before dispatch, preserving their
  acceptance across restart and avoiding Writer handoff or Turn submission.
- Keep streaming ledger schedulers within their commit groups and wait for
  publisher Writer locks to be released before reopening.
- Finish unpublished server cleanup after its child exits, including when its
  process group disappears during the ownership census.
- Reduce debug-build executable verification cost while preserving fresh
  SHA-256 integrity checks.

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
