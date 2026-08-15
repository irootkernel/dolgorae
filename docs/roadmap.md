# Dolgorae Roadmap

Status: Ordered implementation roadmap. Historical `TASK-000` and Round-4
stabilization `TASK-000-A` remain complete. Round-5 stabilization
`TASK-000-B` is also complete; no production implementation Task is active.

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
- A hyphen-suffixed stabilization Task such as `TASK-000-A` may be inserted
  between production Tasks to resolve later review findings. It may create only
  SOT, checked schemas, toolchain policy, test/fake/probe fixtures, evidence, dispositions,
  and review artifacts, never production code. It uses the ordinary completion
  gate and does not rewrite the historical status of an earlier completed Task.
- An Epic becomes complete only when all of its Tasks and Epic-level acceptance
  checks are complete.

## Task Completion Gate

A Task is `COMPLETE` only when all of the following are true:

1. Its required behavior is implemented within scope.
2. Its designated deterministic verification passes.
3. Affected SOT documents are synchronized.
4. An independent read-only review has completed. A Task touching concurrency,
   recovery, process identity, locking, audit bytes, or external protocol
   semantics additionally requires a stated adversarial attack budget and
   empirical verification of every normative OS/external behavior it introduces.
5. Every blocking finding is fixed in SOT text or a checked artifact, or
   rejected with evidence, and independently confirmed as resolved. A statement
   present only in disposition prose is not implemented.
6. Non-blocking findings are recorded in
   [deferred-feedback.md](deferred-feedback.md).
7. An [implementation note](implementation-notes.md) records the evidence, and
   [review history](reviews/README.md) indexes the preserved input,
   disposition, and closure artifacts.
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
probes against the pinned Codex 0.147.0 profile for crash-history durability,
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

## EPIC-000-A: Round-4 Contract Stabilization

Status: `COMPLETE`

Goal: Close the Codex wire, machine-output, workspace-identity, durability,
process-recovery, and verification-plan gaps found after TASK-000 without
starting production implementation.

### TASK-000-A: Round-4 Contract and Probe Closure

Status: `COMPLETE`

Preserve the immutable Round-4 input; resolve every C/H/M/L finding in a checked
owner disposition; add machine-output and Codex required-subset schemas; pin the
toolchain and Darwin dependency boundary; harden shared probes; and update the
downstream roadmap so every normative mechanism has an owning deterministic
fixture.

Verification: all checked schemas validate; durable probes pass against Codex
0.147.0 with Python/clang/SDK recorded; macOS tests cover kqueue,
`proc_listpgrppids` units, `MNT_LOCAL`, lock descriptor/inode behavior, and
spawn attributes; an independent reviewer spends at least 40 stated adversarial
scenarios across wire, audit, identity, locking, cleanup, and traceability and
returns no unresolved findings.

Epic acceptance: NOTE-002 links the immutable-input and implementation commits,
every Round-4 row has a disposition and evidence, no production source exists,
and TASK-001 remains planned until this Task is complete.

## EPIC-000-B: Round-5 End-to-End Stabilization

Status: `COMPLETE`

Goal: Reconcile Round 5's end-to-end lifecycle, OS-substrate, checked-artifact,
machine-contract, and closure-integrity findings without production code.

### TASK-000-B: Round-5 Contract, Traceability, and Probe Closure

Status: `COMPLETE`

Preserve the immutable Round-5 input and create one owner-disposition row per
finding. Reconcile spawn-image identity, APFS/firmlink workspace identity,
volatile sockets, absolute Git writable roots, timed locks, fail-closed worker
attachment, version-skew shutdown, interrupt expiry, fork boundaries, manifest
comparison/notifications, machine-output evolution, JSON ingest, export
snapshots, redaction digits, and access-generation semantics. Add the checked
requirement reverse index and one offline/live probe entry point. Test fixtures
and the fake app-server are lawful stabilization outputs; production Rust is
not.

Verification: the Round-5 P-1 through P-12 campaign (with byte-1 takeover
replaced by a no-signal fail-closed case and unsupported filesystems rejected),
all checked-schema/error/manifest cross-validation, requirement-index coverage,
Markdown/static checks, and an independent refutation-first review by a lane
that did not author the disposition. The review must include crash-recover-
resume, contested attachment, SIGTERM/approval-expiry, artifact-level schema
inspection, at least 40 recorded counter-searches, and target-platform empirical
checks. Live Codex observations and local performance measurements are recorded
as bounded secret-free evidence.

