# Gomchi Roadmap

Status: Ordered implementation roadmap. Pre-implementation `TASK-000` is
complete; no production implementation Task is active.

This document owns execution order and delivery status. Product requirements
remain authoritative in [specs.md](specs.md); this roadmap must not redefine
them.

## Status Model

Allowed Epic and Task states are `PLANNED`, `ACTIVE`, `BLOCKED`, and `COMPLETE`.

- At most one Epic may be `ACTIVE`.
- Across the entire roadmap, at most one Task may be `ACTIVE`.
- A `BLOCKED` Task continues to occupy the sequential Task slot. Work MUST NOT
  bypass it by activating a later Task.
- Zero active items is valid during a future quiescent SOT-only state.
- A Task may become active only after all preceding Tasks are complete and all
  SOT contradictions affecting it are resolved.
- `TASK-000` is the sole exception: it may be active while resolving the
  pre-implementation contradictions that define its scope, but it may create
  only probe fixtures, evidence, and SOT changes, never production code.
- An Epic becomes complete only when all of its Tasks and Epic-level acceptance
  checks are complete.

## Task Completion Gate

A Task is `COMPLETE` only when all of the following are true:

1. Its required behavior is implemented within scope.
2. Its designated deterministic verification passes.
3. Affected SOT documents are synchronized.
4. An independent read-only review has completed.
5. Every blocking finding is fixed or rejected with evidence and independently
   confirmed as resolved.
6. Non-blocking findings are recorded in
   [deferred-feedback.md](deferred-feedback.md).
7. An implementation note records the evidence.
8. One or more task-scoped Git commits are linked from that note.

This gate does not prescribe how an implementer divides commits or stages files.
Push always requires separate explicit user authorization.

## EPIC-000: Pre-Implementation Stabilization

Status: `COMPLETE`

Goal: Retire external protocol uncertainty and make the product, architecture,
and roadmap deterministic before production implementation begins.

### TASK-000: Architecture Reconciliation and Codex Probe

Status: `COMPLETE`

Reconcile the writer lease, stale-generation cleanup, runtime discovery,
`outcome_unknown`, audit encoding/redaction/durability, protocol bounds, public
error mapping, worktree identity, and worker detachment contracts. Run isolated
probes against the pinned Codex 0.147.0 target for crash-history durability,
stable sandbox read/write enforcement, native-subagent event visibility, and
generated schema shape. Also measure bounded real frame sizes and exercise a
workspace-write crash followed by live `thread/read` and
`thread/fork(lastTurnId)`. Verify advertised effort behavior, turnless-thread
persistence limits, post-turn resume/history, active `turn/interrupt`, and the
macOS lock/process semantics used by recovery. Preserve bounded, redacted fixtures and record every
`review.md` finding as fixed, rejected with evidence, or deferred. Working
results live in [TASK-000 probe evidence](probes/task-000.md).

Verification: parse and link-check every SOT document; verify the generated
schema contains the required stable subset; run repeatable isolated protocol
probes with recorded commands and versions; compile and run the durable macOS
OS-semantics probe; independently review all revised contracts and probe
evidence with an adversarial concurrency/recovery pass. If crash history lacks terminal evidence, record
that recovery converges to `outcome_unknown` and fork rather than weakening the
no-replay rule.

Epic acceptance: all blocking pre-implementation findings are resolved, probe
results and limitations are durable, the Task completion gate passes, and no
production source has been introduced. Only then may `TASK-001` become active.

## EPIC-001: Foundation and Durable State

Status: `PLANNED`

Goal: Establish the Rust program, stable machine contract, workspace policy,
and audit-first run storage on which every process operation depends.

### TASK-001: Rust CLI and Core Contract

Status: `PLANNED`

Implement the Rust 2024 binary skeleton, command parser, UUIDv7 identities,
stable JSON success/error envelopes, exit-status mapping, typed lifecycle and
access enums, and `--human` rendering boundary.

Verification: unit tests for serialization compatibility, unknown-field
tolerance, every exit class, and CLI argument conflicts; formatting and clippy
must pass with warnings denied.

### TASK-002: Workspace Initialization and Discovery

Status: `PLANNED`

Implement Git and explicit non-Git initialization, per-worktree canonical
workspace identity, upward `.gomchi` discovery, minimal policy files, generated
local ignore policy, dirty-worktree baseline capture, and safe permission
creation.

Verification: tests for subdirectory discovery, symlink normalization, Git
worktrees, dirty/untracked preservation, non-Git opt-in, repeated initialization,
and refusal of uninitialized start.

### TASK-003: Manifest, Audit Ledger, and State Projection

Status: `PLANNED`

