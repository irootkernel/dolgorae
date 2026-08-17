# Dolgorae Roadmap

Status: Ordered implementation roadmap. Historical `TASK-000` and stabilization
Tasks `TASK-000-A`, `TASK-000-B`, and `TASK-000-C` are complete. External-runtime
contract stabilization `TASK-000-D` is complete after its fourth consistency pass;
no production implementation Task is active.

This document owns execution order and delivery status. Product requirements
remain authoritative in [specs.md](specs.md); this roadmap must not redefine
them.

## Status Model

Allowed Epic and Task states are `PLANNED`, `ACTIVE`, `IN_REVIEW`, `BLOCKED`,
`COMPLETE`, and `SUPERSEDED`.

- At most one Epic may be `ACTIVE`.
- Across the entire roadmap, at most one Task may be `ACTIVE`.
- `IN_REVIEW` means implementation work is finished but the completion gate is
  still collecting independent review or empirical evidence. It occupies the
  sequential Task slot like `ACTIVE`.
- `SUPERSEDED` preserves historical work whose governing contract has been
  replaced before release. It is not evidence that the replaced contract is
  currently accepted; the replacing Task owns closure.
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

Reconcile the then-current writer-lock contract, stale-generation cleanup, runtime discovery,
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

Status: `SUPERSEDED` by EPIC-000-D

Goal: Replace the pre-implementation per-run server and target vocabulary with
profile-scoped singleton, project-local state, and cross-profile lazy-writer
contracts before production TASK-001 begins. This Epic changes documentation
and checked protocol artifacts only.

### TASK-000-C1: Named Profiles and App-Server Singleton

Status: `COMPLETE`

Define generic named profiles, one compatible app-server singleton per canonical
`CODEX_HOME`, exclusive per-run connections, profile-wide lifecycle and
membership, and the corresponding public/machine terminology.

Verification: profile terminology scan excluding immutable review history,
official/installed app-server capability comparison, protocol JSON/schema
checks, requirement ownership, Markdown links, offline regression gate, and
Git whitespace/scope checks.

### TASK-000-C2: Project-Local Configuration and Runtime Layout

Status: `COMPLETE`

Move portable and machine-local configuration, run/runtime state, locks, and
evidence beneath `.dolgorae/`, retaining only the documented singleton and short
Unix-socket exceptions.

### TASK-000-C3: Lazy Cross-Profile Writer Handoff

Status: `COMPLETE`

Replace startup-selected access with explicit lazy writer acquisition, release,
and user-confirmed idle takeover shared by all profiles in one canonical
workspace. EPIC-000-D supersedes its process-held lease mechanism with durable
writer authority while preserving the product goal.

Epic acceptance: every C1-C3 contract is internally consistent, checked schemas
match the SOT, NOTE-004 records bounded evidence and goal commits, no production
or probe code changes, and TASK-001 remains planned.

## EPIC-000-D: External Runtime Contract Stabilization

Status: `COMPLETE`

Goal: Rebaseline the unreleased v1 contract so interactive clients and workflow
orchestrators share launch-contract coordination while selecting either the
shared-read-only server or a Run-owned dedicated lane generation, with common
controller authorization, workspace writer authority, normalized interactions,
and client-safe replay.

### TASK-000-D: Controller, Projection, and Integration Contract

Status: `COMPLETE`

Treat the repository-root `prompt.md` as user-owned, ignored, non-normative
input; preserve only its digest and disposition in tracked review records. Record its
[requirement disposition](external-runtime-disposition.md) plus the
[singleton-correction disposition](singleton-correction-disposition.md) and
[follow-up disposition](reviews/task-000-d-follow-up-disposition.md), plus the
[second follow-up disposition](reviews/task-000-d-second-follow-up-disposition.md).
Reconcile it with named profiles, the profile-scoped singleton, lazy writer
access, and existing recovery rules. Add controller capability and operator
reset, open same-user observation, explicit same-controller handoff, runtime and
profile capability discovery, purpose/parent metadata, normalized interactions,
and append-time minimal/operational event projections. Rebaseline every checked
v1 schema and error shape before production implementation. This includes a
Dolgorae-owned direct-executable launch contract, direct WebSocket-over-Unix
transport, manager-owned shared/dedicated lane-server epochs, complete profile membership, durable
writer authority, worker-side controller revalidation, a separate operator
capability, discriminated interactions, and separate durable event-record and
delivery schemas. The follow-up additionally owns one global lock hierarchy,
threadless first-write staging, fail-closed background-execution policy,
restorable profile snapshots and operator server-key migration, membership
repair, identity-complete shutdown, deterministic launch cwd/config drift,
projection enforcement, final-response selection, meaningful file-change
approval, secret receipts, and multi-client routing evidence.
The second follow-up originally proposed exclusive Writer Capsules. ADR-019
supersedes that transient topology while retaining complete process-census
cleanup authority, PREPARE/APPLY/COMMIT operation tokens, symbolic
launch cwd and deterministic locale/PATH, independent effective-policy/writer
state, bounded artifacts, profile diagnostics, conditional event identity, and
publication of Runs only after a ready server epoch.

