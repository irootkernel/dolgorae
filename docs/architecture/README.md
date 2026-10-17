# Dolgorae Architecture

Status: Normative target architecture for the `v0.2.0` Personal Alpha, the
first customer-supported release.

This document owns technical structure and invariants. It describes the system
Dolgorae is required to implement for that release; it does not claim that an
earlier milestone preview already implements the complete target. Product
behavior is owned by [the specification](../specs/README.md), rationale by the
[architecture decision records](../architecture-decision-records/README.md),
and implementation progress by the [roadmap](../roadmap/README.md).
Document roles and the required synchronization procedure are defined by the
[documentation authority map](../README.md).

## System Context

The [durable Submit implementation](../specs/dolgorae-v014-durable-operations.md)
retains successor identity alongside native audit records. Definitive refusal
releases only its matched reservation after sync. Controller-authorized lookup
projects the verified ledger prefix without worker startup or attachment reads;
the original acceptance chain hash and revision remain immutable.

The [Close and build recovery implementation](../specs/dolgorae-v014-close-recovery.md)
serializes coordinators with a permanent per-Run lifecycle lock at
`runtime/locks/lifecycle/<run-id>.lock`. It reuses the checked startup-owner
witness format under a distinct lock inode. The held kernel lock and current
process identity govern observed `settling`; durable close intent and its last
failure cause survive owner loss. Explicit build recovery also holds the
startup boundary, verifies complete retained history and writer settlement,
and proves old-generation absence before replacement. Orchestration owns the
durable phase; the semantic and gateway composition layers apply live-owner
proof to observations.

Dolgorae provides one shared execution and durability core behind exactly two
user-facing product facades.

```text
Gul
  |
  v
Dolgorae-Orchestrated Session facade
  |
  +--> Primary Run
  |      `--> Primary Agent
  |
  `--> Orchestration Broker and Collaboration Plane
          +--> Reviewer Specialist Run
          +--> Tester Specialist Run
          +--> durable per-Run mailboxes and Collaboration Exchanges
          +--> Mailbox Scheduler and Activation Manager
          `--> membership, spawn, task, delivery, and recovery state

External AI and existing main control plane
  |
  v
External Specialist Engagement facade
  |
  +--> Reviewer Specialist Run
  `--> Tester Specialist Run

Both facades
  |
  v
Shared Semantic Service
  +--> Aggregate Bootstrap Coordinator and Use-Case Compiler
  +--> Primary Orchestration Tool Bridge and External Specialist Facade
  +--> Controller and Operator authorization
  +--> Run, aggregate, writer, audit, and recovery repositories
  +--> per-Run Worker
  +--> shared Profile Server or Run-owned Dedicated Lane Server
  `--> Codex services
```

In a **Dolgorae-Orchestrated Session**, Dolgorae hosts the Primary Agent and
owns the operational orchestration loop, Brokered Hierarchy membership,
Specialist lifecycle, delegation delivery state, and recovery. Gul is the
canonical presentation and interaction client. Gul owns remote authentication,
user input, and approval UX, but it does not become the hierarchy state owner.

In an **External Specialist Engagement**, another AI remains the Primary Agent
and semantic control plane. Dolgorae supplies selectively hired, durable
Specialist Runs and owns their accepted task and result-delivery boundary
without creating another planner or external task graph.

Every Primary and Specialist remains an independent Run. Aggregate membership
does not merge thread, Worker, Controller, lane, writer, audit, or recovery
identity. The Orchestration Broker stores relationships and operations around
those Runs. Codex-native subagents remain descendants inside one Run and are
never aggregate members or Independent Specialist Runs.

The v1 public boundary contains the Machine CLI and supervised local gRPC
gateway. Both call one semantic service. The checked public Protobuf source and
descriptor remain unchanged by this internal composition revision. The gRPC
adapter is not durable authority. For an active Dolgorae-Orchestrated Session,
the same trusted-client-supervised foreground `dolgorae serve` process hosts the
reconstructable ControlPlaneRuntime and the sole SQLite mutation owner.
EPIC-008 includes the Orchestration Broker and private Primary tool bridge.
EPIC-009 adds the Collaboration Plane, Mailbox Scheduler, and Activation
Manager; EPIC-008 does not require empty hosts or placeholder services for
those future components. Worker sockets and App Server transports remain
private. No installed daemon is introduced.

## Component Model

### Shared Semantic Service

The semantic service owns every public operation independently of wire format.
For initialization it composes Workspace admission with global Profile home
creation: the Workspace layer invokes a preparation callback only after its
compatibility checks, and the semantic layer calls the global Profile owner
through that callback before Workspace publication. Workspace does not depend
on the global Profile module.
It accepts validated domain requests, performs Controller and Operator
authorization at the existing serialization points, coordinates workers and
durable repositories, and returns domain results or typed Dolgorae errors. The
Machine CLI converts those results to closed JSON envelopes; the gRPC gateway
converts them to Protobuf messages and typed gRPC status details. Neither
adapter may duplicate state transitions, idempotency normalization, writer
policy, recovery, redaction, or projection rules.

### Use-Case Compiler

The Use-Case Compiler is a semantic-service boundary, not a public RPC service.
It validates a complete low-level Run request and applies only the closed
classification rules below. It never selects omitted lane, assurance, profile,
purpose, model, instructions, or native-subagent policy values.

- A public parentless `direct_interactive` Run becomes the Primary Run of a
  new Orchestrated Session only when its protected `human_cli` or
  `interactive_client` Controller carrier contains a checked Orchestration
  Launch Intent naming an installed Specialist Policy. Its preallocated Run ID
  is also the session ID. Without that launch intent it is a low-level Run only.
- A brokered Specialist is created only by the authenticated Orchestration
  Broker and receives the reserved presentation parent
  `dolgorae.orchestrated-session.v1 / specialist / <session_id>`.
- An externally hired Specialist is created only by the authenticated External
  Specialist Facade after an engagement has been explicitly opened. It receives
  `dolgorae.external-specialist-engagement.v1 / specialist / <engagement_id>`.
- A raw low-level `managed_agent` Run never opens or joins an External Specialist
  Engagement. A non-reserved parent is authority-neutral generic provenance.
  Reserved namespaces are rejected outside their trusted internal callers.

The compiler also produces safe public projections. `ListRuns` and parent
metadata can help Gul or an external integration present related Runs, but no
projection creates membership or replaces the durable aggregate registry.

### Aggregate Bootstrap Coordinator

The Aggregate Bootstrap Coordinator owns crash-consistent creation of the two
first-class aggregate kinds. Every aggregate has exactly one durable Aggregate
Bootstrap Operation with a stable idempotency key, normalized request digest,
provenance, state, timestamps, and aggregate reference.

For a Dolgorae-Orchestrated Session, the coordinator executes a prepared
cross-store protocol:

1. validate the complete root Run request, Controller carrier, and
   Orchestration Launch Intent;
2. resolve and validate the named Specialist Policy from the machine-local
   workspace registry and include its name, revision, and digest in the
   normalized idempotency request;
3. preallocate one UUIDv7 for both `session_id` and Primary `run_id`, plus one
   bootstrap-operation ID used as the cross-store correlation identifier;
4. commit the `create_orchestrated_session` operation, `creating` session row,
   complete immutable Specialist Policy snapshot, and event in SQLite;
5. append and fsync the matching Run creation intent carrying the same
   bootstrap-operation ID in the Run ledger, then publish the empty root Run
   manifest; and
6. commit bootstrap `ready` and session `active` only after the Run publication
   is authoritative.

The stores are not treated as an impossible atomic filesystem-plus-SQLite
transaction. The bootstrap operation ID and preallocated identities make each
boundary reconcilable. Startup recovery joins the bootstrap operation,
aggregate row, Run allocation tombstone, manifest, and audit ledger. It
completes the original
operation only when evidence proves that doing so cannot duplicate the root Run;
otherwise mutation remains blocked in recovery.

For an External Specialist Engagement, `open_external_engagement` is a
standalone aggregate-bootstrap transaction. Dolgorae generates the engagement
UUIDv7 and bootstrap-operation ID, then commits the
`open_external_engagement` operation, immutable external provenance,
Aggregate Controller Binding, empty active engagement, and event before
returning the identifier. It performs no Run, process, lane, thread, or Turn side effect.
Same-key and same-request replay returns the original engagement identity;
same-key drift is an idempotency conflict. A later
`hire_external_specialist` transaction generates the hire-operation and child
Run identities and commits the member and child reservation before runtime side
effects.

### External Specialist Facade

The External Specialist Facade is a private CLI or MCP adapter over the shared
semantic service. Its checked payload contract is
[`dolgorae-external-specialist-facade-v2.schema.json`](../protocol/dolgorae-external-specialist-facade-v2.schema.json),
with additive accepted-task assignments in
[`dolgorae-external-specialist-facade-v3.schema.json`](../protocol/dolgorae-external-specialist-facade-v3.schema.json).
It supports explicit engagement open and safe get, Specialist hire, task
assignment, bounded await, result collection, cancellation, release, and
engagement close. It does not add a planner, task graph, or autonomous scheduling
loop.

Every External Specialist Facade call requires the protected aggregate-owner
Controller credential established by `open_external_engagement`. The facade
validates the canonical workspace and compares Controller ID, kind, normalized
principal, and capability digest against the immutable Aggregate Controller
Binding before any observation or mutation. External-controller provenance is
checked against its bootstrap digest and event linkage but is not resubmitted
and grants no authority. For member mutations, the Worker accepts a typed
private-facade delegation only after independently matching the presented owner
credential, the Run's immutable External Specialist aggregate binding, and the
current active SQLite membership at its serialization point. The ordinary Run
control path still accepts only the member Run's Controller credential, and no
child credential or recoverable equivalent is persisted.
`hire_external_specialist` additionally
requires a fresh per-Run Controller credential carrier supplied outside
model-visible payloads. The facade resolves the requested Agent Configuration
and access against trusted integration policy, then invokes the ordinary Run
core through a write-ahead hire operation. Raw
`StartRun`, reserved `parent_ref`, or later Run listing cannot be used to infer,
attach, or regroup engagement membership. Existing generic Runs are not attached
in place.

Role and task bytes have separate lifecycles. Hire resolves one stable Role and
Agent Configuration; the compatibility `objective` is stored only as a hiring
rationale digest and never enters instructions. Facade v3 task acceptance JCS
normalizes and persists the complete brief, inline contexts, criteria, target
intent, and deadline before dispatch. The Turn prompt embeds that accepted task
as data and distinguishes contexts from the candidate workspace. Task retry
compares the full accepted request digest, while Role and Agent Configuration
digests remain unchanged across different tasks. Terminal result construction
joins the durable task to its unique assignment receipt and rechecks the
persisted request JCS against that receipt's digest before inspecting schema or
expected-output discriminators. Missing, ambiguous, or mismatched authority
fails closed as engagement integrity corruption.

The following provisioning and isolated-output structure is the legacy
Engagement contract. New successor hires bind explicit Markdown Role and
caller-workspace access; successful provisioning establishes the selected
runtime policy. Retired isolated intent is rejected before new launch effects.
Existing accepted isolated members retain their original resources and result
meaning under original ownership.

An accepted legacy hire initially publishes a logical, threadless Run and
records the member as `unstarted`. Assignment reconstructs a missing Worker from the Run
manifest plus the durable member configuration. The launch root and fixed
session sandbox derive from the requested access: canonical writes reserve the
workspace Writer before startup, isolated writes use the deterministic separate
Git worktree, and read-only work cannot acquire write access. The two write
modes are distinct policies: a task cannot switch a member between isolated and
canonical workspace residency. Successful startup
marks the member `resident`; an absent failed publication is `unavailable`, while
any partially published Run is retained as `recovering`.
The provisioning record acts as a five-minute lease: ordinary concurrent
observation leaves it untouched, while reconciliation after expiry classifies
an absent Run root as unavailable and any partial publication as recovering.

Task dispatch has explicit pre-effect `accepted`, uncertain `dispatching`, and
Turn-bound `running` states under a partial unique index that permits only one
active task per member. The aggregate operation receipt is updated in the same
SQLite transactions as these transitions. A permanent per-engagement operation
lock serializes facade effects across processes, including the engagement-store
eligibility check and the cross-authority Writer repair or acquisition that
follows it. Reconciliation joins task state to
the authoritative Run projection: it redelivers immutable terminal artifacts,
interrupts overdue or unsupported-interaction work, and maps any unproved
effect boundary to an active outcome-unknown refusal until Run evidence proves
quiescence; it never releases Writer authority after a transient control
failure. The private External Specialist interrupt waits up to five seconds
for the addressed Turn's terminal after the interrupt acknowledgement, including
while interactions remain pending. It returns terminal evidence only when that
terminal was observed; callers do not add another ordinary interaction-sensitive
wait. Ordinary Run `Wait` still returns pending interactions immediately.
Isolated terminal output captures a
bounded binary Git patch into the result artifact before worktree cleanup.
For `structured_review_v3`, terminal reconciliation parses the inline report,
validates exact ordered criterion coverage and the evidence/status invariants,
and commits that normalized complete report to the artifact. An isolated-write
task stores the report as `final_response` beside its captured
`isolated_change`. Invalid output terminalizes the task with
`REVIEW_OUTPUT_INVALID`; result collection cannot reinterpret or repair it, and
no isolated change is captured for that invalid report. Deterministic
result-construction failures terminalize the task. A retryable
result-construction failure, including transient patch-capture I/O, leaves the
task active, withholds canonical Writer release when applicable, and is retried
without blocking other task observation or collection only until the durable
task deadline.
Deadline exhaustion records terminal `expired` with `OPERATION_TIMEOUT` and no
result artifact. Reconciliation restores the terminal Turn from the durable
ledger only when the Worker reports `idle` or `paused` with no active or
remembered terminal Turn; ordinary running polling does not replay the ledger.
If that authoritative ledger cannot be verified, reconciliation fails closed
for the engagement instead of treating corrupt or unreadable terminal evidence
as absent and continuing with sibling results.
Canonical Writer release is completed before terminal lifecycle cleanup can be
reported. Because task and Writer commits cross authorities, every facade
reconciliation also checks the actual Writer holder and releases it when that
canonical member has no active task. Result collection inserts delivery receipts
only for the bounded cursor page selected in its SQLite transaction. Abort
records task and member settlement one member at a time, so a
restart can continue after an already closed Run without repeating its work.
Writer census failure before any sandbox mutation rolls the prepared authority
record back to its known prior state so the caller can explicitly retry; only
uncertainty after a policy effect can enter `blocked_unknown`. Reconciliation
never respawns a worker for a durable closed Run. If member close already
removed its runtime record after exact generation-absence proof, reconciliation
may commit the otherwise stranded Writer release directly from that durable
closed Run.

### One-Shot Specialist Review Coordinator

The One-Shot Specialist Review Coordinator is a convenience adapter over the
External Specialist Facade. It is shared by the `dolgorae specialist review`
Machine CLI command and the external stdio MCP tool `dolgorae_review`. Its
checked model-visible shape is
[`dolgorae-specialist-review-tool-v2.schema.json`](../protocol/dolgorae-specialist-review-tool-v2.schema.json),
with the task-aware stdin successor in
[`dolgorae-specialist-review-tool-v3.schema.json`](../protocol/dolgorae-specialist-review-tool-v3.schema.json).

The Coordinator binds the canonical workspace, Reviewer Codex Profile,
aggregate-owner and per-Run Controller credentials, external provenance,
request reference, and idempotency outside the model payload. It then executes
one explicit sequence:

```text
open engagement
  -> reserve and hire read-only Reviewer Run
  -> assign one working-tree review task
  -> await and collect one immutable result
  -> release Reviewer
  -> close engagement
