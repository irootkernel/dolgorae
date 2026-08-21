# Dolgorae Roadmap

Status: Ordered implementation roadmap. `EPIC-000` and `TASK-000-H` are
`COMPLETE`; `EPIC-001` and `TASK-003-B` are `ACTIVE`; `TASK-001`, `TASK-002`,
and `TASK-003-A` are `COMPLETE`. `EPIC-002A` is the first user-usable product
slice. Completing it unlocks `MILESTONE-SR1`, which guarantees the one-shot
Machine CLI review path and lets Codex CLI invoke it through its ordinary shell
tool. The narrow external MCP adapter is included only when the pinned host
passes the explicit per-request identity probe; connection or stdio-process
identity is never treated as retry continuity. This milestone does not wait for
writer authority, the Dolgorae Primary control plane, Brokered Hierarchy, or
Specialist-to-Specialist collaboration.

After `MILESTONE-SR1`, the roadmap deliberately proceeds in four layers:
external Specialist hardening, the minimum supervised Gul Run gateway, the
transport-independent Dolgorae orchestration core and Brokered Hierarchy, live
Primary control-plane integration, and finally the durable Collaboration Plane. `TASK-009-E0` remains the live run-bound
transport probe and occurs only after the Brokered Hierarchy core is complete.
`TASK-000-G` remains superseded because its terminology-only boundary no longer
matches the accepted product contract. `TASK-003-B` is the active implementation
task after completion of TASK-003-A's manifest, JCS, and ledger-record contract.

This document owns execution order and delivery status. Product requirements
remain authoritative in [specs.md](specs.md); this roadmap must not redefine
them.
Document roles and the required synchronization procedure are defined by the
[documentation authority map](README.md).

## Product Milestones

| Milestone | Owning Epic | User-visible capability unlocked |
| --- | --- | --- |
| `MILESTONE-SR1` | `EPIC-002A` | Codex CLI can request one independent read-only working-tree review through `dolgorae specialist review`. The `dolgorae_review` MCP tool is additionally available only when its per-request identity carrier passes TASK-006-E0/E1. |
| `MILESTONE-ES1` | `EPIC-003A` | External AIs can keep and reuse durable Specialist Engagements across multiple tasks and restarts. |
| `MILESTONE-BH1` | `EPIC-003C` | Gul can use Dolgorae as the live Primary control plane and operate a durable Brokered Hierarchy. |
| `MILESTONE-BC1` | `EPIC-003D` | Specialists in one Brokered Hierarchy can use durable bounded lateral collaboration without Primary message relay. |
| `MILESTONE-PA1` | `EPIC-005` | The complete Personal Alpha acceptance campaign passes. |

Milestones are cumulative. An earlier milestone remains usable while later
Epics are implemented. A milestone does not waive its own Task completion gate
or any safety limitation stated in its owning Epic.

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

Task acceptance: all blocking pre-implementation findings are resolved, probe
results and limitations are durable, the Task completion gate passes, and no
production source has been introduced. Only then may `TASK-001` become active.

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

Task acceptance: NOTE-002 links the immutable-input and implementation commits,
every Round-4 row has a disposition and evidence, no production source exists,
and TASK-001 remains planned until this Task is complete.

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

Task acceptance: every Round-5 row is individually verified or rejected with
evidence, no fix exists only in disposition prose, all required live/offline
gates pass, NOTE-003 links task-scoped commits, no production source exists, and
TASK-001 remains planned. These conditions were independently confirmed at
`aacb1b2`; TASK-000-B is complete.

### TASK-000-C: Singleton, Local-State, and Writer Contract Stabilization

Status: `COMPLETE`

Replace the pre-implementation per-run server and target vocabulary with
profile-scoped singleton, project-local state, and cross-profile lazy-writer
contracts before production TASK-001 begins. This Task changes documentation
and checked protocol artifacts only. Its completed scope comprises the three
phases below.

#### Named Profiles and App-Server Singleton

Define generic named profiles, one compatible app-server singleton per canonical
`CODEX_HOME`, exclusive per-run connections, profile-wide lifecycle and
membership, and the corresponding public/machine terminology.

Verification: profile terminology scan excluding immutable review history,
official/installed app-server capability comparison, protocol JSON/schema
checks, requirement ownership, Markdown links, offline regression gate, and
Git whitespace/scope checks.

#### Project-Local Configuration and Runtime Layout

Move portable and machine-local configuration, run/runtime state, locks, and
evidence beneath `.dolgorae/`, retaining only the documented singleton and short
Unix-socket exceptions.

#### Lazy Cross-Profile Writer Handoff

Replace startup-selected access with explicit lazy writer acquisition, release,
and user-confirmed idle takeover shared by all profiles in one canonical
workspace. TASK-000-D supersedes its process-held lease mechanism with durable
writer authority while preserving the product goal.

Task acceptance: every completed phase is internally consistent, checked schemas
match the SOT, NOTE-004 records bounded evidence and goal commits, no production
or probe code changes, and TASK-001 remains planned.

Cold-validation remediation: the original NOTE-004 waiver of the ordinary
independent-review gate was invalid. The current TASK-000-C contract and
evidence received a targeted independent hardening review, the corrected
record is indexed, and the remediation is committed under the planned subject
`[TASK-000-C] Restore independent review evidence` without changing this
successful lifecycle state.

### TASK-000-D: Controller, Projection, and Integration Contract

Status: `COMPLETE`

Rebaseline the unreleased v1 contract so interactive clients and workflow
orchestrators share launch-contract coordination while selecting either the
shared-read-only server or a Run-owned dedicated lane generation, with common
controller authorization, workspace writer authority, normalized interactions,
and client-safe replay.

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

Task acceptance: the input disposition, SOT, architecture, ADRs, checked
schemas, roadmap ownership, and verification fixtures agree; an independent
read-only review has no unresolved blocking finding; an implementation note
links task-scoped commits; no production source exists; TASK-001 remains
planned until this gate is complete. This architecture gate is complete;
TASK-001 remains `PLANNED` as the next implementation task.

Cold-validation remediation: the fourth-follow-up closure checks and aggregate
evidence matrix now derive their verdicts from checked authorities and executed
probe results, NOTE-010 carries the required completion metadata and commit
links, and the corrected evidence is independently reviewed under the planned
subject `[TASK-000-D] Harden closure evidence integrity`. TASK-000-D remains in
its successful lifecycle state.