Epic acceptance: every Round-5 row is individually verified or rejected with
evidence, no fix exists only in disposition prose, all required live/offline
gates pass, NOTE-003 links task-scoped commits, no production source exists, and
TASK-001 remains planned. These conditions were independently confirmed at
`aacb1b2`; TASK-000-B and EPIC-000-B are complete.

## EPIC-000-C: Singleton, Local-State, and Writer Contract Stabilization

Status: `PLANNED`

Goal: Replace the pre-implementation per-run server and target vocabulary with
profile-scoped singleton, project-local state, and cross-profile lazy-writer
contracts before production TASK-001 begins. This Epic changes documentation
and checked protocol artifacts only.

### TASK-000-C1: Named Profiles and App-Server Singleton

Status: `COMPLETE`

Define generic named profiles, one compatible app-server singleton per canonical
`CODEX_HOME`, exclusive per-run proxy connections, profile-wide lifecycle and
membership, and the corresponding public/machine terminology.

Verification: profile terminology scan excluding immutable review history,
official/installed app-server capability comparison, protocol JSON/schema
checks, requirement ownership, Markdown links, offline regression gate, and
Git whitespace/scope checks.

### TASK-000-C2: Project-Local Configuration and Runtime Layout

Status: `PLANNED`

Move portable and machine-local configuration, run/runtime state, locks, and
evidence beneath `.dolgorae/`, retaining only the documented singleton and short
Unix-socket exceptions.

### TASK-000-C3: Lazy Cross-Profile Writer Handoff

Status: `PLANNED`

Replace startup-selected access and promote/demote with explicit lazy writer
acquisition, release, and user-confirmed idle takeover shared by all profiles in
one canonical workspace.

Epic acceptance: every C1-C3 contract is internally consistent, checked schemas
match the SOT, NOTE-004 records bounded evidence and goal commits, no production
or probe code changes, and TASK-001 remains planned.

## EPIC-001: Foundation and Durable State

Status: `PLANNED`

Goal: Establish the Rust program, stable machine contract, workspace policy,
and audit-first run storage on which every process operation depends.

### TASK-001: Rust CLI and Core Contract

Status: `PLANNED`

Implement the Rust 2024 binary skeleton, command parser, UUIDv7 identities,
stable JSON success/error envelopes, exit-status mapping, typed lifecycle and
access enums, help/version/unknown-command output, `--human` rendering boundary,
pinned Rust 1.97.1 toolchain,
machine-output schema validation, injectable monotonic clock, identity/boot/
enumeration providers, and named fault barriers. Establish the single safe
Darwin `libc` wrapper, duplicate-detecting `RawValue` ingest, and in-repo JCS
ownership; commit Cargo.lock without adding a dependency absent an ADR.

Verification: unit tests for serialization compatibility, unknown-field
tolerance, every exit class, and CLI argument conflicts; formatting and clippy
must pass with warnings denied on the pinned toolchain. Fake time and every
fault barrier are addressable without sleeping.

### TASK-002: Workspace Initialization and Discovery

Status: `PLANNED`

Implement Git and explicit non-Git initialization, per-worktree canonical
workspace identity, upward `.dolgorae` discovery, minimal policy files, generated
local ignore policy, dirty-worktree baseline capture, and safe permission
creation. Establish `--state-root`, `lock-root.json`, mandatory local-APFS
workspace and state-root checks with no override,
unanimous-manifest reconstruction, and strict config/profile schemas.

Verification: tests for subdirectory discovery, symlink normalization, Git
worktrees, dirty/untracked preservation, non-Git opt-in, repeated initialization,
and refusal of uninitialized start; libc realpath case aliases and the
device/inode-guarded `/System/Volumes/Data` firmlink normalization versus
case-sensitive distinct paths; non-APFS/nonlocal refusal; state-root
reconstruction/conflict; nested/Git-contained non-Git and mode/root-changing
re-init refusal.

### TASK-003-A: Manifest, JCS, and Ledger Record Schema

Status: `PLANNED`