```

The Coordinator uses the same semantic Run and External Specialist Engagement
services as later long-lived integrations. It does not own a separate state
model. For the preview boundary it permits one active Reviewer and one active
task, performs no general queue scheduling, and treats busy, terminal, timeout,
cancellation, mutation detection, invalid output, and outcome uncertainty as
checked non-success results. Cleanup is best effort after a transport failure,
but success is returned only after the result artifact and read-only
postcondition are authoritative.

In v3, Profile preparation, target capture, and Reviewer hire occur before task
acceptance. Acceptance persists the full JCS request and starts the deadline;
dispatch, the one Turn, terminal reconciliation, and report validation consume
that single budget. The immutable Reviewer Role is fixed independently of the
brief and context. The result projection and immutable artifact preserve the
same criterion assessments, evidence limits, and overall assessment. v1/v2
coordinator state and result artifacts keep their prior interpretation.

The recoverable CLI carrier retains a caller-supplied request reference and
Controller binding in an orchestration one-shot operation receipt before Profile
preparation. This receipt connects existing engagement, Run/task, capture, and
Profile lifecycle authorities. An operation lock
serializes the original coordinator and explicit cleanup. Internal aggregate,
Reviewer and capture-settlement carriers outlive the CLI, while only their
outer recovery Controller authorizes their use by a fresh process. Inspection
uses existing-only read-only storage and never invokes collection or replay.
Engagement allocation and its receipt binding share one transaction; capture
reservation precedes materialization and does not imply capture publication.
Recoverable Reviewer starts carry the prepared server state into Run admission;
they cannot launch a replacement generation before membership registration.
The existing admission lock checks that the selected generation is still current.

Temporary-server ownership belongs to the Profile lifecycle service. Its
checked ownership record is written during the existing locked launch PREPARE
and connects the workspace/request identity to the launch reservation and exact
generation. Shared-server attachment does not acquire ownership. The stop
PREPARE validates that ownership under the same locks as generation selection
and membership admission, then uses the existing verified shutdown path without
implicit interruption. Unknown pre-publication process identity remains blocked;
the coordinator must not add a raw PID signalling or independent cleanup path.

The external MCP adapter is intentionally distinct from the later run-bound
Primary and collaboration bridges. It does not need to prove a Dolgorae source
Run or source Turn, but it still needs an explicit per-request retry identity.
The adapter never treats the MCP connection, JSON-RPC request ID, or stdio
process as continuity. In replay-safe mode the trusted host injects one UUIDv7
under `tools/call params._meta` key
`xyz.rootkernel.dolgorae/externalRequestRef`; the model-visible argument schema
cannot supply it. The adapter persists that reference and normalized request
digest before opening the engagement. Same-reference replay returns the
original review, while changed input returns `IDEMPOTENCY_CONFLICT`. If the
pinned host cannot prove preservation of that metadata across every supported
retry and reconnect boundary, the MCP tool is not registered and the Machine
CLI remains the SR1 carrier. The Reviewer profile
omits the adapter, and the semantic layer rejects nested
first-class Specialist hiring, preventing recursive review invocation.

The opt-in SR1 campaign retains only bounded findings, identifiers, and
digests in the checked
[`dolgorae-specialist-review-acceptance-v1.json`](../protocol/dolgorae-specialist-review-acceptance-v1.json)
artifact. Its schema rejects missing review rounds, unresolved findings,
workspace-change claims, non-terminal cancellation cleanup, credential or
private-endpoint disclosure, and any MCP advertisement under the selected
`mcp_unavailable` disposition. Raw model output is not retained.

#### Planned External Specialist MCP host boundary

The v0.1.4 successor must resolve host feasibility in TASK-067/TASK-068 before
contract freeze. The historical external-review disposition and run-bound
Primary transport are separate authorities; neither proves successor Codex
and Claude host support. Preserve ADR-031's trusted invocation identity boundary.

The selected trusted host/adapter owns identity creation and protected durable
retention before delivery; Dolgorae owns accepted execution and its receipt/
lookup linkage. TASK-067 must name installation, persisted-state/schema,
credential and recovery owners and define concurrent/restart/corruption failure
behavior. The selected pre-contract candidate is the Dolgorae-owned synchronous
native hook and stdio adapter in
[ADR-041](../architecture-decision-records/README.md#adr-041-qualify-a-protected-native-host-hook-binding-for-external-specialist-mcp).
It durably stores native-event/input linkage before dispatch. Codex resolves
the binding from native call/thread metadata without rewriting public input.
Claude carries a private opaque binding in hook-rewritten input; the adapter
verifies and removes it before public processing. Claude's native hook journal
belongs to the protected identity-state boundary described in ADR-041. Its
state generation is initialized explicitly and never recreated by normal
delivery or lookup. Both actual hosts passed the
[prototype scenarios](../specs/dolgorae-v014-host-contract-qualification.md),
including physical host loss before acknowledgement and supported lookup.
Independent completion review accepted bounded native feasibility, and the
roadmap records completed H1 publication. Production adapter implementation
and actual consumer-runtime acceptance remain pending; this decision
advertises no product runtime support.
Reconnect, process replacement and response loss recover one logical identity;
a deliberate new call with identical arguments remains distinct. Connection,
stdio lifetime, JSON-RPC request ID and model-controlled text confer no such
authority. If an alternative carrier is needed, adopt its successor ADR and
checked shapes explicitly. Both mandatory host routes require version-pinned
pre-contract qualification; failure leaves contract freeze/Gate A and dependent
work unavailable. Runtime execution remains separately authorized.

#### Planned three-consumer qualification boundary

The [v0.1.4 consumer matrix](../specs/dolgorae-consumer-acceptance-v014.md)
separates Aquarium semantic judgment/backend selection, Podway structural
receipt/cursor/progression and Gul client/UX ownership from Dolgorae execution,
identity, result, recovery and evidence-export ownership. Mandatory compatibility
does not activate a consumer backend. A group has no implied Podway Action
meaning without a supported contract and explicit Podway mapping decision.

EPIC-018 publishes the qualified immutable contract package after current
consumer requirement review and accepted external implementation plans. It does
not wait for Gul. TASK-069 opens EPIC-019 from a completed Gul contract Epic.
Phase 3 produces the intended runnable provider artifact after implementation,
cleanup and mandatory Aquarium/Podway actual-path qualification. Gul's Phase 4
product Epics consume that fixed artifact and close with their own complete
campaign evidence. Phase 5 TASK-088 independently checks provider facts and
consumer compatibility using completed artifacts, without a concurrent Gul
development Task. Combined Gate C requires both sequential acceptance results.

Exact source, binary/package, contract, runtime and relevant configuration
identities bind all final evidence. Cleanup and packaging require applicability
checks and affected requalification; TASK-089 cannot replace the qualified
artifact with an unqualified build. Missing external implementation or runtime
evidence blocks the owning acceptance rather than becoming an optional handoff.
These execution requirements change no single-writer, generation or one-Turn
invariant and prove no runtime support.

#### Planned v0.1.4 state and component ownership

Public v2 and successor Machine/facade adapters call the shared semantic
services. They do not own separate lifecycle, task or result truth. Accepted
legacy records retain their version-qualified readers and frozen public-v1
projection. Modern Run creation provenance is immutable and separate from
effective access, writer ownership and current verification.

Workspace capture owns exact admitted Markdown Role, common, dependency and
selected-guidance bytes. Task admission owns the accepted configuration and
task identity. Result publication owns the complete immutable result identity,
including accepted change components and checked structured output. The
[closed digest contract](../specs/dolgorae-v014-method-and-digest-contract.md)
separates these facts from request-envelope identity and presentation previews.
Rust computes and validates product identities; adapters expose authorized
read-by-key facts without private-store scraping or prompt reconstruction.

Engagement and Orchestrated Session services retain their separate ownership
and authorization boundaries. Their task records own conservative accepted/
uncertain round accounting, original receipt linkage and current settlement
proof. Group admission durably accepts an immutable bounded assignment set;
member provisioning and upstream dispatch may then progress or fail separately.
Partial dispatch cannot rewrite the group manifest. Group collection includes
terminal and unknown members. Deterministic finding merge reads immutable
complete sources, preserves conflicts and coverage, and makes no model call.
An explicit synthesis uses ordinary task admission and its own round/result.
Internal lateral queues and wait-cycle detection remain session-owned; external
Engagements do not gain lateral messaging.

Profile lifecycle owns compatible fixed read-only pools, admission leases and
dedicated generation accounting. The selected K/cap bounds belong to the
successor requirements. Leasing cannot transfer writer authority or make one
member's passivation stop another. Generation shutdown still requires complete
owned membership, workload and descendant proof. Native-subagent eligibility
is an immutable configuration constraint, not a per-lease privilege.

The existing worker and lifecycle services own close/recovery transitions and
exact-build process proof. One retained close owner survives restart; process
replacement does not create a new logical operation. Predictable capacity
refusal is separate from integrity failure. Admission reserves enough durable
space to finalize accepted work and report cleanup failures.

Ledger maintenance owns immutable generation manifests, checkpoints, segment
chains and durable watermarks. It takes the workspace lifecycle lock before
the ledger writer lock, closes admission and establishes a safe maintenance
boundary. Readers pin one generation and never combine old and new segments.
New data and directory entries are synced before the atomic manifest switch;
recovery validates the selected generation. Reader/recovery pins retain old
generations. Profile membership compaction and explicit content retention
preserve active/unknown references, accepted identities, receipts and audit
continuity. Content retirement records a tombstone rather than claiming that
the accepted operation never existed. Automatic deletion remains disabled.

TASK-075 separates reusable display observation from fresh mutation proof.
Checked source identities and verified published history bound reuse; database
connections and commit notifications remain process-local optimizations over
durable state. Stream sampling has four separate work slots alongside eight unary slots, with
a twelve-slot blocking executor. Response deadlines bound waiting while admitted
blocking work retains its permit until completion. Explicit
result maintenance preserves original identities and protected references. The
[observation runtime](../specs/dolgorae-v014-observation-runtime.md) owns these
component invariants and their measurement boundaries. The private
`global_profile` owner retains registry authority. The Profile module captures
checked definition lists for display; the gateway projects those observations
without reading the registry directly. The private
`storage_observation` module owns checked short database leases, counters and
commit notification; engagement and orchestration remain the schema and result
identity owners and use that module without transferring semantic authority.
`storage_observation` uses `ledger_generation` only for shared workspace
maintenance leases. While holding a shared lease, `ledger_generation` asks the
maintenance owner whether a retained checkpoint or content operation still
closes admission.
This read-only namespace check acquires no additional lock or database lease;
maintenance alone validates its retained owner and operation records. The check
also covers new Run publication, so process exit cannot reopen admission before
the original operation resumes. Maintenance uses storage observation for checked content
references and retains exclusive effect ownership; artifact reading uses the
orchestration owner to verify publication identity before body access. Orchestration
returns a `ledger_generation` shared maintenance lease to protect a live result
reader through full body verification and range selection. The artifact module
owns the common per-Run quota lease and checked logical byte count. Worker
response storage and brokered publication share that lease; the worker uses the
artifact quota API without gaining database or retention authority. Orchestration
also uses the artifact quota lease through preparation and publication; artifact
observation resolves normalized references through the orchestration store.
Artifact quota checks reuse workspace secure-file/directory validation.
Maintenance opens the engagement-owned result schema through its owner and uses
storage observation for retained content; the storage helper uses Run paths,
canonical hashes and Darwin file identity for its checked private references.
The storage helper classifies ledger lease errors without erasing path or
capacity failures. Its workspace-scoped busy boundary carries no invented Run
identity; Run publication uses its known identity and registered startup error.
Orchestration regression tests exercise maintenance-owned protection of cached
legacy chunks; runtime retention authority remains with maintenance.

These boundaries select future implementation responsibilities. TASK-068
publishes their checked shapes; later implementation Tasks provide actual
runtime behavior and artifact-bound three-consumer qualification.
The [H1 package](../specs/dolgorae-v014-h1-package.md) selects changed manifest,
configuration, instructions, Run-state and intent generations individually.
Audit, membership, workspace and Profile-server records retain their existing
generations; new group, pool, queue and maintenance sidecars carry new state.

#### Independent review readiness

[EPIC-016](../roadmap/README.md#epic-016-reliable-independent-specialist-review)
implements the adopted
[readiness requirements](../specs/README.md#independent-review-readiness).
Reviewer preparation owns explicit Profile overrides and default resolution.
The shared task prompt and output-contract code supplies the
complete v3 contract to both the one-shot coordinator and reusable facade;
the strict validator remains the authority before immutable result publication.
The validator produces bounded structural diagnostics, while the coordinator
or facade attaches known execution identity. The engagement store binds that
sanitized diagnostic to the original task and its terminal failure before
response publication. Initial errors and authorized await/collect after restart
project the same stored facts. TASK-057 owns the persisted contract and legacy
record handling; raw provider output stays outside diagnostic storage and
projection.

The production `review_output` module depends on `jcs` and `machine`. Its tests
also reference `specialist` and `task_request`; the existing full-source
dependency scan includes those test-only edges. The module expands the checked
v3 verdict schema and its finding dependency for prompt construction and
structural validation. Semantic validation binds the accepted criteria and
evidence. Diagnostics contain contract-owned paths and categories, original-output
digests, and allowlisted key metadata. The runtime attaches known execution
identity before the engagement terminal transaction. The one-shot coordinator
reads the Machine response projection and the reusable facade reads the typed
Turn response. Both retain oversized responses' terminal byte length and digest
without artifact reads.
Orchestration schema v4 stores the diagnostic in the task and terminal operation
response atomically. Failure delivery receipts have no artifact and retain the
failed task state. Existing successful receipts and legacy records keep their
meaning; absent historical diagnostics remain absent.

Temporary-server cleanup belongs to the Profile lifecycle service. The review
coordinator records durable launch intent and cleanup responsibility at the
server-creation boundary, before model/effort and Reviewer validation can fail.
It carries authority only for a verified generation it created exclusively.
Both preparation errors and later completion use the same guarded cleanup
path, after resolving the lifecycle obligations of any engagement, Reviewer,
or capture already created. Absent objects require no fabricated identity or
closure receipt. The lifecycle service revalidates generation, process
identity, and membership under its existing serialization before signalling.
Concurrent admission or uncertain state cannot grant cleanup authority.
Forced termination cannot rely on process-local destructors for cleanup.

TASK-058 established the public one-shot lookup and recovery boundary over the
existing state and lifecycle services. A caller-retained reference established
before effects maps durably to the request and known owned objects.
`legacy_one_shot` engagements remain outside the `external_v1` facade's owner
checks. A new process uses the dedicated inspection command under local
workspace observation authority; cleanup requires the original Controller
capability. The reference itself grants no mutation authority. Read-only lookup
cannot start a replacement review or repair state; ambiguous evidence produces a
checked unknown or blocked outcome. Recovery mutations use the checked
one-shot authority and existing lower-level services, with no new job framework
or skill-owned supervisor.

Skill packaging derives its schema dependency closure from checked protocol
artifacts. Installed copies carry the same contract bytes and resolve local
references without the source tree. The source-owned package utility derives
`resources/protocol/` and its inventory from canonical schema IDs and reference
closure. The inventory records roots, examples, and byte digests; it grants no
runtime authority. Installation checks canonical equality and copies only the
declared package files to a new destination. Installed-resource validation uses
only the package's local registry and rejects remote retrieval.
TASK-057 and TASK-058 own the corresponding wire and CLI contracts. The Epic
does not change public-v1 gRPC.

### Direct Review Target Coordinator

The accepted [caller-workspace contract](../specs/caller-workspaces-v1.md)
replaces new source materialization at EPIC-017's integrated cutover. The
Coordinator admits bounded source metadata, pins Git objects or index entries,
serves checked candidate file/diff reads and validates their receipt provenance.
It keeps live context distinct from candidate reads. Workspace/index/dirty
fingerprints are sampled before dispatch and acceptance; they do not establish
snapshot isolation. A missing runtime/tool boundary prevents dispatch.

Source bytes stay in the caller workspace or existing Git object database.
Admission and mutable resampling use the workspace's explicitly accepted Git
or non-Git mode. A non-Git caller root never inherits source selection from an
ancestor Git repository.
Persistent target metadata and read receipts are protected control state.
The [direct review state contract](../protocol/dolgorae-direct-review-state-v1.schema.json)
owns immutable reader/Run bindings, issued reads, sticky violations and history
inspection intents and completions.
These records contain no source tree. The review coordinator retains request,
Reviewer, task, Turn and result ownership independently of target assurance. Direct target v2
and review v4 contain no capture root or settlement credential. Every new launch
facade uses the canonical caller cwd; none owns that root for cleanup.

Direct target admission reuses the review-target coordinator's bounded source
reads. It persists only selected paths, modes, object IDs and content
fingerprints. Staged reads reopen admitted blobs; historical reads use their
pinned objects. Neither substitutes live index or worktree content. Mutable
scope acceptance repeats the original selection, including the comparison HEAD.
The trusted reader stores each read receipt before returning candidate bytes.
Receipts bind the original review, target, side, path, content and byte range;
full-file context passes through the same reader as changed hunks.
The initial Turn input includes only bounded target identity and inventory size.
The same reader exposes the complete immutable target metadata in bounded byte
pages. Metadata receipts cannot support candidate evidence or source locations.
The protected binding's read and publication bound accounts for JSON escaping
of all retained paths and entries. Smaller receipt/history record limits do not
reduce the admitted inventory or prevent reopening the original reader.

A checked-review Worker must retain its reader binding and any provenance
violation through restart. Native, plugin or subagent tool items outside that
reader invalidate candidate provenance even if fingerprints remain unchanged.
Supported runtime qualification must establish complete correlated tool history
or a verified restriction to the reader before dispatch. Read-only sandboxing,
instructions and a declared dynamic tool alone do not qualify this boundary.
Without that proof, admission rejects the review before Reviewer allocation.
Checked-review admission accepts only the empirically exercised executable pin
and native tool/history configuration. Profiles with additional enabled features
or configured MCP servers, apps, agents, hooks, notifications, plugins,
marketplaces or skills are unsupported for this boundary. This restriction is
independent of ordinary caller-workspace startup support. The owned Worker
rechecks its admitted Profile and native configuration before a task Turn.

The direct review coordinator captures the selected Markdown Role separately
from the accepted task, reserves one fresh read-only Dedicated Reviewer through
the existing Engagement store and binds its reader before Worker startup.
Protected request state preserves the original input, Profile binding, Role
snapshot and Controller carriers. Repeated requests inspect their original
Engagement instead of allocating or dispatching again. A result requires the
original Run's durably completed Turn, matching native history completion and
validated receipts before collection. Reviewer closure and Engagement closure
are recorded independently of that verdict; neither owns the caller root.

The reader's immutable Run-to-review binding lives with protected workspace
control state. The Worker resolves that binding before starting or connecting
an app-server and verifies the original Run and canonical caller cwd. Recovery
reopens this same reader and receipt store; it never substitutes a fresh review
or grants writer access. The Turn coordinator exposes a separate candidate-read
tool, checks native Thread/Turn/call correlation and persists unmediated tool
observations before verdict acceptance. Notifications, authoritative terminal
items and recovery history use the same provenance check.
Checked review completion always reads back the original Turn's native history
after journaling its terminal status. Inline terminal items alone cannot qualify
provenance; a lost or malformed history read leaves acceptance unverified.
The reader persists a history-check intent before that terminal journal append
and an idempotent completion only after correlated history inspection. An
unfinished intent prevents acceptance across restart; recovery inspects the
original Thread and Turn before recording completion. Known tool violations
remain independently durable and cannot be cleared by successful recovery.
Before marking inspection complete, the reader durably stores the bounded final
response extracted from that original history. Acceptance uses this response,
even when the terminal notification omits items or contains a different response.
The terminal journal independently establishes the original Turn's outcome.
The host adds the sampled scope's assurance limits to the accepted verdict;
model-written limit text cannot remove that disclosure.

Workspace-owned launch preparation keeps caller roots separate from legacy
managed worktrees. Successor preparation admits canonical Git or non-Git roots
without source materialization and rejects explicit copy or isolated-write
intent before allocation. Run contract and policy selection remain independent
of root ownership; a legacy Run using the caller cwd retains its legacy startup
semantics. TASK-066 connects the Machine adapter to these services. Its modern
member startup uses the same captured instruction, reservation, caller-root,
policy proof and writer admission machinery as an independent modern Run.
Engagement and broker stores remain the authority for reserved member identity
and aggregate configuration; they cannot choose a different root or grant
writer authority. Existing accepted snapshots remain the input to recovery.
The session store distinguishes accepted legacy policy snapshots from captured
Markdown policy sessions. Both feed the existing broker lifecycle through
explicit versioned policy and Role variants. A modern Primary's startup identity
includes its selected policy name; physical activation consumes the same
accepted capture receipt. Child startup uses its retained Role configuration
and Profile binding without consulting current authoring files.
Reusable Reviewer sessions install the candidate tool before their first
Thread. Each accepted review task retains its own immutable reader root and
receipts; the accepted task record owns the active reader identity.
The Worker resolves the authenticated assignment's task identity before
binding its reader, and accepts that change only while idle. Recovery keeps
the original Thread and task histories, rather than constructing new evidence.

Facade services accept an explicit internal source contract. `CallerWorkspace`
rejects new legacy capture and isolated-write requests before workspace discovery,
credential admission, Run reservation or provider dispatch. The review-target,
one-shot, External Engagement and broker services enforce this contract; broker
publication also checks it before runtime preparation. Historical inspection and
settlement retain their original ownership checks. TASK-066 activates this
admission at the public boundary. New adapter operations select
`CallerWorkspace`, including Primary requests in accepted legacy sessions.
`LegacyBeforeCutover` is an internal historical-record preparation contract;
it does not choose a Run version or lane or authorize a new public copy request.

External and
brokered provisioning share the workspace service rather than calling launch
helpers through the External facade. The original durable hire reservation or
broker publication-start event binds the aggregate and reserved Run before
managed-worktree creation. Only the first committed attempt permits creation;
a replay or recovered publication may reuse only the original secure,
registered worktree. Missing resources or ambiguous reservation persistence
prevent creation; Run absence alone never proves that materialization did not
occur. These records grant neither caller-root nor capture-settlement cleanup
authority.

### Immutable Review Target Coordinator

The following structure describes existing capture-backed records. Until
integrated cutover the legacy implementation remains available. Afterwards,
only accepted-record inspection, recovery and owner-bound settlement use it;
new capture creation is rejected before effects. Missing historical resources
are never recreated.

The Immutable Review Target Coordinator introduced by EPIC-004 is a shared
semantic subsystem beneath the scoped Specialist Review adapter and external
consumers such as Aquarium. It owns target resolution, source eligibility,
immutable materialization, manifest and whole-target identity, capture-time
drift detection, post-execution validation, settlement, cleanup, and unknown
outcome preservation. It does not own Reviewer selection, Aquarium policy,
Orca provider supervision, or Mulgae adjudication and publication.

The Coordinator reads Git objects, the index, and eligible worktree paths
without mutating the source repository. Whole-tree targets use `current/`;
transition targets use `before/` and `after/`. Captures live in an owner-only
Dolgorae home, use read-only materialized files and
directories, and carry a safe manifest with relative paths, byte sizes,
SHA-256 digests, content classifications, inclusion dispositions, resolved Git
objects, and the whole-target digest. The source repository path and private
tool state are not Reviewer-visible target data.

Capture and settlement are separate versioned Machine operations. A successful
capture publishes an opaque reference only after every candidate has passed
eligibility and secret checks and the source identity has been re-observed.
Their checked requests, manifest, terminal receipt, capture result, and
settlement result are owned by
[`dolgorae-review-target-v1.schema.json`](../protocol/dolgorae-review-target-v1.schema.json).
The same transaction binds the capture to one backend kind and immutable
lifecycle identity, stores only the digest of a random settlement owner
credential, and delivers the credential through an owner-only file or inherited
descriptor supplied by the trusted caller outside model-visible input. Neither
the secret nor its carrier path enters provider-visible data or a machine
result.

Settlement is idempotent and backend-owned. Its compare-and-set transaction
requires the opaque capture reference, protected owner credential, expected
capture revision, and a checked terminal receipt whose backend kind, lifecycle
identity, terminal state, state revision, and evidence digest match the stored
owner. The Coordinator revalidates the referenced terminal evidence before it
deletes source bytes. A stale revision, foreign credential, mismatched receipt,
active or unknown state, missing evidence, or concurrent winning settlement
leaves the capture unchanged. Exact replay returns the accepted settlement;
different post-settlement input is a conflict. The scoped Specialist Review
coordinator settles after authoritative Dolgorae engagement and Reviewer state;
Aquarium's Orca-backed review settles only after authoritative Orca lifecycle
state. The Coordinator never infers cancellation or settlement from elapsed
time. A crash after source bytes move into the settlement staging tree but
before the durable record advances is recovered by revalidating that same tree
under the settlement lock before completing the compare-and-set.

The scoped Specialist Review Coordinator composes this subsystem with the
existing External Specialist Facade. It validates the canonical executable's
version, capability result, file identity, and SHA-256 immediately before
source-bearing launch, creates one fresh Reviewer, passes only the immutable
capture root, validates the checked review result and capture digest, and
reports technical verdict independently of engagement, Run, and settlement
state. The completed working-tree v1 path remains a compatible entry point; the
new target contract is additive and versioned.

The Coordinator keeps source-workspace authority and Codex Profile ownership
on the engagement while overriding only the managed Reviewer's launch working
directory with the immutable capture root. It verifies the pinned executable
identity immediately before that source-bearing launch. Engagement storage
schema v4 retains the v2 removal of incorrect global result-content uniqueness
and normalizes members, tasks, scoped operations, artifacts, and delivery
receipts for reusable engagements. The v2-to-v3 migration maps uncertain
executing or accepted work to `interrupted_unknown` and preserves a ready
result as `completed_not_delivered`. The v3-to-v4 migration adds task diagnostics
and rebuilds delivery receipts with nullable artifact references. These
migrations preserve existing rows transactionally.

### Role Source and Policy Resolver

EPIC-017's successor resolver explicitly reads one common Markdown body and
one named global Role body through protected no-follow descriptors. It owns
bounded admission and raw-byte captures; runtime configuration owns project and
ancestor AGENTS suppression. Native Profile guidance may coexist as user input
outside those captures, subordinate to the managed developer prefix. No Role
text selects a Profile, cwd or access. The compiler
stores Role captures in policy v3 and aggregate bootstrap captures common once
for its children. Agent Configuration v3 and manifest v3 retain immutable
instruction identity. Retry, generation recovery and child activation consume
accepted bytes without reopening live sources.

`instructions` owns descriptor-relative source admission, exclusive template
publication and `dolgorae.instructions/v2` composition. Explicit `init` installs
the common, general and reviewer files after workspace admission; ordinary
discovery does not create them. Named fault barriers cover capture entry, the
bounded read and acceptance for standalone, policy and bootstrap admission.
The successor configuration reader is separate
from the legacy v1/v2 reader. `markdown_policy` prepares policy v3 captures and
bootstrap instructions for the later integrated adapters. These internal APIs
do not activate public modern starts. Successor configuration preparation checks
the selected executable against the qualified pin before shared Profile readiness
or Run allocation; unsupported pins return `INSTRUCTION_DISCOVERY_UNSUPPORTED`.
Project instruction byte limits
do not suppress native Profile guidance; the contract permits that separate user
contribution and does not include it in the immutable Role claim. Retained
capture readback and legacy preparation remain available. Connecting manifest
v3 to thread startup remains with TASK-063; the public adapter cutover remains
with TASK-066.

The Run reader distinguishes legacy instruction metadata from successor source
captures. Manifest v3 retains the exact common/Role snapshot and explicit
requested access and selected Role; its Agent Configuration reconstructs the
developer prefix from those accepted bytes. Legacy manifests keep their original
wire fields. Recovery validates cross-field identity before using either
generation and does not reopen instruction sources.

Caller-workspace coordinator sessions use native same-thread settings updates
and matching settings notifications rather than changing only local sandbox
labels. Thread creation controls project/ancestor instruction discovery and
checks the returned model, cwd, approval and sandbox policy. These metadata
checks remain distinct from empirical enforcement qualification: without that
qualification, policy stays unverified and modern admission remains unavailable.
The coordinator journals the returned provisional Thread identity before
checking those startup observations. For a new caller Thread, it then calls
`thread/name/set` with a bounded Run label and reads the same Thread with
`includeTurns: true`. Startup binding requires the original identity and an
empty Turn list. This metadata operation is the persistence barrier for the
pinned native implementation: its
[`thread/name/set` handler](https://github.com/openai/codex/blob/01fc69f4026735edfdf6789820549727a4867b11/codex-rs/app-server/src/request_processors/thread_processor.rs#L1848)
updates the Thread store, whose
[`persist_thread` operation](https://github.com/openai/codex/blob/01fc69f4026735edfdf6789820549727a4867b11/codex-rs/thread-store/src/local/live_writer.rs#L291)
writes the initial metadata without a task Turn. A failure after Thread
creation quarantines startup while retaining that provisional identity; a
retry cannot create a replacement. Matching readback remains separate from
the required empirical process-loss persistence qualification.

Before a caller policy update, the dedicated connection requires two bounded
`thread/loaded/list` observations containing only its original Thread, with no
continuation cursor, around a same-Thread idle-status read. Another loaded
Thread, including an idle native child, blocks the update before mutation.
Unknown or incomplete observations also block it. The guard permits native
child execution but refuses transitions whose effect on a loaded child's
permissions is unproven.
These reads do not replace the independent operating-system workload census
or qualify filesystem enforcement.

Caller policy evidence has a separate checked successor envelope. It binds the
operation, original Run and Thread, Worker identity and control-socket epoch,
Dedicated Server identity and socket, server key and epoch, immutable Profile
binding digest and effective policy. Acceptance compares these facts with the
current runtime record, durable policy and prepared writer transaction. An
unverified policy, mismatched generation or missing original binding cannot
commit writer authority. The frozen `WriterAccessChanged` response retains its
legacy meaning and cannot serve as this proof.
While holding the workspace Writer lock, the coordinator reads the committed
Run projection and its exact audit prefix through the read-only observer.
The prefix must match the projection's head hash, and the projection must
remain unchanged across the read. The live Worker retains exclusive ledger
ownership; policy verification never opens a second ledger writer.
Exact startup retries use the same observer to validate history beyond the
three-record bootstrap prefix. They retain conformance validation and never
reopen a live Run for bootstrap repair. Only the bootstrap prefix
uses the exclusive bootstrap writer.
Runtime qualification is a separate prerequisite. The supported native policy
mechanism is pinned to the observed executable, with explicit thread sandbox,
approval and project-document controls.
`global_runtime` owns the static accepted-binding pin check, which instruction
admission and Worker qualification share. The Worker owns process qualification.
The Dedicated Server also receives the
project-document limit as a process override; merged configuration must confirm
that managed settings have not overridden it before thread initialization.
The Worker derives a private qualification witness from its actual process and
socket identities and the accepted launch inputs. A bootstrap flag or received
proof cannot construct that witness. Five empty operating-system workload
samples are required before and after each startup or policy effect.
Supported skills, plugins, apps and MCP remain available under the existing
trusted-Profile boundary; qualification concerns native execution enforcement.

`run` owns the checked caller startup request used by reservation storage;
`semantic` composes admission from that request without a reverse dependency
from persistence to the service. TASK-063's transport-neutral successor
reservation also retains the observed model, default effort and Profile
capability snapshot. Retries do not resolve omitted launch choices against a
later catalog. A separate immutable startup receipt records the original
verified Thread and policy after reader readiness or Writer commit; a manifest
alone does not prove successful startup.
Successor admission resolves writable and general defaults into explicit private
selections. It remains separate from the frozen legacy start service and its
lazy thread allocation. Unsupported executable pins return
`WORKSPACE_STARTUP_UNSUPPORTED` before Run reservation or caller execution.
For the qualified pin, shared Profile readiness precedes immutable intent
acceptance, and authoritative Writer reservation precedes caller Dedicated
startup. The Worker establishes the original Thread as a reader without a
task Turn; writable startup applies verified policy before Writer commit and
receipt publication. Public adapter activation remains TASK-066 work.

Recovery restores provisional Thread identities from the durable operation
chain. A checked native intent is fsynced before caller Thread startup or
settings dispatch. Ledger verification permits caller `Idle -> OutcomeUnknown`
only after that successor intent; legacy transition rules remain unchanged.
An unmatched startup intent quarantines replay. An unknown startup attaches
only that original Thread as a reader and retains uncertainty until explicit
reconciliation proves idle empty history.
Explicit resume can retire a pending writer guard with current qualified
Reader proof. It cannot turn interrupted writable startup into success or
reacquire Writer authority. Without a retained native identity, recovery
reports the unavailable original binding. Pre-execution Writer contention
seals the original Starting Run as StartFailed under the startup lock.
After the entire old generation is verified absent, caller Writer retirement
reads the committed projection and audit prefix under the Writer lock. It uses
the snapshot owner's Thread-generation and policy-epoch selection without
requiring the guard to be Active. The public snapshot still requires Active
authority for a Write policy. Retirement verifies the holder's generation and
server binding, retains the guarded transaction, compares both audit-head
sequence and hash, and records Unknown policy before releasing authority.

`OrchestrationStore` owns durable acceptance of the prepared successor captures
in its workspace database. Its private
[`dolgorae.accepted-policy-session/v1`](../protocol/dolgorae-accepted-policy-session-v1.schema.json)
receipt binds one session/root identity, canonical workspace and Dolgorae home,
request identity, policy v3, and each child configuration v3 and Profile binding
v2. One transaction publishes the complete receipt and its JCS digest; all
children bind the same captured common body and the installed Role bytes.
The complete receipt has a 64 MiB retained-record bound, separate from the
65,536-byte combined raw source ceiling. Acceptance validates these bonds and
bounds before publication. A retry looks up the
receipt before source admission, while recovery and reactivation validate only
retained receipt bytes. Source edits, source removal, and installed-policy
changes or removal therefore cannot change an accepted session. The existing
session-commit fault barriers distinguish failure before acceptance from an
unknown response after commit; retry preserves the accepted identity. This
private capture acceptance does not publish a physical Run, thread, or active
session, and leaves legacy session rows and readers unchanged.

`session_policy` validates the versioned legacy and caller capture variants
used by `orchestration`, while `semantic` binds the accepted session to the
physical Primary and starts caller members from retained configurations. The
`migration` module reads legacy inputs and publishes only new authoring files;
it cannot mutate accepted records. The direct review adapters share their
checked prompt and result logic with reusable external tasks, and the Worker
binds each accepted task to its own Reader before starting a Turn. Runtime
capability projection reads selected Profile qualification without starting a
server.
One-shot and reusable Reviewers share the aggregate kind but retain distinct
durable engagement authority kinds. Worker startup reads reusable membership
configuration and access only for `external_v1`; `legacy_one_shot` retains its
accepted Reviewer plan and fixed read-only boundary. Missing reusable fields
cannot downgrade a member into the one-shot path.

The existing JSON resolver below is the legacy v2 authoring path. Explicit
migration produces new Markdown sources and policy input without replacing
accepted snapshots or choosing collision winners. After integrated cutover,
new JSON authoring is rejected with a migration requirement.

A bounded resolver implements the specification's
[Specialist Role Sources](../specs/README.md#specialist-role-sources).
Role sources are authoring inputs; they are neither an execution registry nor
authority for a model-originated request. Common Role files are protected
user-local inputs, and project Role files are portable project policy.
The existing strict `config.yaml` shape and immutable workspace identity remain
unchanged; optional Role directories do not gate earlier workspace admission.

The policy-admission service resolves explicit scope/name references, captures
each source once using safe descriptors, validates the bounded source objects,
and compiles their character into the explicitly supplied execution and policy
controls. It owns the create-exclusive installation of the expanded policy
into the protected workspace registry. The read-only validate path uses the
same compiler but publishes nothing. No model-facing tool loads a source file,
selects a source scope, or bypasses policy installation.

The Aggregate Bootstrap Coordinator reads only the installed policy, resolves
current global Profile bindings into Agent Configuration snapshots, and commits
the resolved session-policy snapshot and digest with the prepared bootstrap.
The broker subsequently uses that immutable session snapshot. Recovery never
reconstructs character from current source files, the current registry, or a
Profile name. Installed-policy and session-snapshot contracts are distinct.
Their active v2 contracts use global Profile Agent Configuration snapshots,
while the old checked v1 policy remains historical input rather than changing
meaning.

### Orchestration Broker

The Orchestration Broker is an internal Dolgorae control-plane component used
by a Dolgorae-Orchestrated Session. It is not an LLM-visible peer and does not
expose Specialist Controller credentials to Gul or the Primary Agent.

It owns:

- Orchestration Session identity, approval policy, status, and revision;
- the immutable, schema-validated Specialist Policy snapshot and its JCS digest;
- Primary-to-Specialist membership and immutable role and Agent Configuration
  snapshots;
- write-ahead Specialist spawn operations and aggregate-scoped idempotency;
- accepted Specialist task dispatch, completion, and result-delivery receipts;
- broker-mediated Collaboration Exchanges and per-Run durable mailboxes;
- virtual-actor passivation, activation operations, and actor residency;
- deterministic priority, fairness, wait-cycle, queue, and backpressure policy;
- degraded, recovering, completed, and aborted aggregate state;
- safe result redelivery without replaying semantic work; and
- SQLite event-log replay and runtime reconciliation.

The broker invokes the same semantic Run core as other trusted clients, but
through internal aggregate-aware transactions and separately held per-Run
Controller capabilities. A model-originated request is advisory until accepted
under the session's explicit approval policy. No model receives a Controller
capability or a private Worker address.

Each broker-held capability has one owner-only mode-0600 carrier below the
workspace state root at `orchestration/broker-credentials/<run-id>.json`. The
carrier uses the ordinary checked Controller credential shape, remains outside
SQLite, is checked against its stored public Controller binding before every
Specialist side effect, and is deleted only after authoritative member release.
SQLite, events, model input, and public projections contain the public binding
and capability digest, never the raw capability.

### Primary Orchestration Service

The Primary Orchestration Service is a transport-independent broker adapter. Its
checked model-facing payload contract is
[`dolgorae-orchestration-tool-v1.schema.json`](../protocol/dolgorae-orchestration-tool-v1.schema.json).
It implements Specialist request and operation wait, safe listing, bounded task
assignment, task wait, result collection, private `read_specialist_result`, and
graceful release. TASK-025 selected native Codex `item/tool/call` on the
isolated-home campaign pin (locally installed Codex CLI 0.155.1). That historical
campaign did not change the then-current Codex App Server 0.153.4 compatibility
baseline, and the isolated probe is not the production adapter. Deterministic
fixtures prove native worker Turn/call binding and show that shared MCP identity
is ambiguous without a host-controlled carrier; Dedicated Lane isolation does
not invent Turn or call identity. Unit tests use an internal fake adapter
against the same service. The deterministic fake `dispatch_task` helper may
still return a completed task for legacy result tests. Production dispatch
returns only the accepted Turn identity; TASK-050/051 own observation and
result settlement.

The bridge binds session, Primary Run, source Turn, tool-call ID, inherited root
priority, Controller authority, and idempotency outside model arguments. The
model cannot provide or override those fields. `request_specialist` resolves a
role only from the session's immutable, schema-validated Specialist Policy
snapshot, whose global-Profile successor is defined by
[Specialist Role Sources](../specs/README.md#specialist-role-sources).
The model cannot choose Codex Profile, model, credential, priority, or access
outside that policy.

For `user_approval_required`, the service records one approval-waiting spawn
operation and opens one normalized Primary Run `user_input` interaction before
child provisioning. A trusted client, including the provider acceptance client
or Gul, resolves that interaction through the existing Controller path. The
semantic service distinguishes broker-owned approvals from Codex-owned pending
requests and routes the response to the durable spawn operation, not to an
unrelated App Server request. Approval commits the operation transition before provisioning;
rejection closes it without allocating a child Run. For `fully_delegated`, the
service provisions only a role and access combination explicitly admitted by
the immutable policy. Neither mode bypasses cardinality, capabilities, writer,
idempotency, or recovery checks.

Before allocating a child, the service applies the role's deterministic reuse
policy. Compatible means identical active-session membership, role reference,
role snapshot digest, Agent Configuration digest, and admitted access. The
target collaboration algorithm selects idle first, then lower pending mail
count, then lower Run ID. Busy-member and mail-count-based live selection
belongs to EPIC-009. For EPIC-008, TASK-048 follows the
[v0.1.3 live provider slice](../specs/README.md#v013-live-provider-slice):
only `never` and `reuse_idle_compatible` are admitted, with deterministic
selection among compatible idle members and no live mailbox requirement.
Existing target schemas and historical core fixtures retain their meaning.

The durable reuse and replay guarantees apply to both delivery stages. A reuse
result exposes the existing member's original spawn operation ID and is
first committed in an aggregate reuse receipt keyed by the session, trusted
idempotency key, and normalized request digest; it appends no new spawn or
membership row. The Primary Run tool-call/result ledger then persists the
complete response with its source Turn and tool-call identity. The receipt,
rather than current queue state, restores the exact selection when a response
is lost before the ledger append.

Production provisioning enters through the shared semantic composition layer,
which re-resolves the immutable global Profile binding, validates the exact
Agent Configuration, and starts the preallocated managed-agent Run with its
broker Controller. The Worker binds the initial thread to the spawn operation
before the member becomes ready. On an ambiguous cross-store publication, the
Broker observes the reserved Run: a ready Run settles without replay, a known
absence may safely republish, and partial or unreadable state remains
`recovery_required`.

Task assignment never auto-hires a missing role. A bounded tool wait may expire
without cancelling the durable operation or task. Waits run outside the Run
drain and SQLite mutation transactions, so Controller responses, cancellation,
and other Run operations remain serviceable. `any` and `all` are evaluated from
fresh durable observations until the bounded transport deadline; notifications
are only an optimization. Assignment `blocking` uses the earlier of the fixed
60-second acceptance budget and the task's acceptance-anchored deadline, but
always returns the immutable acceptance receipt. Exact-call replay never waits
again, while a new call may observe later state.

Before reserving the dispatch boundary, the Broker checks the persisted
acceptance deadline. A known pre-dispatch task at or beyond that deadline uses
the existing durable expiry-control transition and returns its retained task
identity without Writer handoff, Turn submission, or interruption. Retry of a
crossed dispatch boundary retains the existing terminal-proof and unknown
outcome rules.

Task cancellation records its intent before an external effect. A known
pre-dispatch task settles without contacting the Worker. Once submission may
have happened, the Broker uses the member Controller to request ordinary Turn
interruption and then requires authoritative terminal evidence. An interrupt
acknowledgement, transport loss, or a dispatch/cancel race without terminal
proof settles `interrupted_unknown`; it never manufactures `cancelled` or
`expired`. A completed Turn that wins the race remains pending result
construction for TASK-051. Release is a graceful retirement operation that
stops admission of new work. In EPIC-008, it waits for authoritative task and
result-delivery quiescence under the existing Run lifecycle, writer,
interaction, and process-safety rules. Mailbox quiescence is an additional
requirement only when EPIC-009 adds collaboration; EPIC-008 must not introduce
placeholder mailbox waits.

Primary result collection uses the delivery-receipt sequence as a caller-held
cursor. Each request names `after_sequence`; the broker replays receipts after
that point, fills the bounded page with newly completed tasks in the same SQLite
transaction, and returns the last delivered sequence as `next_after_sequence`.
An empty page echoes the input cursor. This preserves ordered receipt replay
without letting an old page consume the capacity needed to deliver later
results. `read_specialist_result` is the frozen private reader for actual
Primary-owned bytes; `collect_specialist_results` remains only the delivery
cursor.

Task-context reads through the authenticated Primary bridge may use that
Primary's published Specialist results without an external Controller carrier.
The internal reader verifies the publication's Primary Run binding and artifact
integrity. Other Controller-only artifacts still require their Controller;
public artifact reads retain the same authorization rules.

### Live Provider Integration Boundary

EPIC-008 connects existing orchestration, Run, Worker, Controller, writer,
artifact, and process-ownership components. It does not rebuild their stores or
introduce a general scheduling framework. The normative release slice is
[v0.1.3 Live Provider Slice](../specs/README.md#v013-live-provider-slice).

The production tool bridge constructs trusted call context and delegates to the
Primary Orchestration Service. Request identity is derived from a proved source
Run/Turn/tool-call identity, not model arguments, environment markers, socket
identity alone, or a reconnect-local JSON-RPC request number. Worker/server
generation is checked as an authority fence, not used to manufacture a fresh
semantic idempotency key on retry. Preserve durable call and reuse receipts.
Native registration is the App Server `item/tool/call` server request on a
Run-scoped host tool advertisement, not a shared Profile edit or MCP
`config.toml` registration. The isolated TASK-025 probe lives in
`src/live_transport.rs` and the hidden `__live-transport-mcp` stdio entry; it
must not be wired as the production adapter in TASK-025. TASK-047 performs
source authentication on the live request and MUST NOT treat the probe
`TrustedBinding` helper as that proof.

TASK-047 implements that boundary in `TurnCoordinator`, `WorkerSession`, and
`primary_bridge`, with the checked model-facing contract isolated in
`primary_tool`. `WorkerSession` injects that contract into `TurnCoordinator`;
the App Server layer does not own or duplicate the tool schema, transport-bound
field list, or safe error mapping. `TurnCoordinator` validates the native
request against its current Thread, active Turn, registered tool name, and
Worker generation, then constructs `PrimaryCallContext` without consulting
model arguments. A bounded background dispatch opens the existing
orchestration store. Dispatch reserves one completion slot before work starts,
and shutdown drains every reserved completion before stopping, so an admitted
call cannot be silently dropped by mailbox backpressure. Only the owning
generation may answer the saved JSON-RPC destinations. The semantic idempotency
key excludes generation, so a replacement Worker can recover the same durable
call result while an old completion cannot answer through the replacement.
Only Primary manifests add the dynamic tool to `thread/start`; no Profile or
MCP configuration is edited. The live adapter rejects operations whose
production effects belong to later Tasks before invoking any effectful
`OrchestrationAdapter` method.

The future-collaboration portion of TASK-025 probes only source Run/Turn/call
binding through an inert schema-shaped stub in an isolated test environment.
Temporary test-only tool registration is allowed when needed for that proof.
Production Specialist collaboration registration, advertisement, operation
handlers, and mailbox/scheduler services remain in EPIC-009. This limit does
not reduce TASK-025's retry, cancellation, bounded-wait, disconnect, or restart
proof for the selected Primary transport.

The production OrchestrationAdapter connects preallocated child identities to
existing semantic Run operations. Raw managed-Run admission stays separate from
broker-owned admission. Global Profile and Agent Configuration snapshots remain
immutable; bridge registration must not edit a shared Profile for one Run.
If a separate bridge process is unavoidable, use the existing TASK-046 ownership
registration and shutdown rules instead of a new cleanup mechanism.

The accepted-task boundary reuses task-content validation/composition where
applicable without calling the External Specialist CLI/facade as the Broker's
backend. Its aggregate and Controller ownership are different. Policy/access
checks and a single active member-task reservation precede writer or Turn
effects. Exact acceptance replay is checked before fresh busy admission.
Unsupported queue/collaboration/activation policies are refused before live
Session allocation; future schema values and historical snapshots remain valid.

The production assignment path resolves each context reference against the
Primary Run's authorized immutable artifact observation, verifies the complete
bytes, and persists those bytes with their media type, length, and SHA-256 in
the accepted task JCS. The same record carries the Broker `task_id`, target Run,
exact objective and expected-output strings, requested access, and durable
deadline origin. Dispatch presents that record through the target member's
broker-held Controller carrier and uses the Broker `task_id` as the Turn
idempotency identity. A successful submit records the authoritative target Turn
and leaves completion to later observation. An exact accepted-call replay reads
the durable receipt before busy admission and never submits another Turn.
Canonical workspace write requested from the active Primary tool Turn is a
typed pre-acceptance writer conflict; there is no implicit interrupt or
cross-Controller handoff.

Turn dispatch must return an acceptance outcome independently from completion.
The runtime observes accepted Turns and settles results without blocking its
approval, cancellation, or event handling. SQLite remains authority for request,
deadline, dispatch evidence, and delivery; in-memory notifications and in-flight
handles are reconstructable. No SQLite transaction or global mutation lock may
span a model Turn or client wait. Known pre-effect work may be resumed, but an
unknown publication/Turn boundary may not be replayed. This is a bounded
execution adapter, not the EPIC-009 Mailbox Scheduler.

Internal live-Primary ownership, frozen by TASK-025:

1. **Admission versus execution.** The Broker reserves `task_id` and the
   acceptance receipt before effects. The adapter submits that identity and
   reports Turn acceptance, a known pre-effect rejection, or ambiguous
   submission. Completion is observed on the associated Turn. Settlement and
   delivery stay with the Broker.
2. **Request versus response destination.** The Worker classifies the App Server
   tool request. The run-bound bridge authenticates and forwards it, and holds
   the pending reply destination for the current Worker generation. The Broker
   owns durable operation state. Existing Run/control-plane events observe
   approval and completion without occupying `drain_run`. The owning Worker
   replies only after validating that destination. Broker approvals carry an
   internal origin and spawn-operation reference.
3. **Business rejection versus infrastructure failure.** Commit authenticated
   final rejections, including busy assignment, onto the trusted call identity
   so exact retry returns them. Reconstruct an accepted result from the
   operation receipt if the outer tool ledger missed the reply. Do not cache
   unauthenticated or incomplete attempts.
4. **Authentication versus admission.** Probe `TrustedBinding` is not production
   source authentication. Worker/bridge binds Run/Thread/Turn/generation.
   Broker authorizes the aggregate operation. Task admission compares requested
   access with the member's admitted rights and Role policy before writer
   movement or Turn dispatch. Execution rechecks writer and working-root
   conditions immediately before effects.

Task-result validation checks the original accepted request before inspecting
an output discriminator. Actual immutable result bytes must exist before the
Broker publishes completion or a readable artifact reference. Use a durable
write-ahead association and idempotent reconciliation between existing artifact
storage and SQLite; do not claim atomicity across them. A Primary-owned result
projection or the frozen private `read_specialist_result` reader gives the
Primary and its Controller access without exposing a child credential or
arbitrary child files. The shared artifact reader obtains published Specialist
result associations through the Orchestration Store's read-only observer API;
orchestration alone owns the SQLite schema and publication-state interpretation.
Broker approval observations use the same read-only boundary and cannot run
publication recovery as a side effect of a public read. The specification fixes
assignment receipt, wait, and UTF-8 result-page rules.
TASK-051 implements the reader. A page is a nonempty UTF-8-boundary-preserving
prefix before EOF; invalid byte ranges or an undersized next-character budget
return a checked error, not a lossy or non-progressing page.

Each implementation Task owns its effect-boundary recovery tests. TASK-052 adds
cross-component restart/retirement acceptance, including isolated working
roots, pinned thread/Profile identity, writer ownership, retained results, and
unknown outcomes. It cannot be used to defer unsafe intermediate behavior.
Provider conformance uses generated public clients and the real gateway; only
the upstream Codex boundary may be faked in explicitly labeled deterministic
cases. Pinned live Codex evidence is separately required. Gul UI and consumer
integration remain a later acceptance boundary, not a provider dependency.

### Collaboration Plane

EPIC-009 adds the Collaboration Plane for logical direct Specialist
communication while preserving hub-and-spoke authority. The five components
below describe that target plane. Component 1 reuses the bridge established by
EPIC-008; components 2 through 5 are added by EPIC-009:

1. **Run-Bound Internal Tool Bridge**: EPIC-009 reuses the transport selected by
   TASK-025 and the Primary orchestration surface connected by TASK-047, then
   adds the Specialist collaboration surface. The Primary surface remains
   EPIC-008 functionality; this composition requires neither a second bridge
   nor a placeholder collaboration surface in EPIC-008. Both surfaces bind
   source Run, source Turn, and tool-call identity outside model-controlled
   arguments. The Specialist surface remains unregistered and unadvertised in
   production until EPIC-009.
2. **Collaboration Service**: validation, role resolution, idempotency, artifact
   normalization, wait-cycle checks, and transactional enqueue.
3. **Durable Mailbox Store**: SQLite tables for exchanges, mailbox items,
   activation operations, delivery state, and the hash-chained event log.
4. **Mailbox Scheduler**: one event-driven scheduler using a dirty Run set and
   deterministic queue selection. Specialists never poll SQLite.
5. **Activation Manager**: compare-and-swap activation and safe passivation of
   Virtual Actor Runs.

The normal path is commit then wake. The service commits the exchange, mailbox
item, event, and activation marker in one transaction, then marks the target Run
dirty and signals one in-memory notification. Startup and a low-frequency global
reconciliation scan repair a lost wake or expired pre-dispatch lease. In-memory
channels are optimizations and never authority.

A Specialist-to-Specialist request is a bounded Collaboration Exchange, not a
Controller operation. Neither endpoint can use it to mutate the other's Run,
writer, lifecycle, role, or credentials. The Primary Agent receives operational
visibility and final Specialist results but is not required to relay each
consultation body.

### CLI Front End

The visible `dolgorae` invocation is short-lived. It:

1. resolves the canonical workspace;
2. parses and validates machine-oriented input;
3. validates a credential carrier and transfers its fd for a mutation;
4. resolves the explicit run ID and performs only preliminary authorization;
5. discovers or starts the owning worker;
6. exchanges one request/response with the worker, or reads the fsynced
   projection directly for projection-only `events`;
7. emits the stable stdout envelope and exits.

It never talks directly to app-server and never writes the audit ledger while a
worker owns the run. Start-time bootstrap is the only period in which the
front-end may create the run directory and initial records before worker
ownership transfers.

### Gul Consumer Contract

The [Gul consumer specification](../specs/gul-consumer-v1.md) defines the approved
v0.1.3 surface. TASK-053 publishes its checked wire through the immutable
consumer lock, generated clients, fixtures, and pre-extension descriptor under
`docs/protocol/`; this publication does not change runtime capabilities.
Completed TASK-025 transport, private receipts, execution
identity, rejection replay, blocking waits and result paging are unchanged.

The shared semantic/Broker layer owns the two Controller-authorized read-only
aggregate queries. GetOrchestratedSession captures consistent aggregate state,
policy identity, counts, close disposition and an independent revision.
ListOrchestratedSessionResults pages immutable publication records with explicit
Primary-owned ArtifactRef/RunRef values. Reads do not start processes, repair
state or acknowledge private result delivery. Aggregate revisions are not Run
ProjectionStamp counters. Clients coalesce bounded snapshot refreshes, including
aggregate-only changes that need not emit a Primary Run event.

The existing ledger/timeline owners persist accepted human text before submit
acknowledgement and expose the complete Controller-safe chronology. Gul filters
user input for Prompt History; it does not become a second history authority.
Ordinary new prompts cannot enter an active Primary Turn; exact receipt replay
and current Interaction responses retain their distinct paths. No human-input
queue, steering or automatic interruption is introduced.

The shared semantic close coordinator serves legacy public CloseRun and modern
Machine root close/recovery. Frozen public-v1 projection guards stay in the
gateway adapter; Broker child retirement and Writer release select the Run's
retained manifest version. Whole-session close records durable intent, stops new admission
and retires owned children through the Broker. Both semantic and Worker
admission consult the orchestration-owned durable close intent before accepting
a fresh Primary Turn; accepted receipt replay precedes this check.
Admission races with spawn/task
publication use the existing serialization order; a per-root close gate admits
one settler while waits hold no SQLite transaction or global mutation owner.
Compatible concurrent calls observe the retained operation. Unknown effects
prevent successful closed state. A Primary lifecycle timeout retains the
unknown outcome without sealing the Run closed; the ordinary Run timeout-close
path does not satisfy whole-session completion. Close settlement checks the
Primary's durable unknown-outcome evidence even when a later Pause has changed
its lifecycle; only matching terminal or reconciled history clears that blocker.
For a writable modern Primary, the coordinator confirms any requested
interruption, transitions the original Thread to verified Reader and releases
its Writer before sealing the root. The terminal snapshot therefore does not
depend on a stale write-policy proof whose Writer has already been retired.
Root recovery accounts for retained aggregate intent without new semantic work
or auto-resume.
History, results, workspace changes and unrelated runtimes remain intact.
Primary Pause/Interrupt does not imply aggregate pause.

The consumer contract's bounded-close outcome table is the semantic authority.
The gateway keeps RunMutationResponse unchanged: gRPC OK requires settled whole-
session closure. Accepted but progressing intent uses SESSION_CLOSE_IN_PROGRESS
with the durable operation ID in existing error details; completed response
context and aggregate close projection correlate that same ID. Transport loss
can omit the ID and is reconciled by the known root, never blind tokenless retry.
Reads only observe; the existing root recovery owner advances retained work.

The TASK-053 checked provenance matrix maps each query field to durable owner,
derivation, revision boundary and implementing Task. Semantic owners may read
private provider stores, but no client depends on those structures. Source
corruption cannot become a zero count and no read performs repair. Gul's browser
DTOs, tokens, history navigation and presentation remain Gul-owned contracts.

Gul develops with an explicitly selected contract-derived mock before release;
production injection never falls back to that mock or the Machine CLI. Actual
integration uses the separately released provider. Frozen and pre-extension
clients remain regression fixtures for later implementation. Future Podway
observations are an optional read-only surface with no direct FSM mutation path.

### Public gRPC Gateway

`dolgorae serve --socket <absolute-path>` is a supervised foreground
re-execution of the same binary. It is optional for finite Machine CLI or
low-level external-AI operations and mandatory for every live
Dolgorae-Orchestrated Session, from Standalone Primary through Brokered
Hierarchy and later Brokered Specialist Collaboration. The process hosts the
reconstructable `ControlPlaneRuntime`. Deterministic aggregate tests may host
the same runtime in-process behind a fake adapter, but provider acceptance
requires the real foreground gateway. Actual Gul acceptance is separate.
Gul or another trusted
same-user client starts and supervises it; Dolgorae never installs a launchd
unit. It binds only the supplied Unix socket, checks every accepted
connection with the platform peer-credential API, and offers unary operations
plus Run-scoped event streams. The process serves multiple workspaces.
Workspace-scoped calls after InspectWorkspace supply a canonical path and expected
ID; bootstrap accepts the path alone. Capability and global Profile reads are
not workspace-scoped. There is no durable or
authoritative global in-memory Run registry. Dirty sets, activation leases, and
scheduler caches are reconstructable from SQLite.

Historical TASK-023 provides 24 methods. TASK-053 freezes the additive public
consumer wire: complete existing timeline plus two read-only aggregate queries,
27 required methods over a 36-method descriptor. TASK-054 implements timeline
and TASK-055 implements the aggregate queries through a SQLite read-only
snapshot boundary. Result publication order is durable and distinct from
private delivery receipts; Controller-bound cursors fix the session,
projection version, publication head and last returned order. Every durable
orchestration event advances the independent aggregate revision. TASK-029
enables the remaining nine original methods. Preserve historical evidence
separately from the current release profile. Runtime advertisement includes
only complete implementations, never a planned method or generated stub.
Missing optional later functionality must not block the required profile;
incomplete handlers remain unavailable.

The gateway holds the installation-scoped Dolgorae-home
`rpc/gateway.lock` for its lifetime and publishes `gateway.json` with
boot UUID, PID/start identity, binary digest, socket path/inode, server instance
ID, and protocol range. The lock is never acquired by an ordinary semantic
operation and therefore is outside the global operation lock hierarchy. A
second gateway returns `RPC_SERVER_ALREADY_RUNNING`.

Socket traversal is descriptor-relative and no-follow. The supplied path must
be absolute, its existing parent must be a current-uid-owned mode-0700 directory,
and the new node is mode 0600. Symlinks, non-socket collisions, foreign nodes,
unsafe permissions, and stale nodes not bound to the exact prior record return
`RPC_SOCKET_UNSAFE`. A graceful shutdown stops new calls, drains admitted unary
calls for at most five seconds, terminates open streams with
`SERVER_SHUTDOWN`, and unlinks only the inode it bound.

The ownership split is strict: Gul may create and validate the private parent,
choose an unused pathname, launch the process, and verify readiness. Dolgorae
alone owns the singleton lock/record, bind, node mode, stale proof, unlink, and
graceful cleanup. A client never unlinks the provider socket, including after a
failed start. `RPC_SOCKET_UNSAFE` instructs the client to fix or replace its
private socket parent/path before a new attempt; gateway restart with unchanged
unsafe inputs is not a remediation.

The gateway uses a bounded `tokio` runtime only to operate tonic HTTP/2, UDS
acceptance, cancellation, and per-stream delivery. Blocking semantic operations
enter a bounded worker pool. Each event stream has an independent queue limited
to 32 envelopes or 4 MiB and five seconds of stalled delivery. Pressure closes
only that stream with `SLOW_CONSUMER`; it never blocks ledger append, App Server
draining, another Run stream, or an active turn.

Gateway loss has no worker, Run, writer, or App Server lifecycle consequence.
An admitted mutation may continue after its caller loses the response, so the
client applies the operation's idempotency or reconciliation contract rather
than interpreting connection loss as failure. A new gateway reconstructs
projections from authoritative workspace state and resumes streams from the
client's durable cursor.

The adapter maps shared semantic DTOs into typed Protobuf projections. Run,
writer, policy, assurance, recovery, interaction, lineage, capability, and
required-action states use closed enums/structures. Full Controller Interaction
payloads use a typed `oneof`; only the protected response remains bounded JSON.
Run/Writer/Interaction snapshots and events carry a common revision stamp so a
client never combines incompatible aggregates to enable a mutation. Public
filesystem output uses a UTF-8/opaque-byte path `oneof`, and capability blockers
use a closed code enum. Durable event delivery uses a typed event `oneof`;
heartbeat and stream-end variants are non-durable and do not consume cursor
values. No business decision depends on parsing diagnostic text or private
worker state.

The shared projection capture owns the SPEC-015 revision mapping: Run revision
is the durable audit head, Writer revision is the persisted authority revision,
and Interaction revision is the last durable record changing the complete
Run-scoped Interaction view. The capture validates every consumed record and
identity together, including Writer observation updates without an authority
revision change. Gateway-local locking cannot serialize a worker. Capture
contention returns a state conflict rather than mixed projections.

The event append owner durably binds the historical projection stamp to each
new event. Replay reads that binding and never substitutes current state. Old
audit records without reconstructable historical stamps remain readable by
the existing Machine event path; the public stamped stream requires an explicit
fresh-snapshot rebase before it crosses that legacy boundary. No startup or
read path upgrades history by rewriting it. Expected Run revision checks belong
to the mutation owner, after authenticated exact idempotency replay lookup and
before admission of a new operation, including Writer acquire/release.

The `gateway` module owns transport admission and bounded async delivery;
`gateway_socket` owns the singleton record, peer identity, and socket lifetime.
`gateway_service` translates checked requests into shared semantic operations,
including broker-originated approval observation and resolution; it does not
open the Broker store or invoke the private Primary bridge directly.
`machine` owns the current registered error-code vocabulary; `gateway` owns its
gRPC status, retry, and recovery mapping.
The `controller` module owns the descriptor-relative confined carrier walk;
the adapter supplies only the checked path and expected public identity.
`interaction` owns the normalized durable Interaction record, reconstruction at
a captured ledger head, and observer-safe summary policy shared by Machine CLI
and public gRPC, plus the protected response byte bound. `interaction_payload`
owns the normalized payload DTOs and their
transport-neutral field validation; both adapters validate the complete payload
there before formatting it. The adapters retain their output formatting and
observation filters.
`snapshot` owns current Controller authorization against its captured binding;
protected readers call it before and after reading protected material.
`gateway_observation` reads bounded events and Interactions from a captured
durable prefix and applies that shared Controller authorization. It delegates
artifact metadata and byte ranges to `artifact`, the same immutable artifact
reader used by Machine CLI `run artifact show` and `run artifact read`. That shared
reader owns immutable file-change reference validation, visibility authorization,
retention, safe file access, full-digest verification, and range bounds. Run enumeration likewise enters the shared
semantic service; the adapter cannot define a different filtering policy.
`timeline` owns the Controller-authorized accepted-input and safe-event
projection shared by Machine CLI and gRPC. It joins accepted Turn identity to
the durable input record, uses `interaction` for captured-head Interaction
state, and delegates inline and long-input byte verification to `artifact`.
`gateway_projection` formats typed snapshots and `gateway_event` formats
historical client-safe events; neither chooses product transitions. `snapshot`
performs bounded cross-owner durable capture for both adapters, including the
Run, Controller, Writer, worker identity, and Interaction observation. An accepted
Turn response instead uses its committed receipt and immutable manifest without
recapturing live state; this historical projection carries no current Controller
authorization snapshot. The ledger owner supplies common canonical-cursor
parsing and observation-integrity errors to both adapters. Audit and
event modules validate the hash-bound persisted event representation together;
transport modules never reconstruct authority from diagnostic text.

The adapter preserves `recognized_unsupported` as a distinct Interaction
support value. Profile model normalization rejects duplicate IDs, duplicate or
empty effort tokens, and zero or multiple defaults before either adapter emits
a profile; `ModelCapability.is_default` is the only default-model source.

Profile observation may execute the existing finite Codex version and schema
probes in disposable output to validate the registered launch definition. It
does not persist a new Profile binding or start, stop, or migrate a Profile
Server. A live model catalog is read only from an already-running server after
process, socket, and account-home identity validation and is revalidated against
the same generation before publication. Stopped servers expose an unavailable
blocker and no invented model catalog.

### Per-Run Worker

The worker is a hidden re-execution mode of the same `dolgorae` binary. It is the
sole owner of:

- the run control socket;
- the run's private direct WebSocket connection to its immutable shared or
  dedicated execution lane;
- JSON-RPC request IDs and correlation state;
- run lifecycle and pending interactions;
- the audit append handle and materialized state;
- participation in durable writer-authority transactions;
- cleanup of its own connection and worker process, plus participation in its
  exact recorded dedicated-lane generation cleanup.

The worker does not own the shared singleton process or another run's App
Server descendants. It owns dedicated-lane lifecycle only through durable
staged records and exact census identities; the profile manager performs the spawn
transaction. Connection loss, worker loss,
`turn/interrupt` request, terminal turn evidence, singleton loss, and
background-execution absence are separate facts and are never inferred from one
another.

One worker serves one run. There is no shared supervisor or global in-memory
registry. Concurrent start/recovery attempts for the same run are serialized by
a per-run startup lock.

Thread supervision stays inside that worker. Reader and drain failures, and
panics in independent control callers, latch a failed-generation condition.
Returned reader errors and Primary tool panics use the same failure latch and
mailbox fence. An owned startup connection is registered before its WebSocket
upgrade and initialization waits; containment shuts it down even before a Run
session is attached. Cancellation precedes mailbox locking so a blocked writer
cannot delay containment. Each independent control caller registers its owned
connection until that caller exits. After failure notification, the owner
allows a fixed 250 ms for existing typed replies, closes remaining connections,
and joins callers. It observes failure while waiting for normal caller exit,
so a late caller panic cannot strand it in a blocking join. Primary reservation and bridge-entry admission share the
failure fence, without holding the mailbox lock during bridge execution. Calls
cancelled before bridge admission release their reservation. Already-admitted
work keeps its original durable operation and is never replayed. The drain
retains its coordinator and current
response owner across unwind so it can wake callers and record uncertainty through the ordinary fallible
journal. Poisoned journal state remains unavailable. The control owner persists
the existing exact-generation recovery marker, stops serving, and detaches the
session before releasing its startup owner. Recovery reuses the recorded
generation's process/history/writer proof; the failure latch grants no signalling,
replacement, replay or journal-repair authority.

The worker is detached from the transient CLI before it accepts requests. It
runs through the hidden `__worker` argv mode after a single fork and `setsid()`,
with no controlling terminal, null stdin/stdout/stderr, and `umask(077)`. It
ignores terminal-originated `SIGINT` and `SIGHUP` for itself and handles
`SIGTERM` as a bounded shutdown request. Early-start and internal diagnostics
go to a 0600 log capped at 1 MiB with one rotation. The CLI holds byte 0 of the
run's startup lock before fork. As its first post-`setsid`/re-exec operation,
the worker opens that lock once, acquires byte 1, and never closes or reopens
the descriptor while serving. Startup fd 3 may emit `bound` only after byte 1
is held, the socket is bound, and the runtime identity is atomically persisted
and directory-fsynced; the CLI then releases byte 0. A later `ready` object
means replay and compatibility validation completed. A structured failure may
replace either acknowledgement. The bound wait is ten seconds; the ready wait
is 330 seconds, covering the normative five-minute replay budget plus the
bounded session handshake. EOF before an expected object
is `TRANSPORT_FAILURE`; timeout never authorizes signalling the worker. The CLI
parent keeps byte 0; the child's inherited byte-0 startup-lock fd is
`FD_CLOEXEC` before `__worker` re-exec and is not fd 3. Startup status fd 3 is
explicitly preserved across that re-exec. The worker
opens one new startup fd for byte 1 and marks both it and fd 3 `FD_CLOEXEC`
before opening the App Server connection.
The CLI creates no thread before fork. Between fork and re-exec the child
performs only async-signal-safe operations; worker threads are created only
after re-exec.
These rules make ownership handoff explicit and ensure
that Ctrl-C or command substitution cannot terminate the worker or keep the
caller's output pipe open.

### Historical Transient Writer-Capsule Candidate (Superseded)

This section is retained to explain the evaluated candidate. It is
non-normative and superseded by **Sticky Execution-Lane Topology** below.

The profile manager is lock-serialized logic in the Dolgorae executable, not a
resident Dolgorae daemon. It computes a launch contract from canonical
`CODEX_HOME`, the absolute direct Codex executable and checked global argv,
resolved executable identity, sanitized explicit environment, deterministic
symbolic `profile_state_directory_v1` cwd policy, normalized process-static configuration, version, schema,
and feature digests. Runtime-mutable configuration is observed but excluded
from the key and compatible-generation comparison. Immutable bindings retain
their observation bytes for integrity checks. Compatible profile names are aliases
for one `server_key`.
Different stopped definitions for one canonical home may coexist, but a
different contract cannot start while another verified lifetime is active.

The Codex Profile supplies deterministic `PATH`, `LANG`, and `LC_ALL`; caller
shell, virtual-environment, and locale state is never inherited. The concrete
launch directory is derived only after `server_key` is known and is not itself
hashed into that key, avoiding a fixed-point identity. Profile start, stop,
restart, migration, and repair are PREPARE/APPLY/COMMIT transactions: locks
protect only revision-bound intents and commits, while spawn, network, policy,
process, and user waits occur in APPLY with no file lock held.

The manager launches the validated executable followed by `app-server --listen
unix://<dedicated-socket>`. It never uses the official daemon or default Codex
control socket. Shared-read-only workers connect to the shared Profile Server.
Codex 0.157.1 binds the requested Unix listener in its protected per-user
socket directory and publishes a deterministic symlink at the requested path.
Dolgorae verifies that rendezvous and records the physical socket identity;
the directory name does not make this direct App Server a managed daemon.
Once the recorded server scope is absent, one shared socket cleanup routine
checks the deterministic link and recorded physical inode before removing
either entry. It can clear a dangling exact link or a matching physical socket
whose link was already removed; a changed entry blocks cleanup.
A Dedicated Lane Server preflights both socket paths before spawn. Its
unpublished-start cleanup uses the same deterministic path and owner checks
only after the spawned group is proven absent; it never treats an unverified
entry as a socket owned by that start.
A failed Dedicated launch records its bounded startup phase and failure
classification in the private Dedicated log. The Worker reports its failure
code through the startup handoff even when it exits before readiness.
Connection, initialization, qualification and thread-start failures also record
their phase and error classification there before the owned server is stopped.
Caller qualification, Thread startup, and retained-Thread connection startup
retain the failing substage and original typed error before uncertain
acceptance is quarantined. A rejected settings or Thread observation identifies
the checked predicate without retaining the returned content. These diagnostics omit RPC
payloads and error prose. A process-quiescence refusal includes its checked
predicate and bounded kernel observation; a socket refusal identifies the
failed socket-identity check. Diagnostic detail never changes the returned
error, durable provisional Thread, or retry fence. A direct-review coordinator
with checked candidate tools retains the same startup diagnostics and
quarantines a refused persistence barrier after provisional Thread creation.
Its read-only Reviewer safety policy does not exempt it from that fence.
On restart it restores native-start intent and the original provisional Thread;
missing identity leaves startup quarantined, while a retained identity permits
only read-only attachment and checked reconciliation without a new task Turn.
A startup-child refusal also records the expected Dedicated leader scope,
the prior observation for that PID, and the failed identity comparisons. These
values come from the refused check; diagnostics add no kernel lookup or wait.
Initial descendant admission additionally identifies its census call site and
bounded typed ancestor chain. It uses two complete kernel observations to bind
every ancestor PID, birth and UID to the actual Dedicated leader. These
observations, including the initial entry matched after a membership lookup,
compare identity and parent links rather than zombie state. A
normal exit can change that state, but each admitted child must still be proven
absent before readiness. The underlying process census likewise permits a
normal transition to zombie state between its short and full BSD reads while
requiring stable PID, parent, UID, group and session. It retains the full BSD
birth identity and reports the final observed zombie state. That state alone
does not prove absence. On Darwin, BSD lookups can return
`ESRCH` for an exited but unreaped child while the independent kernel group
census still lists its PID. That membership difference keeps the sample dirty
and prevents it from counting toward readiness; BSD `ESRCH` alone does not
complete the workload proof. A differing
initial group/session is frozen at admission; later drift remains a refusal.
If an entry changes scope identity between the kernel reads used to assemble a
global census, the sampler discards that entire observation and takes a fresh
complete census, at most three observations in total. When full BSD birth lookup
is denied, one fresh short BSD lookup may establish that PID, parent, UID or
group changed, invalidating the entire sample, or independently establish
`ESRCH`. A denied lookup alone never omits a process. Stable permission errors,
persistent inconsistency and malformed kernel replies still refuse the proof. This
bounded read-only sampling does not retry any command or reset the startup
deadline; the resulting census must still pass the recorded Worker, Dedicated
leader and child birth, UID, group, session and ancestry checks.
The post-configuration startup proof preserves its predicate before mapping a
refusal to the coordinator's busy error. Both the entry and repeated startup
socket checks preserve socket-identity refusal details.
Runtime qualification failures identify the failed process-proof predicate and
its bounded kernel observation in that private log. These observations explain
the refusal and do not replace or weaken the required proof.
Policy verification after WorkerReady records its failing substage and typed
error in the same private log. A policy-proof refusal retains its checked
process predicate before mapping to outcome uncertainty. No RPC content or
provider error prose is retained, and verification never retries the refusal.
On AArch64, executable integrity checks use the SHA-256 library's CPU-checked
hardware backend with its software fallback. Every identity sample still hashes
the complete executable. This reduces qualification cost without extending the
shared startup deadline or substituting cached digests for content verification.
Writer pre-effect census failures record the same predicate and observation in
the existing private Dedicated log before cancelling the prepared transition.
Manifest loading and Primary tool preparation finish before Dedicated startup,
so a preparation failure leaves no server to stop.