### TASK-000-E: CLI-First Brokered Subagent Contract

Status: `SUPERSEDED` by `TASK-000-F`

Make the existing machine CLI a complete, discoverable foundation for a future
trusted host adapter that lets ordinary Codex and Gul-hosted direct sessions
request Independent Dolgorae Runs as subagents without receiving Run authority.

Add the brokered hub-and-spoke use case to SPEC-012, ADR-010/020, architecture,
runtime capabilities, writer semantics, and downstream verification ownership.
The broker remains each child's `automation` Controller, uses the existing
credential and Run commands, creates a `managed_agent` Dedicated Run with opaque
parent provenance, and returns only bounded safe status/result material. Keep
MCP transport, public sockets, parent-held delegation capabilities, nested
authority, peer messaging, and transient same-thread Writer Capsules outside
this task.

Verification: parse and meta-validate every checked schema; run the SOT
consistency and link gates; validate positive capability fixtures and rejection
of a missing brokered-subagent feature; exercise a deterministic fake-broker
composition for ordinary-Codex-shaped and Gul-direct-shaped parent references;
prove Controller canaries never enter model-visible data; and prove competing
Dolgorae writers in one canonical workspace converge on one authority plus
`WRITER_BUSY` without claiming serialization of external editors.

The task was superseded before its completion gate. Its broker-held Controller,
hub-and-spoke orchestration, bounded result, and writer-conflict requirements
are carried into `TASK-000-F`; no completion or independent-review evidence is
inferred from the superseded state.

### TASK-000-F: Public Local gRPC SOT and Protocol Contract

Status: `COMPLETE`

Rebaseline the unreleased public v1 contract around two adapters sharing one
semantic service, with a supervised local gRPC gateway suitable for Gul and
without weakening Controller, writer, recovery, event, audit, or private-
transport boundaries.

Amend ADR-001, add ADR-021 and SPEC-015, revise architecture/security/lifecycle,
and publish `dolgorae.public.v1` Protobuf source plus deterministic descriptor,
error mapping, mutation policy, capabilities, timeline, artifact, and machine-
CLI derivatives. Generalize `create-successor` as
`CreateWriteContinuation`, make Run allocation idempotent, add side-effect-free
Controller verification, preserve threadless first-write rules, and retain the
TASK-000-E broker composition over either public adapter. Add a derived
implementation memo and assign production implementation to downstream tasks;
this stabilization Task MUST NOT add production Rust code.

Complete the Gul-facing v1 draft with absolute-path workspace bootstrap,
version-zero capability negotiation, full typed capability/profile/Run/writer/
interaction/lineage projections, typed Controller Interaction payloads and
limits, durable accepted Run configuration, typed event `oneof` plus aggregate
revision stamps, typed error actions including write continuation,
discoverable Controller-carrier schema policy, same-principal continuation
receipts, lossless output paths, closed capability blockers, exact StartRun
replay, stream-end behavior, and explicit protected-input/socket ownership
rules. The public v1 MUST remain unfrozen until all 30 memo criteria are
verified and every runtime-pending case is closed.

Verification: parse and meta-validate every JSON artifact; compile and lint the
Protobuf module; regenerate and byte-compare the descriptor; prove command,
error, capability, method-kind, mutation-policy, cursor, artifact, timeline,
and Controller-carrier consistency; run Markdown links, obsolete sole-CLI/
public-socket scans, semantic run-state fixtures, and `git diff --check`.
Independently review at least the server-lifecycle, UDS attack, stream-pressure,
credential TOCTOU, ambiguous-mutation, threadless-write, continuation-lineage,
timeline-redaction, artifact, error-mapping, and version-evolution boundaries.
Descriptor-backed adapter conformance MUST parse concrete Protobuf fixtures,
compare normalized Machine/gRPC projections, preserve all Interaction support
states, reject invalid profile default-model catalogs, and report production
runtime cases separately rather than hardcoding completion.

Task acceptance: all 30 acceptance rows in the local-gRPC implementation memo
resolve to active authorities and deterministic verification; one independent
read-only review reports no unresolved blocking finding; an implementation note
and review index record the completed evidence and task-scoped commit; no
production source exists; and `TASK-001` remains `PLANNED` until this gate is
complete.

Cold-validation remediation: NOTE-011 now records its completion date, exact
task commit, and independent reviewer identity; active verification ownership
names the completed carry-forward TASK-000-F rather than superseded TASK-000-E,
and the consistency lint rejects future superseded owners. The isolated result
is reviewed under the planned subject `[TASK-000-F] Close carry-forward
evidence gaps` without changing this successful lifecycle state.

TASK-000-F closure status: at that checkpoint, `TASK-000` and
`TASK-000-A` through `TASK-000-D` were complete, `TASK-000-E` was superseded by
a completed `TASK-000-F`, and the Epic-level hardening pass found no unresolved
valid finding. `TASK-000-G` subsequently reopened a terminology-only gate and was then
superseded by `TASK-000-H` when the accepted two-use-case model introduced
durable internal orchestration state and related contract changes. Earlier
completion evidence remains valid for its reviewed snapshot, but `TASK-001` is
not eligible until `TASK-000-H` closes.

Validation record (2026-08-20): task hierarchy normalization is `d83f2cf`;
TASK-000-F closure is `9b8a712`; cold-validation task remediations are
`49c4422`, `5e7cea6`, and `2730c4f`; Epic hardening commits are `b61678c`,
`ddbb5a9`, `d1c2cb9`, `eff0b3d`, and `6c65628`. Final whole-workspace review
`r_01a01b5d-e074-78a5-859a-40c5a76d232a` and exact committed correction delta
`r_01a01b74-34cf-7c66-acbf-eedbadb69899` have complete six-role coverage,
passing CI decisions, committed publication, and zero unresolved valid
findings. Four Gaori gates, the exact Codex 0.147.0 retained aggregate, Buf,
Ruff F, compileall, JSON, and whitespace checks pass. DF-001 and DF-002 retain
two informational future-environment triggers; neither changes current Epic
acceptance. The commit containing this record performs the final lifecycle
transition without activating `TASK-001` or freezing public v1.