Implement run directory creation, fixed manifest semantics, the in-repo RFC 8785
`sha256-jcs-v1` canonicalizer, duplicate rejection, lossless-number adaptation,
record-kind schema, normative redaction, marker escaping, payload representation,
and file/directory permissions.

Verification: RFC 8785 vectors; UTF-16 key order; `1.0`, `1e2`, `-0`, `0.1`,
`2^53+1`, `1e400`, and `1e21`; duplicate keys; marker/redaction transform order;
empty-token, plural, separator-digit, and trailing-digit vectors; payload caps;
permissions; arrays, non-ASCII,
and string-encoded JSON boundaries.

### TASK-003-B: Ledger Durability, Repair, and Projection

Status: `PLANNED`

Implement O_APPEND writing, bounded group commit, every write-ahead barrier,
deterministic torn-tail evidence and idempotent repair, full replay, atomic
`state.json` with its fsynced watermark, and observer publication.

Verification: crash injection before/after every fsync/effect barrier; middle
corruption versus torn tail; repeated repair; ahead/stale/missing projection;
100-millisecond publication under the injectable clock; no state head beyond a
durable ledger record.

### TASK-003-C: Lifecycle Seals and Ledger Conformance

Status: `PLANNED`

Implement bootstrap records, idempotency-intent schema, `start_failed` authority,
terminal seals, closed record-kind enum, canonical fixed-point verification, and
the checked ledger conformance fixture.

Verification: virgin/failed/closed allocation and reconstruction; reserved but
unaccepted idempotency release; seal refusal on invalid history; every record
kind and transition; mutation refusal after integrity failure and confirmed
delete escape.

Epic acceptance: a run can be allocated and reconstructed from its ledger
without starting Codex, and all persisted formats are versioned.

## EPIC-002: Worker and Codex App-Server Integration

Status: `PLANNED`

Goal: Provide reconnectable per-run process ownership and a strict stable-subset
adapter for profile-scoped Codex singleton accounts and threads.

### TASK-004: Per-Run Worker and Unix IPC

Status: `PLANNED`

Implement detached hidden worker re-execution, fixed short private socket paths,
versioned runtime discovery records, persistent local locks, fd-3
startup handoff, per-run startup serialization, stale-socket recovery, bounded
request/response IPC, ledger-backed event streaming, reconnection, worker
discovery, on-demand proxy generation startup, version-frozen control v1, and the one
shared controllable fake app-server/worker fixture used by later Tasks.

Verification: fake worker tests for concurrent starts, changed `$TMPDIR`, stale
and colliding sockets, ten-second startup timeout, CLI/worker version skew,
caller Ctrl-C and command substitution, inherited-signal reset, oversized or
malformed frames, cross-run identity rejection, slow-observer backpressure, and
worker restart.
Control fixtures require digest-skewed hello/status/shutdown during replay,
mutation rejection with `DOLGORAE_PROTOCOL_MISMATCH`, active-turn shutdown, fd-3
survival, byte-1 loser zero-side-effect behavior, and verified stale-socket unlink.

### TASK-005: Profile Registry, Singleton, and Compatibility Doctor

Status: `PLANNED`

Implement profile CRUD, argv and absolute `CODEX_HOME` validation, inherited
environment preparation, schema generation into temporary storage, required
stable-subset comparison, app-server handshake, `codexHome` matching,
`model/list`, tested/unverified verdicts, profile snapshotting, singleton keys,
epochs, membership recovery, and server lifecycle commands.
The required-subset manifest is checked input, not a TASK-013 invention.

Verification: fake executable matrices for missing commands, wrapper argv,
profile-name collision, home mismatch, incompatible same-home singleton,
unsupported/older/newer versions, missing schema fields,
additive fields, login failure, and successful 0.147.0 compatibility.
Also cover `$ref` resolution, requiredness/type/enum changes, pagination,
early-ID behavioral rejection, absent-thread errors, version-drift refusal and
explicit recover acceptance.

### TASK-006: Thread and Turn Lifecycle

Status: `PLANNED`

Implement private proxy JSONL connection ownership, initialize/initialized, thread
start/resume/fork, model fixation, effort validation, turn start/interrupt,
one-active-turn serialization, local image input, send/submit/wait behavior,
idempotency, usage capture, and terminal readback using TASK-004's shared fake
app-server fixture; TASK-006 does not create another fake core.