The pinned topology campaign supersedes the transient Writer Capsule portion
with ADR-019. TASK-000-D now owns immutable `control_mode` and `execution_lane`,
sticky Dedicated Execution Lanes, per-workspace concurrent writers, separated
server/workload/writer state, requested/achieved assurance, fixed thread
residency, profile-wide lane enumeration, and a fresh lineage-linked dedicated
successor when a shared Run needs write. Exact 0.147.0 gates A/C/E and the
closed-generation history barrier passed; cross-server migration and
background-terminal authority failed, but Dolgorae's exact process-census
cleanup campaign passed. The retained native-subagent conclusion is invalid:
its parser reported no collaboration item while bounded wire evidence contains
`subAgentActivity` and `collabAgentToolCall`. Codex-native child threads are
distinct from independent Dolgorae Runs and workers. The corrected exact-version
campaign recognizes both shapes, proves the enabled parent/child lifecycle and
restart history, and permits `supported`; active or unknown native state still
makes quiescence-requiring transitions fail closed. The disabled diagnostic
produced a child and cannot advertise `unavailable`.

Verification: parse and meta-validate all protocol JSON; resolve every cross-file
reference; prove command/data/error enum equality and typed positive/negative
instances; validate credential carrier bounds and secret exclusions; exercise
controller mismatch/reset, observer visibility, handoff expiry/races,
interaction reconnect/staleness, cursor replay/profile filtering, direct
WebSocket fragmentation/ping/close/multi-client behavior, singleton crash and
restart reconciliation, writer crash boundaries, policy transitions, and
reasoning non-retention through deterministic and pinned live fixtures. Run the full offline gate,
requirement reverse index, Markdown links, obsolete-contract scans, and Git
whitespace checks. At the third-follow-up historical checkpoint TASK-000-D was
complete, but the fourth follow-up reopened the gate for corrected contracts,
fresh live campaigns, a reproducible evidence package, and independent review.

Epic acceptance: the input disposition, SOT, architecture, ADRs, checked
schemas, roadmap ownership, and verification fixtures agree; an independent
read-only review has no unresolved blocking finding; an implementation note
links task-scoped commits; no production source exists; TASK-001 remains
planned until this gate is complete. This architecture gate is complete;
TASK-001 remains `PLANNED` as the next implementation task.

## EPIC-001: Foundation and Durable State

Status: `PLANNED`

Goal: Establish the Rust program, stable machine contract, workspace policy,
and audit-first run storage on which every process operation depends.

### TASK-001: Rust CLI and Core Contract

Status: `PLANNED`

Implement the Rust 2024 binary skeleton, command parser, UUIDv7 identities,
stable JSON success/error envelopes, exit-status mapping, typed lifecycle,
control-mode, purpose, execution-lane, assurance, and access enums,
external-runtime commands and controller/capability types,
help/version/unknown-command output, `--human` rendering boundary,
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
creation. Establish project-local `.dolgorae/runtime/locks/`, mandatory
local-APFS workspace checks with no override, and strict shared/local YAML
config/profile schemas.

Verification: tests for subdirectory discovery, symlink normalization, Git
worktrees, dirty/untracked preservation, non-Git opt-in, repeated initialization,
and refusal of uninitialized start; libc realpath case aliases and the
device/inode-guarded `/System/Volumes/Data` firmlink normalization versus
case-sensitive distinct paths; non-APFS/nonlocal refusal; missing/replaced local
lock refusal; nested/Git-contained non-Git and mode-changing re-init refusal.