### TASK-000-G: Agent Topology Terminology Alignment

Status: `SUPERSEDED`

This Task introduced useful distinctions among Primary Runs, Independent
Specialist Runs, and Codex-native subagents, but treated three topologies as
user-selected composition modes and prohibited durable hierarchy ownership.
The accepted two-use-case model and internal Brokered Hierarchy recovery state
change actual product and persistence contracts. `TASK-000-H` owns the active
reconciliation and review. No completion or independent-review evidence is
inferred from this superseded state.

### TASK-000-H: User-Case and Internal Contract Reconciliation

Status: `COMPLETE`

Reconcile the active SOT around exactly two user-facing use cases:
Dolgorae-Orchestrated Session and External Specialist Engagement. Preserve
independent Run identity and the checked public v1 Protobuf shape while adding
an internal Orchestration Broker and durable aggregate state for
Dolgorae-created Specialists.

The Task owns:

- canonical use-case and terminology updates;
- first-class Orchestration Session, External Specialist Engagement, Aggregate
  Bootstrap Operation, membership, write-ahead spawn or hire, Specialist task,
  and result-delivery contracts;
- Runtime Profile and Agent Configuration separation;
- explicit `control_mode`, lane, assurance, purpose, and native policy at the
  semantic boundary;
- Dedicated in-place policy transitions with `policy_epoch`, while
  `run_generation` remains Worker and connection lifetime only;
- generation-immutable instruction and Turn-scoped access-context separation;
- relocation of mutable profiles, Run, writer, lock, orchestration, evidence,
  and cache authority to Application Support;
- explicit release, verify, acquire for cross-Controller hierarchy writer
  movement;
- enforced-invariant versus agent-behavior-policy separation;
- offline shell policy and external side-effect caveats;
- deterministic interaction escalation and Controller-authorized whole-Run
  export;
- `user_input` artifact reconciliation;
- the checked `dolgorae-orchestration-state-v1.schema.json`, semantic validator,
  and fixtures;
- deterministic Orchestrated Session bootstrap behind the unchanged public root
  `StartRun`, including prepared cross-store recovery;
- explicit External Specialist Engagement open and private hire facade, with raw
  `managed_agent` Runs excluded from aggregate inference;
- the immutable Specialist Policy snapshot contract;
- the private Primary orchestration tool payload contract, including task
  cancellation; and
- the private External Specialist Facade payload contract, complete open/get/
  hire/assign/await/collect/cancel/release/close operation set, and Machine CLI
  carrier;
- the Specialist-review-first delivery sequence, `EPIC-002A`, and cumulative
  product milestone boundaries; and
- the checked one-shot Specialist Review CLI and external MCP payload contract,
  examples, validator ownership, and recursion-prevention boundary.

This Task MUST NOT change the checked `dolgorae.public.v1` Protobuf source or
descriptor. Existing Run RPCs remain the low-level Gul contract and shared
semantic execution core. External AI integrations use the checked private
External Specialist Facade rather than inferring an engagement from raw Runs. A
future additive aggregate-query surface is outside this Task. Gul v1 uses the
existing Run list, parent projection, Run events, and Controller Interaction
surface only as a safe operational view; none of those projections replaces the
internal aggregate registry as recovery authority.

Verification: parse and meta-validate every JSON artifact; validate positive and
negative orchestration-state, Specialist Policy, Primary orchestration tool,
External Specialist Facade, and one-shot Specialist Review fixtures; prove
write-ahead aggregate bootstrap and
child identity, aggregate membership, and idempotency invariants; scan active SOT
for obsolete
external-only hierarchy ownership, hidden interactive defaults, project-local
mutable authority, access-policy-driven Run generations, and immutable
current-access wording; byte-compare the public Protobuf source and descriptor
against the TASK-000-F snapshot; check local Markdown links and code fences;
and obtain an independent read-only review focused on crash recovery, authority
ownership, duplicate Specialist creation, unknown task replay, writer races,
accidental Gul wire changes, and the supplemental Specialist-review-first
roadmap input under `docs/reviews/`.

Task acceptance: active SPEC, architecture, ADR, roadmap, TODO, guide, and
checked artifacts contain no contradictory ownership or state-machine rule;
all TASK-000-H fixtures and static gates pass; public wire bytes are unchanged;
one independent review reports no unresolved blocking finding; a task-scoped
commit and implementation note are recorded; and `TASK-001` remains `PLANNED`
until this gate completes.

## EPIC-001: Foundation and Durable State

Status: `ACTIVE`

Goal: Establish the Rust program, stable machine contract, workspace policy,
and audit-first run storage on which every process operation depends.

### TASK-001: Rust CLI and Core Contract

Status: `COMPLETE`

Implement the Rust 2024 binary skeleton, command parser, UUIDv7 identities,
stable JSON success/error envelopes, exit-status mapping, typed lifecycle, aggregate, control-mode, purpose, execution-lane,
assurance, policy-epoch, and access enums,
external-runtime commands and controller/capability types, including the
`brokered_independent_subagent_runs` discovery flag,
the adapter-independent semantic-service interface, shared domain DTOs,
`dolgorae.public.v1` generated types and descriptor digest,
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

Status: `COMPLETE`

Implement Git and explicit non-Git initialization, per-worktree canonical
workspace identity, upward `.dolgorae` discovery, minimal policy files, generated
local ignore policy, dirty-worktree baseline capture, and safe permission
creation. Establish the Application Support per-workspace mutable state root, its
`runtime/locks/` and `orchestration/` authorities, mandatory local-APFS checks
with no override, and strict portable-policy and machine-local profile schemas.

Verification: tests for subdirectory discovery, symlink normalization, Git
worktrees, dirty/untracked preservation, non-Git opt-in, repeated initialization,
and refusal of uninitialized start; libc realpath case aliases and the
device/inode-guarded `/System/Volumes/Data` firmlink normalization versus
case-sensitive distinct paths; non-APFS/nonlocal refusal; missing/replaced local
lock refusal; nested/Git-contained non-Git and mode-changing re-init refusal.

### TASK-003-A: Manifest, JCS, and Ledger Record Schema

Status: `COMPLETE`

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

Status: `ACTIVE`

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

