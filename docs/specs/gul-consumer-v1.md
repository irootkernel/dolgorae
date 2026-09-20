# Gul Consumer Contract v1

Contract ID: `dolgorae.gul-consumer/v1`

Status: Approved Required State, adopted 2026-09-20. This document defines the
Dolgorae v0.1.3 consumer requirements. It is not evidence of implemented methods,
a published wire lock, a release candidate passing QA, or actual Gul integration.

Authority: this specification owns provider wire semantics, authorization,
lifecycle, safety, and compatibility alongside [the product specification](README.md).
Gul's source-of-truth documents own browser-facing APIs and presentation behavior.
Client obligations here are limited to interoperability and safety. [ADR-039](../architecture-decision-records/README.md#adr-039-freeze-the-gul-consumer-contract-before-v013)
records the decision. The [roadmap](../roadmap/README.md) alone owns Task identity,
order, and status. Checked wire artifacts remain under `docs/protocol/`.
TASK-053 publishes the exact Protobuf messages, error mappings, generated
clients, fixtures, and immutable contract lock before Gul pins them.

## 1. Delivery boundaries

Dolgorae publishes one contract; Gul retains a pinned consumer copy, not a
second independently editable source of truth. The two gates are separate:

| Gate | Provider obligation | Consumer work it permits |
| --- | --- | --- |
| Contract-ready, TASK-053 | Checked wire and semantic policies, reproducible clients/fixtures, immutable source revision and digests | Gul contract repinning, explicit mock, core, UI, authentication, persistence, files, and simulated operation/recovery flows |
| Released provider, after TASK-026 and separate RC QA | Every required operation implemented and tested against the exact released artifact | Gul production adapter, native supervision/carriers, and actual integration acceptance |

Actual Gul is never a prerequisite of TASK-026, v0.1.3 release eligibility, or
later provider work. Provider tests use the real public gateway and semantic
path. Deterministic fake-Codex cases and separately authorized live-Codex evidence
must be labeled separately. A mock is never selected as a production fallback.

TASK-025 completed its private transport contract independently of this consumer
amendment. Preserve that completion, the selected transport, private receipts,
blocking waits, UTF-8 result paging, and the established product/campaign pin
boundaries. This amendment does not reopen that work or authorize new live use.

### 1.1 Intentional release-scope change

The v0.1.3 product decision expands the original eight-task provider preview to
12 Tasks and a consumer-ready provider milestone. The declared method inventory
increases from 34 to 36 and the required profile from 24 to 27. Complete history,
authoritative aggregate observations, and whole-session close increase
implementation and release-verification work. This cost is accepted so the first
provider used by Gul does not require reconstruction of authority from caches,
low-level Run relationships, or model output. Actual Gul acceptance remains
separate; queueing, advanced Run methods, and Podway are not pulled into this scope.

## 2. Required public operations

The required v0.1.3 profile contains 27 methods: the historical TASK-023 set of
24, complete Controller timeline, and two read-only aggregate queries.

| Service | Required methods |
| --- | --- |
| RuntimeService | GetCapabilities, InspectWorkspace, ListProfiles, GetProfile |
| RunService | StartRun, ListRuns, GetRun, SubmitTurn, InterruptTurn, PauseRun, ResumeRun, CloseRun, RecoverRun, ReconcileRun |
| ObservationService | WatchRunEvents, ListRunTimelineItems |
| InteractionService | ListPendingInteractions, GetControllerInteraction, ResolveInteraction |
| WriterService | GetWorkspaceWriterStatus, AcquireWriter, ReleaseWriter |
| ControllerService | VerifyController |
| ArtifactService | GetArtifact, ReadArtifactChunk |
| OrchestrationService | GetOrchestratedSession, ListOrchestratedSessionResults |

TASK-053 adds only the two unary read methods and their typed messages to
`dolgorae.public.v1`. It preserves existing field numbers, request meanings,
and low-level Run behavior. The extended descriptor therefore declares 36
methods. The other nine original methods remain unavailable until TASK-029:
ListProfileDiagnostics, SetDefaultEffort, ForkRun, VerifyRun, DeleteRun,
CreateWriteContinuation, and Prepare/Commit/CancelWriterHandoff.

Descriptor presence is not availability. `supported_methods` advertises only
fully implemented methods. Required features, profile compatibility, credential
contract, typed errors, limits, and protocol versions are also checked. A
contract-ready artifact must not make an incomplete runtime advertise success.
Gul must not require exact equality with the supported-method set: additional
optional methods do not invalidate the required subset. Unadvertised operations
are never invoked. In particular, a required-action hint naming continuation
must not enable it while its RPC is unavailable.

No Operator, policy editing, child mutation, generic Execute, TCP, bidirectional
stream, or Podway-control API is added by this contract.

## 3. Handshake, provisioning, and credentials

The transport remains standard local gRPC over a same-user private Unix socket.
Gul manages its private parent directory and supervises only the foreground
Dolgorae gateway. Dolgorae owns singleton state, socket bind/mode, verified stale
cleanup and unlink. Gul never unlinks a provider socket or controls internal
Worker/Profile Server processes.

GetCapabilities uses protocol zero and the supported range; later requests use
the negotiated version. The contract lock records source revision, descriptor,
credential schema and digest algorithm, client/mutation/error policies, generated
clients, and fixture digests. TASK-053 must correct the known credential-schema
hash mismatch and test that the advertised SHA-256 equals the distributed schema
file bytes. Copying the same wrong constant into several fixtures is not proof.

The carrier root is `~/.dolgorae/controller-carriers/`, with Gul credentials in
`gul/<installation-id>/`. Use the advertised typed root/encoding/principal
policies, same-uid non-symlink 0700 parents and exclusive 0600 regular files.
Capability bytes, carrier paths, internal paths, and secret digests remain
backend-only and never enter browser payloads or logs. Creation is Gul-local;
Dolgorae validates and binds the supplied carrier. No Operator capability enters
Gul. Existing side-effect-free VerifyController adoption remains available.

Profile lookup is user-global: ListProfiles and GetProfile do not take WorkspaceRef.
Workspace-scoped and Run-scoped calls still use verified canonical paths and IDs.
Workspaces, Profiles, and Specialist Policies are provisioned on the host outside
Gul. A host-configured list of policy names is only a selection convenience;
Dolgorae resolves and validates the policy at StartRun. No private registry read
or CLI fallback is permitted in the production adapter.

A Gul-created Orchestrated Session uses a new `interactive_client` carrier with
`orchestration_launch.use_case=dolgorae_orchestrated_session` and an explicit
`specialist_policy_name`. StartRun supplies `direct_interactive`, no parent, and
complete Profile/lane/assurance/purpose/model settings under the existing
admission contract. DirectInteractive alone does not create a session. A
rejected orchestration request must not silently fall back to a low-level Run.
Keep the same credential, request and idempotency identity after response loss;
never mint another root to conceal an uncertain allocation.

## 4. Sequential human prompt admission

Ordinary human prompts use sequential explicit submission. The provider rejects
a fresh ordinary SubmitTurn before acceptance/effects while a Primary Turn is
active, including while waiting for an Interaction. The existing busy/conflict
contract supplies the typed error. Neither provider nor client may turn a rejected
or unsent prompt into a queue entry, steering input, automatic send, or automatic
interrupt. Gul owns the unsent-draft UI; draft presence grants no runtime authority.

Exact replay of a previously accepted request is evaluated before fresh-busy
admission and returns its existing receipt without another Turn or history item.
Two genuinely new submissions with identical text remain two requests. Fresh
concurrent submissions are serialized by Dolgorae, not ordered by browser clocks.

Current approval/question answers use ResolveInteraction and remain available.
Explicit InterruptTurn remains a separate user action. An interrupt receipt is
not terminal proof; a new ordinary prompt requires actual terminal evidence and
fresh eligible state. Outcome-unknown or unresolved recovery still blocks
conflicting work. Gul does not send a draft automatically when the state changes.
Primary pause/interrupt is not an aggregate pause or a Podway-node operation.

## 5. Durable prompt history and complete timeline

Implement the complete existing ListRunTimelineItems contract, not a partial
method that silently drops declared item kinds. Supported safe kinds remain
user_input.accepted, assistant_response.final, interaction.opened,
interaction.resolved, and turn.terminal. The root Controller authorizes reads.
Accepted human input on the Primary Run is distinguishable from internal
Specialist tasks, tool prompts, Role instructions, and protected answers. Gul
owns its user-only history view; those other input classes must not be reported
as accepted human prompts.

| Property | Required behavior |
| --- | --- |
| Source | Dolgorae's durable accepted-input record and Controller-safe timeline |
| Content | Original accepted UTF-8 text, preserving multiline text and line endings; no model-generated summary replaces it |
| Identity | Stable Run, Turn and ledger item/cursor identity; provider identity/order where supplied |
| Order | Authoritative normalized ledger/provider chronology, never notification arrival, timestamp sorting, or text equality |
| Admission | Persist accepted input before submit acknowledgement; pre-acceptance rejection is not accepted history |
| Lifetime | Retain history after failed/interrupted Turns, restart, pause and close; ordinary cache cleanup never deletes provider history |
| Privacy | No hidden reasoning, protected Interaction answer, credential, raw private tool payload, or internal file path |

Reuse the existing bounds: default page size 100, maximum 500; exclusive
`after_cursor`; captured head and optional next cursor; filtered cursor gaps are
valid. Each page represents its stated captured head. A client must not claim a
single immutable snapshot across pages with different captured heads. It merges
by stable item identity, follows the returned cursor, and performs a subsequent
refresh for new items. A bounded traversal can retain an initial head as its
upper watermark and process later items in the next refresh. Ledger cursor gaps
must not be interpreted as missing human submissions or user-facing ordinals.

Text up to 1 MiB is inline; larger accepted text up to the existing 8 MiB public
request bound uses a Controller-only `user_input` artifact. The timeline itself
provides the ArtifactRef before GetArtifact/ReadArtifactChunk. Preserve exact
bytes, total length and SHA-256. Safe image metadata retains order, detail, type,
length and digest only; no source path or image bytes are promised by history.

History is not retry authority. Gul may cache authorized timeline items for
presentation, but its operation-attempt/replay store must not retain SubmitTurn
prompts or protected Interaction responses for crash-safe re-execution. After
ambiguous submit, correlate actual accepted evidence or preserve OutcomeUnknown;
never reconstruct and resubmit from the history cache. Unsent, rejected, and
acceptance-unknown attempts must not be classified as accepted history.

All timeline-dependent invalidation and action rules remain enforceable because
timeline is mandatory in this release profile. Missing support blocks this
profile; it must not be worked around by weakening freshness or fabricating
empty history. Reconnection, pagination, multiple browsers, and session closure
must restore the same original user history without duplicate accepted input.

## 6. Authoritative session and result observations

The two new methods are read-only, Controller-authorized queries. They must not
start a Worker, repair state, acknowledge result delivery, alter last-access
state, or invoke a model. An unknown or damaged aggregate is not an empty active
session. ListRuns and reserved parent references may support navigation, but do
not establish authoritative aggregate membership or grant child control.

TASK-053 owns exact field numbers, enum numbers and checked wire encodings for
the following semantics. It must publish complete positive/negative fixtures
before declaring the contract ready; none of these details may be left for Gul
to guess during implementation.

### 6.0 Field sourceability is a freeze prerequisite

Before declaring TASK-053 contract-ready, publish a checked field-provenance
matrix alongside the consumer fixtures. Every response field, lifecycle value,
count, recovery classification, and reference must name its durable owner or an
explicitly planned durable record, derivation, authorization/redaction rule,
revision boundary, and positive/negative fixture. Record the implementing Task
for any missing persistence. Required fields without a viable owner block freeze;
optional fields cannot be invented to fill an attractive presentation model.

Dolgorae may read its own private durable stores through their semantic owners.
The prohibition is on requiring Gul or an external conformance client to read
those stores, or treating model prose, process-local guesses, or read-side repair
as the source of a public fact. A bounded read transaction is permitted; a query
must not acquire a long-lived mutation owner or repair inconsistent records.

| Public field family | Source and implementing owner to prove at freeze |
| --- | --- |
| Session/root identity, lifecycle, composition, approval policy | Existing orchestration session/bootstrap/member records; TASK-055 maps only checked values. TASK-052 owns close transitions. |
| Policy identity and digest | Accepted immutable session Policy snapshot, not the current registry file. Any exposed policy revision must have a persisted source, not reuse the aggregate revision. |
| Aggregate revision and counts | One consistent orchestration-store observation. TASK-053 enumerates included/excluded states, overlap between counts, and every write path that must advance the revision; TASK-055 implements the projection and required writer-side revision updates. |
| Close intent, operation ID, disposition and recovery class | TASK-052's planned durable close record and existing verified lifecycle/recovery evidence. TASK-053 freezes the record invariants and exhaustive mappings before their implementation. |
| Result/task identity, format, order and publication time | TASK-051's recoverable immutable publication association, linked to the accepted task; TASK-055 only reads it. The private delivery cursor is not the publication sequence. |
| Result ArtifactRef, owner, length and SHA-256 | TASK-051's committed Primary-owned artifact metadata and publication association; existing ArtifactService validates access and bytes. |
| Captured publication head and page token | Durable publication sequence and a bounded session/projection/head-bound token; TASK-055 owns paging, not delivery acknowledgement. |
| Capture time, source revision and availability | Capture time is explicitly observation metadata, not a persisted lifecycle event. Revision comes from the source store; missing/corrupt source evidence is unavailable or a typed error, never a zero count. |

The matrix must distinguish existing fields from planned records. This is a
sourceability proof, not a claim that TASK-051/052/055 already pass runtime QA.
If counts span multiple stores, TASK-053 must define a coherent durable projection
or reject the field; it must not promise an unsupported cross-store atomic read.
The implementing Tasks later prove those fixtures through the real public path.

### 6.1 GetOrchestratedSession

Request: RequestContext, root RunRef, and root ControllerCarrierRef.
The session ID equals its Primary Run ID in v1. Reject a low-level Run or a
foreign aggregate under the existing typed invalid-target/authorization rules;
wrong credentials never disclose existence or private mismatch reasons.

Response: ResponseContext plus a typed OrchestratedSessionProjection containing:

- session ID, Primary RunRef and independent aggregate revision;
- typed aggregate lifecycle preserving creating, active, degraded, recovering,
  completing, aborting, completed and aborted meanings;
- typed standalone-primary/brokered-hierarchy composition and approval policy;
- immutable Specialist Policy name, revision and snapshot SHA-256, not role
  instruction bodies or account credentials;
- counts of nonretired members, nonterminal spawn operations, pending approvals,
  accepted unfinished tasks, unknown-outcome tasks and published results;
- typed close-progress disposition, durable close intent/operation identity when
  present, and required recovery/action classification;
- snapshot timestamp and source revision, with unavailable state explicit.

Counts and lifecycle come from one consistent aggregate-store observation.
Their revision is not a Run ProjectionStamp and must never be compared as if
both counters share a domain. Reads of Run/Writer/Interaction state remain
separate. A stale aggregate blocks affected aggregate actions; no equality of
unrelated counters is required. TASK-053 freezes whether a count includes each
accepted/dispatching/terminal state and tests zero versus unknown independently.
A completed/aborted session remains queryable while its retained root exists.

### 6.2 ListOrchestratedSessionResults

Request: RequestContext, root RunRef, root ControllerCarrierRef, optional opaque
page cursor and item limit (default 100, maximum 500). The cursor is bounded,
versioned and bound to the session, projection version, publication head and
last item. It grants no authorization. New queries revalidate the Controller.
It is not a Run ledger cursor or the private collect/delivery receipt cursor.

Response: ResponseContext, captured publication head/revision, ordered result
items and an optional next cursor. Each immutable item includes stable result
and accepted-task IDs, safe Specialist Run/role references, publication order
and timestamp, result format, exact byte length/SHA-256, and a permitted
Primary-owned ArtifactRef with its explicit owner RunRef. No child Controller,
private database ID/path, arbitrary child artifact, or raw instruction is exposed.

Publication history is append-only for the retained session. A page token fixes
the publication head for its traversal; concurrent later publications appear
on a fresh traversal. Verify all paging boundaries, foreign/malformed tokens,
limit/byte bounds, interruption and restart. TASK-053 must bind exact cursor and
response-size rules in checked artifacts. Use existing artifact quotas and a
bounded metadata page rather than inlining unbounded content.

Publish only after actual immutable bytes and metadata are committed and the
write-ahead association is recoverable. Collected results remain discoverable;
querying does not acknowledge private delivery. Failed/unpublished work remains
visible through aggregate counts or existing Run observations, not fabricated
successful results. Corruption is a typed integrity error, not silent omission.

Public conformance clients must discover result references through this query,
then use GetArtifact and bounded chunks with the root Controller. A fixture must
not inject an ID obtained from private state. Do not manufacture a Primary final
response to transport a Specialist artifact. This publication path supplements,
and does not change, TASK-025's private reader/receipt semantics.

### 6.3 Refresh and future compatibility

No aggregate event stream is added in v0.1.3. Clients refresh these bounded
unary queries on relevant notifications, opening/reconnection, and a coalesced
rate-bounded observation schedule while work is active. A Primary Run event is
not guaranteed for every aggregate-only transition, so event-only refresh is
insufficient. A read does not take the runtime mutation owner for its lifetime.
A fresh typed snapshot, never a parsed diagnostic or model sentence, resolves
the displayed closing/result state.

## 7. Whole-session close

Session termination covers the whole owned aggregate. Existing root CloseRun is
aggregate-aware only for a durably registered Orchestrated root. Ordinary
low-level Runs and external engagement members retain their established meaning.
Gul submits the root Controller and expected Run revision; it never iterates
child mutations or receives child credentials. Admission also revalidates actual
aggregate membership/activity under Broker serialization, not a client count.

A non-interrupting close rejects before effects if any owned Primary/Specialist
execution, pending approval, accepted unfinished task or in-flight spawn prevents
quiescent closure. `interrupt=true` is explicit user intent and requires Gul's
confirmation for active work. It is not inferred from disconnect or timeout.

The Broker persists close intent before effects, rejects new work admission,
settles in-flight spawn/dispatch/publication races and retires only owned
Specialists using existing interruption, terminal proof and process identity.
No SQLite transaction or global mutation lock is held across waits. The durable
operation resumes recovery after gateway replacement without repeating work.

A successful closed response requires all owned execution accounted for, no
unsettled spawn/accepted task, no unknown live effect, children retired and the
root closed with its Writer authority settled. For this public root operation,
interrupt=false records completing/completed intent and interrupt=true records
aborting/aborted intent, even if active work settles concurrently. Persist that
choice at admission and never reinterpret it on retry or recovery. Retained
failure history does not itself forbid closure once its effects are known.
An interrupt acknowledgement alone never satisfies these conditions.

### 7.1 Bounded-call outcomes and correlation

TASK-053 must encode the following method-specific mapping in checked error,
mutation, client-policy and conformance artifacts. These are Required State
semantics, not currently advertised error support. Keep RunMutationResponse's
existing shape and reserve gRPC OK for authoritatively completed closure.

| Boundary | Public outcome | Meaning and follow-up |
| --- | --- | --- |
| Rejection before close intent commits | Existing typed authorization, revision, busy or invalid-target error; no newly allocated close operation ID | This call accepted no close intent. An independently existing intent may still be present and is observed separately. |
| Durable intent committed; bounded wait ends with settlement still in progress and no known unresolved-effect blocker | gRPC FAILED_PRECONDITION with DolgoraeErrorDetail code SESSION_CLOSE_IN_PROGRESS, action REFRESH_SNAPSHOT, retry FORBIDDEN, recovery SNAPSHOT_REQUIRED, root run_id and durable operation_id | Accepted intent, not failed termination or confirmed closure. Read GetOrchestratedSession; do not resubmit CloseRun. |
| Effects require reconciliation or recovery | Existing typed OUTCOME_UNKNOWN or RECOVERY_REQUIRED with the corresponding required action/classification and durable operation_id when authorized and known | CloseRun's method-specific retry classification remains FORBIDDEN. Observe first, then use authorized root RecoverRun/ReconcileRun only when the provider requires it. |
| All whole-session completion conditions hold | gRPC OK with RunMutationResponse.run closed and ResponseContext.operation_id identifying the retained close operation when one exists | Confirmed closure. Aggregate and other snapshots remain separate reads; no implicit atomic multi-projection response. |
| Transport loss or caller deadline without a typed provider result | Ordinary transport error, possibly without operation_id | Client acceptance is unknown. Discover retained intent by root through GetOrchestratedSession before any further mutation. A timeout never cancels committed intent. |

REFRESH_SNAPSHOT, FORBIDDEN and SNAPSHOT_REQUIRED above denote the corresponding
existing RequiredClientAction, RetryClassification and RecoveryClassification
enum values. SESSION_CLOSE_IN_PROGRESS is a new checked semantic error code, not
a new RPC, enum family or generic job framework. Gul maps it to its own pending
close state rather than a generic operation failure. A transport can still lose
this typed result; the transport-loss row always remains necessary.

The Broker allocates one opaque operation ID when close intent is durably
accepted, before external effects, and binds it to the root, initiating request
identity, Controller authorization generation and interrupt choice. It is not
the per-call client_request_id, server instance ID, bootstrap ID, or an
idempotency token. Authorized in-progress/error details, completed response
context and GetOrchestratedSession's retained close record refer to that same
operation across gateway restart and root recovery. When no public close intent
has ever been recorded, the query represents its absence explicitly, not by
inventing an ID. Already-settled aggregates need no new retirement operation.

GetOrchestratedSession exposes the retained close operation identity and typed
progress even when the initial response was lost. Its response context is not
repurposed as the queried close operation. Observing the record grants no new
Controller rights and does not repair or advance it. A renewed authorized
Controller may observe/recover the same intent after an explicit Controller
reset; the stored initiating generation cannot authorize a stale credential.

CloseRun has no application idempotency key. After durable acceptance or an
ambiguous transport result, transparent or blind tokenless retry is forbidden.
Concurrent or repeated calls may not allocate a second intent or change an
accepted interrupt choice: the semantic owner returns the existing outcome for
compatible calls or a typed conflict. A matching client_request_id alone never
makes a mutation safe to replay. A new close request is eligible only after
provider evidence establishes no accepted intent, with fresh revision and
explicit user action. Otherwise reconcile the retained operation.

Existing RecoverRun and ReconcileRun on an Orchestrated root account for the
owned aggregate when required to resolve retained intent, without auto-resuming
paused work. TASK-052 implements this scope through the shared semantic/Broker
layer. TASK-053 must prove all outcome/correlation cases with contract fixtures;
TASK-052/056/026 prove the implemented failure boundaries. Unresolved mappings
block contract freeze rather than being delegated to Gul.

Close preserves prompts, results and workspace changes. It does not delete
history, roll back files, destroy unrelated sessions, stop healthy shared
Profile Servers or change a Podway node. Browser close, hide and navigation are
local actions. Pause/Interrupt retain Primary-scoped semantics and cannot be
presented as stopping every Specialist. A complete/aborted aggregate remains
inspectable; inconsistent root-closed/children-unsettled state is reported as
recovery-required, never presented as successful whole-session termination.

## 8. Compatibility and release evidence

TASK-053 publishes the first immutable consumer wire lock. All later v0.1.3
implementation preserves it. New optional methods or events in later versions
must not require existing Gul clients to adopt new operations to keep using the
frozen profile. Unknown decision-critical enums/details still fail closed.
Do not bypass digest checks or replace semantic compatibility with a version
string. Future additive descriptors need an explicitly verified compatibility
mapping to the frozen consumer baseline while retaining exact artifact identity.

TASK-056 retains the original generated consumer without regeneration and adds
an immutable-baseline Buf breaking check plus behavioral scenarios to the
normal deterministic gate. Also retain a pre-extension low-level client to
verify original Run behavior. A current source/descriptor equality check alone
cannot establish backward compatibility. EPIC-009 and TASK-028/029/031 rerun
these clients; they must not rewrite the baseline to make new behavior pass.

| Scenario family | Required proof before v0.1.3 release eligibility |
| --- | --- |
| Negotiation | Protocol zero, required 27-method profile, digest identity, unsupported optional methods, typed blockers |
| Input/history | Busy rejection, exact replay, same-text new request, simultaneous clients, long/Unicode text, all timeline kinds, page/restart/close recovery |
| Orchestration | Explicit bootstrap, both approval modes, no child authority, real Primary/Specialist execution |
| Result discovery | Public list to Primary-owned artifact/chunks, correct length/digest, authorization, collection/restart retention |
| Termination | Running/waiting/degraded sessions, pending spawn, cancel/complete races, response loss/restart, no false closure or unrelated shutdown |
| Compatibility | Frozen and pre-extension clients, optional future methods, event/stamp/cursor handling, no CLI/private-state fallback |

Release requirements are profile-specific. Applicable TASK-023 safety cases,
TASK-026 provider acceptance and this contract's cases are required. Later
full-surface TASK-032/034 cases do not accidentally block the preview. The
internal production_runtime_eligible flag remains the Personal Alpha owner's
qualification; Gul does not read that private manifest as an extra launch gate.
Public capabilities and typed profile blockers remain binding and cannot be
waived by calling a build a preview.

RC QA verifies the exact committed candidate/build, not a floating worktree or
old diagnostic result. Release, installation and live-account use require their
own authorization. Actual-Gul MILESTONE-BH1 uses the real released provider and
Gul application in its separate acceptance campaign; provider-only tests cannot
claim it.

## 9. Post-v0.1.3 non-goal and compatibility

Podway observation is outside this release. Detailed future requirements belong
to [the deferred Podway owner](../todo/README.md#read-only-podway-observation),
not this v0.1.3 contract; Gul owns its visualization requirements. A future
optional read-only surface must not block existing chat, history, approval,
result or close operations. Gul has no direct FSM mutation or node-jump path;
change requests remain ordinary prompts judged by the executing LLM.
