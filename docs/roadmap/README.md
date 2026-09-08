# Dolgorae Roadmap

Status: Ordered implementation roadmap. `EPIC-000`, `TASK-000-H`, `EPIC-001`,
`TASK-001`, `TASK-002`, `TASK-003-A`, `TASK-003-B`, and `TASK-003-C` are
`COMPLETE`. `EPIC-002`, `TASK-004`, `TASK-005`, `TASK-006`, and `TASK-007` are
`COMPLETE`. `EPIC-003`, `TASK-008`, `TASK-009`, `TASK-010`, `TASK-011`,
`TASK-012`, and `TASK-013` are `COMPLETE` and form the first user-usable
product slice. `EPIC-004`, `TASK-014`, `TASK-015`, and `TASK-016` are
`COMPLETE` and establish the `v0.1.0` Integration Preview product boundary.
`EPIC-005`, `TASK-017` through `TASK-021`, `EPIC-006`, and `TASK-022` are
`COMPLETE` and establish the writer, recovery, and reusable external Specialist
safety foundation. `EPIC-013` and `TASK-036` through `TASK-038` are also
`COMPLETE` and establish the global Codex Profile cutover before `EPIC-007`.
`EPIC-012` and `TASK-035` are `COMPLETE`; they provide development-channel
production infrastructure but do not add a user-facing product milestone.
Completing `EPIC-003` unlocks `MILESTONE-SR1`, which guarantees the one-shot
Machine CLI review path and lets Codex CLI invoke it through its ordinary shell
tool. The narrow external MCP adapter is included only when the pinned host
passes the explicit per-request identity probe; connection or stdio-process
identity is never treated as retry continuity. This milestone does not wait for
writer authority, the Dolgorae Primary control plane, Brokered Hierarchy, or
Specialist-to-Specialist collaboration.

After `MILESTONE-SR1`, `EPIC-004` implements common immutable review targets,
extends Specialist Review to dirty and historical Git state, and activates
Dolgorae as Aquarium independent-review's Codex backend. After the `v0.1.0`
Integration Preview, the implementation roadmap proceeds through access and
recovery safety, external Specialist hardening, the global Codex Profile
cutover, the supervised Gul Run gateway and Brokered Hierarchy core, live
Primary control-plane integration, the durable Collaboration Plane, operator
and audit interfaces, and final conformance and Personal Alpha acceptance.
`TASK-025` remains the live run-bound transport probe and occurs only after the
Brokered Hierarchy core is complete.
`TASK-000-G` remains superseded because its terminology-only boundary no longer
matches the accepted product contract. `TASK-003-C` completed the lifecycle-seal
and ledger-conformance contract after TASK-003-B's durable ledger, repair,
replay, projection, and observer-publication work. EPIC-001 completed after its
Epic-level acceptance checks passed.

This document owns execution order and delivery status. Product requirements
remain authoritative in the [specification](../specs/README.md); this roadmap
must not redefine them.
Document roles and the required synchronization procedure are defined by the
[documentation authority map](../README.md).

## Product Milestones

| Milestone | Owning Epic | User-visible capability unlocked |
| --- | --- | --- |
| `MILESTONE-SR1` | `EPIC-003` | Codex CLI can request one independent read-only working-tree review through `dolgorae specialist review`. The `dolgorae_review` MCP tool is additionally available only when its per-request identity carrier passes `TASK-011` and `TASK-012`. |
| `MILESTONE-IR1` | `EPIC-004` | Aquarium independent-review uses Dolgorae, rather than an Orca-created Codex terminal, for immutable workspace, staged, dirty, HEAD, commit, and range review with a fresh Codex Reviewer. |
| `MILESTONE-ES1` | `EPIC-006` | External AIs can keep and reuse durable Specialist Engagements across multiple tasks and restarts. |
| `MILESTONE-BH1` | `EPIC-008` | Gul can use Dolgorae as the live Primary control plane and operate a durable Brokered Hierarchy. |
| `MILESTONE-BC1` | `EPIC-009` | Specialists in one Brokered Hierarchy can use durable bounded lateral collaboration without Primary message relay. |
| `MILESTONE-PA1` | `EPIC-011` | The complete Personal Alpha acceptance campaign passes. |

Milestones are cumulative. An earlier milestone remains usable while later
Epics are implemented. A milestone does not waive its own Task completion gate
or any safety limitation stated in its owning Epic.

## Release Train

| Version | Classification | Required completion boundary | Cumulative product milestones |
| --- | --- | --- | --- |
| `v0.1.0` | Integration Preview | `EPIC-004` complete; completed `EPIC-012` development producer included without extending product scope | `MILESTONE-SR1`, `MILESTONE-IR1` |
| `v0.1.1` | Root Transition Preview | The fixed-home prerequisite from `TASK-017`; the full Task completes in the `v0.1.2` cycle | `MILESTONE-SR1`, `MILESTONE-IR1` |
| `v0.1.2` | Milestone Preview | `EPIC-006` and `EPIC-013` complete, including the preceding `EPIC-005` safety layer | Through `MILESTONE-ES1` |
| `v0.1.3` | Milestone Preview | `EPIC-008` complete, including the preceding `EPIC-007` control-plane core | Through `MILESTONE-BH1` |
| `v0.1.4` | Milestone Preview | `EPIC-009` complete | Through `MILESTONE-BC1` |
| `v0.2.0` | Personal Alpha and first customer-supported release | Every currently planned product Epic from `EPIC-005` through `EPIC-011` plus `EPIC-013` complete, including `EPIC-010` operator and audit interfaces | Through `MILESTONE-PA1` |

The `v0.1.x` releases are cumulative previews and do not claim Personal Alpha
readiness, the complete target specification, or customer support. A completion
boundary makes a version eligible for release; it does not itself create a
release, change the changelog, authorize a tag or publication, or prove an
installed runtime. New Epics adopted later do not enter the `v0.2.0` boundary
unless this table is explicitly revised.

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
- The completed `EPIC-000` and `EPIC-001` identifiers and historical suffixed
  Task identifiers are retained. New Epic and Task identifiers use independent
  zero-padded integer sequences, allocate the greatest number ever present plus
  one, and never change or become reusable after allocation. Identity does not
  encode execution order; roadmap position and explicit dependencies do.
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
5. Every blocking finding is fixed in the owning SOT, implementation, or checked
   artifact, or rejected with independently verified rationale.
6. Deferred findings are assigned to an owning roadmap task or public issue.
7. One or more task-scoped Git commits contain the completed change.