A legacy public-v1 dedicated Run first owns only a durable logical lane; its
first input lazily starts that lane's physical App Server. Modern starts
initialize the physical server and thread before successful startup, as
described below. Each dedicated generation uses the identical immutable launch
contract and canonical `CODEX_HOME`, a distinct short socket, UUIDv7 lane ID,
process generation, globally unique server epoch, process group, and log
drainer. Each run worker performs HTTP Upgrade and becomes a distinct masked
WebSocket client over the appropriate socket. The adapter handles
text/continuation frames, ping/pong, close, size limits and invalid frames, then
hands normalized JSON-RPC objects to the existing correlation layer. The
`app-server proxy` command is not part of the supported v1 topology because it
preserves WebSocket framing while adding another process.

Dedicated lane sockets use `/tmp/dolgorae-<uid>/c/<compact-hash>.sock`; the hash is the
first 160 bits of domain-separated SHA-256 over the full server key, lane ID,
and process generation. Internal state binds the compact name to the full preimage,
path, device/inode, and process identity. Machine projection exposes only the
resulting SHA-256 identity digest.

Each connection performs its own `initialize`/`initialized`. A legacy public-v1
run remains threadless until first turn; first input uses `thread/start`,
recovery uses `thread/resume`, and history fork uses `thread/fork`. The worker
owns only that client connection, one thread binding, correlation, lifecycle,
audit and interactions. The connected App Server owns turn execution, commands,
native subagents and its process tree. A narrowly tested user-input connection
may advertise `experimentalApi`. Caller-workspace connections also advertise
it for the pinned `thread/settings/update` request and
`thread/settings/updated` notification. This opt-in does not add dynamic tools
or authorize other experimental methods.