### TASK-003-A: Manifest, JCS, and Ledger Record Schema

Status: `PLANNED`

Implement run directory creation, fixed manifest semantics including controller
digest/generation, immutable control mode/lane, requested/achieved assurance,
purpose/parent metadata and capability snapshot, the in-repo RFC 8785
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
`state.json` with its fsynced watermark, append-time client-event normalization,
reasoning-content suppression/non-retention, and observer publication.

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
discovery, direct WebSocket connection recovery, version-frozen control v1, and the one
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

Implement project-local `local.yaml` profile CRUD, direct executable, normalized
global argv, absolute `CODEX_HOME`, and explicit environment-map validation;
deterministic environment preparation; schema generation into temporary storage; required
stable-subset comparison, app-server handshake, `codexHome` matching,
`model/list`, tested/unverified verdicts, restorable immutable profile snapshots,
closed configuration classification, symbolic launch-cwd policy and derived cwd, singleton keys,
epochs, operator server-key migration, append-only membership repair,
identity-complete shutdown, profile log drainer, profile diagnostic journal,
symbolic launch-cwd policy, explicit PATH/LANG/LC_ALL, PREPARE/APPLY/COMMIT
server operations, full-key short-socket collision checks, and server lifecycle commands.
The required-subset manifest is checked input, not a TASK-013 invention.

Verification: fake executable matrices for missing commands, rejected wrapper argv,
profile-name collision, home mismatch, incompatible same-home singleton,
unsupported/older/newer versions, missing schema fields,
additive fields, login failure, and successful 0.147.0 compatibility.
Also cover `$ref` resolution, requiredness/type/enum changes, pagination,
early-ID behavioral rejection, absent-thread errors, version-drift refusal and
operator migration/rollback. Probe configuration mutations and classify each
input as static, migratable, runtime-mutable, or ignored. Implement binary-level runtime capabilities,
profile-specific interaction/capability snapshots, and pre-allocation rejection
of missing required capabilities. Bare doctor remains offline; launch behavior
is tested only by explicit `--launch-probe`. TASK-005 owns the selected 0.147.0
native feature policy: reject raw global `multi_agent` arguments, inject exactly
one profile-owned `--enable multi_agent` pair, treat absence as enabled, reject
explicit public disable with `NATIVE_SUBAGENT_DISABLE_UNAVAILABLE`, retain the
disable launch only for diagnostic probes, advertise enabled-but-incomplete
observation as `unverified`, and
make active or unverified native state block every quiescence-dependent
transition. A policy change requires a new server key and operator-authorized
profile migration with no silent hot reload. Dedicated-lane campaigns prove
identical-contract same-home shared/dedicated coexistence, globally unique server
epochs, fixed logical-lane residency, same-lane resume only after exact prior-
generation absence, and exact cleanup without unrelated signals. Cross-server
same-thread resume is a negative test and MUST remain rejected. A future native
terminal API is optional hybrid evidence.

### TASK-006: Thread and Turn Lifecycle

Status: `PLANNED`

Implement private direct WebSocket-over-Unix connection ownership, HTTP Upgrade,
masking, fragmentation, ping/pong, close, frame/message bounds,
initialize/initialized, thread
start/resume/fork, model fixation, effort validation, turn start/interrupt,
one-active-turn serialization, local image input, send/submit/wait behavior,
required caller idempotency, generic waiting-interaction states, usage capture,
and bounded inline/artifact root-turn final-response extraction during terminal readback using TASK-004's shared fake
app-server fixture; TASK-006 does not create another fake core.

Verification: deterministic fake app-server scenarios for every request and
notification ordering, request/thread/turn/generation mismatch, duplicate
terminal messages, malformed output, send timeout, caller death, same/different
idempotency payloads, fixed-model enforcement, advertised and unadvertised
effort, forkable-status matrix, and provisional-thread absence/unreadability.
Also test PREPARE-before-effect idempotency, phase-marked/phase-null/commentary-only messages, foreign thread
events, two simultaneous connections/turns, disconnect isolation, approvals,
user input, native descendants, and profile-global notifications.

