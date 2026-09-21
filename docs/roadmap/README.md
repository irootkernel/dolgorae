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
`EPIC-007`, `TASK-023`, and `TASK-024` are `COMPLETE`; they establish the
supervised gateway and transport-independent Brokered Hierarchy core without
claiming a live model-facing Primary tool. `EPIC-014` and `TASK-039` through
`TASK-045` are `COMPLETE`; the Epic precedes `EPIC-008` in delivery order.
`EPIC-015` and `TASK-046` are `COMPLETE` as the detached-process corrective
boundary immediately before `EPIC-008`.
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
Brokered Hierarchy core, EPIC-014, and EPIC-015 are complete. The revised
EPIC-008 delivers the live provider boundary `MILESTONE-BH1-P` through twelve
Tasks, including the Gul consumer contract, complete prompt history, read-only
aggregate/result queries, and frozen-consumer regression. TASK-025, TASK-053,
and TASK-047 are complete; TASK-048 is the next planned Task. `v0.1.3` remains owned by EPIC-008; actual Gul
acceptance is tracked separately and MUST NOT be inferred from provider QA.
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
| `MILESTONE-BH1-P` | `EPIC-008` | Dolgorae operates a live Primary and durable Brokered Hierarchy with the 27-method Gul consumer profile, complete prompt history and whole-session closure, independently verified without actual Gul. |
| `MILESTONE-BH1` | [Deferred Gul acceptance](../todo/README.md#actual-gul-consumer-acceptance) | Actual Gul integration is verified against the provider; provider QA alone does not unlock this milestone. |
| `MILESTONE-BC1` | `EPIC-009` | Specialists in one Brokered Hierarchy can use durable bounded lateral collaboration without Primary message relay. |
| `MILESTONE-PA1` | `EPIC-011` | The complete Personal Alpha acceptance campaign passes. |

Delivered provider capabilities are cumulative. An earlier capability remains
usable while later Epics are implemented. `MILESTONE-BH1` is a separate consumer
acceptance branch: neither BH1-P nor later provider work implies actual Gul QA.
A milestone does not waive its own Task completion gate or any safety limitation
stated in its owning Epic.

## Release Train

| Version | Classification | Required completion boundary | Cumulative product milestones |
| --- | --- | --- | --- |
| `v0.1.0` | Integration Preview | `EPIC-004` complete; completed `EPIC-012` development producer included without extending product scope | `MILESTONE-SR1`, `MILESTONE-IR1` |
| `v0.1.1` | Root Transition Preview | The fixed-home prerequisite from `TASK-017`; the full Task completes in the `v0.1.2` cycle | `MILESTONE-SR1`, `MILESTONE-IR1` |
| `v0.1.2` | Milestone Preview | `EPIC-006` and `EPIC-013` complete, including the preceding `EPIC-005` safety layer | Through `MILESTONE-ES1` |
| `v0.1.3` | Milestone Preview | All twelve revised `EPIC-008` Tasks complete, including `dolgorae.gul-consumer/v1`; preceding `EPIC-007`, `EPIC-014`, and `EPIC-015` included; real Gul is not a prerequisite | Through `MILESTONE-ES1`, plus `MILESTONE-BH1-P`; excludes actual-Gul BH1 acceptance |
| `v0.1.4` | Milestone Preview | `EPIC-009` complete on the verified provider boundary | v0.1.3 provider capabilities plus `MILESTONE-BC1`; no automatic Gul acceptance claim |
| `v0.2.0` | Personal Alpha and first customer-supported release | Every currently planned product Epic from `EPIC-005` through `EPIC-011` plus `EPIC-013` complete, including `EPIC-010` operator and audit interfaces | Through `MILESTONE-PA1` |

The `v0.1.x` releases are cumulative previews and do not claim Personal Alpha
readiness, the complete target specification, or customer support. Read-only
Podway observation is a separate post-v0.1.3 candidate, not automatically part
of v0.1.4 or v0.2.0. A completion boundary makes a version eligible for release; it does not itself create a
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

This general gate does not prescribe how an implementer divides commits or
stages files. EPIC-008 additionally requires one initial completion commit per
Task as described in its adopted execution rules below. Push always requires
separate explicit user authorization.

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

Status: `COMPLETE`

Canonical Outcomes: [orchestration specification](../specs/README.md#spec-012-orchestration-boundary-and-compatibility),
[gateway specification](../specs/README.md#spec-015-supervised-local-grpc-and-gul-integration),
[architecture](../architecture/README.md),
[checked orchestration tool protocol](../protocol/dolgorae-orchestration-tool-v1.schema.json),
[gateway implementation](../../src/gateway.rs),
[orchestration implementation](../../src/orchestration.rs), and
[native gateway tests](../../tests/gateway_native.rs)

Goal: Add Dolgorae's own Primary orchestration authority and durable Brokered
Hierarchy over the hardened independent Run and Specialist foundations.

### TASK-023: Supervised Control-Plane Runtime and Minimum Gul Run Gateway

Status: `COMPLETE`

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
a controlled gateway restart. The minimum gateway alone claims neither BH1-P nor actual Gul acceptance.
BH1-P additionally requires TASK-024 and the revised EPIC-008 consumer-profile Tasks;
actual-Gul BH1 retains its separate consumer gate.

### TASK-024: Durable Orchestration Session and Brokered Hierarchy Core

Status: `COMPLETE`

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

## EPIC-014: Role/Task Separation and Structured Completion Review

Status: `COMPLETE`

Canonical Outcomes: [external engagement specification](../specs/README.md#external-specialist-engagement),
[one-shot review specification](../specs/README.md#one-shot-specialist-review-adapter),
[facade architecture](../architecture/README.md#external-specialist-facade),
[ADR-036](../architecture-decision-records/README.md#adr-036-separate-stable-specialist-roles-from-accepted-task-content),
[one-shot v3 protocol](../protocol/dolgorae-specialist-review-tool-v3.schema.json),
[facade v3 protocol](../protocol/dolgorae-external-specialist-facade-v3.schema.json),
[task implementation](../../src/task_request.rs),
[one-shot implementation](../../src/review.rs), and
[facade implementation](../../src/external_engagement.rs)

Goal: Keep reusable Specialist character independent of assigned work, make
accepted inline context model-readable and durable, and add criterion-complete
review results without changing the v1/v2 contracts or the EPIC-008 release
boundary. This Epic precedes EPIC-008 only in delivery order. It does not enable
Aquarium, MCP, a public gRPC change, a stable release, or an installed runtime.

### TASK-039: Freeze Role/Task and Completion-Review Contracts

Status: `COMPLETE`

Synchronize the specification, architecture, ADR, checked protocol, and roadmap
for additive Specialist Review and External Specialist Facade v3 contracts.
Fix request and report bounds, exact string preservation, accepted-task JCS
identity, inline-context provenance, completion criteria, deadline start,
recovery meaning, and v1/v2 compatibility. Do not advertise the capability as
released or runtime-installed before the later completion gate.

Verification: schema meta-validation and positive/negative examples; authority
and compatibility review across the owning documents; unchanged public v1
Protobuf source and descriptor.

### TASK-040: Stable Reviewer Role and Accepted Task Composition

Status: `COMPLETE`

Remove task objective bytes from the Reviewer Role and Agent Configuration.
Treat hire objective as non-executable rationale, construct one shared bounded
accepted-task type, preserve exact multiline UTF-8, and persist and digest the
decoded request as JCS before dispatch. Different tasks and hiring rationales
must retain the same stable Reviewer configuration identity.

Verification: configuration-digest invariance, exact Unicode/line-ending and
shell-metacharacter preservation, NUL and bound rejection, and pre-dispatch
failure tests.

### TASK-041: Inline Context Binding and Candidate Separation

Status: `COMPLETE`

Validate at most 64 contexts with unique IDs, caller-declared provenance, and
actual bounded content. Permit criteria to reference only accepted context IDs.
Bind context to the durable task before the Turn and present it as evidence in a
separate prompt section from the immutable review candidate. Reject arbitrary
host paths, bare artifact references, mutation, and oversize instead of
truncating or resolving them implicitly.

Verification: readable-context delivery, unknown/duplicate reference rejection,
candidate/context distinction, accepted-byte persistence, and retry conflict
tests.

### TASK-042: Additive v3 CLI and Reusable Facade Execution

Status: `COMPLETE`

Add `specialist review --request-stdin --format json` for the checked v3 request
and add the facade v3 assignment shape while preserving all legacy carriers.
Route both through existing create/assign/collect/close semantics, keep the
one-shot Reviewer fresh and read-only, bind Profile and credentials outside the
request, and start the v3 deadline at durable task acceptance.

Verification: stdin and option-conflict tests; staged and HEAD black-box
reviews; one fresh Reviewer/Turn; reusable assignment, idempotency, deadline,
disconnect, and authority regressions.

### TASK-043: Criterion-Complete Result and Recovery

Status: `COMPLETE`

Implement `dolgorae-specialist-review-result/v3` with ordered criterion
assessments, four checked statuses, bounded candidate/context/caller/unavailable
evidence, remaining gaps, evidence limits, and overall assessment. Validate the
report before immutable artifact commit and preserve the same report in caller
projection and redelivery. Never synthesize completion from prose or empty
findings.

Verification: every status and evidence basis; missing, duplicate, unknown, and
reordered criteria; malformed or oversized output; artifact/collection round
trip; crash and result-redelivery tests.

### TASK-044: Compatibility and Adversarial Regression

Status: `COMPLETE`

Prove unchanged v1/v2 request, result, and persisted-state meaning together with
v3 idempotency, deadline, cancellation, crash, disconnect, staged/committed
candidate, and adversarial task-content behavior. Confirm public gRPC descriptor
identity and the existing MCP-unavailable disposition. Do not run live Codex
targets without separate explicit authority.

Verification: focused Rust/schema/E2E suites, fake-runtime payload canaries,
legacy fixtures, outcome-unknown and settlement recovery, and the complete
default repository gate.

### TASK-045: Documentation, Full Gate, and Independent Review

Status: `COMPLETE`

After implementation and compatibility checks, update README, operations,
implementation tips, and the source-distributed `use-dolgorae` skill to the
actually supported v3 contract. Run the complete deterministic gate and obtain
an independent read-only review. Resolve every blocking finding before moving
the Tasks and Epic through their completion states; task-scoped commits remain
required by the ordinary completion gate.

Verification: `make PYTHON_BIN=.venv/bin/python test`, source skill validation,
Markdown links, independent review, no unresolved blocking findings, and exact
task-scoped commit evidence.

Epic acceptance: Role and Agent Configuration identity is stable across tasks;
one-shot and reusable v3 assignments preserve and deliver exact accepted task
and context; criterion-complete reports survive artifact collection and
recovery; legacy contracts remain compatible; default gates and independent
review pass; and no Aquarium, MCP, public gRPC, release, installation, or
publication state is changed by this Epic.

## EPIC-015: Detached Process Ownership Recovery

Status: `COMPLETE`

This corrective Epic precedes live Primary control-plane work. It addresses
processes stranded when disposable development or E2E owner homes are removed,
without changing ordinary shared Profile Server lifetime or the broader
managed-session cleanup planned in TASK-031.

### TASK-046: Register and Reap Verified Dolgorae Orphans

Status: `COMPLETE`

Add private boot-scoped registrations for shared servers, log drainers,
dedicated servers, and workers; retire them on ordinary verified shutdown.
Expose a versioned inspect/digest/cleanup CLI with exact identity and group
checks, explicit selectors, and fail-closed ambiguity. Make E2E teardown
remove temporary homes and then verify and clean only its own registered
orphans. Do not infer orphan status from PPID 1, signal pre-registration legacy
processes, or stop healthy shared servers on client exit.

Verification: focused CLI/schema/identity tests, deletion and process-group
regressions, repeated E2E execution with no surviving new test processes, the
complete default repository gate, and independent adversarial review before
the Task and Epic become `COMPLETE`.

## EPIC-008: Live Dolgorae Provider and Brokered Hierarchy

Status: `ACTIVE`

Detailed SOT: [EPIC-008 execution dossier](../todo/EPIC-008-live-provider.md)

Goal: Connect the existing gateway and durable Broker to actual pinned Codex
Primary/Specialist execution and deliver a verified public provider interface.
Complete `MILESTONE-BH1-P` without waiting for Gul. Actual Gul consumer QA stays
outside this Epic and does not become a blocked placeholder Task.

Authorities: [provider slice](../specs/README.md#v013-live-provider-slice),
[provider acceptance](../specs/README.md#provider-and-gul-acceptance-boundaries),
[live integration architecture](../architecture/README.md#live-provider-integration-boundary),
[Gul consumer contract](../specs/gul-consumer-v1.md),
[ADR-038](../architecture-decision-records/README.md#adr-038-deliver-the-live-provider-independently-of-gul),
and [ADR-039](../architecture-decision-records/README.md#adr-039-freeze-the-gul-consumer-contract-before-v013).

Scope: both approval modes; trusted tool identity; ready-member task acceptance,
actual dispatch, bounded wait, deadline/cancellation; admitted access and writer
safety; readable checked results/artifacts; redelivery; release/abort; and
restart recovery; complete Controller timeline and original prompt history;
sequential human input without queue/steering; public aggregate/result reads;
whole-session closure; and frozen-consumer compatibility. Preserve existing
external v1/v2/v3 review behavior.

Non-goals: Gul code or UI, busy-target queues, lateral collaboration, mailbox
scheduling, activation/passivation, new generic task/artifact frameworks,
multiple supported internal transports, Podway observation/control, the nine
remaining TASK-029 methods, and release/publication/installation operations. Unsupported live policies fail
closed under the provider slice rather than silently losing future semantics.

Execution order is exactly TASK-025, TASK-053, TASK-047, TASK-048, TASK-049,
TASK-050, TASK-051, TASK-054, TASK-055, TASK-052, TASK-056, TASK-026. Four new
Tasks cover the approved consumer scope; completed TASK-023/024/025 remain
complete. TASK-053 is the contract-ready handoff for Gul mock/UI development;
actual integration waits for the separately released v0.1.3 artifact. Each Task
contains its implementation, targeted tests, directly affected contracts/docs,
and independent review before one initial task-scoped completion commit.
The general completion gate still applies. Fix task-local crash/security
behavior before that commit; later Tasks do not excuse unsafe intermediate
behavior. Incomplete paths remain explicitly unavailable, never placeholder
success. A later discovered defect gets explicit corrective ownership without
rewriting completed history to preserve a count. Planning adoption and stable
release metadata are separate operations, not extra implementation Tasks.
TASK-025 is `COMPLETE`; its private transport selection and implementation
handoff remain accepted. TASK-053 must publish the checked consumer contract
before TASK-047 begins. This planning amendment neither implements the new
wire nor activates another Task.
No staging, commit, live credentials, publication, or installation is authorized
merely by this plan; use the existing applicable approval boundaries.

### TASK-025: Pinned Transport Probe and Live Provider Contract

Status: `COMPLETE`

Freeze the internal live-Primary contract in the specification and architecture:
accepted execution identity, Worker/Broker routing, business-rejection replay,
and layered authorization. Production wiring of those boundaries remains with
TASK-047 through TASK-051. The campaign pin is the locally installed Codex CLI;
the product compatibility baseline remains 0.153.4.

Depends on `TASK-023`, `TASK-024`, `EPIC-014`, and `EPIC-015`. Start here.
Compare private MCP and native run-bound tool candidates on the checked Codex
pin and select one supported path. Prove source Run/Turn/call binding, stable
retry identity, concurrent-call separation, cancellation, bounded wait,
connection loss, and bridge restart without model-controlled identity or
credentials. Verify Dedicated Lane requirements when shared identity is
ambiguous; isolation is not a substitute for Turn/call proof. The
future-collaboration portion checks only source Run/Turn/call binding through an
isolated inert stub. Temporary test-only registration is allowed when needed
for that proof. Production Specialist collaboration tool registration,
advertisement, operation handlers, mailbox, and scheduler implementation belong
to EPIC-009. This limit does not reduce the Primary transport proof above.
Preserve the separate external review MCP disposition. Do not upgrade Codex
implicitly.

Freeze the selected minimal private request/response and result-read contract,
including text/context bounds, accepted-call replay versus new observation,
`blocking`/wait semantics, busy and unsupported-policy error mapping, and
Primary-authorized artifact consumption. Preserve existing v1 meanings; add a
narrow private successor only where required. Register no incomplete live
capability. Synchronize owning docs, checked schemas/examples, and the
verification index before later implementation. Record the exact pin and
sanitized verdicts without retaining credentials, raw provider prose, or local
runtime identifiers in tracked docs.

Verification: deterministic positive/negative schema and trust-boundary tests;
separately authorized isolated pinned-live probes with an explicit adversarial
budget; unchanged public Protobuf source/descriptor; independent read-only
review. If no candidate meets the contract, this Task is `BLOCKED` and later
Tasks cannot start. A negative result does not justify fake-only release.
Plan approval does not authorize a Profile/account or token use. Required live
execution needs separate approval and isolated test state; missing live evidence
prevents completion, even when deterministic probes pass.

Task acceptance: one proved transport and a complete bounded contract for the
remaining Tasks, including executable probe/fixture code and no unresolved
blocking findings. Next: TASK-053 under the approved consumer amendment.

### TASK-053: Freeze the Gul v0.1.3 Consumer Contract

Status: `COMPLETE`

Depends on completed `TASK-025`. Publish the checked wire and fixtures for
[the consumer specification](../specs/gul-consumer-v1.md). Add only
GetOrchestratedSession and ListOrchestratedSessionResults and their typed
messages to public v1. Preserve all existing field numbers and low-level
meanings. Freeze the required 27-method profile over the extended 36-method
descriptor. Leave unimplemented methods unavailable in runtime capabilities.

Close exact message/enum numbers, cursor and byte limits, authorization,
aggregate revision/count meanings, typed errors, result artifact owner and
whole-session CloseRun semantics. Preserve TASK-025 private receipts/waits/reader
and its selected transport. Correct advertised credential-schema SHA-256 values
and verify them against the actual distributed file bytes. Publish an immutable
source revision with schema/descriptor/policy/fixture digests, generated clients
and a reproducible lock. A target document or a floating worktree is not a lock.

Verification: pre-extension baseline Buf breaking check, old low-level client
compatibility, complete 27-method positive/negative fixtures, unsupported-method
classification, schema-byte digest equality, bounds/authorization/redaction,
and independent read-only review. No live runtime or Gul is required for this
contract publication. Commit/publication permission remains separate.

The primary freeze risk is aggregate-aware CloseRun. Bind the consumer contract's
bounded-call outcome table exactly: SESSION_CLOSE_IN_PROGRESS versus confirmed
RunMutationResponse, durable operation_id across response/detail/query/restart,
transport ambiguity, method-specific forbidden retry and root reconciliation.
Use the existing operation_id fields; do not turn pending closure into gRPC OK.
Add fixtures for intent-before-response loss, concurrent closes, mismatched
interrupt choices, unknown child effects and already-settled sessions.

Publish a checked field-sourceability matrix for both new queries. Each field,
count, lifecycle/recovery value and artifact reference names its existing or
planned durable owner, state predicate, revision boundary, authorization rule,
fixture and implementing Task. Private producer stores are allowed sources;
private consumer access, model-text interpretation and read-side repair are not.
Unowned required fields block freeze. Do not require downstream runtime Tasks
complete merely to prove a planned durable design.

Task acceptance: Gul can pin exact checked artifacts and develop pre-release
mocks/UI without unresolved provider shapes, close mappings or field sources.
No new runtime implementation is claimed. Next: TASK-047.

### TASK-047: Trusted Live Primary Tool Bridge

Status: `COMPLETE`

Depends on `TASK-053` and the completed TASK-025 transport. Implement the selected bridge through the production
Primary Orchestration Service. Bind session, source Run/Turn/call, current
authority, inherited priority, and idempotency outside model arguments. Use
existing durable call/reuse receipts. Separate generation fencing from semantic
retry identity; a reconnect must not manufacture new work. Keep registration
Run-scoped without mutating a shared global Profile. Use existing process
ownership rules if the selected bridge requires a process. Only fully wired
operations may execute; incomplete operations reject before effects.

Verification: source substitution, stale generation, duplicate/different-input
call, concurrent call isolation, cancellation/disconnect, and bridge restart;
credential/socket/database/raw-frame canaries; focused pinned-live dispatch
check as required by the completion gate. No collaboration registration or
external-review MCP change.

Completion evidence includes the isolated pinned Codex 0.153.4 campaign: an
actual Primary Turn completed one `list_specialists` call, the durable result
retained trusted Run/Turn/call and idempotency bindings in the versioned
envelope, and no shared Profile state was mutated. The focused maintainability
confirmation review reported all four requested structural fixes resolved with
no direct regression.

Task acceptance: the actual Primary tool reaches the existing service with
verified authority and replay behavior, while unsupported effects remain
unavailable. Next: TASK-048.

### TASK-048: Brokered Run Provisioning and Approval Binding

Status: `PLANNED`

Depends on `TASK-047`. Connect the existing spawn operation and preallocated
child identity to real semantic Run/Worker/thread creation and the fixed Role,
Agent Configuration, global Profile, and admitted access. Reuse protected
broker-held Controller carriers. Implement both approval policies. Connect
broker-originated Primary Interactions to their durable spawn operation through
the shared Controller resolution path; distinguish them from Codex requests.
Apply the v0.1.3 provider slice rather than the general target reuse algorithm:
admit only `never` and `reuse_idle_compatible` for live reuse. Reject unsupported
live queue/collaboration/activation policies before Session allocation; do not
implement busy/mail-count selection. Preserve durable reuse receipts, exact
replay, raw managed-Run rejection, and reserved-parent rejection.

Verification: approval before allocation, reject/approve/retry and response loss,
registry/snapshot invariance, cardinality and allowlist denial, duplicate spawn,
credential canaries, cross-store publication failure and known/unknown recovery.
Use real adapters with isolated fake Codex for deterministic cases and the
required separately authorized live behavior checks.

Task acceptance: policy-admitted actual Specialists can be created once through
either approval mode without changing existing aggregate ownership. Next:
TASK-049.

### TASK-049: Accepted Task Admission and Live Dispatch

Status: `PLANNED`

Depends on `TASK-048`. Connect separate work assignment to existing ready
Specialists. Reuse applicable task-content helpers without making the External
Facade the Broker backend or requiring review criteria for ordinary work.
Preserve exact bounded multiline UTF-8 and bind authorized artifact context to
readable immutable bytes. Validate policy/member access before task reservation
or writer effects. Replay an existing accepted request before applying fresh
busy admission; refuse new busy assignments without queueing or auto-hiring.

Persist accepted request identity and deadline origin before effects. Separate
Turn acceptance from completion instead of synchronously returning
`CompletedTask`. Add only the necessary bounded runtime observer and durable
acceptance evidence. No transaction/global mutation lock spans execution.
Connect admitted writes to existing writer/isolated-root mechanisms now;
unsupported active-Primary writer yield is a typed pre-effect conflict, not an
implicit interrupt. Never claim atomic cross-Controller handoff.

Verification: Role/task digest separation, Unicode/CRLF/context bounds and
integrity, forged access, same-key replay/conflict, concurrent admission, busy
rejection, independent members, correct working root/sandbox, writer conflicts,
acceptance-response loss, pre/post-dispatch crash, and no automatic replay after
unknown acceptance. Preserve external review and reusable engagement contracts.

Task acceptance: actual tasks are durably accepted and dispatched once with
correct authority and traceable Turn acceptance. Next: TASK-050.

### TASK-050: Bounded Wait, Durable Deadline, and Cancellation

Status: `PLANNED`

Depends on `TASK-049`. Implement the selected tool contract's operation/task
waits, `any`/`all`, and `blocking` behavior with a bounded transport budget.
Serve approvals, cancellation, and other Run observations while waiting. Anchor
execution expiry to durable acceptance across retry/restart; the transport wait
neither refreshes that deadline nor cancels work on timeout/disconnect. Exact
call retry returns its original receipt; a new call observes new state.

Handle known pre-dispatch cancellation and ordinary running-Turn interrupt with
authoritative terminal proof. Preserve `interrupted_unknown` when acceptance or
outcome is ambiguous. Do not implement mailbox priority, queueing, or activation.

Verification: injected-clock boundary tests, `any`/`all`, same-call/new-call
behavior, approval responsiveness, wait disconnect, remaining budget after
restart, cancel/complete/expiry races, and interrupt acknowledgement without
terminal proof. New live/OS assumptions require their designated empirical
checks before completion.

Task acceptance: waits are responsive and bounded, deadlines survive retries,
and cancellation never manufactures a known outcome. Next: TASK-051.

### TASK-051: Readable Results, Artifacts, and Redelivery

Status: `PLANNED`

Depends on `TASK-050`. Connect terminal observation to the accepted request,
applicable output validator, existing immutable artifact store, and Broker
completion/delivery receipts. Verify accepted-request integrity before output
discriminator selection. Preserve existing v3 structured review when requested;
do not make it mandatory for general work. Implement the minimal checked
Primary result/read contract selected by TASK-025, including a permitted public
result projection without exposing child credentials or arbitrary child files.

Completion must reference real bytes and verified length/SHA-256. Coordinate
artifact and SQLite publication with write-ahead recovery, not imaginary
cross-store atomicity. Preserve collect cursors and exact-call/reuse receipts.
Lost delivery returns the same result, not a new target Turn. Persist the
recoverable association to permitted Primary-owned artifacts for TASK-055's
public discovery query. Do not fabricate a Primary final response to carry
Specialist results. No new generic artifact service or change to TASK-053's
frozen public descriptor.

Verification: actual Primary content consumption; above-inline-bound Specialist
results; metadata/chunk reads, length/digest, authorization, redaction, range,
missing/expired/malformed/oversized/integrity failures; corrupted accepted task
and discriminator downgrade; artifact/receipt crash windows; page replay and
later-page delivery without repeat execution.

Task acceptance: Primary and its authorized client can consume actual Specialist
results through the intended private/public boundaries. Next: TASK-054.

### TASK-054: Complete Controller Timeline and Durable Prompt History

Status: `PLANNED`

Depends on `TASK-051`. Move complete ListRunTimelineItems implementation from
TASK-029 into v0.1.3. Reuse the ledger/timeline/artifact owners. Support every
accepted safe item kind, Controller checks, stable Run/Turn/item identity,
original UTF-8 text and line endings, exclusive cursor and bounded captured-head
pagination. Persist accepted user input before submit acknowledgement. Long
input uses the existing Controller-only artifact path; safe image metadata does
not retain source paths or image bytes.

Reject fresh ordinary Primary input during an active Turn before acceptance or
effects. Exact accepted-request replay precedes fresh-busy admission. No queue,
steering, auto-send or auto-interrupt; current Interaction answers remain usable.
An explicit interrupt needs terminal/recovery proof before another submit.
History is retained after failure/interruption/close and is not replay authority.

Verification: complete timeline kinds, multi-page/concurrent append, Korean and
emoji/CRLF, above-inline input, response-loss/restart at acceptance boundaries,
same-key replay versus same-text new request, concurrent browsers, busy rejection
without new history, closed-session recovery and secret/reasoning exclusion.
Advertise controller_timeline only after the complete contract passes.

Task acceptance: a public client restores ordered original prompts and safe
conversation history after restart and close. Next: TASK-055.

### TASK-055: Public Orchestrated Session and Result Observations

Status: `PLANNED`

Depends on `TASK-054`. Implement TASK-053's two read-only aggregate queries through
the real public gateway and shared semantic/Broker layer. Authenticate the root
Controller; return consistent lifecycle, independent aggregate revision, policy
identity, defined counts, close/recovery disposition. No read-side repair,
Worker startup or result acknowledgement. Low-level roots do not become sessions.

List stable publication records from TASK-051 with bounded session/head-bound
paging and explicit Primary-owned ArtifactRef/RunRef values. Retain published
results after private collection and session close. Clients discover references
through this query, not model prose or private fixture hooks. Query refresh must
cover aggregate-only changes without relying on a Primary event for each one.

Verification: foreign/wrong-Controller/non-session denial, zero versus unavailable
counts, coherent revision snapshots, malformed/foreign paging, concurrent
publication/restart, public discovery then metadata/chunks/digest, no child
credential leak, no mutation-on-read, original Run/external facade compatibility.

Task acceptance: public clients can observe session state and discover permitted
results without reconstructing aggregate authority. Next: TASK-052.

### TASK-052: Live Hierarchy Retirement and Restart Recovery

Status: `PLANNED`

Depends on `TASK-055`. Connect graceful release, session completion/abort, and
cross-component restart to existing lifecycle and recovery owners. Stop new
admission while retiring; retain accepted/unknown work and undelivered results
until their authoritative disposition. Reconstruct bridge bindings, pinned
Run/Profile/thread identity, access, isolated working roots, deadline, writer
state, and delivery receipts after gateway/Worker replacement. Reuse TASK-046
process identity and orphan rules. A client disconnect does not stop a healthy
shared Profile Server. Do not add unsolicited Primary Turns or auto-resume
paused Runs.

For an authoritatively registered Orchestrated root, CloseRun terminates the whole
owned aggregate under the consumer contract. Reject interrupt=false while owned
work/spawn/approval is active; interrupt=true is explicit confirmed intent.
Persist close intent before effects, serialize against new admission and
in-flight spawn/dispatch/publication, and retire children only through the Broker.
Unknown effects prevent successful closed state. Preserve history/results/files,
unrelated sessions and healthy shared Profile Servers. Root Recover/Reconcile
must account for retained aggregate close intent when needed, without auto-resume
or a new Primary Turn. Bounded callers reconcile through the read-only aggregate
query and fresh Run/Writer/Interaction reads; no blind tokenless retry. Primary
Pause/Interrupt remain Primary-scoped, not aggregate pause or Podway control.

Verification: controlled and crash restart during approval, dispatch, execution,
result publication/delivery, and retirement; original Run/thread identities;
no duplicate/orphan child; writer and credential-carrier lifetime; unknown
preservation; isolated-root recovery failure; clean verified test teardown.
These supplement, rather than defer, prior Task-local recovery checks.

Task acceptance: the connected live hierarchy safely retires or reconstructs
its existing durable work without semantic replay, including whole-session
close from active, waiting, degraded and restart-interrupted states.
Next: TASK-056.

### TASK-056: Frozen Consumer and Cross-Version Regression

Status: `PLANNED`

Depends on `TASK-052`. Run the exact TASK-053 generated consumer and fixtures
without regeneration against the candidate. Keep a pre-extension low-level
consumer too. Integrate explicit immutable-baseline Buf breaking checks,
advertised schema-byte digest equality and behavioral scenarios into the normal
deterministic gate. Matching current source and descriptor is not sufficient.

Cover all required methods, sequential submit/history, session/result paging,
whole-session close/recovery, event variants/stamps/cursors, missing required
versus added optional capabilities, unknown decisive enums/errors and protected
boundaries. Define explicit verified descriptor compatibility for later additive
versions without disabling artifact identity checks or changing old meanings.
Future EPIC-009 and TASK-028/029/031 retain and rerun this baseline unchanged.

Separate preview provider eligibility, Personal Alpha qualification, and actual
Gul acceptance. Future-owner runtime tests must not accidentally block v0.1.3.
Verification includes non-regenerated old-client execution, fault scenarios,
static contract checks and independent review; no real Gul dependency.

Task acceptance: the frozen public consumer works and future changes have an
executable backward-compatibility boundary. Next: TASK-026.

### TASK-026: Provider Conformance and v0.1.3 Handoff

Status: `PLANNED`

Depends on `TASK-056` and therefore every preceding EPIC-008 Task. Implement the
planned `private_boundary` driver using generated public-v1 clients against an
actual isolated `dolgorae serve` process and production semantic/Broker paths.
Extend existing native gateway fixtures; do not build a Gul clone or new SDK.
Deterministic cases may fake Codex and must say so. Separately authorized pinned
live Codex must demonstrate actual Primary calls, both approval policies,
Specialist execution, readable result consumption, and supported recovery.
Missing live prerequisites block this Task; missing Gul does not.

Verification: protocol-zero negotiation and exact 27-method consumer profile;
complete prompt-history timeline through page/restart/close; sequential human
admission; public session/result observations; Session bootstrap; approvals; event cursor replay; ready/busy task behavior; existing
access/writer safety; a Specialist result above the inline bound consumed by
Primary and discovered by the public result-list query before external-client
bounded artifact reads, without private ID injection;
length/SHA-256; typed failure/redaction matrix; exact retries; restart/redelivery;
release/abort; complete deterministic repository gate, required live evidence,
source-skill/Markdown checks, and independent read-only review with no unresolved
blocking findings. Do not promote planned test paths to passing evidence.

Update operations, implementation tips, source skill, and user-facing guidance
to actual capabilities. Provide verified request/response examples, Controller
carrier handling, call sequence, retries, deadlines, event reconnect, artifact
reads, and explicit version limits. At closeout promote enduring dossier content
to canonical owners, remove the dossier/TODO entry, and replace `Detailed SOT`
with `Canonical Outcomes` links. No stable release, tag, publication, install,
or real Gul integration is performed by this Task.

Epic acceptance: all twelve Tasks and their completion gates pass; the actual
provider and pinned Codex operate a durable live hierarchy; existing features
remain compatible; public Protobuf matches the TASK-053 frozen additive contract;
no unsupported
capability is advertised; and `MILESTONE-BH1-P` is complete. v0.1.3 becomes
eligible for separate release-candidate QA and authorized publication as a
Milestone Preview, not Personal Alpha. Actual-Gul `MILESTONE-BH1` remains
unclaimed until its deferred consumer campaign passes.

## EPIC-009: Brokered Specialist Collaboration

Status: `PLANNED`

Goal: Add durable bounded Specialist-to-Specialist collaboration to one active
Brokered Hierarchy without making the Primary Agent a message relay.

### TASK-027: Durable Mailbox, Virtual Actor, and Collaboration Plane

Status: `PLANNED`

Depends on completed `EPIC-008` (`MILESTONE-BH1-P`) and the `TASK-023`/`TASK-024`
foundations, including selected transport TASK-025 and provider acceptance
TASK-026. It does not depend on actual Gul acceptance. Integrate the checked
Specialist collaboration tool through the selected run-bound transport. Enable
the deferred busy-target queue, reuse-any and activation policies only with their
checked implementation and compatibility tests. Add the
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
reasoning leakage; and byte-identical public Protobuf source and descriptor
against the TASK-053 baseline. Rerun TASK-056 with unchanged generated clients;
new collaboration policies must not add a human-input queue, rewrite existing
policy snapshots/history, or change whole-session close semantics.

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
redaction/authorization, and pre-ready failures that create no Run. Rerun
TASK-056 unchanged; activating declared event variants must preserve existing
history, projection freshness and consumer behavior.

### TASK-029: Complete Gul gRPC Surface and Extended Operational Conformance

Status: `PLANNED`

Depends on `TASK-026` and TASK-056's frozen-consumer baseline. Extend the
27-method consumer gateway to all 36 methods in TASK-053's extended public-v1
descriptor. Add the nine remaining original RPCs covering profile diagnostics,
default-effort, fork, verification, deletion, write continuation and writer
handoff with complete safe projections. Timeline implementation belongs to
TASK-054 and aggregate queries to TASK-055; this Task extends their conformance
rather than reimplementing or postponing them. Complete bounded independent Run streams, exhaustive typed
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
regressions. Run the TASK-056 consumer unchanged, including operation when new
optional methods are advertised. All timing uses injectable clocks and no test
binds TCP.

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
escaped-process limitation reporting. Rerun TASK-056 with unchanged clients;
new instruction composition must not rewrite stored user input, introduce
steering, expose child authority or change aggregate closure.

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
It additionally drives a generated public-v1 Go conformance harness, not actual
Gul and not evidence for MILESTONE-BH1, over one local gRPC channel, observes at
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