Modern starts initialize a dedicated thread before successful startup and
verify the selected runtime policy. Writable startup uses the existing
prepare/apply/verify/commit writer transaction without a synthetic Turn.
Initial runtime qualification, configuration readback and turnless Thread
binding or original-Thread rebinding share one monotonic process-census deadline
for each new physical Worker connection, capped at ten seconds and the session
transport budget. They may wait for same-user Dedicated startup
descendants, retaining each observed PID and BSD birth until exact kernel
absence. Every sample revalidates both leaders, socket identity and complete
scope membership. Identity drift, scope escape, unreadable census and Worker
workload fail immediately. Live startup children reset the empty count;
qualification still requires five complete empty samples 100 milliseconds
apart. Only the actual owned Dedicated Server creates this in-memory startup
window. Binding success or failure consumes it permanently; connection startup
also closes it before WorkerReady when no eager Thread binding is requested.
Diagnostic phases and later resumes cannot restore it. No task Turn or provider
dispatch occurs inside the window. Failure retains the original or provisional
Thread, unknown-state retry fence and Writer guard. Policy proofs and access
transitions retain their immediate refusal.
For a prepared writable Send or Submit that reuses an active Writer, the CLI
wraps policy verification in the existing admitted-mutation request. The Worker
validates the exact admission, Controller and write intent before verification;
verification is preparation and does not complete the parent Turn admission.
Unwrapped verification remains serialized behind any pending mutation. The
original Thread, idle-state, process-census and Writer proof checks still apply.
New caller Worker generations publish [private runtime schema version 2](../protocol/dolgorae-worker-runtime-v2.schema.json) before
Dedicated launch. Its bounded startup ownership witness binds the immutable
Worker generation, app-server epoch, Dedicated identity and physical socket.
The control locator and its replaceable socket epoch remain separate. Every
admitted child and each verified nonleader ancestor are durably recorded before use; the
witness becomes complete only after binding and final empty-scope verification,
before WorkerReady. An incomplete witness cannot prove generation absence,
even with no recorded children. Version 1 remains unchanged; frozen readers
reject version 2 rather than ignore its ownership evidence.
For reusable caller Specialists, the aggregate Role snapshot digest is the
accepted Markdown Role body digest from Agent Configuration v3. Membership
authorization verifies the stored configuration digest before using that capture;
it does not hash the mutable source or substitute the facade's role reference.
Legacy member bindings retain their original role-reference digest.
Primary startup applies authenticated orchestration launch policy to the
already-resolved request. It checks the launch's Controller, parentless mode
and policy-name binding without passing the resolved schema through authoring
request resolution again.
Worker startup ownership serializes all runtime writes. Rewrites preserve the
latest witness revision, children and completion state; connection completion
reloads the committed record instead of overwriting it with a startup copy.
Normal control-socket replacement revalidates the generation and recorded
scopes without reopening startup. Strict quiescence, stale-runtime cleanup and
Writer retirement consume the same recorded identities and complete scope
censuses. Unknown members never become owned through a shared UID or scope;
signals require exact revalidated ownership. This does not remove the existing
between-poll escape limitation of the trusted personal-alpha model.
A startup-only group/census mismatch discards its sample after each differing
nonleader PID has a matching Dedicated identity or an exact `ESRCH` lookup.
Typed observations are validated before the lookup; disappearance cannot erase
an observed violation. Every other scope, leader, socket, Worker and tracked-child
check still runs before discarding the sample. The discarded sample resets the
empty count without extending the deadline or satisfying qualification.
An unavailable turnless binding after process loss is an explicit recovery
limit; recovery cannot report a replacement thread as the original. Accepted
instruction captures and startup selection survive that failure independently
of native thread persistence.
Thread identity and empty-history storage do not prove that native startup
settings survive process loss. Rebinding supplies the frozen launch inputs and
the recovery-selected read-only policy, then checks the returned binding and
settings. A bounded writable-resume mechanism probe may supply its accepted
startup settings explicitly; that probe does not authorize automatic writer
reacquisition during product recovery.
Partial caller-thread startup keeps its nonretryable `OUTCOME_UNKNOWN` result
through Worker startup handoff and Dedicated startup error projection. Cleanup
does not turn an accepted, uncertain native effect into a retryable no-op.