Epic acceptance: an initialized workspace can run a multi-turn read-only session
through a fake app-server while preserving one thread and reconnecting CLI
callers.

### TASK-006-A: External Controller and Observer Boundary

Status: `PLANNED`

Implement strict controller credential creation and fd/file ingestion,
domain-separated digest storage, constant-time mutation authorization before
effects, controller/purpose/parent run metadata, open same-uid client-safe
observation, worker-side `SCM_RIGHTS` credential revalidation under the mutation
lock, fd/stdin-only interaction responses, and explicit operator controller reset. Profile-wide interrupting server
control, server-key migration, and membership repair require the distinct
operator capability and complete membership.

Verification: valid fd/file credentials; create-exclusive mode 0600 output;
wrong owner/mode, symlink, oversize, malformed base64url, argv/environment leak,
zeroization and mismatch cases; every mutating command versus every observer;
same credential across runs; reset for idle reader/writer, paused and
outcome-unknown runs; rejection for active, pending, handoff and unverifiable
states; failure before binding change; controller generation and audit proof.

## EPIC-003: Access, Interaction, and Recovery Safety

Status: `PLANNED`

Goal: Enforce Dolgorae's one-durable-writer-authority-per-worktree scope,
master-controlled interaction, and conservative failure semantics.

### TASK-007: Durable Writer Authority and Cross-Profile Handoff

Status: `PLANNED`

Implement the per-worktree durable writer authority state machine, with BSD
`flock(2)` used only as a short transaction serializer, close-on-exec descriptor
hygiene, project-local permanent lock validation,
read-default sandbox selection, explicit `--write`/acquire/release, idle-only
cross-profile same-controller prepare/commit/cancel handoff, and fail-closed
background-execution uncertainty before activating or releasing authority.
Persist `effective_policy` and `writer_authority` independently and implement
revision-bound PREPARE/APPLY/COMMIT/cancel transitions without external waits
under file locks.
Acquire/release retains the same worker, byte-1 owner, logical lane, and thread.
Policy changes occur within the current dedicated generation or, after exact
absence and a durable-history barrier, within its same-lane successor
generation. A shared-readonly Run is never promoted: it creates a lineage-linked
dedicated successor Run. Startup locks use the pinned timed-record-lock layout
and offsets.

Verification: multiprocess tests proving multiple readers, one writer authority
per worktree, no shared-singleton restart on policy change, deterministic writer
conflicts, crash boundaries for `none→reserved→active` and
`active→releasing→none` including every proof/failure landing, missing/replaced
local lock refusal, PID reuse refusal, safe pause/close release and unknown-state
blocking, fixed thread residency, dedicated-successor creation, source-lane
retirement during handoff, destination failure leaving `none`, acquire races,
idle handoff,
active/waiting/cross-controller refusal, expiry, stale writer/run generations,
cancel/commit races and requester-failure-with-no-writer, and
separate locks for distinct canonical workspaces, permanent writer/startup
pathnames, held-fd/path and historical-inode splits, linked-worktree Git writable
roots, access-policy mappings, explicit unsupported-transition refusal, a fresh
lineage-linked dedicated successor Run for shared-readonly to writer, and verified incumbent retirement
before write-to-read authority release.
Successor tests must prove fixed workspace/profile/control mode, a new
same-principal destination Controller, non-decreasing assurance, capability
union and revalidation, supported model/effort overrides, recomposed instruction
prefixes, and non-inheritance of source Controller instructions or hidden history.
Also cover `F_SETLKWTIMEOUT`, spawn-image versus final-image identity, and
fail-closed byte-1 control timeout without any activity-derived signal.
Add deterministic interleavings for the normative lock matrix and every
threadless first-write crash boundary; `acquire-write` on a threadless run is a
state conflict. No task claims OS ownership of shared App Server descendants.

### TASK-008: Pending Requests and Approvals

Status: `PLANNED`

Implement discriminated normalized command/file approval and pinned experimental
user-input interactions; generation- and server-epoch-qualified
request IDs; fsync-before-delivery `pending`, schema-validated and idempotent
`respond`, first-valid-response wins, observer reconnect, reader auto-decline;
explicit one-shot writer decisions without public session-scoped approval. Recognize the
permission and MCP elicitation methods and reply method-not-found without
creating pending lifecycle state.
Reader auto-decline is the configured `approvalPolicy:"never"`, not a duplicate
interceptor.

