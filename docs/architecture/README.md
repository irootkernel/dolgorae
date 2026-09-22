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

An accepted hire initially publishes only a logical, threadless Run and records
the member as `unstarted`. Assignment reconstructs a missing Worker from the Run
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
failure. Isolated terminal output captures a
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

### Immutable Review Target Coordinator

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
schema v3 retains the v2 removal of incorrect global result-content uniqueness
and normalizes members, tasks, scoped operations, artifacts, and delivery
receipts for reusable engagements. The v2-to-v3 migration maps uncertain
executing or accepted work to `interrupted_unknown` and preserves a ready
result as `completed_not_delivered`; both migration steps rebuild their tables
transactionally without losing rows.

### Role Source and Policy Resolver

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
isolated-home campaign pin (locally installed Codex CLI 0.155.1). That campaign
pin is not a change to the Codex App Server 0.153.4 product compatibility
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
arbitrary child files. The specification fixes assignment receipt/wait and UTF-8 result-page rules.
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

Whole-session root CloseRun records durable close intent, stops new admission
and retires owned children through the Broker. Admission races with spawn/task
publication use the existing serialization order; waits hold no SQLite/global
mutation owner. Unknown effects prevent successful closed state. Root recovery
accounts for retained aggregate intent without new semantic work or auto-resume.
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
27 required methods over a 36-method descriptor. TASK-054 implements timeline,
TASK-055 the aggregate queries; TASK-029 enables the remaining nine original
methods. Preserve historical evidence separately from the current release
profile. Runtime advertisement includes only complete implementations, never a
planned method or generated stub. Missing optional later functionality must not
block the required profile; incomplete handlers remain unavailable.

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
from the key. Compatible profile names are aliases for one `server_key`.
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
A dedicated Run first owns only a durable logical lane; its first input lazily
starts that lane's physical App Server with the identical immutable launch
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

Each connection performs its own `initialize`/`initialized`. A newly created
run remains threadless until first turn; first input uses `thread/start`,
recovery uses `thread/resume`, and history fork uses `thread/fork`. The worker
owns only that client connection, one thread binding, correlation, lifecycle,
audit and interactions. The connected App Server owns turn execution, commands,
native subagents and its process tree. A narrowly tested user-input connection
may advertise `experimentalApi`; no other experimental feature follows.

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
The profile manager owns their lifecycle. A dedicated worker requests lazy
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
log-drainer identity, and process census. A new Run publishes the logical lane
with null thread and absent physical server; first input starts its initial
generation. Its physical server may also be absent while the Run is paused.
Resume starts a new generation only after exact old
generation/descendant absence, five complete empty samples, no active or
unknown turn/interaction/native descendant, and a durable-history barrier.
The thread then resumes in the same logical lane. Infrastructure state,
workspace writer authority, effective Codex policy, and background workload
state are four independent facts.

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
Codex 0.153.4 production pin. The original negative probe remains historical
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
connection, lifecycle, writer, observed epoch and runtime locator. Startup
replays the journal, validates every referenced manifest/runtime record, and
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
Membership mutations take home then server locks. New registration checks the
exact ready home record and migration fence under those locks, closing the
quiescence-to-stop admission race; release remains valid for shutdown cleanup.

The global order is operator, home, server keys in binary order, handoff,
writer, run startup locks in UUID-byte order, then in-process run mutation
mutexes in the same order. Worker startup admission is the sole inversion: it
takes one Run startup range before home/server revalidation and retains it
through `bound`; no path may hold a home or server lock while waiting to acquire
a Run startup range. Home/server locks are released before spawn; only that
startup range spans the bounded `bound` wait and is released before `ready`
or on failure. This is also the sole exception to the external-wait prohibition.
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
before performing effects. Different allocation keys remain independent. This
lock is distinct from Writer authority and POSIX worker startup locks. Both the canonical workspace and its Dolgorae-home
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
append a competing successor. Replay is a pure projection operation separated
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
absent. After spawn succeeds, the launcher retains the registration until it
verifies that the entire group is absent, including when startup fails. Test
harnesses move isolated homes away from their recorded owner paths, then clean
up processes
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
rustfmt, clippy, and `aarch64-apple-darwin`; Cargo.lock is committed. Blocking
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
fcntl/flock inspection, `fstatfs/MNT_LOCAL/APFS`, and boot-UUID sysctl. Core recovery
receives safe typed verdicts through injectable monotonic-clock, boot, identity,
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

The tested 0.153.4 manifest is `tested`. A newer compatible version is
`unverified` and that verdict is written to every run generation. Older or
otherwise unlisted versions are rejected unless a future SOT revision adds
them to the tested set.

Runtime code tolerates additive data but never infers lifecycle progress from
uncorrelated or unknown messages. This preserves compatibility without claiming
that a schema probe can guarantee all future runtime behavior.