The profile manager spawns the singleton suspended in a new process group.
Its `posix_spawn` attributes use `SETSIGDEF` for every catchable signal and
`SETSIGMASK` with an empty mask so caller signal state does not leak into Codex;
there is no child-side callback. The manager
records the direct executable identity before continuation, and publishes it
only after WebSocket compatibility validation. It owns the singleton process
identity and epoch. The singleton uses null stdin and sends stdout/stderr to a
profile-scoped Dolgorae log-drainer in its process group; it never inherits a
CLI or command-substitution pipe. The drainer applies diagnostic redaction and
maintains mode-0600 `server.log` and `server.log.1` at 1 MiB each. Its exact
identity is part of profile state, and loss fences new attachment until a
controlled restart. Run workers neither signal the singleton nor its drainer.
The profile manager owns their lifecycle. A dedicated worker requests
lane-generation startup through the same staged manager logic and owns only
that generation's exact recorded lifecycle; it never signals an unrelated
process or the shared server.

### Sticky Execution-Lane Topology

The current architecture has one shared read-only server lane and zero or more
Run-owned dedicated logical lanes per profile. Lane choice is immutable. A
shared Run's thread is never loaded by a dedicated server; a dedicated Run's
thread is never loaded by the shared server or a different dedicated lane.
Read/write policy changes and workspace writer acquisition occur within one
dedicated process generation. A shared Run that later needs write creates a
lineage-linked dedicated write continuation.

Each dedicated lane has a UUIDv7 lane ID, append-only process-generation
journal, globally unique server epochs, short socket identity, exact leader and
log-drainer identity, and process census. A new legacy public-v1 Run publishes
the logical lane with null thread and absent physical server; first input
starts its initial generation. Modern startup succeeds only after initializing
the physical generation and thread and verifying the selected access policy.
Its physical server may also be absent while the Run is paused.
Resume starts a new generation only after exact old
generation/descendant absence, five complete empty samples, no active or
unknown turn/interaction/native descendant, and a durable-history barrier.
The thread then resumes in the same logical lane. Infrastructure state,
workspace writer authority, effective Codex policy, and background workload
state are four independent facts.

The Worker persists the resumed thread generation and a new unverified policy
epoch for its current native server before publishing idle. Resume performs no
Turn submission. External member retirement reconciles the global membership
journal after Run closure; Profile shutdown never adds observations after a
Run's terminal seal.

One canonical workspace has at most one writer. A profile may have concurrent
dedicated writers in different workspaces; the home coordinator serializes
launch contracts, not workspace writer cardinality. Profile stop/restart and
migration enumerate the shared lane plus every dedicated-lane record. Restart
brings back the shared server and starts dedicated generations lazily on Run
resume.

The writer component owns the workspace-scoped revisioned authority record and
its permanent writer and handoff lock identities. The workspace component
creates that private layout, while the semantic composition layer coordinates
short PREPARE and COMMIT mutations with Controller-authorized worker policy
changes. No filesystem lock remains held while a worker or App Server answers.

`control_mode` is independent of `purpose` and lane. Direct interactive Runs
are controlled by a human CLI or interactive client. Managed Runs are
controlled by an orchestrator, the internal Orchestration Broker, or automation.
The semantic service requires explicit control mode, purpose, execution lane,
and assurance for every Run. A product facade may resolve those fields from an
explicitly selected use case, but hidden defaults do not exist. Purpose and its optional creation label are
immutable. Only the Controller sees and resolves full normalized interactions
through `run interaction get`; observers receive strict summaries without
payload, response-schema, artifact, thread, turn, item, or server identity. No Controller capability
enters LLM-visible data.

#### User-Case Facades and Internal Topology

The product-facing choice is exactly one of two use cases. Internal topology is
a durable relationship over independent Runs rather than a third public mode.

| Product use case | Root or Primary | Specialist control | Durable aggregate owner |
| --- | --- | --- | --- |
| **Dolgorae-Orchestrated Session** | one `direct_interactive` Primary Run | internal Orchestration Broker with one credential per `managed_agent` Specialist | Dolgorae |
| **External Specialist Engagement** | Primary Agent remains outside Dolgorae | external `workflow_orchestrator` or `automation` Controller | Dolgorae for Specialist operational state, external AI for semantics |

The unchanged public v1 mapping is deterministic:

| Aggregate mapping | Public or internal trigger | Public parent projection |
| --- | --- | --- |
| Orchestrated Session root | public `StartRun`, `direct_interactive`, protected `human_cli` or `interactive_client` carrier with launch intent, no parent | none |
| Brokered Specialist | internal broker operation with a broker-held `automation` Controller | `dolgorae.orchestrated-session.v1 / specialist / <session_id>` |
| External Specialist Engagement | private `open_external_engagement` creates an empty durable engagement; private `hire_external_specialist` adds each member | `dolgorae.external-specialist-engagement.v1 / specialist / <engagement_id>` on hired Run projections only |

The client supplies every low-level Run field explicitly. Classification does
not infer lane, purpose, assurance, profile, model, effort, instructions, or
native-subagent policy.

A Dolgorae-Orchestrated Session with no active Specialist is in **Standalone
Primary composition**. Adding at least one owned Specialist produces
**Brokered Hierarchy composition**. This is a dynamic aggregate state, not a
public mode, alternate lifecycle, or change of orchestration owner.

The authoritative internal aggregate model is:

```text
AggregateBootstrapOperation
  operation_id + aggregate_kind + aggregate_id
  idempotency_key + request_digest + provenance + state

OrchestratedSession
  session_id == primary_run_id
  bootstrap_operation_id
  status + approval_policy + immutable Specialist Policy snapshot digest + revision
  brokered members[]

ExternalSpecialistEngagement
  engagement_id + external_controller_ref
  bootstrap_operation_id
  status + revision
  hired members[]

SpawnOperation
  aggregate + idempotency + nullable pre-approval child Run + lifecycle
  accepted provisioning states bind one preallocated child Run

RunAggregateBinding
  aggregate kind + aggregate ID + bootstrap/spawn/hire operation ID
  optional role + role snapshot digest + Agent Configuration digest

SpecialistTask
  source provenance + target Run + inherited priority + result delivery state

CollaborationExchange
  source Specialist + target Specialist + root task + execution/delivery state

MailboxItem
  target or source Run + sequence + kind + priority + claim/delivery state

ActivationOperation
  Run + trigger + compare-and-swap lease + activation outcome
```

A Run may belong to only one active aggregate. Active reparenting, role
conversion, and transfer between the two use cases require a new Run. In an
External Specialist Engagement, a Specialist cannot hire another first-class
Dolgorae Specialist in v1; the external AI hires each role directly. Native
Delegation remains an in-Run runtime detail.

Codex Profile and Agent Configuration are distinct. Codex Profile owns the
executable, `CODEX_HOME`, deterministic environment, process-static Codex
configuration, and verified capabilities. Agent Configuration owns the role
reference and normalized instructions, model, default effort, purpose, required
capabilities, and Codex Profile snapshot reference. Multiple roles may share
one Codex Profile when their process launch contract is identical.

`parent_ref` remains authority-neutral provenance and presentation metadata.
Reserved namespaces are emitted only after the authenticated Broker or External
Specialist Facade has accepted an operation; receiving the same bytes through a
raw Run request is rejected. For a brokered Specialist, authoritative
membership, parent Run, role, ownership, and lifecycle come from the
orchestration journal. For an external Specialist, the durable engagement,
bootstrap and hire operations, member record, and validated external provenance
own operational membership. Listing or grouping parent references never
reconstructs missing authority.

Native Subagent Policy is orthogonal to both use cases. Native children share
the parent Run's thread tree, policy, and authority, and never become aggregate
members, Independent Specialist Runs, peer Workers, or Dedicated Lane Servers.
The selected Codex Profile must explicitly acknowledge
`native_subagents: enabled`; v1 does not claim disable enforcement for the
Codex 0.158.0 minimum runtime. The original negative probe remains historical
0.147.0 evidence.

Instruction composition is split into a generation-immutable role and behavior
contract and a Turn-scoped access context. The immutable contract does not
contain current access. Each Turn receives authoritative access, writer facts,
`policy_epoch`, and the closed network policy immediately before acceptance.

`run create-write-continuation` remains separate from history fork. It is used
for a `shared_readonly` source or a Dedicated source whose in-place policy
transition is unavailable or unverified. A verified Dedicated policy change is
performed in place on the same logical lane, process generation, connection,
and thread and increments only `policy_epoch`.

Codex 0.147.0 achieved only `best_effort_personal_alpha`. Same-home
coexistence, Sticky Dedicated policy transitions, different-workspace
concurrent writers, closed-generation history resume, process census, and exact
cleanup passed for the tested configuration. Cross-server same-thread migration
and background-terminal discovery failed. The corrected native-subagent
campaign proved lifecycle observation and restart history, while disable
enforcement remains unavailable. These results do not claim strong containment.

### Profile Registry and Singleton Membership