This gate does not prescribe how an implementer divides commits or stages files.
Push always requires separate explicit user authorization.

## EPIC-000: Pre-Implementation Stabilization

Status: `COMPLETE`

EPIC-000 established the product boundary, architecture, public protocol,
workspace model, controller and writer authority, run-state projection, and
external compatibility baseline used by implementation. Its stabilization tasks
`TASK-000` through `TASK-000-H` are complete or superseded as recorded below.

| Task | Status | Result |
| --- | --- | --- |
| `TASK-000` | `COMPLETE` | Reconciled the initial architecture and pinned external-runtime contract. |
| `TASK-000-A` | `COMPLETE` | Closed schema, traceability, and deterministic validation gaps. |
| `TASK-000-B` | `COMPLETE` | Completed contract and compatibility stabilization. |
| `TASK-000-C` | `COMPLETE` | Established singleton, local-state, and writer invariants. |
| `TASK-000-D` | `COMPLETE` | Established controller, projection, and integration contracts. |
| `TASK-000-E` | `SUPERSEDED` | Replaced by the public local RPC and two-use-case design. |
| `TASK-000-F` | `COMPLETE` | Froze the public local gRPC SOT and protocol contract. |
| `TASK-000-G` | `SUPERSEDED` | Replaced by TASK-000-H terminology and ownership rules. |
| `TASK-000-H` | `COMPLETE` | Reconciled the two public use cases and internal orchestration ownership. |

## EPIC-001: Foundation and Durable State

Status: `COMPLETE`

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
creation. Establish the Dolgorae-home per-workspace mutable state root, its
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

Status: `COMPLETE`

Implement O_APPEND writing, bounded group commit, every write-ahead barrier,
deterministic torn-tail evidence and idempotent repair, full replay, atomic
`state.json` with its fsynced watermark, append-time client-event normalization,
reasoning-content suppression/non-retention, and observer publication.

Verification: crash injection before/after every fsync/effect barrier; middle
corruption versus torn tail; repeated repair; ahead/stale/missing projection;
100-millisecond publication under the injectable clock; no state head beyond a
durable ledger record.

### TASK-003-C: Lifecycle Seals and Ledger Conformance

Status: `COMPLETE`

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

Status: `COMPLETE`

Goal: Provide reconnectable per-run process ownership and a strict stable-subset
adapter for profile-scoped Codex singleton accounts and threads.

### TASK-004: Per-Run Worker and Unix IPC

Status: `COMPLETE`

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

Status: `COMPLETE`

Implement per-workspace Dolgorae-home `local.yaml` profile CRUD, direct executable, normalized
global argv, absolute `CODEX_HOME`, and explicit environment-map validation;
deterministic environment preparation; schema generation into temporary storage; required
stable-subset comparison, app-server handshake, `codexHome` matching,
`model/list`, tested/unverified verdicts, restorable immutable profile snapshots,
closed configuration classification, symbolic launch-cwd policy and derived cwd, singleton keys,
epochs, operator server-key migration, append-only membership repair,
identity-complete shutdown, profile log drainer, profile diagnostic journal,
symbolic launch-cwd policy, explicit PATH/LANG/LC_ALL, PREPARE/APPLY/COMMIT
server operations, full-key short-socket collision checks, and server lifecycle commands.
The required-subset manifest is checked input, not a TASK-032 invention.

Verification: fake executable matrices for missing commands, rejected wrapper argv,
profile-name collision, home mismatch, incompatible active same-home singleton,
unsupported/older/newer versions, missing schema fields,
additive fields, login failure, and successful 0.149.0 compatibility.
Also cover `$ref` resolution, requiredness/type/enum changes, pagination,
early-ID behavioral rejection, absent-thread errors, version-drift quiescent
automatic rollover, active-member refusal, and both migration-first and
stop-first reservation races, malformed-fence rejection, plus operator
migration/rollback. Malformed fence coverage includes both transaction ID and
phase corruption; post-stop phase-write and start-authorization failures share
the rollback path. Stop-commit partial cleanup and final commit-record write
failure are separately covered lifecycle stages; operator reconciliation proves
and commits a blocked ready replacement under the same migration locks. Invalid
or traversal-shaped confirmation keys fail before filesystem access. Launch
probes cannot select rollover, and blocked/absent generations are covered by
operator state-reset repair without rewriting in-flight prepared/applying
transactions. Durable migration keys are canonical before path access, and a
duplicate rollover starter attaches to an already-ready requested generation.
Membership registration is fenced atomically with the automatic migration
quiescence proof.
Probe configuration mutations and classify each
input as static, migratable, runtime-mutable, or ignored. Implement binary-level runtime capabilities,
profile-specific interaction/capability snapshots, and pre-allocation rejection
of missing required capabilities. Bare doctor remains offline; launch behavior
is tested only by explicit `--launch-probe`. TASK-005 owns the selected 0.149.0
native feature policy: reject raw global `multi_agent` arguments, inject exactly
one profile-owned `--enable multi_agent` pair, treat absence as enabled, reject
explicit public disable with `NATIVE_SUBAGENT_DISABLE_UNAVAILABLE`, retain the
disable launch only for diagnostic probes, advertise enabled-but-incomplete
observation as `unverified`, and
make active or unverified native state block every quiescence-dependent
transition. A policy change requires a new server key; a completely quiescent
singleton rolls over through the durable migration transaction, while a live
membership requires operator-authorized migration. Dedicated-lane campaigns prove
identical-contract same-home shared/dedicated coexistence, globally unique server
epochs, fixed logical-lane residency, same-lane resume only after exact prior-
generation absence, and exact cleanup without unrelated signals. Cross-server
same-thread resume is a negative test and MUST remain rejected. A future native
terminal API is optional hybrid evidence.

### TASK-006: Thread and Turn Lifecycle

Status: `COMPLETE`

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

### TASK-007: External Controller and Observer Boundary

Status: `COMPLETE`

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

## EPIC-003: External Read-Only Specialist Review Preview

Status: `COMPLETE`

Goal: Deliver the first user-usable Dolgorae product slice as early as the
independent Run core permits. An external Codex CLI remains the semantic control
plane and invokes exactly one independent read-only Reviewer Specialist for a
bounded working-tree review.

This Epic owns `MILESTONE-SR1`. It uses the final External Specialist
Engagement model and shared semantic Run core rather than a disposable preview
implementation, but deliberately restricts the first slice to one-shot,
read-only review.

### TASK-008: Read-Only Specialist Runtime Baseline

Status: `COMPLETE`

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

### TASK-009: Durable External Review Engagement Core