Implement run directory creation, fixed manifest semantics, RFC 8785
`sha256-jcs-v1` `audit.jsonl`, atomic `state.json`, bounded group commit,
write-ahead durability barriers, lossless-number adaptation, canonicality
verification, bounded `payload_unrepresentable` records, torn-tail repair
evidence, replay, final seals, normative redaction, and file/directory
permissions. Preserve failed start attempts as
`start_failed`.

Verification: deterministic JCS and redaction golden records; duplicate keys,
large numbers, canonical fixed points, middle corruption versus torn tail,
ahead/stale projection rebuild, crash-before-action barriers, payload caps,
permissions, arrays/non-ASCII/string-encoded JSON boundaries, and exact
redaction-table tests.

Epic acceptance: a run can be allocated and reconstructed from its ledger
without starting Codex, and all persisted formats are versioned.

## EPIC-002: Worker and Codex App-Server Integration

Status: `PLANNED`

Goal: Provide reconnectable per-run process ownership and a strict stable-subset
adapter for the pinned Codex account and thread.

### TASK-004: Per-Run Worker and Unix IPC

Status: `PLANNED`

Implement detached hidden worker re-execution, fixed short private socket paths,
versioned runtime discovery/identity sidecars, persistent local locks, fd-3
startup handoff, per-run startup serialization, stale-socket recovery, bounded
request/response IPC, ledger-backed event streaming, reconnection, worker
discovery, and on-demand generation startup.

Verification: fake worker tests for concurrent starts, changed `$TMPDIR`, stale
and colliding sockets, ten-second startup timeout, CLI/worker version skew,
caller Ctrl-C and command substitution, inherited-signal reset, oversized or
malformed frames, cross-run identity rejection, slow-observer backpressure, and
worker restart.

### TASK-005: Target Registry and Compatibility Doctor

Status: `PLANNED`

Implement XDG target CRUD, argv and absolute `CODEX_HOME` validation, inherited
environment preparation, schema generation into temporary storage, required
stable-subset comparison, app-server handshake, `codexHome` matching,
`model/list`, tested/unverified verdicts, and target snapshotting.

Verification: fake executable matrices for missing commands, wrapper argv,
target-name collision, home mismatch, unsupported/older/newer versions, missing schema fields,
additive fields, login failure, and successful 0.147.0 compatibility.

### TASK-006: Thread and Turn Lifecycle

Status: `PLANNED`

Implement stdio JSONL process ownership, initialize/initialized, thread
start/resume/fork, model fixation, effort validation, turn start/interrupt,
one-active-turn serialization, local image input, send/submit/wait behavior,
idempotency, usage capture, terminal readback, and the minimal controllable fake
app-server core required for deterministic verification.

Verification: deterministic fake app-server scenarios for every request and
notification ordering, request/thread/turn/generation mismatch, duplicate
terminal messages, malformed output, send timeout, caller death, same/different
idempotency payloads, and fixed-model enforcement.

Epic acceptance: an initialized workspace can run a multi-turn read-only session
through a fake app-server while preserving one thread and reconnecting CLI
callers.

## EPIC-003: Access, Interaction, and Recovery Safety

Status: `PLANNED`

Goal: Enforce Gomchi's one-writer-app-server-per-worktree scope,
master-controlled interaction, and conservative failure semantics.

### TASK-007: Reader/Writer Lease and Access Transitions

Status: `PLANNED`

Implement the per-worktree BSD `flock(2)` lease held only by the worker,
close-on-exec descriptor hygiene, persistent local lock-root validation,
reader/write sandbox selection, nonqueued promotion, idle-only
promotion/demotion, effective-write start/resume/fork and writer-recover
acquisition, and identity-verified
stale process-group cleanup before starting a new app-server.

Verification: multiprocess tests proving multiple readers, one app-server writer
lane per worktree, no replacement app-server before crash cleanup, deterministic
writer conflicts, start-failure release, foreign/nonlocal lock-root refusal,
PID/PGID reuse refusal, pause/close/unknown release, promotion races, and
separate locks for distinct canonical workspaces.

### TASK-008: Pending Requests and Generation Approvals

Status: `PLANNED`

Implement normalized approval, user-input, and MCP-elicitation requests;
generation-qualified request IDs; `pending` and schema-validated `respond`;
reader auto-decline; explicit writer decisions; and generation-scoped approval
expiry.

Verification: fake server-request coverage for each kind and decision, stale
generation responses, unknown request kinds, malformed responses, indefinite
waiting, interrupt during waiting, and writer lease retention.

### TASK-009: Pause, Close, Crash Reconciliation, and Fork

Status: `PLANNED`