Verification: deterministic fake app-server scenarios for every request and
notification ordering, request/thread/turn/generation mismatch, duplicate
terminal messages, malformed output, send timeout, caller death, same/different
idempotency payloads, fixed-model enforcement, advertised and unadvertised
effort, forkable-status matrix, and provisional-thread absence/unreadability.

Epic acceptance: an initialized workspace can run a multi-turn read-only session
through a fake app-server while preserving one thread and reconnecting CLI
callers.

## EPIC-003: Access, Interaction, and Recovery Safety

Status: `PLANNED`

Goal: Enforce Dolgorae's one-writer-app-server-per-worktree scope,
master-controlled interaction, and conservative failure semantics.

### TASK-007: Reader/Writer Lease and Access Transitions

Status: `PLANNED`

Implement the per-worktree BSD `flock(2)` lease held only by the worker,
close-on-exec descriptor hygiene, persistent local lock-root validation,
reader/write sandbox selection, nonqueued promotion, idle-only
promotion/demotion, effective-write start/resume/fork and writer-recover
acquisition, and identity-verified
stale process-group cleanup before starting a new app-server.
Promotion/demotion retains the same worker and byte-1 owner while replacing
only its proxy generation; startup locks use the pinned timed-record-lock
layout and offsets.

Verification: multiprocess tests proving multiple readers, one writer proxy
lane per worktree, no replacement app-server before crash cleanup, deterministic
writer conflicts, start-failure release, foreign/nonlocal lock-root refusal,
PID/PGID reuse refusal, pause/close/unknown release, promotion races, and
separate locks for distinct canonical workspaces, permanent writer/startup
pathnames, held-fd/path and historical-inode splits, linked-worktree Git writable
roots, access-policy mappings, and generation replacement on promote/demote.
Also cover `F_SETLKWTIMEOUT`, spawn-image versus final-image identity, and
fail-closed byte-1 control timeout without any activity-derived signal.

### TASK-008: Pending Requests and Generation Approvals

Status: `PLANNED`

Implement normalized command/file approval requests; generation-qualified
request IDs; `pending` and schema-validated `respond`; reader auto-decline;
explicit writer decisions; and generation-scoped approval expiry. Recognize the
three pinned unsupported request methods and reply method-not-found without
creating pending lifecycle state.
Reader auto-decline is the configured `approvalPolicy:"never"`, not a duplicate
interceptor.

Verification: fake server-request coverage for both supported kinds and every decision, stale
generation responses, unknown request kinds, malformed responses, indefinite
waiting, interrupt during waiting, writer lease retention, no replay after
restart, exact response schemas, reader auto-decline, live-observed command/file
request mappings, and method-not-found behavior for all recognized unsupported
methods.

### TASK-009-A: Pause, Close, and Lifecycle Shutdown

Status: `PLANNED`

Implement idle pause/resume, interrupting pause/close, immutable close,
generation-level access instruction replacement, verified socket cleanup,
start-failed bootstrap authority, terminal seals, and final-state restrictions.

Verification: idle/running/waiting pause/close matrices, interrupt terminal
deadline and outcome-unknown landing, control-v1 pause/close/recover under
binary skew, stale socket ownership, start failure before/after bound, seal crash
points, demote/promotion generation replacement, and no lease release before
child cleanup.

### TASK-009-B: Process Identity and Group Recovery

Status: `PLANNED`

Implement four-verdict worker/app-server identity, boot-session proof, complete
provisional identity, kqueue continuity, persisted member snapshots,
`proc_listpgrppids` enumeration, fail-closed worker attachment, permanent
lock-inode rules, and no-force cleanup continuation.

Verification: every identity read failure; ESRCH/live/zombie/reaped/recycled PID;
leader-first and leaderless persisted-member cleanup; new group members; ten-
second absence deadline; inode unlink/recreate; reboot proof; revalidated live
worker control timeout returning `RUN_BUSY` with no signal; no unrelated signal
under injected PID/PGID reuse.

### TASK-009-C: History Reconciliation, Outcome Unknown, and Fork

Status: `PLANNED`

Implement persisted thread-history reconciliation, `outcome_unknown`, lease-free
transient read-only reconcile-to-paused, no-replay enforcement, manifest-defined
forkable boundaries, explicit `fork --fresh`, and provenance-preserving inherited
run instructions.