Status: `COMPLETE`

Implement the minimal External Specialist Engagement production path required
by a one-shot review, using the existing checked External Specialist Facade and
the Dolgorae-home SQLite WAL authority. Implement explicit open, safe
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
multiple Specialists, and isolated-write operation belong to `EPIC-006`.

Verification: crash before and after each SQLite commit, child Run reservation,
Worker publication, thread creation, task acceptance, result artifact commit,
delivery receipt, release, and close; exact same-key replay; different-payload
idempotency conflict; duplicate and orphan prevention; raw `managed_agent` Run
exclusion; read-only access enforcement; successful result collection; Ctrl-C
cancellation; and fail-closed `interrupted_unknown` without task replay.

### TASK-010: One-Shot Specialist Review CLI and Checked Result Contract

Status: `COMPLETE`

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
`dolgorae-specialist-review-tool-v2.schema.json` request, success, finding, and
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

### TASK-011: External MCP Per-Request Identity Probe

Status: `COMPLETE`

Depends on `TASK-010`. Validate the pinned Codex CLI against the MCP
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

### TASK-012: Narrow Codex CLI MCP Review Adapter

Status: `COMPLETE`

Depends on `TASK-011`. Implement a private stdio MCP server entry point for
external AI hosts and expose exactly one model-facing tool named
`dolgorae_review` only under the disposition selected by TASK-011. The tool
accepts the checked review request shape and invokes the same one-shot semantic
service as TASK-010. Canonical workspace, Runtime Profile, aggregate-owner
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
not depend on the later run-bound `TASK-025` probe.

Verification: MCP initialize/list/call lifecycle for the selected disposition;
concurrent client calls with independent one-shot engagements; exact replay only
with the same trusted external request reference; no duplicate Reviewer Run;
connection loss; cancellation; malformed and oversized payloads; adapter-bound
workspace and profile enforcement; recursion prevention in the Reviewer
profile; and secret, socket, database-path, raw-frame, and hidden-reasoning
canaries.

### TASK-013: Codex CLI Specialist Review Preview Acceptance

Status: `COMPLETE`

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
mandatory. If and only if TASK-011 selected `replay_safe_meta` and TASK-012
implemented the adapter, the campaign additionally executes the equivalent MCP
path. Otherwise acceptance records the checked `mcp_unavailable` disposition
and no MCP tool is advertised. Preserve bounded command,
environment, event, and result evidence without credentials or unbounded model
output.

Verification: deterministic fake-adapter tests plus the opt-in live Codex CLI
campaign; one independent read-only review of the Epic implementation; schema,
link, formatting, and secret scans; and task-scoped commits for every Task.

Epic acceptance: mark `EPIC-003` complete only when every Task above passes the
ordinary completion gate and the live acceptance campaign succeeds. Completion
unlocks `MILESTONE-SR1`: the owner may immediately use Dolgorae from Codex CLI
for one-shot independent read-only Specialist review through the Machine CLI.
The MCP tool is part of the milestone only when TASK-011 selected
`replay_safe_meta` and TASK-012 proved the adapter. The milestone remains a
preview and does not claim reusable Specialist pools, canonical workspace
writes, Dolgorae Primary orchestration, Brokered Hierarchy, lateral
collaboration, or Personal Alpha readiness.

## EPIC-004: Immutable Review Targets and Aquarium Activation

Status: `COMPLETE`