Implement per-workspace Application Support `local.yaml` profile CRUD, direct executable, normalized
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
states; failure before binding change; controller generation and audit proof;
and a broker-owned automation credential whose non-secret child identity may be
shown to a parent without granting mutation authority.

Epic acceptance: an initialized workspace can run a multi-turn read-only session
through a fake app-server while preserving one thread, reconnecting CLI callers,
and enforcing the external Controller and observer boundary.

## EPIC-002A: External Read-Only Specialist Review Preview

Status: `PLANNED`

Goal: Deliver the first user-usable Dolgorae product slice as early as the
independent Run core permits. An external Codex CLI remains the semantic control
plane and invokes exactly one independent read-only Reviewer Specialist for a
bounded working-tree review.

This Epic owns `MILESTONE-SR1`. It uses the final External Specialist
Engagement model and shared semantic Run core rather than a disposable preview
implementation, but deliberately restricts the first slice to one-shot,
read-only review.

### TASK-006-B: Read-Only Specialist Runtime Baseline

Status: `PLANNED`

Build on the EPIC-002 read-only Run path and add the minimum production contract
for an Independent Specialist Reviewer. Resolve one immutable Reviewer Agent
Configuration, compile it to a `managed_agent` Run, enforce canonical-workspace
read-only sandboxing and `networkAccess:false`, and prevent writer acquisition,
approval-based file or command mutation, nested first-class Specialist hiring,
and access to any Controller credential or peer Run address.

The Reviewer Runtime Profile MUST NOT register the external
`dolgorae_review` MCP tool, so a Reviewer cannot recursively hire another
Reviewer through the host integration. The Reviewer receives only the explicit
review objective, current working-tree context, and bounded role instructions.
It returns a final response and checked structured findings without hidden
reasoning or raw protocol projection.

Verification: prove filesystem writes are denied for tracked, untracked, Git
metadata, and linked-worktree paths; shell network is denied; the role and
Agent Configuration snapshot are immutable; the external-review MCP server is
absent from the Reviewer profile; direct CLI attempts to hire or control another
Run are denied; final findings validate against the checked review result
schema; and no Controller capability, carrier path, Worker socket, database
path, or raw App Server frame appears in prompts, output, events, or logs.

### TASK-006-C: Durable External Review Engagement Core

Status: `PLANNED`

Implement the minimal External Specialist Engagement production path required
by a one-shot review, using the existing checked External Specialist Facade and
the Application Support SQLite WAL authority. Implement explicit open, safe
get, write-ahead Reviewer hire, one read-only task assignment, bounded await,
result collection, cancellation, release, and close. Reserve engagement,
operation, member, child Run, and task identities before runtime side effects;
derive and persist idempotency receipts; and store successful results as
immutable artifacts before reporting completion.

The preview boundary is intentionally narrow: one active Reviewer member, one
active review task, `read_only` access only, no task queue, no Specialist reuse
after the one-shot adapter closes the engagement, no Brokered Hierarchy, and no
Specialist-to-Specialist collaboration. A busy or terminal Reviewer fails with
a typed result instead of preemption or implicit replacement. If Turn
acceptance or outcome is not authoritative, record `interrupted_unknown` and do
not replay automatically. Full cross-restart continuation, reusable members,
multiple Specialists, and isolated-write operation belong to `EPIC-003A`.

Verification: crash before and after each SQLite commit, child Run reservation,
Worker publication, thread creation, task acceptance, result artifact commit,
delivery receipt, release, and close; exact same-key replay; different-payload
idempotency conflict; duplicate and orphan prevention; raw `managed_agent` Run
exclusion; read-only access enforcement; successful result collection; Ctrl-C
cancellation; and fail-closed `interrupted_unknown` without task replay.

### TASK-006-D: One-Shot Specialist Review CLI and Checked Result Contract

Status: `PLANNED`

Implement the user-facing convenience operation:

```text
dolgorae specialist review \
  --workspace <absolute-or-discoverable-workspace> \
  --profile <reviewer-runtime-profile> \
  --scope working-tree \
  --format json
```

The command is an adapter composition, not a third use case. It performs open,
hire, assign, await, collect, release, and close against the shared External
Specialist Engagement service. Add the checked
`dolgorae-specialist-review-tool-v1.schema.json` request, success, finding, and
error shapes. Register `specialist.review` in the checked machine-output schema
and place the successful checked review result in the envelope's `data` field.
JSON is the canonical machine result; human output is a rendering of that
result. The command MUST report failure when the Reviewer fails,
times out, is interrupted with unknown outcome, produces invalid structured
output, or appears to mutate the workspace.

The preview supports only `working-tree` scope. Later scope expansion is
additive and must not reinterpret the preview command. The adapter owns all
aggregate and per-Run Controller carriers, external provenance, idempotency
keys, engagement cleanup, and bounded result-artifact retrieval outside the
model-visible payload.

Verification: successful no-finding and multi-finding reviews; deterministic
severity ordering; malformed Reviewer output; timeout; Ctrl-C during startup and
active Turn; failure between each composed operation; no leaked temporary
carrier; no orphaned active engagement after a clean command; exact JSON Schema
validation; and repeated invocation against the same workspace without hidden
state reuse.

### TASK-006-E0: External MCP Per-Request Identity Probe

Status: `PLANNED`

Depends on `TASK-006-D`. Validate the pinned Codex CLI against the MCP
2026-07-28 stateless request model before claiming reconnect-safe review
idempotency. A connection, JSON-RPC request ID, or stdio process lifetime MUST
NOT be used as conversation or logical-request continuity. Probe whether the
host can generate one UUIDv7 per logical tool invocation and preserve it on
every attempt in the checked vendor metadata key
`xyz.rootkernel.dolgorae/externalRequestRef` under `tools/call params._meta`.
The reference is host-controlled and is never a model argument.

The probe selects exactly one disposition:

1. `replay_safe_meta`: custom `_meta` survives the supported retry and reconnect
   paths. Same reference and same normalized request return the original review;
   same reference with different input returns `IDEMPOTENCY_CONFLICT` without
   allocating another Reviewer Run.
2. `mcp_unavailable`: replay-safe metadata preservation is not proven. The MCP adapter is not
   advertised for `MILESTONE-SR1`; Codex CLI uses the one-shot Machine CLI
   command through its shell tool instead.