Correlate file approvals from the initial revision-0 file-change item and every
patch update with exact add/delete/update snapshots, using 64-KiB aggregate inline diffs
or digest-bound 0600 artifacts up to 8 MiB. Secret-bearing user-input resolution
uses first-success plus an opaque receipt and stores no content digest/HMAC.

Verification: fake server-request coverage for all supported kinds and every
decision, duplicate/same-key/different-key responses, controller mismatch,
stale generation responses, inline/artifact/stale change snapshots,
secret/non-secret retry semantics, unknown request kinds, malformed responses, indefinite
waiting, interrupt during waiting, writer-authority retention, no replay after
restart, exact response schemas, reader auto-decline, live-observed command/file
request mappings, and method-not-found behavior for all recognized unsupported
methods.

### TASK-009-A: Pause, Close, and Lifecycle Shutdown

Status: `PLANNED`

Implement idle pause/resume, interrupting pause/close, immutable close,
generation-level access instruction replacement, verified socket cleanup,
start-failed bootstrap authority, terminal seals, and final-state restrictions.
Worker cleanup covers its worker, connection, and an owned Dedicated Run Server's
recorded command descendants; the shared singleton is excluded.

Verification: idle/running/waiting pause/close matrices, interrupt terminal
deadline and outcome-unknown landing, control-v1 pause/close/recover under
binary skew, stale socket ownership, start failure before/after bound, seal crash
points, acquire/release authority transitions, and no authority release before
protocol-supported background absence; unverified execution remains blocked.

### TASK-009-B: Process Identity and Group Recovery

Status: `PLANNED`

Implement four-verdict worker and Dedicated Run Server process identity,
boot-session proof, complete
provisional identity, kqueue continuity, persisted member snapshots,
`proc_listpgrppids` plus all-PID BSD parent/session census, observation across
reparent/group/session changes, fail-closed worker attachment, permanent
lock-inode rules, and no-force cleanup continuation. Treat
`CommandExecution.processId` as an opaque correlation hint.

Verification: every identity read failure; ESRCH/live/zombie/reaped/recycled PID;
leader-first and leaderless persisted-member cleanup; new group members;
immediate command-notification census; 100-millisecond polling; TERM/5-second/
KILL and ten-second total deadlines; five complete empty samples; deliberate
setsid/reparent detection; incomplete census; inode unlink/recreate; reboot
proof; revalidated live worker control timeout returning `RUN_BUSY` with no
signal; no unrelated signal under injected PID/PGID reuse.

### TASK-009-C: History Reconciliation, Outcome Unknown, and Fork

Status: `PLANNED`

Implement persisted thread-history reconciliation across a proved-absent old
epoch and compatible new epoch, `outcome_unknown`,
non-authoritative read-only reconcile-to-paused, no-replay enforcement, manifest-defined
forkable boundaries, explicit `fork --fresh`, and provenance-preserving inherited
run instructions.

Verification: every durability and app-server boundary; absent/unreadable/
accepted first turn; completed/clean-interrupted/crash-interrupted/failed fork;
source identity unavailable; fresh escape without source thread/ledger mutation;
transient early-ID timeout/malformed/oversize; proof that no unknown input is
replayed.

Epic acceptance: failures cannot create two Dolgorae writer proxies, signal an
unverified process, resume an ambiguously owned thread, silently replay a turn,
cross account boundaries, or falsely claim known outcomes.

## EPIC-004: Operator and Audit Interfaces

Status: `PLANNED`

Goal: Complete the master-facing operational surface and make every durable run
independently inspectable.

### TASK-010: Status, Events, Results, and Change Observation

Status: `PLANNED`

Implement workspace-scoped run listing, same-uid observer status and strict
interaction summaries, controller-authorized full interaction retrieval,
minimal/operational client-safe event queries/following, a separate profile
diagnostic query/event cursor, stable cursors, bounded artifact show/read/export,
final-response inline/artifact/unavailable envelopes, effort
updates, and best-effort pre/post workspace observations with explicitly
unverified attribution. Implement the checked command-tagged machine-output
schema, retryability/details matrix, 30-second stream heartbeat, exclusive
cursor, closed audit-record envelope/kind enum, measured workspace changes,
4,096-path bound, invalid-UTF8 path representation, and a hard prohibition on
reasoning/raw-wire projection.