`~/.dolgorae/profiles.yaml` is user-global and stores named Codex Profile launch
definitions, including explicit non-secret environment values but no
credential. The home root lock serializes registry reads and atomic
write-temp/fsync/rename/directory-fsync updates. Agent Configurations are
separate immutable Run snapshots and are not inferred from Profile display
names. Different names with an identical resolved launch contract and
`server_key` remain valid aliases; a Run records the selected name and complete
snapshot.
`~/.dolgorae/workspaces/<workspace-id>/specialist-policies/` stores checked named
Specialist Policy JSON documents. A launch resolves one explicit name, validates
all referenced Agent Configurations against current profile capabilities, and
copies the complete policy plus JCS digest into the session before root Run
allocation. Existing sessions never reread the registry.
The mode-0600 `~/.dolgorae/state.json` `global-profile-v2` generation marker
and mode-0600 `profile-bindings.json` are validated before
stateful access. Unmarked nonempty, malformed, unsupported, partial, and mixed
homes fail closed as `LEGACY_STATE_UNSUPPORTED`; no legacy bytes are inspected
for migration or changed. The gate is active for every production stateful
command.

One locked global registry read produces a `ResolvedGlobalProfile`; launch preparation
consumes that owned value and embeds the complete binding in the workspace-
scoped `run-manifest/v2`. Recovery accepts only that manifest's complete
definition, launch snapshot, and JCS digests. It has no registry or caller
environment input.

The affected-contract census is:

| Contract | Successor decision |
| --- | --- |
| global registry and binding history | `global-profile-registry/v1` plus `profile-binding-history/v1` |
| Run manifest and binding | `run-manifest/v2` plus `global-profile-binding/v2` |
| Profile Server state | `profile-server-state/v2` records the complete named launch snapshot, process/socket identity, epoch, and membership revision |
| runtime discovery | new `runtime-discovery/v2` binds a Run to an exact server generation |
| membership journal/index | new `profile-membership/v2`, global by `server_key` |
| Agent Configuration | `agent-configuration/v2` binds the selected Profile and binding digest |
| machine success/error envelopes | `machine/v2` and `error-contract/v2`, prepared by TASK-036 |
| public Profile DTO | unchanged: it already presents a selected launch contract and capability status |
| Profile diagnostics/events | unchanged in TASK-037: their Profile and server-key fields retain their meaning |
| Specialist facade and review tools | `external-specialist-facade/v2` and `specialist-review-tool/v2` use explicit global Profile selection |

The global membership lock remains held from the empty-membership proof through
the destructive lifecycle commit. Admission uses the same lock. Consequently,
stop, restart, migration, removal, replacement, and physical generation change
cannot race a member into a server after the guard has decided it is quiescent.
Both `active` and `unknown` outcomes block; corrupt or mismatched journal/index
bytes return `PROFILE_MEMBERSHIP_INCOMPLETE` and also fail closed.

The Dolgorae home contains a canonical-home coordinator at
`homes/<home-key>/{home.lock,active.json}` and contract state at
`profiles/<server-key>/{server.lock,state.json,membership.jsonl,members.json,diagnostics.jsonl,epoch,server.log,server.log.1}`.
The server directory has only this lifecycle state and membership file set;
there is no parallel `runtime-*` journal. All components are current-uid-owned
mode 0700/0600 and descriptor-relative.
Membership lock acquisition validates the opened `server.lock` descriptor as a
current-uid-owned regular file with mode 0600 before locking it; invalid existing
nodes are rejected without permission repair or membership mutation.
The socket node uses the validated short path
`/tmp/dolgorae-<uid>/p/<base32-first-160-server-key-bits>.sock`; its full path
and device/inode are recorded in profile state.

Profile state records the restorable immutable launch snapshot, server and
launch-contract identity, canonical home, process/executable/log-drainer/socket
identity, lifecycle, compatibility verdict, server epoch, and membership
revision. Migration and quiesce revisions live in the home-authoritative
`active.json` transition record. The
hash-chained, directory-fsynced `membership.jsonl` is authoritative;
`members.json` is an atomic derived snapshot bound to its revision/checksum.
Membership records workspace/run/controller, worker generation, thread,
connection, lifecycle, writer, observed epoch and runtime locator. Startup and
Operator lifecycle checks accept legacy manifest v2 and caller manifest v3
with the same Run, workspace and global Profile binding identity checks. A
caller's manifest version alone never makes it an orphan. Startup replays the
journal, validates every referenced manifest/runtime record, and
rejects missing/corrupt/revision-mismatched history; it does not rebuild by
scanning incidental project directories. The common membership reader and
appender compare the derived index and persisted server-state identity/revision
before any append, so an ordinary write cannot conceal an earlier inconsistency.
Replay rejects any nonempty journal without a terminal LF before either reads
or appends can change the journal, derived index, or server state; it never
normalizes an unterminated tail.
Operator repair verifies the valid
prefix and exact confirmed orphan and appends a tombstone/new revision. A
startup transaction holds
home-keyed `home.lock` before contract-keyed `server.lock`, validates or claims
the sole `active.json` contract, then validates or reserves a monotonically higher epoch,
persists state, registers and fsyncs membership, connects and initializes the
worker, then publishes its generation ready. A new process always consumes a
new epoch; reconnect does not.

Stop/restart uses fence, unlocked quiesce, and commit phases. Fence persists a
quiesce revision under operator/home/server locks and rejects new work; no lock
is held while turns or processes are awaited. Commit revalidates the same
revision and exact identities, then proves process-group, log-drainer, and
socket-inode absence before clearing state. Restart invalidates old connections
and forces cross-epoch member reconciliation. Corrupt, missing, or unverifiable
membership blocks the operation; an apparently empty partial index never
authorizes termination.

Worker startup acquires its Run startup range with the normal ten-second
contention budget, then revalidates the home and server lifetime while retaining
that range until the child publishes its runtime record.
Unlocked quiesce shuts down each verified Run worker through its frozen
identity-bound control path, then acquires that same range before opening its
ledger. It records the result as the audit-v1 `profile_observed`
operator-override payload keyed by the quiesce revision. Thus no Profile Server
signal precedes the durable per-Run terminal, quiescent, or uncertain result,
and an in-flight worker election cannot escape the Profile fence.

Server-key migration is a home transaction. A generation-starting command may
perform it without operator authority only after exact process/socket and
complete membership evidence prove the source has zero live or orphan members;
interrupting or otherwise non-quiescent migration remains operator-only. Old
and new server locks are acquired in ascending decoded-key order. A
home-authoritative migration record with a valid ID, phase, and canonical
recorded server keys fences both stop and start reservations,
prevents unrelated transition-token replacement and double membership, and
is checked and created under the home and ordered old/new server locks for
both migration authorities. An active record with an invalid transaction ID
or a record with an unknown phase fails closed; `migration_blocked` stays
active. Failure before new ready retains old membership or lands in
`migration_blocked` when rollback cannot be proved; phase-write and start
authorization failures after the old stop use that same compensation path.
Migration stop distinguishes process termination from durable commit and can
settle a partial commit idempotently. Final record persistence retries before
fencing the ready replacement as `migration_blocked`. Operator migrate can
reconcile that fence without manual process or JSON edits by proving the exact
ready old/new generation and terminalizing the recorded transaction. Confirmed
keys pass canonical identity validation before profile-path construction.
Doctor launch probes use a no-rollover start policy. A blocked transaction with
neither generation present is repairable only through operator state reset's
two-lifetime absence proof; `prepared` and `applying` are never terminalized by
that recovery while their migration may still be in flight. A concurrent
duplicate rollover attaches when re-proof finds the requested generation
already ready.
An operator reset may target a previous active server key after the current
Profile contract changes. It binds that key to the same canonical home and
Profile through the prior state, rechecks the active record under the home and
server locks, and never clears an unrecorded lifetime.
Membership mutations take home then server locks. New registration checks the
exact ready home record and migration fence under those locks, closing the
quiescence-to-stop admission race; release remains valid for shutdown cleanup.

The global order is operator, home, server keys in binary order, handoff,
Run lifecycle, writer, run startup locks in UUID-byte order, then in-process
run mutation mutexes in the same order. Worker startup admission is an inversion: it
takes one Run startup range before home/server revalidation and retains it
through `bound`; no path may hold a home or server lock while waiting to acquire
a Run startup range. Home/server locks are released before spawn; only that
startup range spans the bounded `bound` wait and is released before `ready`
or on failure.

Explicit quiescent build handoff is the other bounded exception. It holds one
Run lifecycle lease and startup range before taking the workspace writer lock.
Under that writer boundary it proves current Controller and retained state,
no pending admission, interaction, unresolved Submit or unknown outcome, no Run
writer holder, and the exact idle worker with no owned workload. Only then may
it call frozen control-v1 Status and Shutdown (each with a ten-second deadline)
and wait up to ten seconds for verified process exit. Those idle control paths
do not acquire writer, startup, home or server locks. Handoff acquires no upward
lock while the writer lock is held, and releases it before replacement startup
or home/server revalidation. The startup range remains held through replacement
publication. This fence prevents an admission between quiescence proof and
shutdown; it does not permit waiting for a Turn, user or unverified process.

Every other operation persists a revision-bound intent and drops file locks
before process, network, turn, or user waits, following the global order
without acquiring upward.

### Persistent Run Store

The Run store is located below the per-workspace Dolgorae-home state
root and is never agent-writable. `audit.jsonl` is the sole per-Run event
authority. `state.json`, transcripts, status views, and exports are
projections. The Codex thread remains independently stored in the pinned
`CODEX_HOME`.

| Concept | Sole authority |
| --- | --- |
| Profile launch contract and accepted migrations | Immutable profile snapshot and accepted generation contracts |
| Shared singleton process, log drainer, and server epoch | Profile manager `state.json` under home/server serialization |
| Dedicated logical-lane generation identity, server epoch, census, and log drainer | Run lane-generation record under writer/run serialization |
| Membership | Append-only profile `membership.jsonl` |
| Run lifecycle and active turn intent | Run audit ledger reconciled with App Server history |
| Codex root thread | Run thread binding |
| Controller | Run controller record and generation |
| Workspace writer | Durable workspace `writer.json` |
| Interaction | Run interaction journal |
| Client event | Schema-validated durable event record in the run ledger |
| Projection/replay metadata | Delivery-time envelope |

Derived indexes, runtime locators, sockets, and materialized projections never
override these authorities.

Profile operations have their own bounded diagnostic journal and cursor because
startup can fail before any Run exists. Its minimal same-uid projection contains
only redacted status/code/message; operator-authorized operational projection
may add bounded redacted detail. A Run directory, ID, and audit genesis are
published only after profile state commits a ready non-null server epoch.

The run-private artifact store is a bounded projection adjunct, not an event
authority. It accepts only exact file-change diffs, bounded `user_input`
payload artifacts, and final responses; writes create-exclusive mode-0600
files; records byte length and SHA-256; and enforces 8-MiB/file-change or
user-input, 32-MiB/final-response, and 256-MiB/run quotas. Public reads use
opaque artifact IDs and verified base64 chunks of at most 1 MiB. Inline final
responses are at most 1 MiB. Client presentation and download limits may be
stricter but never enlarge provider bounds; complete downloads verify both
length and digest. Artifact
metadata carries `observer` or `controller_only` visibility; interaction-derived
artifacts are controller-only. Internal paths and reasoning content never cross
the machine boundary.

The manifest stores controller metadata, a domain-separated capability digest,
controller generation, accepted profile/model, closed purpose and optional
label, parent metadata, required/validated capabilities, instruction-contract
versions, normalized Controller-instruction length/digest, and the initial and
current default effort. It also stores an optional immutable aggregate binding.
A Primary binding carries Orchestration Session ID, Aggregate Bootstrap
Operation ID, and Specialist Policy digest. A Specialist binding carries
aggregate kind and ID, spawn or hire operation ID, role reference, role snapshot
digest, and Agent Configuration digest. Generic low-level Runs carry no binding.
The binding is Run-side reconciliation evidence and must match, but never
replace, SQLite aggregate authority. These facts reconstruct
`RunConfigurationProjection` after restart; workspace/profile defaults never
overwrite an existing Run.
Raw capability bytes exist only in the caller-owned credential
carrier and are consumed before worker discovery; they never enter argv,
environment, logs, audit, runtime records, or machine output.

For public gRPC, the only accepted Controller carrier is an absolute protected
file reference below the canonical mode-0700 Dolgorae-home directory
`controller-carriers/`. The Protobuf request contains the path and
expected public Controller ID/generation, never capability bytes. The semantic
service reopens the file beneath an already validated directory descriptor and
revalidates root containment, regular-file type, no-symlink identity, current
UID, mode 0600, 4-KiB bound, Controller identity/generation, and target-Run
authorization immediately before each authorized read or mutation. The
side-effect-free `VerifyController` operation runs this same check without
opening a worker or changing durable state.

The capability response publishes the checked credential schema identity and
digest plus the carrier policy. This permits a trusted Gul backend to create a
new generation-1 credential with create-exclusive semantics under
`controller-carriers/gul/<installation-id>/`; it does not grant Gul access to
Operator credentials or add a credential-generation RPC. Continuation
authorization compares normalized principals and requires a new Controller ID
and capability.

### Durable Aggregate Store

`~/.dolgorae/workspaces/<workspace-id>/orchestration/orchestration.sqlite3` is the sole
transactional authority for aggregate bootstrap operations, Orchestration
Sessions, External Specialist Engagements, membership, spawn or hire operations,
Specialist tasks,
Collaboration Exchanges, mailbox items, activation operations, and result
delivery. It uses SQLite WAL, foreign keys, `synchronous=FULL`, a bounded busy
timeout, and one workspace mutation owner. A hash-chained append-only
`orchestration_event` table commits with state changes. `orchestration/state.json`
and JSONL exports are replaceable materializations validated against
[`dolgorae-orchestration-state-v2.schema.json`](../protocol/dolgorae-orchestration-state-v2.schema.json)
with cross-object invariants enforced by the Rust orchestration implementation.

The state owner creates an External Specialist Engagement entirely inside one
SQLite transaction. Orchestrated Session bootstrap coordinates SQLite with the
Primary Run ledger through preallocated identities and the same bootstrap
operation ID recorded in both stores; neither store is allowed to infer or
overwrite the other after a crash.
The state owner allocates every child Run ID and commits a write-ahead spawn or
hire operation before creating a Worker, physical lane generation, Codex thread,
or other external effect. It commits a Collaboration Exchange, target mailbox
item, event row, and optional activation marker before issuing an in-memory
wake. It commits successful execution, immutable result reference, source result
mailbox item, and delivery-pending state in one result-outbox transaction.
Aggregate and Run state coordinate through immutable operation IDs, idempotency
keys, request digests, and monotonic revisions.

Recovery may resume an operation proven not to have crossed its effect boundary,
redeliver an already committed result, reactivate a passivated Run, or return an
expired claim to the queue when no Turn acceptance evidence exists. It never
silently creates a replacement Specialist and never reruns work whose acceptance
or outcome is unknown.

The orchestration database is not a task-planning database for an external AI.
For an External Specialist Engagement it stores only the accepted Specialist
boundary, member ownership, task submission, result, delivery, and recovery facts
needed to make Dolgorae's own effects durable. Specialist-to-Specialist
Collaboration Exchanges are accepted only for one active Orchestration Session.

### Mailbox Scheduling and Virtual Actor Residency

The Mailbox Scheduler is event-driven and has no per-Specialist polling task.
One in-memory dirty Run set coalesces mailbox changes, Turn completion,
activation completion, and state changes. A single notification wakes the
scheduler, which consults SQLite and the authoritative core Run state. One
global reconciliation timer, default 30 seconds, repairs missed wake signals and
expired pre-dispatch claims.

For a dispatchable Run, the deterministic selection key is:

```text
internal recovery or lifecycle work
starvation override
root priority: interactive, normal, background
dependency-unblock boost
earliest deadline
oldest mailbox sequence
```

A request arriving while the target is running or waiting remains queued. The
scheduler never preempts an active Turn. At most two consecutive same-source
items are selected when another source is eligible at the same effective
priority. Queue, source-target, session, blocking-wait, depth, and root-task
limits come from the immutable session collaboration-policy snapshot.

Actor residency is separate from core Run lifecycle:

| Actor residency | Process meaning | Scheduler action |
| --- | --- | --- |
| `unstarted` | member provisioned but first generation is not ready | wait for spawn operation |
| `resident` | Worker generation is present | dispatch only when core Run is idle |
| `passivating` | safe shutdown is being committed | retain queue and reevaluate after completion |
| `passivated` | logical Run and thread persist, Worker is absent | request `on_mail` activation |
| `activating` | one activation owner holds the lease | retain additional mail |
| `recovering` | crash reconciliation is active | retain mail, do not dispatch |
| `unavailable` | automatic activation or compatibility failed | retain mail and expose blocker |
| `terminal` | closed, retired, or released | reject new work |

Activation uses a `passivated -> activating` compare-and-swap, a bounded lease,
and the existing Worker, lane, thread-resume, and compatibility protocols. A
failed activation never consumes the queued request. Safe passivation requires
idle Run state, no writer, no pending interaction, no blocking outbound
exchange, no dispatchable mail, verified background cleanup, and resumable
thread state.

## Process and Transport Topology

For N live Runs across P active launch contracts, the normal baseline is N
workers and N WebSocket connections. Each active contract has at most one
shared-read-only Profile Server plus one physical Dedicated Lane Server for
each currently running dedicated generation. A stopped logical lane contributes
no process; a successor replaces, rather than overlaps, its prior generation.
Commands and supported Codex native-subagent work may temporarily create
additional descendants inside the selected physical generation.

```text
Machine CLI invocation ----+
                           |
Gul -- gRPC/HTTP2/UDS -----+--> shared semantic service
                                   +--> Orchestration Broker
                                   +--> Collaboration Service
                                   +--> SQLite mailbox and event authority
                                   +--> Mailbox Scheduler / Activation Manager
                                   |
                                   | private framed JSONL/UDS
                                   v
dolgorae worker [run R, worker generation G]
  |
  | HTTP Upgrade + masked WebSocket over selected private Unix socket
  v
  +-- shared_readonly -> shared Profile Server [server key K, epoch E]
  |
  `-- dedicated -> logical lane L -> Dedicated Lane Server [epoch E2]
                                      |
                                      +-- Codex-owned command/native descendants