Verification: exact custom `_meta` capture before model-controlled arguments are
processed; same-reference retry; changed-input conflict; client reconnect;
server restart; concurrent calls; response loss before and after durable result
commit; proof that connection/process identity is ignored; proof that failure to
preserve metadata selects `mcp_unavailable`; and a checked disposition artifact.

### TASK-006-E1: Narrow Codex CLI MCP Review Adapter

Status: `PLANNED`

Depends on `TASK-006-E0`. Implement a private stdio MCP server entry point for
external AI hosts and expose exactly one model-facing tool named
`dolgorae_review` only under the disposition selected by TASK-006-E0. The tool
accepts the checked review request shape and invokes the same one-shot semantic
service as TASK-006-D. Canonical workspace, Runtime Profile, aggregate-owner
Controller, per-Run Controller, external provenance, request identity, and
idempotency are adapter-bound and MUST NOT be model arguments.

In `replay_safe_meta` mode, every call requires the checked
`params._meta` external request reference and derives idempotency only from that
reference plus the normalized adapter-bound request. Missing metadata is a typed
failure. Same-reference input drift returns `IDEMPOTENCY_CONFLICT`,
`retryable:false`, and `fix_host_request_carrier` without allocating another
Reviewer Run. In `mcp_unavailable` disposition, the
server does not register the tool and the CLI carrier remains the supported SR1
path. The adapter does not require a Dolgorae source Run or source Turn and does
not depend on the later run-bound `TASK-009-E0` probe.

Verification: MCP initialize/list/call lifecycle for the selected disposition;
concurrent client calls with independent one-shot engagements; exact replay only
with the same trusted external request reference; no duplicate Reviewer Run;
connection loss; cancellation; malformed and oversized payloads; adapter-bound
workspace and profile enforcement; recursion prevention in the Reviewer
profile; and secret, socket, database-path, raw-frame, and hidden-reasoning
canaries.

### TASK-006-F: Codex CLI Specialist Review Preview Acceptance

Status: `PLANNED`

Run an opt-in live acceptance campaign against the pinned Codex CLI and one
prepared Reviewer Runtime Profile. The host Codex CLI performs a nontrivial
working-tree change, invokes the mandatory Machine CLI Specialist Review path,
receives independent structured findings from a separate Reviewer Run and Codex
thread, addresses at least one concrete finding, and may invoke a second clean
Machine CLI review.

The campaign MUST prove that the Reviewer cannot modify the canonical
workspace, does not receive the host Codex hidden context, cannot invoke the
review adapter recursively, returns stable machine-readable findings, leaves no
credential or private endpoint in observable output, and cleans up or records a
safe non-success state after cancellation or failure. The Machine CLI path is
mandatory. If and only if TASK-006-E0 selected `replay_safe_meta` and TASK-006-E1
implemented the adapter, the campaign additionally executes the equivalent MCP
path. Otherwise acceptance records the checked `mcp_unavailable` disposition
and no MCP tool is advertised. Preserve bounded command,
environment, event, and result evidence without credentials or unbounded model
output.

Verification: deterministic fake-adapter tests plus the opt-in live Codex CLI
campaign; one independent read-only review of the Epic implementation; schema,
link, formatting, and secret scans; and task-scoped commits for every Task.

Epic acceptance: mark `EPIC-002A` complete only when every Task above passes the
ordinary completion gate and the live acceptance campaign succeeds. Completion
unlocks `MILESTONE-SR1`: the owner may immediately use Dolgorae from Codex CLI
for one-shot independent read-only Specialist review through the Machine CLI.
The MCP tool is part of the milestone only when TASK-006-E0 selected
`replay_safe_meta` and TASK-006-E1 proved the adapter. The milestone remains a
preview and does not claim reusable Specialist pools, canonical workspace
writes, Dolgorae Primary orchestration, Brokered Hierarchy, lateral
collaboration, or Personal Alpha readiness.

## EPIC-003: Access, Interaction, and Recovery Safety

Status: `PLANNED`

Goal: Enforce Dolgorae's one-durable-writer-authority-per-worktree scope,
Controller-authorized interaction, and conservative failure semantics.

### TASK-007: Durable Writer Authority and Cross-Profile Handoff

Status: `PLANNED`

Build on TASK-006-B's read-only Specialist and ordinary reader baseline.
Implement the per-worktree durable writer authority state machine, with BSD
`flock(2)` used only as a short transaction serializer, close-on-exec descriptor
hygiene, Application Support permanent-lock validation, explicit
`--write`/acquire/release, idle-only
cross-profile same-controller prepare/commit/cancel handoff, and fail-closed
background-execution uncertainty before activating or releasing authority.
Persist `effective_policy` and `writer_authority` independently and implement
revision-bound PREPARE/APPLY/COMMIT/cancel transitions without external waits
under file locks. Implement the operator-authorized `workspace writer reset`
repair, which is the only v1 escape from a `blocked_unknown` record and requires
proved absence of every recorded worker plus a complete empty census per
recorded dedicated lane generation.
Acquire/release retains the same worker, byte-1 owner, logical lane, and thread.
Policy changes occur within the current dedicated generation or, after exact
absence and a durable-history barrier, within its same-lane successor
generation. A shared-readonly Run is never promoted: it creates a lineage-linked
dedicated write-continuation Run. Startup locks use the pinned timed-record-lock layout
and offsets.