Verification: every durability and app-server boundary; absent/unreadable/
accepted first turn; completed/clean-interrupted/crash-interrupted/failed fork;
source identity unavailable; fresh escape without source thread/ledger mutation;
transient early-ID timeout/malformed/oversize; proof that no unknown input is
replayed.

Epic acceptance: failures cannot create two Dolgorae writer app-servers, signal an
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
unverified attribution. Implement the checked command-tagged machine-output
schema, retryability/details matrix, 30-second stream heartbeat, exclusive
cursor, closed audit-record envelope/kind enum, measured workspace changes,
4,096-path bound, and invalid-UTF8 path representation.

Verification: cursor replay/follow tests, observer disconnect, concurrent reader
and writer observations, external-edit contamination, missing usage, and every
lifecycle projection; midstream error/end envelopes, raw-in-envelope behavior,
path truncation, Git/non-Git algorithms, and every command `data` variant.

### TASK-011: Verify, Export, and Confirmed Delete

Status: `PLANNED`

Implement full ledger verification, directory bundle export, closed/start-failed
deletion with mandatory confirmation, refusal of every pre-existing export
path, exact bundle inventory/permissions/disclosure, integrity-failed export and
delete escape, and the rule that Codex threads are never deleted or auto-imported.

Verification: clean/corrupt ledger cases, active-run delete refusal, missing
Codex history export, output collision, deterministic hashes, excluded runtime/
recovery artifacts, plaintext residual warning, deletion scope, and orphan
Export cases capture one fsynced ledger-head watermark, copy only that complete
prefix, and regenerate bundled projections from it.

### TASK-012: Agent Governance and Process Cleanup

Status: `PLANNED`

Implement the generation-immutable Dolgorae developer-instruction prefix, subordinate run
instructions, `.dolgorae` reservation, access-aware mutation policy, explicit Git
and background-process rules, managed-run context fencing, master-only
independent-run control, bounded app-server process-group shutdown, and cleanup
audit records.

Verification: prompt-composition snapshots and conflicting run-instruction tests
are separate from sandbox-policy enforcement tests; also cover self-read-only
and attempted cross-run CLI control, marker-removal limitation reporting,
malformed/foreign/nonexistent managed markers, non-exec MCP marker absence,
write-heavy native-subagent delegation language,
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

Extend TASK-004's shared fake app-server core into a controllable conformance executable
and fixtures covering the full required method/field manifest, schema
compatibility, unknown additive data, server requests, terminal history,
native-subagent opaque/event passthrough, and every documented error mapping.

Verification: the complete unit/integration suite passes without network,
credentials, timing-sensitive sleeps, or real Codex quota. Injectable time
drives every timeout; named fault barriers cover every durability/effect edge;
control v1 and all machine-output/error variants are included.

### TASK-014: Crash, Concurrency, and Security E2E

Status: `PLANNED`

Run native macOS process tests for simultaneous workers, close-on-exec lock
ownership and crash handoff, socket permissions, stale process cleanup, caller
termination, ledger crash recovery, dirty workspace preservation, and
fail-closed correlation.

Verification: drive each named fault barrier and injected identity/boot/
enumeration schedule deterministically, then run 100 stress iterations as
supplemental evidence. A pass requires every barrier case and iteration to
succeed; random seeds alone are not scheduling proof. Retain bounded failure
evidence without secrets or unbounded logs.

### TASK-015: Two-Profile Live Smoke and Alpha Acceptance

Status: `PLANNED`

Run opt-in live smoke tests against prepared primary and secondary profiles,
both at app-server 0.147.0 or a separately recorded compatible version. Profile
names and local wrapper paths are runner inputs and are not normative fixtures.
Cover profile-home isolation, singleton sharing within a profile, separation
between profiles, read session, writer conflict, multi-turn resume,
effort change, approval round trip, pause/resume, fork, audit verification, and
export, including command/file approval plus pending-request restart. Do not
persist credentials or secret-bearing raw output in fixtures.

Verification: both profile reports pass on Apple Silicon macOS; required-subset
and early-ID gates match each executable; every SOT contract has deterministic
evidence; all blocking independent review findings are resolved. A future
version is accepted for an existing run only through the separately recorded
`recover --accept-version-change` path.

Epic acceptance: mark the personal alpha ready only after TASK-015 and the full
Task completion gate are satisfied.