```

The worker control socket resides below `/tmp/dolgorae-<uid>/s/`. Dolgorae opens
`/tmp` without following symlinks, creates each missing component with
`mkdirat`, validates `EEXIST` with `fstatat`, and accepts the root only when each
private component is owned by the current uid with mode 0700. The root is
volatile OS-managed state and is recreated after tmp cleanup. No-symlink enforcement applies below the
resolved `/tmp` root; operations beneath the directory use descriptor-relative
`*at()` calls. Its filename is the RFC 4648 uppercase, unpadded, 32-character
base32 encoding of the domain-separated workspace-digest/run-UUID preimage in
SPEC-002.
The composed path must fit the macOS `sun_path` limit; overflow fails with
`RUNTIME_PATH_INVALID`. There is no sibling identity sidecar. The durable
`~/.dolgorae/workspaces/<workspace-id>/runtime/runs/<run-id>.json` record is the sole identity authority for
the volatile socket: an existing path without an exact matching record fails
with `RUNTIME_PATH_COLLISION`, and only the byte-0 winner may unlink it after
the recorded generation is proved absent. Every request also contains the
full workspace identity, run ID, expected worker generation, and boot UUID.
Before it sends any ordinary request the CLI compares its own Dolgorae version,
executable digest, and mutation protocol version against the three that record
published, so a cross-run or version-skewed connection fails closed with
`DOLGORAE_PROTOCOL_MISMATCH` rather than mutating under another build's
semantics. That comparison is an early rejection, not the authority: a build
that predates it would simply not perform it. Every ordinary request therefore
also carries the caller's own version, mutation protocol version, and
executable digest, and the worker refuses a request whose declared build is not
its own under the same code. A separate version-frozen control protocol v1 accepts
only `hello`, bounded `status`, and `shutdown` across binary-digest changes.
Those operations validate workspace, run, generation, boot, and live process
identity; all other requests reject version skew. `shutdown` is identity-bound
and interrupts an active turn before cleanup.

The live worker watches the socket pathname and containing private directory.
On `ENOENT`, it reopens and validates `/tmp`, recreates the private hierarchy,
binds a replacement listener at the deterministic path, records its inode,
increments `control_socket_epoch`, and atomically replaces the runtime record.
Accepted CLI connections and the App Server WebSocket are independent of that listener
replacement. An occupied or unsafe replacement path is fail-closed: the worker
persists `control_recovery_required` in its own identity-matching runtime record,
interrupts an active turn, and stops. The flag survives worker exit without
overwriting the durable Turn outcome. Ordinary mutations refuse it; authorized
recovery removes the locator only after process-absence proof and private-root
validation, preserving every foreign socket occupant.

The actual socket path and process identity are discoverable from
`~/.dolgorae/workspaces/<workspace-id>/runtime/runs/<run-id>.json`; discovery never recomputes a path from
`$TMPDIR`. The record contains the full worker identity tuple and App Server
connection identity: PID, PGID, UID, start seconds/microseconds, live executable
path/device/inode/SHA-256, together with
the boot-session UUID, run generation, access state, socket path, Dolgorae
version, binary digest, IPC protocol version, socket inode,
`control_socket_epoch`, `server_key`, `server_epoch`, and `run_generation`. A
new shared or dedicated lane-server epoch never validates a stale connection
generation.
`~/.dolgorae/workspaces/<workspace-id>/runtime/writer.json` is durable workspace authority, not a recoverable
pointer. It stores the writer state and all facts required to reconcile a lost
worker against its selected lane-server epoch and thread/turn. Other runtime records remain
recoverable caches; the fsynced run ledger owns run history.

Writer transaction and startup locks live at fixed paths below
`~/.dolgorae/workspaces/<workspace-id>/runtime/locks/`. The writer and handoff files are `writer.lock` and
`handoff.lock`; startup files are `startup/<run-id>.lock`. The directory is
opened through descriptor-relative, no-symlink operations, must be
current-uid-owned mode 0700, and resides on the already-required local APFS
workspace. The mechanisms never share an inode. Lock files are
create-exclusive and permanent. The writer lock serializes durable-authority
transactions; it is not a lifetime truth source. The startup file has two POSIX
byte-range locks: byte 0 is the transient CLI starter claim and byte 1 is the
worker lifetime claim. The 8192-byte file body has separate version-1,
zero-padded, SHA-256-checksummed owner records at `[0,4096)` for byte 0 and
`[4096,8192)` for byte 1; a short file is `Unverifiable`. Each record contains the
range, workspace/run/generation, boot UUID, Dolgorae process tuple and executable
path hash. Invalid/unknown/all-zero slots do not override the kernel lock;
locked ranges without a matching valid record are `Unverifiable`. After acquiring its byte
a process updates its slot with `pwrite` on that same fd and fsyncs before
proceeding, then clears and fsyncs the slot immediately before releasing.
`F_GETLK` is
queried for both ranges; `l_pid <= 0` is `Unverifiable`, while a positive
`l_pid` is only a hint and is always checked against
the matching owner record. Normal attachment to an answering socket takes
neither byte. The shared semantic core also excludes overlapping startup-file
users for one Run within a process, before opening the file and until its
last descriptor is closed: POSIX range locks do not exclude sibling threads,
and a sibling descriptor close would release the process's held ranges. This
transient process guard returns `RUN_BUSY` on contention and retains only active
claims; it never replaces the kernel locks or durable identity checks. Lazy
worker election takes the startup range before reading runtime/projection state
or preparing membership and a dedicated epoch, then carries that same descriptor
through the worker handoff. Byte-range acquisition uses `F_SETLKWTIMEOUT` with the Darwin
`flocktimeout` layout and a ten-second relative timeout. After timeout, a
contender may terminate only an exact byte-0 transient starter bound by kqueue
and revalidation. A byte-1 owner is a serving reader or writer worker and
requires control `hello`; a live `Match` that does not answer returns
`RUN_BUSY`, and no activity-derived condition authorizes signalling it.
`Mismatch` or `Unverifiable` is never signalled. After
exact exit, contenders race to acquire byte 0 and only the winner starts or
recovers. A worker that loses byte 1 reports fd-3 `RUN_BUSY` and exits with no
socket, ledger, or runtime mutation. Each owner process opens the file once; identity revalidation uses
`fstat` on that held fd and never a second open, including through a hardlink.
The descriptor is never reopened or closed during ownership and is marked
close-on-exec before the App Server connection opens.

The worker/App Server transport is direct WebSocket over the Dolgorae-owned Unix
socket. The socket supplies local endpoint isolation; WebSocket supplies the
actual app-server framing. The worker remains the correlation and audit
interposition point without owning the shared process.

App-server WebSocket frames are drained independently of every CLI observer.
Every App Server uses null stdin and separate nonblocking stdout/stderr pipes to
its scoped bounded log drainer. A failed file sink switches the live drainer to
drain-and-drop and marks the generation degraded; loss of the drainer process
fences new attachment and requires controlled restart.
Observers read fsynced ledger records by cursor and therefore cannot exert
backpressure on the active protocol stream. Limits are 16 MiB per WebSocket
frame, 32 MiB per reassembled message, 1 MiB per diagnostic line, 2 MiB per raw selected ledger payload with a
3 MiB post-transform allowance, and 8 MiB per
CLI-worker frame. CLI oversize affects only its caller; diagnostic oversize
retains metadata and continues. Invalid or oversized WebSocket input closes the
connection and quarantines
an accepted active turn as `outcome_unknown`. A solicited `thread/read` response
is recognized only after its unique matching top-level `id` appears before byte
16 MiB; outstanding request count is never a classifier. Once recognized it is
consumed by a constant-memory streaming visitor retaining required turn/status
fields plus raw-wire length and SHA-256. It has the 120-second deadline but no
arbitrary total size cap. An ambiguous oversize prefix fails compatibility and
follows SPEC-006's active-turn quarantine rule.

The TASK-006 implementation keeps this boundary split into two internal worker
components. `app_server` owns the strict WebSocket and JSON-RPC transport,
including duplicate-member rejection and server-request precedence; `turn`
owns thread attachment, one-active-turn serialization, durable intent,
interaction correlation, idempotent replay, and authoritative terminal
readback. These components are production adapters used by the per-Run worker,
but they do not themselves authorize an external mutation. Controller
credential ingestion, same-uid observer projection, and public command wiring
remain the TASK-007 boundary.

For TASK-074 startup cancellation, `app_server` obtains its registered Unix
connection through `darwin::DeadlineStream`. This edge reuses the existing
nonblocking connect and deadline adapter while registering owned cancellation
before connect or WebSocket upgrade can wait. WebSocket framing and JSON-RPC
remain in `app_server`; raw socket and poll calls remain in `darwin`. Neither
adapter acquires Controller, process-signalling or replay authority.

## Workspace Identity and Local Layout

The canonical workspace ID is SPEC-002's full lowercase SHA-256 over the
domain-separated libc `realpath(3)` byte sequence. The same raw digest feeds the
socket derivation; no component performs case folding, Unicode normalization,
or an alternate path hash.

The canonical workspace is agent-visible and contains only portable project
policy:

```text
<workspace>/.dolgorae/
  .gitignore
  config.yaml
  roles/  # optional shared Role sources
```

All machine-local configuration and mutable authority are outside the workspace:

```text
~/.dolgorae/
  state.json
  profiles.yaml
  roles/  # optional common Role sources
  rpc/
  controller-carriers/
  operator/
  homes/
  profiles/
  workspaces/
    <workspace-id>/
      workspace.json
      specialist-policies/
      runs/
      idempotency/
        run-start/
      runtime/
        locks/
        runs/
        writer.json
      orchestration/
        orchestration.sqlite3
        state.json
        exports/
      evidence/
      cache/