Verification: multiprocess tests proving multiple readers, one writer authority
per worktree, no shared-singleton restart on policy change, deterministic writer
conflicts, crash boundaries for `none→reserved→active` and
`active→releasing→none` including every proof/failure landing, missing/replaced
local lock refusal, PID reuse refusal, safe pause/close release and unknown-state
blocking, fixed thread residency, dedicated write-continuation creation, source-lane
retirement during handoff, destination failure leaving `none`, acquire races,
idle handoff,
active/waiting/cross-controller refusal, expiry, stale writer/run generations,
cancel/commit races and requester-failure-with-no-writer, and
separate locks for distinct canonical workspaces, permanent writer/startup
pathnames, held-fd/path and historical-inode splits, linked-worktree Git writable
roots, access-policy mappings, explicit unsupported-transition refusal, a fresh
lineage-linked dedicated write-continuation Run for shared-readonly to writer, and verified incumbent retirement
before write-to-read authority release.
Write-continuation tests must prove fixed workspace/profile/control mode, a new
same-principal destination Controller, non-decreasing assurance, capability
union and revalidation, supported model/effort overrides, recomposed instruction
prefixes, and non-inheritance of source Controller instructions or hidden history.
Also cover `F_SETLKWTIMEOUT`, spawn-image versus final-image identity, and
fail-closed byte-1 control timeout without any activity-derived signal.
Include brokered children created from Gul-shaped and ordinary-Codex-shaped
parents in the same-workspace writer race; client origin and parent reference
must not affect the one-writer result.
Measure the exact SPEC-007 writer turn carrier, including
`excludeSlashTmp:false` and `excludeTmpdirEnvVar:false`, against the pinned
profile and prove that a writer turn can write both the workspace and the OS
temporary directory. The TASK-000 probe campaign used the excluding variant, so
these two normative field values have no prior live evidence.
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

Epic acceptance: durable writer authority, Controller-authorized interaction,
pause and close, process identity, and outcome-unknown reconciliation are safe
and independently reviewed. These safety mechanisms harden the already usable
read-only Specialist Review Preview without delaying `MILESTONE-SR1`.

## EPIC-003A: External Specialist Engagement Hardening

Status: `PLANNED`

Goal: Generalize the one-shot read-only Specialist Review Preview into a durable,
reusable external Specialist service while the external AI remains the only
semantic control plane.

### TASK-009-D1: Reusable External Specialist Engagements

Status: `PLANNED`

Depends on `TASK-009-C` and builds directly on `EPIC-002A`. Remove the preview's
one-shot lifecycle restriction while preserving its trusted facade and
aggregate model. Support multiple independently hired Specialists in one
engagement, long-lived members, repeated sequential tasks per Specialist,
explicit get, cancel, release, complete, and abort, safe host reconnect,
completed-result redelivery, and exact aggregate-scoped idempotency across
Dolgorae restarts. Reconcile every accepted task through the TASK-009-A through
TASK-009-C lifecycle and outcome rules.

Retain one active Turn per Specialist and no implicit preemption. The external
control plane explicitly waits, retries, hires another member, or releases the
member. Add `isolated_write` only through a separate isolated workspace or
worktree policy. Canonical workspace writes require the external host to
quiesce its own writer and participate in TASK-007 writer authority. External
Specialists still cannot use the Brokered Collaboration Plane or hire nested
first-class Specialists.

Verification: engagement and member recovery across every restart boundary;
multiple roles and members; repeated tasks without context or idempotency
confusion; completed-not-delivered redelivery without target Turn replay;
ambiguous accepted or running task to `interrupted_unknown`; durable cancel,
release, complete, and abort; host disconnect and reconnect; isolated-write
artifact production; canonical writer conflict; nested-hire and collaboration
denial; and no external task-graph inference.

Epic acceptance: completion unlocks `MILESTONE-ES1`. External AI hosts may keep,
reuse, recover, and explicitly coordinate durable Specialist Engagements beyond
the one-shot review preview.

## EPIC-003B: Dolgorae Orchestration Control Plane and Brokered Hierarchy Core

Status: `PLANNED`

Goal: Add Dolgorae's own Primary orchestration authority and durable Brokered
Hierarchy over the hardened independent Run and Specialist foundations.

### TASK-009-D1A: Supervised Control-Plane Runtime and Minimum Gul Run Gateway

Status: `PLANNED`

Depends on `TASK-009-D1`. Implement the production host required before any
live Gul Orchestrated Session is claimed: foreground `dolgorae serve`, the
single-instance gateway record and lock, private Unix-socket lifecycle,
peer-UID validation, pinned tonic/prost generation, and one reconstructable
`ControlPlaneRuntime` per foreground process. SQLite remains durable authority;
the runtime owns only reconstructable schedulers, dirty sets, activation leases,
stream queues, and in-flight adapter state.

Implement the 24-method frozen public-v1 minimum path listed under
`MILESTONE-BH1` in the checked capabilities and gRPC conformance artifacts:
capability/workspace/profile bootstrap, Primary Run start/get/list/submit and
basic lifecycle recovery, Run event streaming, Controller interaction handling,
basic writer acquire/release/status, Controller verification, artifact metadata,
and bounded artifact chunk retrieval. Route every
implemented RPC into the same semantic service used by the Machine CLI. The
runtime MUST advertise only actually implemented methods. Timeline, profile
diagnostics, advanced Run operations, writer handoff, deletion,
verification, and the full operator-facing conformance surface remain in
`TASK-010-A`.

This Task does not yet create a Dolgorae Primary or Brokered Hierarchy. It makes
the real Gul transport and runtime ownership available to the following
transport-independent aggregate implementation and later live Primary tool.

Verification: protocol-zero handshake; exact BH1 method advertisement; unknown
or unavailable method fail-closed behavior; private socket path, symlink,
permission, peer UID, singleton, readiness, graceful shutdown, and crash restart;
Machine CLI/gRPC semantic parity for every minimum method; StartRun response
loss; protected interaction response loss; event replay; artifact metadata,
bounded chunk, authorization, range, retention, and digest failures; basic writer
recovery; Controller carrier TOCTOU and secret canaries; and reconstruction of the
ControlPlaneRuntime without treating memory as durable authority.

Task acceptance: Gul can launch `dolgorae serve`, negotiate public v1, create and
operate ordinary low-level Runs through the minimum frozen Run path, and survive
a controlled gateway restart. No Brokered Hierarchy milestone is claimed until
TASK-009-D2, TASK-009-E0, and TASK-009-E1 also complete.

### TASK-009-D2: Durable Orchestration Session and Brokered Hierarchy Core

Status: `PLANNED`

Depends on `TASK-009-D1A`. Implement the first-class `Dolgorae-Orchestrated Session` aggregate over the
independent Run core and the hardened Specialist execution path. Implement
prepared Aggregate Bootstrap Operations coupled to a parentless Primary
`StartRun` with checked Orchestration Launch Intent, the machine-local
Specialist Policy Registry, explicit approval policy and immutable Specialist
Policy snapshot, one-active-aggregate membership, immutable role and Agent
Configuration snapshots, preallocated child Run identity, write-ahead spawn
operations, aggregate-scoped idempotency, accepted Specialist tasks,
completed-not-delivered result retention, safe redelivery, owned-member
completion and abort, degraded Primary recovery, and fail-closed
`interrupted_unknown` handling.