Implement idle pause/resume, interrupting pause/close, immutable close,
four-verdict process/group recovery, boot-session identity, cleanup continuation,
stable thread-history reconciliation, `outcome_unknown`, lease-free transient
read-only reconcile-to-paused, no-force fail-closed recovery, no-replay
enforcement, last-confirmed-turn fork, explicit `fork --fresh`, inherited run
instructions, and start-failed restrictions.

Verification: process-kill tests at every start/turn/terminal boundary, missing
Codex/Gomchi history cases, outcome-unknown command fencing and lease release,
terminal-evidence recovery to paused, transient `thread/read`-only reconcile,
Absent/Mismatch/Match/Unverifiable identity cases, group survivors, PID reuse,
boot change, wedged worker cleanup, recovery crash continuation, fresh read-only
escape, closed-source fork, unknown-source fork, write-fork conflict, and
cross-target rejection.

Epic acceptance: failures cannot create two Gomchi writer app-servers, signal an
unverified process, resume an ambiguously owned thread, silently replay a turn,
cross account boundaries, or falsely claim known outcomes.

## EPIC-004: Operator and Audit Interfaces

Status: `PLANNED`

Goal: Complete the master-facing operational surface and make every durable run
independently inspectable.

### TASK-010: Status, Events, Results, and Change Observation

Status: `PLANNED`

Implement workspace-scoped run listing, status projections, normalized and raw
redacted event queries/following, stable cursors, final result envelopes, effort
updates, and best-effort pre/post workspace observations with explicitly
unverified attribution.

Verification: cursor replay/follow tests, observer disconnect, concurrent reader
and writer observations, external-edit contamination, missing usage, and every
lifecycle projection.

### TASK-011: Verify, Export, and Confirmed Delete

Status: `PLANNED`

Implement full ledger verification, directory bundle export, closed/start-failed
deletion with mandatory confirmation, refusal of every pre-existing export
path, and the rule that Codex threads are never deleted or auto-imported.

Verification: clean/corrupt ledger cases, active-run delete refusal, missing
Codex history export, output collision, deletion scope, and orphan thread
non-import behavior.

### TASK-012: Agent Governance and Process Cleanup

Status: `PLANNED`

Implement the immutable Gomchi developer-instruction prefix, subordinate run
instructions, `.gomchi` reservation, access-aware mutation policy, explicit Git
and background-process rules, managed-run context fencing, master-only
independent-run control, bounded app-server process-group shutdown, and cleanup
audit records.

Verification: prompt-composition snapshots and conflicting run-instruction tests
are separate from sandbox-policy enforcement tests; also cover self-read-only
and attempted cross-run CLI control, marker-removal limitation reporting,
detached-worker signal/stdout behavior, five-second graceful/forced cleanup, and
escaped-process limitation reporting.

Epic acceptance: every public command and audit workflow in `specs.md` is
available against the deterministic fake environment.

## EPIC-005: Conformance and Personal Alpha Release

Status: `PLANNED`

Goal: Establish release evidence for the supported Apple Silicon macOS and two
real Codex targets.

### TASK-013: Deterministic Protocol Conformance Suite

Status: `PLANNED`

Extend TASK-006's fake app-server core into a controllable conformance executable
and fixtures covering the full required method/field manifest, schema
compatibility, unknown additive data, server requests, terminal history,
native-subagent opaque/event passthrough, and every documented error mapping.

Verification: the complete unit/integration suite passes without network,
credentials, timing-sensitive sleeps, or real Codex quota.

### TASK-014: Crash, Concurrency, and Security E2E

Status: `PLANNED`

Run native macOS process tests for simultaneous workers, close-on-exec lock
ownership and crash handoff, socket permissions, stale process cleanup, caller
termination, ledger crash recovery, dirty workspace preservation, and
fail-closed correlation.

Verification: run a documented fixed number of iterations with recorded random
seeds and schedules; a pass requires every prescribed seed to succeed. Retain
bounded failure evidence without secrets or unbounded logs.

### TASK-015: Two-Target Live Smoke and Alpha Acceptance

Status: `PLANNED`

Run opt-in live smoke tests against the prepared `codex` and `codex-hsy`
targets, both at app-server 0.147.0 or a separately recorded compatible version.
Cover target-home isolation, read session, writer conflict, multi-turn resume,
effort change, approval round trip, pause/resume, fork, audit verification, and
export. Do not persist credentials or secret-bearing raw output in fixtures.

Verification: both target reports pass on Apple Silicon macOS; every SOT
contract has deterministic evidence; all blocking independent review findings
are resolved.

Epic acceptance: mark the personal alpha ready only after TASK-015 and the full
Task completion gate are satisfied.