Canonical Outcomes: [specification](../specs/README.md#immutable-review-targets-and-scoped-specialist-review),
[architecture](../architecture/README.md#immutable-review-target-coordinator),
[ADR-032](../architecture-decision-records/README.md#adr-032-separate-immutable-review-targets-from-backend-supervision),
[checked review-target protocol](../protocol/dolgorae-review-target-v1.schema.json),
[implementation](../../src/review_target.rs), and
[black-box contract tests](../../tests/e2e/test_review_target_cli.py)

Goal: Implement reusable immutable review targets, extend Dolgorae Specialist
Review to dirty and historical Git state, and replace the Orca-created Codex
terminal in Aquarium independent-review with Dolgorae. Mulgae,
independent-review, and orca-review share source-scope semantics without sharing
one orchestration lifecycle. This Epic is not complete until the exact Aquarium
installation has activated and verified the Dolgorae-backed path and Dolgorae
has revalidated the resulting runtime Completed Confirm.

### TASK-014: Immutable Review Target Foundation

Status: `COMPLETE`

Implement the accepted `workspace`, `staged`, `dirty`, `head`, `commit`, and
two-dot or three-dot `range` meanings from ADR-032 and
[review-target strategy analysis](../architecture-decision-records/review-strategy-analysis.md).
Task, Epic, and
special-request identifiers provide authority and focus but MUST resolve to one
source scope. Mulgae-only patch and stdin remain extensions.

Add versioned `review-target.capture` and `review-target.settle` Machine
operations, exposed by the corresponding `dolgorae review-target capture` and
`dolgorae review-target settle` CLI commands. Capture returns an opaque capture
reference, resolved source identities, safe manifest digest, whole-target
digest, included and excluded dispositions, a backend-readable immutable root,
and an owner-binding digest. Capture also binds one backend kind and immutable
lifecycle identity and delivers a random settlement owner credential only
through a caller-supplied protected `0600` output file or inherited descriptor
bound outside model-visible input; no machine result or provider-visible content
exposes the credential bytes or carrier path.

Settlement is an idempotent compare-and-set operation requiring the capture
reference, protected owner credential, expected capture revision, and a checked
terminal receipt bound to the stored backend kind and lifecycle identity. It
revalidates the receipt's authoritative terminal state, backend state revision,
and stable evidence digest immediately before cleanup. Active, unknown, stale,
foreign-owner, mismatched-lifecycle, missing-evidence, and concurrent losing
requests preserve the capture. Exact accepted replay returns the original
settlement; changed post-settlement input is a conflict.

Capture MUST NOT modify the source worktree, index, refs, or Git metadata. It
fails on capture-time drift, unresolved conflicts, escaping links, special
files, snapshot mutation, invalid revisions, and recognized credentials,
private keys, or tokens in tracked or untracked candidates. `workspace` and the
after side of `dirty` materialize one final worktree-over-index byte sequence per
path. `staged` materializes the captured index over captured `HEAD`, while
`head`, `commit`, and `range` use only resolved Git objects; none of those four
scopes may substitute current worktree bytes. Captures live outside the source
repository under Dolgorae home and disclose the same-user
visibility limitation.

Verification: checked request/result schemas; unit and black-box tests for all
six scopes, root commits, both range forms, staged/unstaged/untracked/deleted/
recreated/renamed paths, capture drift, conflicts, unsafe paths, tracked and
untracked secrets, mutation detection, idempotent replay, foreign credentials,
stale revisions, mismatched lifecycle receipts, forged or missing terminal
evidence, simultaneous settlement races, cleanup, timeout, and unknown recovery;
source and index before/after identity proof; `make test`; and an independent
adversarial read-only review.

### TASK-015: Scoped Specialist Review Runtime

Status: `COMPLETE`

Depends on `TASK-014`. Add a versioned Specialist Review request whose target is
`{kind, revision?}`. `workspace`, `staged`, `dirty`, and `head` reject a
revision; `commit` requires one revision; `range` requires one exact `A..B` or
`A...B` expression. The completed `specialist review --scope working-tree` v1
request and result retain their existing spelling and meaning.

The new path captures through TASK-014, starts one fresh managed Codex Reviewer
through the External Specialist Engagement, exposes only the immutable target
root as review context, validates the checked result and captured bytes, and
settles the capture only after the engagement and Reviewer Run reach an
authoritative outcome. The result binds the resolved Git identities, target and
manifest digests, Reviewer identity, review verdict, engagement and Run state,
settlement, capture-time source identity, capture-integrity proof, and the
absence of workflow-issued source mutation. A later source change by another
actor does not stale the captured target or result. Bounded-wait exhaustion
observes authority once and preserves active or unknown state; cancellation
requires explicit user authority.

Verification: compatibility tests for v1; checked v2 CLI and Machine carriers;
one fresh Reviewer for every scope; dirty and historical end-to-end campaigns;
wrong-scope, source-drift, executable-drift, timeout, cancellation, unknown,
result-tampering, and cleanup tests; exact executable version, file identity,
capability result, and SHA-256 evidence; `make test`; opt-in live Codex CLI
acceptance; and an independent adversarial read-only review.

### TASK-016: Aquarium Activation and Runtime Completed Confirm

Status: `COMPLETE`

Depends on `TASK-015`, `TASK-035`, and Aquarium `TASK-024`. The Aquarium owner
consumed one exact enrolled Dolgorae development generation, completed the
runtime activation under Aquarium's authority, and returned a Completed Confirm
that survived Dolgorae's independent revalidation. The handoff contract below
records the evidence shape required at that external authority boundary.

Copy the following request verbatim. Aquarium owns the exact generation fields
created from the completed TASK-035 producer and must replace every `REQUIRED:`
marker with exact evidence before returning the Completed Confirm:

```text
Aquarium independent-review Dolgorae activation request

Validated Dolgorae generation:
- project ID: dolgorae
- repository commit: REQUIRED: exact clean local-main producer commit
- development version: REQUIRED: v0.1.0-dev.<commit-prefix>
- canonical executable path: REQUIRED: exact immutable generation path
- executable SHA-256: REQUIRED: exact enrolled artifact digest
- capability/contract digest: REQUIRED: exact runtime capability digest
- generation lease evidence: REQUIRED: stable reference proving the invoked generation stayed leased

The capability/contract digest is SHA-256 over the compact JSON `data` object
from `runtime capabilities` with object keys sorted lexicographically.

Required outcome:
1. Aquarium independent-review must use this exact Dolgorae generation to
   capture the selected target and run one fresh Codex Reviewer.
2. independent-review must create no Orca Run, Task, Dispatch, worker, or
   terminal.
3. orca-review must retain Orca provider and lifecycle supervision while using
   the same immutable target meanings and captured bytes.
4. workspace, staged, dirty, head, commit, and A..B/A...B range must have the
   EPIC-004 meanings. Task, Epic, and special-request identifiers are authority
   and focus, not source scopes.
5. Mulgae remains operationally independent, but corresponding scope meanings
   and resolved target identities must be semantically conformant.
6. Review preparation must not modify the source worktree, index, refs, or Git
   metadata. Dirty staged/unstaged/untracked/deleted/recreated state and Git
   history targets must be supported.
7. Missing or changed Dolgorae executable identity, source drift, unsafe files,
   secret detection, incompatible capability, or incomplete settlement must
   fail closed. Do not silently fall back to Orca for independent-review.
8. Settlement must prove the capture owner, expected capture revision, bound
   backend lifecycle, and authoritative terminal evidence. Active, unknown,
   stale, foreign-owner, mismatched, or concurrent losing requests must retain
   the capture and recovery evidence.

Requested procedure:
1. Inspect the exact active Aquarium review contract, independent-review,
   orca-review, supervision references, target-inspection scripts, and tests.
2. If the active implementation already satisfies every requirement, make no
   unnecessary change and validate the exact current Aquarium commit.
3. Otherwise, implement the minimum coherent active changes and commit them
   under Aquarium's own authority and validation rules.
4. Exercise all six scopes. Include staged plus unstaged changes, non-ignored
   untracked files, staged deletion, deletion followed by recreation, rename,
   root commit, ordinary commit, two-dot range, three-dot range, source drift,
   secret rejection, timeout, unknown outcome, foreign-owner settlement, stale
   revision, mismatched lifecycle, missing terminal evidence, concurrent
   settlement, idempotent replay, and cleanup.
5. Prove that independent-review uses Dolgorae without Orca objects and that
   orca-review still uses the Orca lifecycle.
6. Return the runtime Completed Confirm below. A documentation-only result,
   uncommitted diff, mutable temporary path, or prose-only success statement is
   insufficient.

Required Completed Confirm:
{
  "status": "completed",
  "completion_scope": "aquarium_runtime_activation",
  "runtime_implementation": "verified",
  "dolgorae": {
    "project_id": "dolgorae",
    "commit": "REQUIRED: exact producer commit",
    "development_version": "REQUIRED: exact development version",
    "canonical_executable_path": "REQUIRED: exact immutable generation path",
    "executable_sha256": "REQUIRED: exact enrolled artifact digest",
    "capability_digest": "REQUIRED: exact runtime capability digest",
    "generation_lease_evidence": "REQUIRED: stable launch lease reference"
  },
  "aquarium": {
    "commit": "REQUIRED: exact Aquarium commit",
    "installed_plugin_digest": "REQUIRED: installed plugin digest"
  },
  "independent_review": {
    "backend": "dolgorae",
    "reviewer": "codex",
    "orca_objects_created": false
  },
  "orca_review": {
    "backend": "orca",
    "immutable_target_contract": "verified"
  },
  "scope_matrix": {
    "workspace": "passed",
    "staged": "passed",
    "dirty": "passed",
    "head": "passed",
    "commit": "passed",
    "range_two_dot": "passed",
    "range_three_dot": "passed"
  },
  "mulgae_semantic_conformance": "passed",
  "capture_time_source_stable": true,
  "source_mutation_observed": false,
  "settlement_authorization_tests": "passed",
  "failure_and_recovery_tests": "passed",
  "validation_commands": ["REQUIRED: exact command and result"],
  "independent_review_evidence": "REQUIRED: stable independent-review evidence reference",
  "unresolved_blockers": []
}

If any requirement cannot be proven, return status "blocked" with the exact
requirement, evidence, and smallest remaining action instead of issuing a
Completed Confirm. Do not push or change another repository without separate
authority.
```

Acceptance required re-reading the exact Dolgorae and Aquarium commits,
revalidating the executable and installed plugin digests, comparing the
canonical executable path, development version, and artifact digest with the
enrolled generation, inspecting the lease evidence and every other stable
reference, and rerunning the bounded cross-repository compatibility checks. An
Aquarium claim alone was insufficient: missing evidence, an identity or digest
mismatch, an uncommitted Aquarium change, a documentation-only confirmation, or
any unresolved blocker would have kept the Task `BLOCKED`.

Verification: exact-generation Dolgorae gates from TASK-035; Aquarium's complete
repository validator and independent read-only review; all-scope runtime E2E;
proof that independent-review creates no Orca objects; proof that orca-review
retains Orca lifecycle ownership; capture-time source identity, post-run target
and manifest digests, and workflow source-mutation evidence; failure, recovery,
settlement, cleanup, activation, and rollback evidence; and independent Dolgorae
revalidation of the Completed Confirm.

Epic acceptance: every Task passes the ordinary completion gate; ADR-032,
checked contracts, implementation, and tests agree; the runtime Completed
Confirm binds exact committed and installed artifacts and survives Dolgorae
revalidation; and the active Aquarium independent-review path uses Dolgorae to
run a fresh Codex Reviewer without Orca objects. Completion unlocks
`MILESTONE-IR1`. A design handoff alone unlocks nothing.

## EPIC-005: Access, Interaction, and Recovery Safety

Status: `COMPLETE`

Canonical Outcomes: [access specification](../specs/README.md#spec-007-access-and-concurrency),
[lifecycle and recovery specification](../specs/README.md#spec-008-run-lifecycle-recovery-and-forking),
[interaction specification](../specs/README.md#spec-009-pending-requests-and-approvals),
[recovery architecture](../architecture/README.md#recovery-and-reconciliation),
[process-cleanup architecture](../architecture/README.md#process-cleanup),
[writer implementation](../../src/writer.rs),
[turn implementation](../../src/turn.rs), and
[worker black-box tests](../../tests/e2e/test_worker_cli.py)

Goal: Enforce Dolgorae's one-durable-writer-authority-per-worktree scope,
Controller-authorized interaction, and conservative failure semantics.

### TASK-017: Durable Writer Authority and Cross-Profile Handoff

Status: `COMPLETE`

Build on TASK-008's read-only Specialist and ordinary reader baseline.
First establish canonical `~/.dolgorae` as the only per-user configuration and
mutable-authority root used by every existing subsystem, with no alternate-root
discovery, migration, or compatibility behavior. Keep stateless version, help,
and capability discovery available.
Implement the per-worktree durable writer authority state machine, with BSD
`flock(2)` used only as a short transaction serializer, close-on-exec descriptor
hygiene, Dolgorae-home permanent-lock validation, explicit
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
The root-transition subset additionally verifies one locator across workspace,
profile, Operator, carrier, review-target, and worker diagnostics; mode-0700
creation; and absence of any additional per-user state root.
Measure the exact SPEC-007 writer turn carrier, including
`excludeSlashTmp:false` and `excludeTmpdirEnvVar:false`, against the pinned
profile and prove that a writer turn can write both the workspace and the OS
temporary directory. The TASK-000 probe campaign used the excluding variant, so
these two normative field values have no prior live evidence.
Add deterministic interleavings for the normative lock matrix and every
threadless first-write crash boundary; `acquire-write` on a threadless run is a
state conflict. No task claims OS ownership of shared App Server descendants.

### TASK-018: Pending Requests and Approvals

Status: `COMPLETE`

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

### TASK-019: Pause, Close, and Lifecycle Shutdown

Status: `COMPLETE`

Implement idle pause/resume, interrupting pause/close, immutable close,
generation-level access instruction replacement, verified socket cleanup,
start-failed bootstrap authority, terminal seals, and final-state restrictions.
Worker cleanup covers its worker, connection, and an owned Dedicated Run Server's
recorded command descendants; the shared singleton is excluded.
Live control-socket cleanup continues to use TASK-004's fail-closed primitive;
TASK-020 owns self-heal after it establishes four-verdict process identity.

Verification: idle/running/waiting pause/close matrices, interrupt terminal
deadline and outcome-unknown landing, control-v1 pause/close/recover under
binary skew, stale socket ownership, start failure before/after bound, seal crash
points, acquire/release authority transitions, and no authority release before
protocol-supported background absence; unverified execution remains blocked.

### TASK-020: Process Identity and Group Recovery

Status: `COMPLETE`

Implement four-verdict worker and Dedicated Run Server process identity,
boot-session proof, complete
provisional identity, kqueue continuity, persisted member snapshots,
`proc_listpgrppids` plus all-PID BSD parent/session census, observation across
reparent/group/session changes, fail-closed worker attachment, permanent
lock-inode rules, and no-force cleanup continuation. Treat
`CommandExecution.processId` as an opaque correlation hint.
Replace TASK-004's private boot marker with the Darwin
`kern.bootsessionuuid` provider, add the boot identity to Profile Server state,
make profile reset treat a non-matching recorded generation as absent, and add
live control-socket self-heal only when the same four-verdict proof authorizes
replacement of an existing pathname.

Verification: every identity read failure; ESRCH/live/zombie/reaped/recycled PID;
leader-first and leaderless persisted-member cleanup; new group members;
immediate command-notification census; 100-millisecond polling; TERM/5-second/
KILL and ten-second total deadlines; five complete empty samples; deliberate
setsid/reparent detection; incomplete census; inode unlink/recreate; reboot
proof; revalidated live worker control timeout returning `RUN_BUSY` with no
signal; no unrelated signal under injected PID/PGID reuse.

### TASK-021: History Reconciliation, Outcome Unknown, and Fork

Status: `COMPLETE`

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

## EPIC-006: External Specialist Engagement Hardening

Status: `COMPLETE`

Canonical Outcomes: [external engagement specification](../specs/README.md#external-specialist-engagement),
[facade architecture](../architecture/README.md#external-specialist-facade),
[ADR-034](../architecture-decision-records/README.md#adr-034-delegate-external-member-control-through-the-aggregate-owner),
[checked facade protocol](../protocol/dolgorae-external-specialist-facade-v2.schema.json),
[implementation](../../src/external_engagement.rs), and
[black-box contract tests](../../tests/e2e/test_external_engagement_cli.py)

Goal: Generalize the one-shot read-only Specialist Review Preview into a durable,
reusable external Specialist service while the external AI remains the only
semantic control plane.

### TASK-022: Reusable External Specialist Engagements

Status: `COMPLETE`

Depends on `TASK-021` and builds directly on `EPIC-003`. Remove the preview's
one-shot lifecycle restriction while preserving its trusted facade and
aggregate model. Support multiple independently hired Specialists in one
engagement, long-lived members, repeated sequential tasks per Specialist,
explicit get, cancel, release, complete, and abort, safe host reconnect,
completed-result redelivery, and exact aggregate-scoped idempotency across
Dolgorae restarts. Reconcile every accepted task through the TASK-019 through
TASK-021 lifecycle and outcome rules.

Retain one active Turn per Specialist and no implicit preemption. The external
control plane explicitly waits, retries, hires another member, or releases the
member. Add `isolated_write` only through a separate isolated workspace or
worktree policy. Canonical workspace writes require the external host to
quiesce its own writer and participate in TASK-017 writer authority. External
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

## EPIC-013: Global Codex Profile Cutover

Status: `COMPLETE`

Canonical Outcomes: [specification](../specs/README.md#spec-003-profile-account-and-singleton-binding),
[architecture](../architecture/README.md#profile-registry-and-singleton-membership),
[ADR-035](../architecture-decision-records/README.md#adr-035-replace-workspace-local-runtime-profiles-with-global-codex-profiles),
and [protocol contracts](../protocol/).

Goal: Replace workspace-local Runtime Profiles with global Codex Profiles that
provide the same explicitly selected account and execution environment to every
workspace, independent of which Codex frontend invoked Dolgorae.

### TASK-036: Global Codex Profile Contract and Hard-Cut Home Layout

Status: `COMPLETE`

Depends on `TASK-022`. Update the behavioral, architectural, and decision
authorities before implementation. Define Profile as one global Codex account
launch contract containing its native executable, canonical `CODEX_HOME`,
validated global arguments, explicit non-secret environment, and process-static
capabilities. Define Specialist Role as the separate character and instruction
concept owned by `TASK-024`.

Prepare one strict mode-0600 `~/.dolgorae/profiles.yaml` registry and the checked
post-cut command contract. Profile CRUD and diagnostics become global, remove
workspace discovery, and reject `--workspace` immediately only when TASK-038
activates the complete cutover. Keep the prepared post-cut `dolgorae init`
contract limited to portable workspace policy and workspace-scoped Run, writer,
audit, and recovery state; it creates no Profile, default account, or
workspace-local `local.yaml`.

Prepare a checked Dolgorae-home generation marker and validator. After the
TASK-038 activation, stateful commands fail closed with
`LEGACY_STATE_UNSUPPORTED` when the fixed home is unmarked, legacy, partial, or
mixed-generation. Do not inspect legacy Profiles, recover legacy Runs, migrate,
delete, overwrite, or silently initialize such a home. Stateless help, version,
and capability discovery remain available. Publish the operator-owned
backup-or-move and fresh-initialization procedure.

TASK-036 is preparatory: its new registry and home path are unreachable from
production commands, and the complete pre-cut CLI, Run, one-shot review, and
External Specialist Engagement behavior must remain usable at task completion.
An existing compatible tracked workspace policy is preserved byte-for-byte;
after activation, `init` recomputes its deterministic workspace ID and creates
fresh empty machine state with `created:true`, then returns `created:false` on
an exact idempotent repeat. Incompatible policy or machine identity fails before
mutation.

Before `TASK-023`, remove the workspace field from public-v1 ListProfiles,
GetProfile, and ListProfileDiagnostics requests and reserve each removed field
number and name. Keep workspace plus `profile_name` on StartRun. Preserve
completed v1 artifacts and introduce distinct successor contracts wherever
semantics or persisted shapes change.

Verification: global registry schema and post-cut CLI contract tests;
initialization without account selection; legacy, partial, mixed-generation,
wrong-owner, permission, symlink, malformed, duplicate, and concurrent-update
failures; descriptor and conformance checks for the corrected public-v1 request
shapes; proof that rejected legacy state is not mutated; and regression proof
that all completed EPIC-006 paths still use the available pre-cut behavior.

### TASK-037: Global Profile Runtime, State, and Immutable Run Binding

Status: `COMPLETE`

Depends on `TASK-036`. Prepare the successor Profile lookup, launch preparation,
Profile Server lifecycle, diagnostics, membership, and removal or migration
guards against the global registry. Keep this path unreachable from production
commands until TASK-038 activates every consumer together. Keep Run, writer
lease, aggregate, audit, and recovery authority isolated below each workspace
state root.

Require explicit Profile selection for every Run and store a complete immutable
resolved Profile snapshot with that Run. Never infer Profile selection from the
calling process, `PATH`, `CODEX_HOME`, frontend name, workspace, or a hidden
default. The same Profile may serve multiple workspaces, and one workspace may
use multiple Profiles. Active or unknown membership in any workspace blocks
unsafe Profile replacement, removal, Profile Server stop, or generation change.

Version every affected persisted or machine-readable contract instead of
changing the meaning of a completed schema in place. Reject legacy-home and
legacy-Run recovery rather than adding a compatibility layer.

Verification: inactive successor-path tests for cross-workspace shared Profile Server and membership;
multiple Profiles in one workspace; deterministic snapshot and server-key
tests; caller-environment isolation; cross-workspace lifecycle guards;
concurrent start, stop, remove, and generation-change races; and restart and
diagnostic behavior under the new home generation; plus regression proof that
the production path still provides the complete pre-cut behavior.

### TASK-038: Specialist Consumers and v0.1.2 Acceptance

Status: `COMPLETE`

Depends on `TASK-037`. Revalidate the prepared registry, Run path, and both
Specialist consumers, then activate the new home gate, global Profile CLI,
account-neutral `init`, Run admission, one-shot Specialist Review, and completed
External Specialist Engagement facade together in the final task-owned change.
No partially active state may be landed or committed. Move both Specialist
paths to explicit global Profile selection.
Update hire validation, immutable Agent Configuration, member persistence,
normalized idempotency input and digest, Run allocation, restart recovery,
completed-result redelivery, and machine output through versioned successor
contracts where required. Preserve aggregate-owner authorization, Controller
authority, writer coordination, immutable target capture, and the existing
external-host semantic-control boundary.

Record the handoff that `TASK-024` owns Specialist Roles, including the common
and project Role sources and Specialist Policy resolution. This Task establishes
the terminology and Profile boundary only; it does not implement Role storage,
selection, precedence, or policy.

Verification: activation-boundary proof that no production command observes a
mixed old/new state; existing compatible project rediscovery with the same
deterministic workspace ID, byte-identical policy, fresh empty state,
`created:true`, then idempotent `created:false`; fail-closed incompatible marker
and workspace-record cases; one-shot and reusable engagement regression matrices across
multiple Profiles and workspaces; same-key replay and different-input conflict;
restart, recovery, and redelivery; caller `codex` versus `codex-hsy` environment
isolation; recursive-review, nested-hire, authorization, target, and writer
denials; updated checked examples and machine schemas; and the complete
repository gate.

Epic acceptance: one global Profile has one account and launch contract, may be
selected explicitly by Runs in multiple workspaces, and behaves identically
regardless of the invoking Codex frontend. No workspace initialization or
tracked project file selects an account. Legacy state fails closed without
mutation, completed EPIC-006 behavior remains usable through the new contracts,
the public-v1 profile request boundary is ready for TASK-023, and `v0.1.2`
remains ineligible until all three Tasks complete.

## EPIC-007: Dolgorae Orchestration Control Plane and Brokered Hierarchy Core

Status: `PLANNED`

Goal: Add Dolgorae's own Primary orchestration authority and durable Brokered
Hierarchy over the hardened independent Run and Specialist foundations.

### TASK-023: Supervised Control-Plane Runtime and Minimum Gul Run Gateway

Status: `PLANNED`

Depends on `TASK-038`. Implement the production host required before any
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
`TASK-029`.

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
TASK-024, TASK-025, and TASK-026 also complete.

### TASK-024: Durable Orchestration Session and Brokered Hierarchy Core

Status: `PLANNED`

Depends on `TASK-023`. Implement the first-class `Dolgorae-Orchestrated Session` aggregate over the
independent Run core and the hardened Specialist execution path. Implement
prepared Aggregate Bootstrap Operations coupled to a parentless Primary
`StartRun` with checked Orchestration Launch Intent, the machine-local
Specialist Policy Registry, common and project Specialist Role sources, explicit
Role resolution, explicit approval policy and immutable Specialist Policy
snapshot, one-active-aggregate membership, immutable Role and Agent
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
No live Primary model tool is claimed until `EPIC-008` completes.

## EPIC-008: Live Dolgorae Control Plane and Brokered Hierarchy

Status: `PLANNED`

Goal: Select and integrate the live run-bound Primary tool transport so Gul can
use Dolgorae as the active semantic control plane with a durable Brokered
Hierarchy.

### TASK-025: Run-Bound Internal Tool Transport Probe

Status: `PLANNED`

Depends on `TASK-023` and `TASK-024`. Validate and close the live transport boundary for the
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
external `dolgorae_review` MCP adapter from `EPIC-003` is a separate external
control-plane adapter and is not blocked or redesigned by this probe.

The durable aggregate broker, Primary Orchestration Service, tool-dispatch
interfaces, and fake handlers are implemented and unit-tested in
`TASK-024`. This Task selects the supported live model-facing transport.
Mailbox, Scheduler, Activation Manager, and collaboration outbox implementation
remain in `TASK-027`.

Verification: live pinned transport probes for both run-bound tool surfaces,
source Run and Turn correlation, concurrent calls, bounded wait timeout,
cancellation, bridge restart, connection loss, and credential canaries;
ambiguous shared identity selects the Dedicated Lane fallback; public Protobuf
source and descriptor remain byte-identical.

Task acceptance: ADR-027, ADR-028, SPEC-012, architecture, both run-bound private
tool schemas, fixtures, verification index, and implementation memos agree; the
probe selects a supported bridge or explicitly blocks `TASK-026` and
`TASK-027`; and an independent read-only review reports no unresolved
blocking finding.

### TASK-026: Live Primary Orchestration Tool and Brokered Hierarchy Acceptance

Status: `PLANNED`

Depends on `TASK-023`, `TASK-025`, and `TASK-024`. Integrate only the checked Primary
orchestration tool through the transport selected by the probe. Bind session,
Primary Run, source Turn, tool-call ID, inherited root priority, Controller
authority, and idempotency outside model arguments. Allow the Primary Agent to
request, await approval for, list, assign, await, collect, cancel, and release
policy-admitted Specialists without receiving a child Controller credential or
mutating another Run directly.

Run one live integration with the actual supported Gul client against the
TASK-023 local gRPC gateway. Create an Orchestrated Session in Standalone
Primary composition, transition it to Brokered Hierarchy by provisioning a
Reviewer, execute and collect one bounded Specialist task, return at least one
Primary or Specialist result above the inline bound through an artifact
reference, retrieve its metadata and one or more bounded chunks, verify total
length and SHA-256, recover the hierarchy after a controlled Dolgorae restart,
and return to a clean completed or active state. A mock, fake adapter, or merely Gul-shaped harness cannot satisfy this
acceptance step. Specialist
messages still route through Primary task operations in this Task; lateral
Specialist collaboration is deferred to `EPIC-009`.

Verification: actual Gul client private-boundary integration;
user-approval-required and fully-delegated live paths; exact tool retry; source
identity canaries; Primary restart; Specialist task result
redelivery; missing, unauthorized, run-lifetime-expired, malformed, oversized,
out-of-range, and integrity-failed artifact reads with their documented typed
errors; Primary degradation and recovery; release and abort; writer
conflict; no credential exposure; no direct peer control; unchanged public Gul
wire; and independent review of the live hierarchy path.

Epic acceptance: completion unlocks `MILESTONE-BH1` only together with the
minimum supervised Gul gateway completed in TASK-023. Gul can use the real
local gRPC path to operate Dolgorae as the live Primary control plane, and
Dolgorae can create, persist, recover, and operate a Brokered Hierarchy. Lateral Specialist collaboration is not yet part
of this milestone.

## EPIC-009: Brokered Specialist Collaboration

Status: `PLANNED`

Goal: Add durable bounded Specialist-to-Specialist collaboration to one active
Brokered Hierarchy without making the Primary Agent a message relay.

### TASK-027: Durable Mailbox, Virtual Actor, and Collaboration Plane

Status: `PLANNED`

Depends on `TASK-023`, `TASK-025`, `TASK-026`, and `TASK-024`. Integrate the checked
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

## EPIC-010: Operator and Audit Interfaces

Status: `PLANNED`

Goal: Complete the Controller-facing operational surface and make every durable Run
independently inspectable.

### TASK-028: Status, Events, Results, and Change Observation

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

### TASK-029: Complete Gul gRPC Surface and Extended Operational Conformance

Status: `PLANNED`

Depends on `TASK-023`. Extend the already operational foreground
`dolgorae serve` gateway from the 24-method BH1 set to all 34 methods in the
frozen `dolgorae.public.v1` descriptor. Add the ten deferred RPCs covering
profile diagnostics, Controller timeline, default-effort, fork,
verification, deletion, write continuation, writer handoff, and their complete
safe projections. Complete bounded independent Run streams, exhaustive typed
`google.rpc.Status` details, advanced cancellation behavior, and all remaining
operator-safe conformance without adding Operator RPCs, TCP, client-streaming,
bidirectional streaming, worker sockets, or App Server transports.

The task MUST preserve the TASK-023 process, socket, peer-UID,
ControlPlaneRuntime, and semantic-service ownership model. `GetCapabilities`
continues to advertise only implemented methods until this Task completes, then
advertises the complete public-v1 descriptor method set required by
`MILESTONE-PA1`.

Verification: deterministic fake-semantic-service tests for every remaining
unary and stream method; full method-kind/descriptor golden tests;
32-envelope/4-MiB/5-second pressure boundaries; independent Run streams on one
channel; continuation lineage; timeline redaction and image metadata; artifact
regression coverage; advanced writer handoff; deletion and verification;
and the exhaustive typed error map. Re-run the TASK-023 socket, restart,
carrier TOCTOU, allocation-loss, Interaction-loss, and secret-canary tests as
regressions. All timing uses injectable clocks and no test binds TCP.

### TASK-030: Verify, Export, and Confirmed Delete

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

### TASK-031: Agent Governance and Process Cleanup

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

Epic acceptance: every public command and audit workflow in
`docs/specs/README.md` is
available against the deterministic fake environment.

## EPIC-011: Conformance and v0.2.0 Personal Alpha Release

Status: `PLANNED`

Goal: Establish `v0.2.0` release evidence for the supported Apple Silicon macOS
and two real Codex targets, completing the first customer-supported Personal
Alpha release.

### TASK-032: Deterministic Protocol Conformance Suite

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

### TASK-033: Crash, Concurrency, and Security E2E

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

### TASK-034: Two-Profile Live Smoke and Alpha Acceptance

Status: `PLANNED`

Run opt-in live smoke tests against prepared primary and secondary profiles
using the checked 0.153.4 compatibility baseline (or a separately migrated
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
`excludeSlashTmp:false` and `excludeTmpdirEnvVar:false`. If any required dedicated-lane behavior fails, TASK-034 and
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

Epic acceptance: mark the `v0.2.0` Personal Alpha ready only after TASK-034 and
the full Task completion gate are satisfied. This is the first release eligible
for customer support; earlier `v0.1.x` milestone previews do not inherit that
claim. TASK-034 alone owns the transition of the checked manifest's
`production_runtime_eligible` field from false to true and must leave it false
on any missing, failed, unverified, or stale production campaign. TASK-000-D
owns only `architecture_contract_eligible`.

## EPIC-012: Development Aquarium Producer

Status: `COMPLETE`

Canonical Outcomes: [producer Make targets](../../Makefile),
[producer implementation](../../tools/dev_aquarium/producer.py), and
[black-box contract tests](../../tests/e2e/test_dev_aquarium_producer.py)

Goal: Enroll Dolgorae as an exact executable producer for Aquarium's isolated
development channel so downstream review activation consumes one immutable,
checksummed, leased generation instead of a mutable repository build path.

### TASK-035: Produce an Exact Dolgorae Development Generation

Status: `COMPLETE`

Depends on `TASK-015`. Add the standard `aquarium-dev-describe` and
`aquarium-dev-build` Make targets. The descriptor identifies project
`dolgorae`, next version `v0.1.0`, artifact kind `executable`, and artifact path
`bin/dolgorae`. TASK-035 depends only on TASK-015, never on TASK-016 or Aquarium
TASK-024. The builder accepts one absolute, caller-created empty
`AQUARIUM_DEV_OUTPUT`, requires a clean local `main`, confines every Cargo and
producer output to that staging root, and writes exactly the executable plus
`manifest.json`.

The descriptor and manifest use the exact closed field sets and newline-delimited
JSON encoding of `aquarium-dev-producer-description/v1` and
`aquarium-dev-artifact-manifest/v1`. The manifest binds the exact source commit,
`v0.1.0-dev.<12-hex-prefix>` version, artifact kind and path, and
`sha256:<lowercase-hex>`. It must reject missing, relative, non-directory,
non-empty, symlink-root, or physically repository-contained output roots and must
not write into the repository or the stable user environment. A bounded locked
release build exports and verifies the exact HEAD tree into staging, builds only
that immutable snapshot with staging-local Cargo state, supervises the entire
build process group, handles repeated interruption, removes partial output, and
publishes the manifest atomically last. `v0.1.0-dev.<sha12>` is a local channel
generation identifier derived from the canonical Cargo package version; it is
not a stable release or a claim that SemVer precedence follows the existing
0.1.0 candidate.

Verification: focused black-box tests cover descriptor bytes, every output-root
rejection, dirty or non-main repository rejection, isolated release build,
manifest identity and checksum, exact output inventory, and repository
non-mutation; the generated executable's runtime capability digest equals the
completed TASK-015 contract digest; the complete repository gate and one
pre-existing Mulgae read-only review pass without consuming TASK-024 or the new
generation; one task-scoped commit exists without push. Completion hands the exact
clean commit to Aquarium TASK-024 for enrollment and publication. It does not
itself install, activate, or release Dolgorae.

Epic acceptance: TASK-035 passes its ordinary completion gate, the producer
contract is promoted to its durable owner, and the temporary dossier is removed
in the approved closeout commit. The Epic completes without push, installation,
or Aquarium mutation.