Verification: cursor replay/follow tests, observer disconnect, concurrent reader
and writer observations, external-edit contamination, missing usage, and every
lifecycle projection; midstream error/end envelopes, filtered cursor gaps,
replay/live deduplication,
minimal-versus-operational fields, reasoning suppression/non-retention, path
truncation, Git/non-Git algorithms, and every command `data` variant. Test
1-MiB chunks, 8/32/256-MiB quotas, digest/range failures, conditional thread/turn
identity, observer/controller interaction and artifact denial matrices, profile
redaction/authorization, and pre-ready failures that create no Run.

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

Implement bounded versioned direct-interactive and managed-agent instruction
prefixes, subordinate run instructions, `.dolgorae` reservation, access-aware mutation policy, explicit Git
and background-process rules, advisory managed-run context, capability-checked
independent-run control, manager-owned bounded singleton shutdown, and cleanup
audit records.

Verification: mode-specific prompt-composition snapshots, controller-kind
compatibility, observer interaction denial, capability non-disclosure, and conflicting run-instruction tests
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
native-subagent opaque/event passthrough, controller/observer matrices,
capability discovery, interaction idempotency, safe event profiles, and every
documented error mapping, including artifact, independent run-state, and
profile-event schemas, canonical upstream file-change kinds, and semantic
multi-diff aggregate bounds.

Verification: the complete unit/integration suite passes without network,
credentials, timing-sensitive sleeps, or real Codex quota. Injectable time
drives every timeout; named fault barriers cover every durability/effect edge;
control v1 and all machine-output/error variants are included.

### TASK-014: Crash, Concurrency, and Security E2E

Status: `PLANNED`

Run native macOS process tests for simultaneous workers, close-on-exec lock
ownership and crash handoff, socket permissions, stale process cleanup, caller
termination, ledger crash recovery, dirty workspace preservation, and
fail-closed correlation. Include operation-token crash points around every
PREPARE/APPLY/COMMIT boundary and concurrent same/different-controller handoff,
operator reset, observer disconnect, and proof that failures never expose a
capability or create two writers.

Verification: drive each named fault barrier and injected identity/boot/
enumeration schedule deterministically, then run 100 stress iterations as
supplemental evidence. A pass requires every barrier case and iteration to
succeed; random seeds alone are not scheduling proof. Retain bounded failure
evidence without secrets or unbounded logs.

### TASK-015: Two-Profile Live Smoke and Alpha Acceptance

Status: `PLANNED`

Run opt-in live smoke tests against prepared primary and secondary profiles
using the checked 0.147.0 compatibility baseline (or a separately migrated
compatible version). Profile
names and local wrapper paths are runner inputs and are not normative fixtures.
Cover profile-home isolation, singleton sharing within a profile, separation
between profiles, read session, writer conflict, multi-turn resume,
effort change, two-controller isolation, same-controller handoff, observer
replay, approval round trip, pause/resume, fork, audit verification, and
export, including command/file approval plus pending-interaction restart. Do not
persist credentials or secret-bearing raw output in fixtures.
The campaign must prove same-home shared Profile Server plus multiple Dedicated
Run Server coexistence, globally unique epochs, fixed thread residency,
dedicated-lane process census and exact cleanup, no unrelated signalling, policy
transitions, profile diagnostic minimal/operational views, and artifact
integrity/range behavior. If any required dedicated-lane behavior fails, TASK-015 and
release remain blocked; absence of a future native terminal API is not itself a
blocker.

Verification: both profile reports pass on Apple Silicon macOS; required-subset
and early-ID gates match each executable; every SOT contract has deterministic
evidence; all blocking independent review findings are resolved. A future
version is accepted for an existing profile only through the operator-authorized
`profile server migrate` transaction; run-local resume/recover/reconcile
commands cannot approve process-static drift.

Epic acceptance: mark the personal alpha ready only after TASK-015 and the full
Task completion gate are satisfied. TASK-015 alone owns the transition of the
checked manifest's `production_runtime_eligible` field from false to true and
must leave it false on any missing, failed, unverified, or stale production
campaign. TASK-000-D owns only `architecture_contract_eligible`.