```

The root is fixed below canonical `HOME`; there is no platform-specific or
environment override. Dolgorae reads and writes no alternate per-user state
root and carries no discovery, migration, or compatibility path for state made
by earlier development versions.

`workspace.json` binds the full workspace ID to the lossless canonical path and
initialization mode. `idempotency/run-start/` is the workspace-scoped allocation
index: one mode-0600 record per `run start` key, named by the key's digest
rather than the key itself, holding the normalized allocation digest and the Run
identity it is bound to. It is fsynced before the Run directory is published, so
a response lost after allocation is reconciled by retrying the identical key
instead of allocating a second Run. A permanent mode-0600 flock file beside the
record, named by the same key digest and scoped to the allocation operation,
serializes reservation lookup, publication, membership admission or failure
cleanup, and receipt capture. The shared semantic core holds it across that
whole allocation; a concurrent identical caller rechecks the published Run
before performing effects. Different allocation keys remain independent.

The index also admits a strict generation-2 caller-start reservation in the same
key space. It records the original Run identity, canonical workspace, Controller
metadata, resolved startup request, immutable Profile launch binding, exact
instruction captures, acceptance time, and Git baseline. A request digest binds
the canonical workspace, Controller, and resolved request independently of live
instruction or Profile files; a separate digest binds the entire accepted intent.
Retries compare the request first and reuse that intent before consulting live
sources. Legacy and caller reservations cannot own the same key. A corrupt
accepted intent fails closed rather than being reconstructed from current files.
Shared Profile readiness supplies the server epoch, advertised model default
and capability snapshot before intent acceptance. Those accepted facts enter
the immutable manifest; retries retain the original resolved model and effort.
Known unsupported admission still fails before reserving a new Run.

Under startup serialization, a retry validates and reuses an already published
manifest, including one whose lifecycle is still `Starting`. Only a missing
manifest is published. Bootstrap and Writer reconciliation continue from the
original durable state; an existing Run is never republished or replaced. A
second lifecycle observation handles a Worker that finishes startup while the
retry checks shared Profile readiness.

The allocation lock is distinct from Writer authority and POSIX worker startup
locks. Both the canonical workspace and its Dolgorae-home
state root must satisfy the v1 local-APFS requirement. The state root is
current-uid-owned mode 0700, mutable files are mode 0600, and no path below it
is included in a Codex writable root or model-visible projection.

Worker and App Server sockets remain below the user-private short `/tmp` roots
specified by the Run and Profile contracts. Those nodes are locators only. An
attach or cleanup decision is authorized by exact Dolgorae-home records,
held locks, process identity, and socket inode.

Git worktrees remain distinct workspaces because each canonical top-level path
has a distinct workspace ID. Codex Profiles may be reused across workspaces,
but Run, aggregate, writer, and lock state are isolated below each workspace
state root. No absolute executable, socket, PID, authentication, Run, or
aggregate state is placed in tracked project policy.

## Manifest and Ledger Model

### Manifest

The manifest is created before externally meaningful app-server work and then
completed with facts learned during start. Its fixed semantic fields include:

- schema version, run ID, canonical workspace and workspace ID;
- Git/non-Git mode and start baseline;
- created timestamp and initial access;
- profile name, argv, expected `CODEX_HOME` snapshot;
- actual app-server version, schema status, and actual `codexHome`;
- Dolgorae version, binary SHA-256, and IPC protocol version;
- fixed model and initial/default reasoning effort;
- immutable run instructions;
- controller ID/kind/instance/subject, controller generation, and the
  domain-separated capability digest (never the capability bytes);
- purpose, optional external label and parent reference;
- required capabilities and the accepted profile capability snapshot;
- Codex thread ID when allocated;
- fork provenance and last confirmed boundary when applicable;
- audit policy and compatibility verdict.

Fields that change during execution belong in ledger events and `state.json`,
not as silently mutable manifest history.

### Audit Ledger

Each JSONL record has this logical envelope:

```text
schema_version
sequence
timestamp
run_id
run_generation
kind
payload
previous_hash
hash
```

Ledger lines use RFC 8785 JSON Canonicalization Scheme (JCS) followed by one
newline. The `sha256-jcs-v1` record hash is lowercase hexadecimal SHA-256 over
the JCS bytes of the record with the `hash` member omitted and the
`previous_hash` member retained. The genesis `previous_hash` is exactly 64 ASCII
zeroes. Sequence starts at one and increases by one. The manifest records the
hash scheme and genesis. Closed and start-failed runs append a final seal event.
`state.json` stores the last projected sequence/hash so truncation or projection
lag is detectable during normal operation; verification still scans the ledger
from genesis.

A `turn_terminal` record carries the whole terminal Turn — thread, turn, status,
reasoning effort, usage, and the final response the notification's authoritative
items supplied — rather than its identity alone. `run status.data.last_terminal`
is therefore reconstructable from durable authority after the worker that
observed it has stopped, which is what a projection-only `status` reads. The
record is appended before any history round trip, so a server that omitted its
items still yields a durable terminal, with the response absent rather than the
terminal lost.

The v1 bootstrap prefix is exactly `workspace_initialized`,
`idempotency_reserved`, then `run_created` or
`write_continuation_created`. The checked idempotency-intent object binds the
operation, caller key, normalized semantic-identity digest, and allocated Run
ID. A short-lived reservation guard prevents concurrent allocation before the
intent is appended; dropping an unaccepted guard releases it, while acceptance
makes the key-to-Run result permanent for exact replay. Replay indexes every
durable intent by operation class and key and rejects a different digest or Run
ID under the same key. Bootstrap is restartable from each exact durable prefix;
a mismatching or overlong prefix fails closed.

Terminal sealing reuses the closed record-kind vocabulary. `start_failed` or
`cleanup_result` supplies the terminal evidence and the immediately following
`lifecycle_transition` is the final seal. Its `previous` state must equal replay,
its `current` state is `start_failed` or `closed`, and `terminal_seal:true` is
permitted only there. Replay rejects an invalid edge, missing evidence,
duplicate bootstrap, dangling terminal evidence, record after seal, or a
canonical parse/serialize result that differs from the stored bytes. Turn,
interaction, reconciliation, and unknown-outcome records must imply an allowed
edge rather than assigning lifecycle unconditionally. A direct running/waiting
interrupt transition to paused/closed additionally carries the checked
`interrupt_terminal_confirmed:true` fact.

Inbound JSON is parsed with duplicate-member rejection and number lexemes held
only through adaptation. The in-repo canonicalizer uses UTF-16 key order and
ECMAScript shortest binary64 rendering and is pinned by RFC 8785 plus Dolgorae
golden vectors; a byte change requires a new hash-scheme version. Before any Dolgorae marker is inserted, every inbound object key
matching `^\$+dolgorae_` is escaped by prefixing one additional `$`. Redaction is
then applied, followed by numeric adaptation; the tokenizer never treats a
Dolgorae-owned marker key as a candidate secret key. A decimal whose finite
binary64 ECMAScript rendering is not numerically equal to the original is
replaced before JCS with
`{"$dolgorae_number":"<original-lexeme>"}`. Invalid
JSON, duplicate members, and otherwise unrepresentable payloads never reach the
canonicalizer. Verification requires each stored line, excluding its newline,
to be byte-identical to the JCS serialization of its own parse. Timestamps use
UTC RFC 3339 with exactly six fractional digits and `Z`.

Each complete line is appended to an `O_APPEND` handle with `write(2)` retried
until all bytes are written. Ordinary streaming records may be group-committed
for at most 100 milliseconds. Using `fsync(2)`, the ledger is synchronized
before every externally observable effect: turn intent/idempotency precede
`turn/start`, approval decisions precede responses, cleanup intent precedes
signals, and preserved-tail evidence is fsynced before ledger truncation. It is also synchronized before
Dolgorae acknowledges an accepted turn ID, pending Controller interaction, terminal
result, or access/lifecycle change. Manifest creation and atomic state
replacement synchronize their containing directories. V1 claims process- and
OS-crash durability after these barriers, not power-loss durability.

The writable audit handle also owns a nonblocking exclusive BSD `flock(2)` for
its lifetime, so a second local writer fails before it can replay, repair, or
append a competing successor. The audit anchor lock is explicitly released after
the final pending flush and scheduler join, even if forked or duplicated
descriptors survive. A guard acquired immediately after flock also releases it
on later constructor errors. Replay is a pure projection operation separated
from file durability and consumes at most 512 MiB, 1,000,000 records, and the
normative five-minute worker budget. Append advances a checked projection with
only the candidate record, rejects the first byte or record that would cross
the replay ceilings, and advances a cached conformance state rather than
revalidating the durable prefix. Sparse observer paging seeks to the first
record after its cursor. The scan, pure projection, and initial conformance
reconstruction share one deadline. Authoritative head, projection, durable
record, and event-page reads are fallible and surface scheduler poison instead
of substituting stale state. These boundaries keep persistence ordering
independent from projection semantics and prevent repeated whole-history append
or paging work.

Any malformed or invalid newline-terminated record, including the final record,
a complete record with a broken hash, or any sequence discontinuity is an
audit-integrity failure. Only nonempty bytes after the file's last newline are
a recoverable torn tail, even when those bytes parse as a complete JSON object
but lack the terminating newline. Recovery writes and fsyncs the deterministic
sequence/hash-named evidence, truncates and fsyncs only those bytes, then appends
and fsyncs `ledger_tail_repaired`; restart completes any prefix idempotently. A torn
tail is never reported as ordinary tampering.

Each selected payload is at most 2 MiB raw and 3 MiB after representation. A
larger or unrepresentable payload becomes a `payload_unrepresentable` record
containing source kind, observed byte length, streaming SHA-256, JSON Pointer
when known, and reason; no original bytes or sidecar are retained. The ledger
includes CLI intent accepted by the worker, lifecycle transitions,
run generations, app-server requests/responses/notifications after
redaction, normalized client events and interactions, controller resets,
approval decisions, state reconciliation, and cleanup results. Its
completeness claim covers Dolgorae lifecycle, main-turn wire traffic exposed by
app-server, approvals, writer-authority transitions, and profile/account provenance. Native
subagent or other content not exposed in plaintext by app-server is retained
only as an opaque event and is not claimed as reconstructable audit. Reasoning
text, summaries, deltas, and internal planning streams are discarded before
representation. Their method, byte length, digest, and suppression reason are
the only durable accounting.

Receipt classification is versioned with the pinned protocol surface. V1
suppresses `item/reasoning/*`, `item/plan/*`, `turn/plan/*`, and reasoning or
plan items carried by `item/started` and `item/completed`. The raw 2 MiB limit
is enforced before classification parses any payload. Normalized client-event
integers stay within the exact JCS-safe interval, and checked tooling enforces
calendar-valid timestamps and every declared UTF-8 byte bound with the same
semantics as Rust.

Client events are normalized into discriminator-bound durable records and
schema-validated before their ledger append. A record owns the canonical
decimal-string cursor and server key/epoch but never a reader's projection or
replay flag. Each observer receives a separate delivery envelope carrying those
two delivery facts. The delivery schema conditionally permits state, final
response, interaction, runtime error, writer, and recovery events in `minimal`;
usage, workspace changes, command, diagnostics, generation, and reasoning-
suppression metadata are `operational` only. Both streams filter the same safe
records by one run-wide sequence cursor; they never project raw audit or app-server payloads. A filtered
record creates a cursor gap rather than a second cursor namespace. Slow or
disconnected observers replay fsynced records and never participate in
WebSocket draining or worker state transitions.

### Materialized State

`state.json` is atomically replaced only to a head at or before the last fsynced
ledger record, at durability barriers and a bounded projection interval. It
contains the current lifecycle state, run generation, thread ID, active/latest turn,
pending requests, access mode, writer-authority observation, default effort, last
event cursor, and ledger head. If it is missing, stale, or invalid, the worker
replays `audit.jsonl` before accepting ordinary mutations; the bounded control
channel remains available from `bound` throughout replay.
If its head is ahead of the durable ledger, it is a stale projection rather
than an audit-integrity failure: Dolgorae rebuilds it and appends
`projection_rewound`.

## Controller and Observer Boundary

The caller creates a self-contained controller credential rather than a global
controller registry. A helper command creates a new mode-0600 file
create-exclusively; integrations may construct the same checked object and pass
it through an inherited descriptor. The CLI validates carrier metadata and
syntax, then transfers the opened descriptor with `SCM_RIGHTS` over the private
worker socket. The request carries public controller generation, invocation,
expected state revision and idempotency facts. Under the mutation lock, the
worker reloads state, reads the bounded secret, compares its digest and all
revision operands, zeroizes it, and applies the transition. No CLI-only check or
`already_validated` claim crosses the serialization boundary.

Observer read paths validate same uid and project/runtime path safety but
require no controller credential. Their output always passes through the
client-safe projection boundary and cannot return full interactions or
controller-only artifacts. Controller read and mutation paths validate the
credential at the serialization point. A controller mismatch deliberately has one error shape so
run existence, controller-ID mismatch, and capability mismatch reveal no
additional credential facts beyond information already available to local
observers.

An installation-scoped operator credential has a separate persisted digest and
generation in the Dolgorae home. Initialization is create-exclusive;
rotation requires the current capability. Profile stop/restart and controller
reset accept it only by protected file/fd, while controller reset accepts the
new controller through a distinct carrier. Server key is public identity, not
authorization.

Operator reset is staged. PREPARE holds operator, then writer when applicable,
then run-startup/run serialization; it revalidates the already-open operator
and new-controller carriers, all blockers and revisions, and fsyncs an operation
token. APPLY drops every file lock before reader-policy/background verification
or worker coordination. COMMIT reacquires the same ordered locks, revalidates
the token, identities and revisions, clears writer authority when proved safe,
and atomically publishes the new controller generation. If APPLY or COMMIT
cannot prove safety, the old controller binding remains authoritative and the
recorded writer failure state is preserved. Paused and outcome-unknown runs
retain their recovery facts. Environment context markers are diagnostic only
and do not participate in authorization.

## State Machine

The worker is the normal state-transition authority. The only bootstrap
exception is a byte-0 owner that proves no worker reached `bound`; it may append
and seal `start_failed`, but only for a Run already allocated after a ready
Profile Server epoch. Failure before profile ready is a profile diagnostic and
has no Run identity. Present `Unverifiable` generations are never rewritten.

The run lifecycle table governs its worker and client connection only. Profile
singleton existence is manager-owned and independent of any one row.

| State | Worker expected | Run WebSocket expected | New turn | Operation results |
| --- | --- | --- | --- | --- |
| `starting` | Maybe | Maybe | No | `idle` or `start_failed` |
| `idle` | Yes | Yes | Yes | `running`, `paused`, or `closed` |
| `running` | Yes | Yes | No | `idle`, `waiting_interaction`, `reconciliation_required`, or `outcome_unknown` |
| `waiting_interaction` | Yes | Yes | No | `running`, `idle`, `reconciliation_required`, or `outcome_unknown` |
| `reconciliation_required` | No, except transient reconciliation | No, except a transient read-only connection | No | `paused` or `outcome_unknown` |
| `paused` | No | No | No | `idle` after resume or `closed` |
| `closed` | No | No | No | Final |
| `start_failed` | No | No | No | Final |
| `outcome_unknown` | No, except transient read-only reconciliation | No, except its transient connection | No | `paused` after reconciliation or `closed` after proven cleanup |

The same run never has two active turns. `send`, `submit`, and idempotent retries
all enter the same serialized turn-start path.
Fork creates a distinct run and never transitions or mutates its source run.

## Request Correlation and Generations

Every outbound app-server request is registered before write and correlated by
JSON-RPC request ID. Thread-scoped messages must match the run's thread ID;
turn-scoped messages must match the active or addressed turn ID. Server
requests are wrapped in a Dolgorae request ID that includes run generation.

Unknown responses, mismatched IDs, duplicate terminal events, and invalid state
transitions are recorded and fail closed. Known-but-unsupported server requests
are recorded and receive method-not-found without stopping the generation;
unparseable frames fail closed. Unknown additive notifications do not change
state and retain bounded redacted evidence unless classified as reasoning, in
which case content is discarded before ledger representation.

A new Worker lifetime and its private direct App Server connection increment
`run_generation`. Access-policy changes do not. A verified Dedicated read/write
transition increments only `policy_epoch` while preserving Worker, connection,
thread, logical lane, process generation, and server epoch. A thread start,
resume, or fork that installs immutable instructions increments
`thread_generation`. Requests, Turns, and interactions from an earlier
`run_generation` are stale and cannot mutate current state; every accepted Turn
also records the `policy_epoch` under which its dynamic access context was
validated.

## Historical Transient Writer Authority Flow (Superseded)

The former shared↔capsule protocol is not part of the normative architecture.
Its rationale and rejected state machine remain in the historical ADR and
review records for auditability. `SPEC-014`, ADR-019, and the Sticky
Execution-Lane sections above are the only executable requirements; no capsule
state name or transition in the historical record may be implemented.

## Turn Execution Flow

Thread start, resume, or fork installs the generation-immutable instruction
contract. Each accepted Turn separately receives an access context derived from
authoritative writer and policy state, including effective access,
`policy_epoch`, writer generation when present, and `networkAccess:false`.
Current access is never trusted from an immutable or stale prompt prefix.


1. Receive the controller fd and, under mutation serialization, validate run,
   command, invocation, controller generation, state revision, capability
   digest and idempotency key; zeroize the secret, then validate the model-fixed invariant,
   image readability, and requested effort.
2. Before any App Server request, policy change, or writer mutation, reserve the
   idempotency key, append a revision-bound operation intent, and fsync the
   ledger. If `--write` is present, the same PREPARE transaction also publishes
   `reserved` authority and a provisional thread identity when needed.
3. Release every file lock. In APPLY, a threadless write starts its thread with
   writer policy; a bound reader keeps its thread `sandbox` value and applies
   writer policy through the turn carrier alone. Verify live effective policy.
4. Reacquire writer then run serialization, revalidate the operation token and
   revisions, fsync a new thread binding when applicable, and publish `active`.
   An indeterminate APPLY never starts a turn and lands in its specified
   reserved or `blocked_unknown` recovery state.
5. For a threadless read turn, send `thread/start`, append its provisional thread ID,
   and fsync before `turn/start`. If turn acceptance is uncertain, recover that
   exact provisional thread and retry only when stable history proves no turn
   was accepted or the thread is absent; unreadable/in-progress evidence is
   never retried.
6. Capture a best-effort pre-turn workspace observation.
7. Send `turn/start` with the fixed model, selected reasoning effort, canonical
   cwd, access-derived sandbox policy, approval policy, and message/images.
   Developer instructions were supplied by thread start/resume for this
   generation because turn start has no such field.
8. Persist the permanent thread binding and accepted Codex turn ID before
   acknowledging `submit`.
9. Stream and audit correlated notifications.
10. Fsync supported server requests as generation-qualified normalized
   interactions before observer delivery.
11. On terminal notification, read back persisted thread history when necessary,
   select the last root-turn `phase:final_answer` item or last phase-null
   compatibility item in authoritative order, capture a post-turn workspace
   observation, and transition to idle. Commentary or absent messages never
   fabricate a final response.

The delivery mode is not part of idempotency identity: `send` waits, while
`submit` returns after step 8.

## Recovery and Reconciliation

Recovery begins by verifying the per-workspace orchestration SQLite database,
replaying its hash-chained event rows, scanning queued mail and nonterminal
operations, and loading Run ledgers, writer state, profile membership, and
runtime records. It first reconciles every nonterminal Aggregate Bootstrap
Operation against its aggregate row and, for an Orchestrated Session, the
preallocated Primary Run tombstone, manifest, matching bootstrap operation ID,
and audit ledger.
It never creates a replacement aggregate or root identity during recovery.
Nonterminal spawn, activation, Specialist task, claim, and Collaboration
Exchange operations are then reconciled against authoritative Run and Codex
history. A committed result may be redelivered. An accepted or running operation
whose outcome cannot be proven becomes `interrupted_unknown` and is never
automatically replayed. Primary loss changes an Orchestration Session to
`degraded` without implicitly closing its owned Specialists or discarding mail.


Recovery never auto-replays user input. The new worker first replays the ledger,
validates profile identity and compatibility, and inspects the pinned Codex
thread with stable history APIs only after the lane-specific barrier below is
proved.

- Confirmed idle history resumes normally.
- If an absent Worker leaves its recorded Dedicated Lane Server alive, recovery
  first retires that exact server through the guarded identity and process-scope
  checks. Before signalling the matching server group, a complete census must
  agree with group enumeration. Every process in that group or session, and every
  descendant, must have the recorded UID. Owned descendants may still be alive
  before the stop; the subsequent five complete empty samples must account for
  them. Recovery proves the complete generation absent before
  retiring Writer authority or removing the old Worker locator.
- A terminal turn absent from Dolgorae's projection is appended as reconciled
  evidence and the run returns to idle.
- An active turn without authoritative terminal evidence produces
  `outcome_unknown`; the replacement connection is closed while durable writer
  authority remains `blocked_unknown`.
- `reconcile` branches by immutable lane. For `dedicated`, it proves the
  recorded Dedicated Lane Server generation and descendants absent, satisfies
  the durable-history barrier, and attaches the same logical lane at a new
  compatible epoch. For `shared_readonly`, it never treats the shared singleton
  as run-owned or terminates it; it validates the currently recorded compatible
  shared epoch and uses profile-level singleton recovery when that server is
  actually unavailable. It then starts a writer-authority-free transient worker
  generation and calls only
  `thread/read(includeTurns: true)` over a read-only connection. It never loads
  or resumes the thread and never starts a turn. It appends old/new key/epoch,
  lane-qualified absence or shared-epoch validation, history, and
  writer-resolution evidence, closes the connection, and
  exits. Confirmed terminal evidence moves an unknown
  run to `paused`; later bare resume uses read access.
- Every history-copying fork scans newest-to-oldest and uses the latest
  status permitted by Dolgorae's completed-only policy in the checked manifest; terminal-but-rejected
  statuses are skipped. Confirmed history with no accepted boundary returns
  `COMPATIBILITY_REJECTED`; only the no-confirmed-turn outcome-unknown fallback
  takes the fresh-thread provenance path after prior generation absence is
  proved.
  `fork --fresh` reads the immutable source manifest and read-only fsynced
  state/runtime projections needed for eligibility and provenance. It never
  reads the source Codex thread or mutates/repairs the source ledger or any
  source projection. It creates an empty threadless read-only run and
  records source run, observed lifecycle state, and unresolved-turn provenance
  without asserting an outcome. It is therefore available even when a source
  in `running` or `waiting_interaction` has an unreachable socket and unverifiable process
  identity.

Every fork copies the source profile snapshot and immutable run instructions,
defaults to read access, and may replace only the fixed model. It cannot change
the profile or account boundary.

The original source ledger is never rewritten during reconciliation or fork.
Projection-only `status`, `events`, and `verify` return their data with an
identity-verdict field and do not fail merely because it is `Unverifiable`.
They read fsynced projections directly and never start, attach, or recover a
worker.

## Process Cleanup

Launchers register detached Profile Servers, their log drainers, and Dedicated
Lane Servers before spawn; a worker registers itself before publishing its
control socket. The private same-user boot-scoped inventory lives outside the
disposable Dolgorae home. A registration becomes visible only after its full
record has been synced and published. A successful launch adds BSD start-time,
UID, session and process-group identity, executable path and device/inode,
launch-command fingerprint, and socket inode when present. Ordinary verified
shutdown retires its registration after group absence is proven. A recorded
leader still awaiting reaping may be retired only when its exact start identity
matches and it is a zombie with no other group members. A stale registration
with the same PID but a different start time does not block retirement of the
current generation. The inventory is a recovery index, never a replacement for
Profile, Run, membership, or Controller authority.
Collection skips entries retired between enumeration and open, while checking
that the private inventory directory retains its device and inode. Missing
entries supply no process-absence evidence. Unsafe or unreadable existing
entries still refuse collection, and cleanup keeps its exact selection and live
process checks.

`runtime orphan inspect` compares each registration with its recorded owner-root
device/inode and live process identity. A reparented process with an intact
owner remains owned. A deleted or replaced owner alone does not authorize a
signal. `runtime orphan cleanup` requires an exact inspection digest and
revalidates group membership before TERM, before any forced KILL, and before
removing a same-inode socket. If TERM ends the leader but leaves descendants,
their UID, session, and process group must still match the registration before
KILL. A provisional registration without a reliable PID, an identity mismatch,
or a foreign group member is unverifiable and fails closed. An unreadable boot
UUID is also unverifiable; only a verified different UUID makes the process
absent. A census of a group that has disappeared is empty; it does not prove
earlier membership. Cleanup pairs that census with the recorded leader and
member identities. After spawn succeeds, the launcher retains the registration
until it verifies that the entire group is absent, including when startup fails.
A failed first cleanup attempt is rechecked after reaping the child; unpublished
socket removal still requires verified group absence. Test harnesses move
isolated homes away from their recorded owner paths, then clean up processes
before deleting the moved homes. Failed cleanup preserves those homes for
diagnosis. Descendants that escaped the registered session or process group are
outside this command's absence claim.

The detached worker owns only its worker process and private client connection.
Pause, close and recovery may request `turn/interrupt`, close that connection,
and terminate an identity-verified worker, but they cannot signal singleton
commands or native subagents. Cleanup records connection close, interrupt
request and terminal history as distinct evidence. Close cannot finalize an
active or uncertain run, nor can worker exit establish command termination.
Profile-wide operator shutdown alone may signal the verified singleton after
complete membership handling.

## Runtime and Dependency Boundary

The implementation is Rust 2024 pinned by `rust-toolchain.toml` to 1.97.1 with
rustfmt, clippy, and `aarch64-apple-darwin`; Cargo.lock is committed. Go 1.26.6
is a validation-only dependency for compiling and executing the immutable
TASK-053 and pre-extension generated clients; it owns no product runtime state.
Blocking
durability and process work uses dedicated OS threads rather than an async
runtime: control/protocol, stdout, stderr, ledger/state authority,
kqueue/liveness, and `sigwait` each have an explicit owner.

The public RPC gateway is the sole exception to the no-async-runtime default.
It uses `tonic`, `prost`, and an adapter-private bounded `tokio` runtime for
HTTP/2-over-UDS transport, cancellation, and server-stream delivery. Durable
state, locks, process control, worker IPC, and semantic transitions remain on
the existing blocking owners. The gateway invokes them through a bounded
blocking pool and may never hold a Tokio task, stream queue, or HTTP/2 channel
as authority evidence.

One `darwin` module is the only unsafe OS boundary. It wraps `libc` bindings for
`posix_spawn` attributes, libproc sampling/enumeration, kqueue, byte-range
fcntl/flock inspection, `fstatfs/MNT_LOCAL/APFS`, and boot-UUID sysctl.
The same module wraps bundled SQLite statement-counter registration and its C
callback. A static atomic counter context outlives every registered connection;
only query and DDL counts cross this safe boundary, without retained SQL text.
Core recovery receives safe typed verdicts through injectable monotonic-clock, boot, identity,
enumeration, and fault-barrier interfaces. RFC 8785 canonicalization is an
in-repository safe module rather than an unspecified serializer dependency.
The dependency-free `paths` module is the sole physical Dolgorae-home locator;
state-owning modules derive their workspace, profile, Operator, and carrier
roots from it rather than rereading platform-specific path conventions.

The approved safe-Rust mechanisms are `clap` for CLI parsing, `uuid` for
UUIDv7, `sha2` for SHA-256, `sha1` only for the RFC 6455 WebSocket accept-key
derivation, `base64` plus `data-encoding` for the pinned base alphabets,
`serde_yaml_ng` 0.10 behind duplicate/unknown-key rejecting typed configuration
adapters, `toml` only for bounded mode-0600 Codex configuration classification,
`zeroize` for in-memory credential carriers, and `serde_json` only behind the
duplicate-detecting `RawValue` ingest visitor owned by SPEC-010. JCS
serialization remains in-repository. Cargo.lock pins
exact versions; adding a runtime dependency or changing one of these mechanism
bindings requires an ADR amendment and conformance fixture.

The shared fake app-server is a test-only Python subprocess under
`tools/fake_app_server/` that speaks the real Unix-socket WebSocket boundary.
TASK-004 is
its sole owner; later tasks consume or extend it. Declarative scenarios are
validated against the checked Codex manifest, and the fake shares no production
parser or state-machine code with Dolgorae.

Dedicated lane-generation descendants are discovered by process-group enumeration plus
all-PID BSD parent/session samples, and an observed identity remains tracked
after reparenting or group/session change. Cleanup sends TERM, then KILL after
five seconds, only to exact revalidated identities and requires five complete
empty censuses within a ten-second total budget. PID reuse, truncated or failed
enumeration, unreadable identities, unregistered survivors, and detected escape
create durable `background_execution:unverified` and block release, handoff,
close, or generation replacement.
The all-PID census uses short BSD metadata for parent, group, UID and session
coverage across users. Exact generation checks still require full BSD birth
identity. Access denial is never process absence; unavailable birth identity
cannot exempt a recorded zombie or authorize a signal.
Deliberate fork/setsid/reparent escape wholly between 100-millisecond polls and
remote side effects remain outside the trusted same-user personal-alpha model;
the prompt discourages them only as defense in depth. A native Codex terminal
API is optional hybrid evidence, not a release dependency.

On worker `SIGTERM`, bounded clean shutdown appends and fsyncs `cleanup_intent`
with reason `generation_shutdown_requested`, rejects new control mutations, and, when a
turn is active, sends `turn/interrupt` and waits up to five seconds for terminal
history before closing its connection. It then appends and fsyncs
`run_generation_stopped` with shutdown reason and the last known turn state and
attempts bounded startup-lock acquisition
for socket unlink. If that lock cannot be acquired, it leaves stale
coordination files for the next verified owner rather than blocking shutdown,
then exits. Active or uncertain writer authority persists as
`blocked_unknown`; worker shutdown never releases it implicitly. Failure of any
step is recorded when the ledger remains writable and produces a nonzero exit.

## Security and Trust Boundaries

Dolgorae is a coordination and audit tool, not a hardened multi-user security
boundary.

It does provide:

- user-private local sockets and mutable Run and aggregate state outside the agent-writable workspace;
- fail-closed account, request, thread, turn, and generation correlation;
- Codex sandbox selection for reader/writer turns;
- durable Dolgorae writer authority per canonical worktree;
- normative recursive redaction and tamper-evident hash chaining;
- explicit approval and destructive-action boundaries;
- bearer-capability separation between one run controller and local observers;
- client-safe projections that discard reasoning before persistence;
- separate controller and local-operator capability boundaries.

It does not provide:

- protection from a hostile process running as the same OS user;
- remote authentication or authorization for observer projections;
- filesystem isolation from editors or non-Dolgorae tools;
- rollback of partial writes;
- attribution of observed changes to Codex;
- control of external MCP/app/plugin side effects;
- shell network access in v1, which is disabled by policy;
- OS-level per-run ownership or termination of commands/native subagents in the
  shared App Server process tree;
- cryptographic signatures or remote audit attestation;
- direct Worker-to-Worker sockets, model-held trust delegation, or peer Run
  control between Independent Dolgorae Runs. Brokered Specialist Collaboration
  is logical direct communication mediated by the internal Collaboration Plane;
  External Specialist Engagement operation remains coordinated by the external
  control plane;
- authorization based on a diagnostic environment marker;
- public Internet, TCP, or direct Tailscale exposure of the local gRPC socket;
- remote authentication or authorization inside Dolgorae; Gul owns that
  boundary before exposing local results;
- Gul access to private worker sockets, profile/dedicated App Server sockets,
  App Server protocol frames, or Operator credentials.

## Compatibility Boundary

The protocol adapter is a strict subset client. Its checked JSON manifest lists
the source schema bundle SHA, resolved JSON Pointers, methods, responses,
notifications, server requests, type/const/requiredness, required enum values,
response-schema IDs, absent-thread errors, forkable statuses, and the early-ID
behavioral observation. Offline compatibility doctor resolves `$ref` and
performs the normative structural comparison. An explicit launch probe checks
handshake, paginated models, codexHome, and absent-thread errors. The opt-in
compatibility, access-safety, and review acceptance gates separately check
history, sandbox, early-ID, server requests, and review lifecycle behavior.

The checked 0.157.0 manifest remains the required-subset comparison baseline.
Runtime versions below 0.158.0 are rejected. The exact 0.158.0 runtime is
`tested` after its complete behavioral campaign passes and both pinned schema
bundle digests match; newer compatible versions remain `unverified`.
The verdict is written to every run generation.

Runtime code tolerates additive data but never infers lifecycle progress from
uncorrelated or unknown messages. This preserves compatibility without claiming
that a schema probe can guarantee all future runtime behavior.

### TASK-072 ledger generation ownership

`maintenance` owns operator-scoped workspace plans, retained Apply ownership,
original-scope observation and multi-Run checkpoint recovery. It uses RunStore,
read-only ledger/projection, mutation-admission and conformance authorities; it does not dispatch
work or retire content. `ledger_generation` owns private immutable checkpoint,
segment and receipt-index files and the atomic per-Run selector. `ledger` remains
the audit append and replay owner and delegates only generation selection and
content verification. Both use the existing Darwin and secure workspace file
boundaries. Product semantics stay in Rust.

The workspace ledger-maintenance gate sits below operator authority and above
Run lifecycle and ledger writer locks during maintenance. Each ordinary ledger
writer takes a nonblocking shared admission lease before its audit flock; this
lease can be borrowed below existing startup/writer holds because it never
waits. Exclusive maintenance refuses when any shared holder exists, then takes
sorted Run lifecycle leases and audit flocks without starting or stopping any
worker. Explicit terminal deletion retains shared admission through directory
removal and sync, including its integrity-failure escape. This bounded admission
exception introduces no upward blocking lock or external effect. Selection and
reserve behavior are owned by the
[v0.1.4 Ledger Capacity and Maintenance Runtime](../specs/dolgorae-v014-ledger-maintenance.md).

Profile maintenance holds the initialized home-root lock, closes membership and
binding admission with one retained global owner, and serializes journal
selection under each Profile Server lock. It does not acquire the workspace
ledger admission lease or require active and unknown Runs to become quiescent.
Run bindings remain planning provenance. Apply checks each planned Profile's
membership source and the binding-history digest; unrelated Run publication,
removal or ledger progress does not invalidate those sources. A membership checkpoint
retains latest member facts, absolute revision and chain head; immutable
historical segments remain in the original
byte-digest calculation. The selected append continues that head. Selection,
activation and progress retain the original operation through interruption.
Binding compaction archives the original bytes before atomic replacement.
Successful Run and session Close receipts remain authoritative when membership
cleanup fails; existing recovery projections report the independent obligation.
The [Profile maintenance contract](../specs/dolgorae-v014-profile-maintenance.md)
owns these bounds and recovery semantics.

## Member release proof ownership

The [release-proof correction](../specs/dolgorae-v014-release-proof-correction.md)
and ADR-042 assign both external and managed release owners a bounded shared
repository in the workspace Engagement database. Base generation 6 retains
`dolgorae.member-release-operation/v1` and a scoped membership counter;
orchestration extension 3 remains unchanged. Durable release admission and
assignment use the same serialization boundary. Lock plus synced witness
establish active ownership; a stored owner reference alone grants no authority.

The initiating Controller and original operation remain immutable across
handoff and recovery. Settlement verifies Run closure, exact owned execution
cleanup and writer settlement before Profile authority retires membership and
the workspace commits terminal membership and completion revisions atomically.
A shared Profile Server remains available to other members. Authorized reads
project an immutable `dolgorae.member-release-proof/v1` only from that settled
record, observing current disposition after collection of the committed result.
TASK-091 publishes checked contracts; TASK-076/077/086 implement these runtime
owners after TASK-092 accepts the corrected consumer selection.