The internal Orchestration Broker holds a separate non-model-visible Controller
capability for every brokered Specialist. Implement the transport-independent
Primary Orchestration Service, tool-dispatch interface, and bounded fake
handlers against the checked schema. Support request, approval wait, list,
assign, await, collect, cancel, and graceful release under both
`user_approval_required` and `fully_delegated`. Implement explicit release,
verify-writer-none, and acquire sequencing for cross-Controller writer movement
without claiming atomic handoff. Do not add or change a public v1 Protobuf field
or RPC. Do not implement live run-bound model transport or lateral Specialist
collaboration in this Task.

Verification: crash at every boundary before and after Orchestration Session
SQLite commit, Primary Run intent fsync and publication, event append, child Run
reservation, Worker publication, thread creation, task dispatch, result append,
and delivery receipt; same-key replay and different-input conflict; duplicate
and orphan prevention; invalid parent, role conversion, reparenting, and
use-case transfer; Primary failure with retained Specialists; completed-result
redelivery without target Turn replay; user-approval-required and
fully-delegated paths; Specialist allowlist denial; raw managed-Run and forged
reserved-parent denial; cross-Controller writer race with `WRITER_BUSY`; schema
and semantic-validator fixtures; capability and secret canaries; and
byte-identical public Protobuf source and descriptor.

Epic acceptance: the complete Orchestration Session and Brokered Hierarchy state
machine is implemented and proven through transport-independent fake adapters.
No live Primary model tool is claimed until `EPIC-003C` completes.

## EPIC-003C: Live Dolgorae Control Plane and Brokered Hierarchy

Status: `PLANNED`

Goal: Select and integrate the live run-bound Primary tool transport so Gul can
use Dolgorae as the active semantic control plane with a durable Brokered
Hierarchy.

### TASK-009-E0: Run-Bound Internal Tool Transport Probe

Status: `PLANNED`

Depends on `TASK-009-D1A` and `TASK-009-D2`. Validate and close the live transport boundary for the
private Primary orchestration tool and the later Brokered Specialist
Collaboration tool. Prove that the pinned Codex App Server can provide a private
run-bound MCP bridge whose source Run, source Turn, tool-call identity,
cancellation, and bounded wait behavior are known without exposing a Controller
credential or allowing model-controlled source identity.

This Task owns registration and source binding for both checked run-bound tool
schemas, source identity and idempotency derivation outside model arguments,
Dedicated Lane fallback when shared-profile invocation identity is ambiguous,
bounded await, cancellation, bridge restart, connection-loss behavior, and
credential, private-socket, database-path, and source-identity canaries. The
external `dolgorae_review` MCP adapter from `EPIC-002A` is a separate external
control-plane adapter and is not blocked or redesigned by this probe.

The durable aggregate broker, Primary Orchestration Service, tool-dispatch
interfaces, and fake handlers are implemented and unit-tested in
`TASK-009-D2`. This Task selects the supported live model-facing transport.
Mailbox, Scheduler, Activation Manager, and collaboration outbox implementation
remain in `TASK-009-E2`.

Verification: live pinned transport probes for both run-bound tool surfaces,
source Run and Turn correlation, concurrent calls, bounded wait timeout,
cancellation, bridge restart, connection loss, and credential canaries;
ambiguous shared identity selects the Dedicated Lane fallback; public Protobuf
source and descriptor remain byte-identical.

Task acceptance: ADR-027, ADR-028, SPEC-012, architecture, both run-bound private
tool schemas, fixtures, verification index, and implementation memos agree; the
probe selects a supported bridge or explicitly blocks `TASK-009-E1` and
`TASK-009-E2`; and an independent read-only review reports no unresolved
blocking finding.

### TASK-009-E1: Live Primary Orchestration Tool and Brokered Hierarchy Acceptance

Status: `PLANNED`

Depends on `TASK-009-D1A`, `TASK-009-E0`, and `TASK-009-D2`. Integrate only the checked Primary
orchestration tool through the transport selected by the probe. Bind session,
Primary Run, source Turn, tool-call ID, inherited root priority, Controller
authority, and idempotency outside model arguments. Allow the Primary Agent to
request, await approval for, list, assign, await, collect, cancel, and release
policy-admitted Specialists without receiving a child Controller credential or
mutating another Run directly.

Run one live integration with the actual supported Gul client against the
TASK-009-D1A local gRPC gateway. Create an Orchestrated Session in Standalone
Primary composition, transition it to Brokered Hierarchy by provisioning a
Reviewer, execute and collect one bounded Specialist task, return at least one
Primary or Specialist result above the inline bound through an artifact
reference, retrieve its metadata and one or more bounded chunks, verify total
length and SHA-256, recover the hierarchy after a controlled Dolgorae restart,
and return to a clean completed or active state. A mock, fake adapter, or merely Gul-shaped harness cannot satisfy this
acceptance step. Specialist
messages still route through Primary task operations in this Task; lateral
Specialist collaboration is deferred to `EPIC-003D`.

Verification: actual Gul client private-boundary integration;
user-approval-required and fully-delegated live paths; exact tool retry; source
identity canaries; Primary restart; Specialist task result
redelivery; missing, unauthorized, run-lifetime-expired, malformed, oversized,
out-of-range, and integrity-failed artifact reads with their documented typed
errors; Primary degradation and recovery; release and abort; writer
conflict; no credential exposure; no direct peer control; unchanged public Gul
wire; and independent review of the live hierarchy path.

Epic acceptance: completion unlocks `MILESTONE-BH1` only together with the
minimum supervised Gul gateway completed in TASK-009-D1A. Gul can use the real
local gRPC path to operate Dolgorae as the live Primary control plane, and
Dolgorae can create, persist, recover, and operate a Brokered Hierarchy. Lateral Specialist collaboration is not yet part
of this milestone.

## EPIC-003D: Brokered Specialist Collaboration

Status: `PLANNED`

Goal: Add durable bounded Specialist-to-Specialist collaboration to one active
Brokered Hierarchy without making the Primary Agent a message relay.

### TASK-009-E2: Durable Mailbox, Virtual Actor, and Collaboration Plane

Status: `PLANNED`

Depends on `TASK-009-D1A`, `TASK-009-E0`, `TASK-009-E1`, and `TASK-009-D2`. Integrate the checked
Specialist collaboration tool through the selected run-bound transport. Add the
Collaboration Service, SQLite Collaboration Exchange and mailbox tables,
transactional result outbox, dirty-set Mailbox Scheduler, Activation Manager,
actor passivation, activation leases, deterministic role selection, inherited
priority, aging, fairness, deadlines, queue limits, backpressure, blocking wait
graph, and result collection.

Keep one active target Turn per Run, queue a busy target without preemption,
wake an `on_mail` passivated target without per-actor polling, retain mail across
activation failure, reject paused or terminal targets according to policy, and
never allow collaboration to mutate peer lifecycle, writer, role, Controller,
or aggregate membership. External Specialist Engagements cannot use this plane
in v1.

Verification: resident idle request-response; busy-target queueing; exact
priority and FIFO tie breaks; aging and source fairness; role-selector
repeatability; fan-out plus `any` and `all` await; one activation under
concurrent mail; startup recovery after commit-before-wake crash; expired
pre-dispatch claim; ambiguous Turn acceptance to `interrupted_unknown`;
transactional result redelivery without replay; source restart and deferred
collection; cross-session, external-engagement, cycle, depth, writer-held
blocking wait, terminal target, queue overflow, and implicit-hire rejection; no
credential, private socket, database path, raw protocol frame, or hidden
reasoning leakage; and byte-identical public Protobuf source and descriptor.

Epic acceptance: completion unlocks `MILESTONE-BC1`. Failures cannot duplicate
or orphan a broker-owned Specialist, create two Dolgorae writer proxies, signal
an unverified process, silently replay a user or Specialist task, cross account
or aggregate ownership boundaries, or falsely claim known outcomes. Specialists
in one Brokered Hierarchy may now collaborate laterally through durable bounded
mailboxes without Primary message relay.

## EPIC-004: Operator and Audit Interfaces

Status: `PLANNED`

Goal: Complete the Controller-facing operational surface and make every durable Run
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

### TASK-010-A: Complete Gul gRPC Surface and Extended Operational Conformance

Status: `PLANNED`

Depends on `TASK-009-D1A`. Extend the already operational foreground
`dolgorae serve` gateway from the 24-method BH1 set to all 34 methods in the
frozen `dolgorae.public.v1` descriptor. Add the ten deferred RPCs covering
profile diagnostics, Controller timeline, default-effort, fork,
verification, deletion, write continuation, writer handoff, and their complete
safe projections. Complete bounded independent Run streams, exhaustive typed
`google.rpc.Status` details, advanced cancellation behavior, and all remaining
operator-safe conformance without adding Operator RPCs, TCP, client-streaming,
bidirectional streaming, worker sockets, or App Server transports.

The task MUST preserve the TASK-009-D1A process, socket, peer-UID,
ControlPlaneRuntime, and semantic-service ownership model. `GetCapabilities`
continues to advertise only implemented methods until this Task completes, then
advertises the complete public-v1 descriptor method set required by
`MILESTONE-PA1`.

Verification: deterministic fake-semantic-service tests for every remaining
unary and stream method; full method-kind/descriptor golden tests;
32-envelope/4-MiB/5-second pressure boundaries; independent Run streams on one
channel; continuation lineage; timeline redaction and image metadata; artifact
regression coverage; advanced writer handoff; deletion and verification;
and the exhaustive typed error map. Re-run the TASK-009-D1A socket, restart,
carrier TOCTOU, allocation-loss, Interaction-loss, and secret-canary tests as
regressions. All timing uses injectable clocks and no test binds TCP.

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
prefixes, role-aware Primary Agent and Independent Specialist Agent wording,
subordinate Run instructions, `.dolgorae` reservation, access-aware mutation
policy, explicit Git and background-process rules, advisory managed-Run
context, both user-facing use cases, Standalone Primary and Brokered Hierarchy
composition, Orchestration-Broker-only Specialist control, external-AI
Specialist boundaries, manager-owned bounded singleton shutdown, and cleanup
audit records.

Verification: control-mode and aggregate-role prompt-composition snapshots,
Controller-kind compatibility, observer interaction denial, capability
non-disclosure, and conflicting Run-instruction tests are separate from
sandbox-policy enforcement tests; also cover Dolgorae-Orchestrated Session and External Specialist Engagement
mappings, Standalone Primary and Brokered Hierarchy composition, self-read-only
and attempted cross-Run CLI control, marker-removal limitation reporting,
malformed/foreign/nonexistent managed markers, non-exec MCP marker absence,
write-heavy Native Delegation language, Independent Specialist result routing
without Controller disclosure,
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
profile-event schemas, the `brokered_independent_subagent_runs` compatibility
feature and Independent Specialist CLI composition, canonical upstream
file-change kinds, and semantic
multi-diff aggregate bounds.
Drive every operation shared by Machine CLI and gRPC from one golden semantic
scenario and require equal normalized result, typed error, durable state,
ledger/event cursor, idempotency receipt, and redaction result after removing
adapter-only envelope and transport metadata.

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
Include gateway lock/record and socket-inode crash points, peer-credential and
carrier-file replacement races, concurrent Run streams, slow-consumer pressure,
gateway restart during an accepted mutation, and proof that no gateway failure
signals a worker/App Server or releases writer authority.

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
transitions, profile diagnostic minimal/operational views, artifact
integrity/range behavior, and the exact SPEC-007 writer turn carrier with
`excludeSlashTmp:false` and `excludeTmpdirEnvVar:false`. If any required dedicated-lane behavior fails, TASK-015 and
release remain blocked; absence of a future native terminal API is not itself a
blocker.
The live campaign also runs one broker-owned Dedicated managed child, returns a
bounded result to a parent-shaped harness, and proves that a conflicting
Dolgorae writer is rejected without exposing the child Controller credential.
It additionally drives a Gul Go harness over one local gRPC channel, observes at
least two independent Run streams, restarts the supervised gateway while a Run
survives, resumes each cursor, verifies Controller adoption without mutation,
and reads large final-response and approval artifacts.

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
