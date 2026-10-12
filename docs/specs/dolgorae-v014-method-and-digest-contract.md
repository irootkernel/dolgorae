# Dolgorae v0.1.4 method and digest contract

Status: TASK-067 successor contract decisions accepted. The
[H1 package](dolgorae-v014-h1-package.md) binds TASK-068's checked shapes;
the roadmap owns their acceptance and handoff status. These contracts do not
advertise runtime support or record external consumer-runtime acceptance.

The [alignment dossier](../todo/TODO-DOLGORAE-V014-RELEASE-ALIGNMENT.md),
[caller-workspace specification](caller-workspaces-v1.md),
[successor requirements](dolgorae-v014-successor-requirements.md) and
[three-consumer acceptance specification](dolgorae-consumer-acceptance-v014.md)
remain authoritative. The [roadmap](../roadmap/README.md) owns delivery status.
Uppercase normative words use the product specification's meaning.

## Version and compatibility boundary

The frozen [public-v1 source](../protocol/dolgorae/public/v1/dolgorae.proto),
descriptor and [consumer lock](../protocol/dolgorae-gul-consumer-v1.lock.json)
MUST remain byte-identical. Existing accepted records retain their versioned
meaning. Public-v2 and the Gul consumer-v2 package are separate contracts.
The existing Machine v4 and Specialist facade v4 MUST NOT be redefined as the
new identity contract; incompatible additions require successor generations.
Modern Agent Configuration already uses schema version 3 inside
[workspace-execution/v1](../protocol/dolgorae-workspace-execution-v1.schema.json).
Its historical identity also remains unchanged.

These method rules govern the successor contract. The existing
[v1 mutation policy](../protocol/dolgorae-rpc-mutation-policy-v1.json) and
[v1 error mapping](../protocol/dolgorae-grpc-error-mapping-v1.json) continue to
govern v1. TASK-068 publishes independently versioned v2 policies, fixtures and
descriptor without changing either frozen file.

## Effects, classification and loss recovery

An effect is an accepted durable state change or an upstream execution effect.
Read observation may perform bounded freshness checks; it MUST NOT start a
Run, acquire writer authority, execute a task or repair persistent state.

Each successor error carries an operation kind, an authorized operation
reference when known, an effect state and a classification. The effect states
are `none`, `accepted`, and `unresolved`. Classification is one of `Rejected`,
`RetrySameKey`, `Unknown`, or `RecoveryNeeded`:

- `Rejected` describes this invocation's refusal with `none` effect. It says
  nothing about an earlier invocation whose outcome remains unresolved.
- `RetrySameKey` permits an explicitly chosen exact-key replay only after the
  operation-specific lookup rule permits it. It never permits a new key.
- `Unknown` preserves an unresolved acceptance or execution effect. A timeout,
  dropped connection or missing typed detail cannot prove non-acceptance.
- `RecoveryNeeded` requires the named supported reconcile/recover action.
  Retained acceptance and result receipts remain valid during recovery.

A caller MUST NOT infer classification from status text. Missing, malformed or
unrecognized effect details on a mutation are `Unknown` until an authorized
lookup or current authoritative state resolves them. Transparent mutation retry
is forbidden. A read-only call can be retried after renegotiation and a fresh
snapshot where required, without implying that its earlier result was current.

`R` below means no product-state effect. `K` means durable keyed acceptance:
intent precedes effect and receipt precedes acknowledgement. `S` means an
existing state mutation without an idempotency-key contract; response loss is
resolved through authoritative state before considering a newly authorized
mutation. `V` means bounded verification observation with no execution or
authority transition. `E` is a read-only event stream.

For every `K` method, authenticated same-key/same-identity replay returns the
original retained result before testing a new-operation revision precondition.
Same-key changed identity is `IDEMPOTENCY_CONFLICT`; it cannot replace the
original operation. Definitive upstream refusal MUST durably resolve its intent
before the same original key can be admitted again. A deadline MUST NOT release
an admitted or unresolved operation. Deleted Runs retain the original identity
and a tombstone; replay MUST NOT allocate a replacement.

### Existing methods and their successor policy

There are 36 RPCs in the frozen public-v1 source. Each has a separate row here.
The errors identify material failure cases; the successor adds typed effect
details. Controller authorization, invalid input and compatible-version
checks apply wherever the request carries their corresponding fields.

| Method | Effect | Material refusal or uncertainty | Required recovery after lost response or ambiguous failure |
| --- | --- | --- | --- |
| `RuntimeService.GetCapabilities` | R | `PROTOCOL_VERSION_UNSUPPORTED` | Negotiate a supported range; discard cached admission on server-instance change |
| `RuntimeService.InspectWorkspace` | R | `WORKSPACE_NOT_INITIALIZED`, `WORKSPACE_INITIALIZATION_CONFLICT` | Read the selected workspace again; never initialize it as a read fallback |
| `RuntimeService.ListProfiles` | R | `UNSUPPORTED_SCHEMA_VERSION` | Refresh compatible capabilities and reissue the bounded read |
| `RuntimeService.GetProfile` | R | `PROFILE_NOT_FOUND`, `PROFILE_MISMATCH`, `COMPATIBILITY_REJECTED` | Reselect an explicitly supported Profile or refresh its observation; never substitute a Profile silently |
| `RuntimeService.ListProfileDiagnostics` | R | `PROFILE_NOT_FOUND`, `EVENT_CURSOR_INVALID` | Refresh Profile identity or restart diagnostics from an explicitly chosen valid cursor |
| `RunService.StartRun` | K | `WRITER_BUSY`, `WORKSPACE_STARTUP_UNSUPPORTED`, `DEDICATED_SERVER_START_FAILED`, `IDEMPOTENCY_CONFLICT`; post-allocation `INTERNAL_ERROR` can retain the original Run | Lookup the original Controller/workspace/start key; replay only that identity or recover its original Run |
| `RunService.ListRuns` | R | `WORKSPACE_NOT_INITIALIZED`, `UNSUPPORTED_SCHEMA_VERSION` | Refresh workspace/capabilities; mixed modern and accepted legacy manifests MUST remain individually readable |
| `RunService.GetRun` | R | `RUN_NOT_FOUND`, `RUN_STATE_CONFLICT` | Obtain a fresh compatible snapshot; a missing Run cannot authorize resubmission of earlier work |
| `RunService.SubmitTurn` | K | `RUN_BUSY`, `WRITER_BUSY`, `SESSION_CLOSE_IN_PROGRESS`, `IDEMPOTENCY_CONFLICT`, `OUTCOME_UNKNOWN` | Lookup original submit key without reopening attachments; accepted means observe that Turn, unresolved means reconcile, authoritative non-acceptance permits exact-key replay |
| `RunService.InterruptTurn` | S | `TURN_NOT_FOUND`, `RUN_STATE_CONFLICT`, `OUTCOME_UNKNOWN` | Read Run and original Turn; interrupt acknowledgement is not terminal proof; reconcile uncertain execution |
| `RunService.SetDefaultEffort` | S | `CAPABILITY_UNSUPPORTED`, `RUN_STATE_CONFLICT` | Read accepted current effort and revision before any new mutation |
| `RunService.PauseRun` | S | `RUN_BUSY`, `RUN_STATE_CONFLICT`, `OUTCOME_UNKNOWN` | Read pause/Turn/background state; reconcile before another lifecycle request |
| `RunService.ResumeRun` | S | `DEDICATED_SERVER_START_FAILED`, `THREAD_RESIDENCY_CONFLICT`, `RECOVERY_REQUIRED` | Read original Run/generation; supported reconcile/recover may resume it; failure cannot authorize a replacement thread |
| `RunService.CloseRun` | S | `SESSION_CLOSE_IN_PROGRESS`, `OUTCOME_UNKNOWN`, `RECOVERY_REQUIRED` | Read durable close operation and owned aggregate; continue the same owner through supported reconcile/recover; never resubmit accepted session-close intent |
| `RunService.DeleteRun` | S | `RUN_BUSY`, `RUN_STATE_CONFLICT` | Inspect Run/tombstone and prior authorized confirmation; no automatic repeated deletion or receipt removal |
| `RunService.RecoverRun` | S | `RECOVERY_REQUIRED`, `DEDICATED_HISTORY_BARRIER_FAILED`, `OUTCOME_UNKNOWN` | Read retained operation/generation and perform its prescribed recovery; a failed recovery cannot authorize redispatch |
| `RunService.ReconcileRun` | S | `THREAD_RESIDENCY_CONFLICT`, `BACKGROUND_EXECUTION_UNVERIFIED`, `OUTCOME_UNKNOWN` | Read updated state and exact generation before another explicitly selected reconciliation |
| `RunService.ForkRun` | K | `TURN_NOT_FOUND`, `THREAD_NOT_FOUND`, `IDEMPOTENCY_CONFLICT`, `OUTCOME_UNKNOWN` | Lookup original fork key and destination; preserve accepted history/provenance boundary |
| `RunService.VerifyRun` | V | `RUN_NOT_FOUND`, `BACKGROUND_EXECUTION_UNVERIFIED`, `RUN_STATE_CONFLICT` | Refresh original Run's verification; a verification result grants no lifecycle or writer effect |
| `RunService.CreateWriteContinuation` | K | `WRITE_CONTINUATION_SOURCE_NOT_TERMINAL`, `WRITE_CONTINUATION_CONTROLLER_INVALID`, `IDEMPOTENCY_CONFLICT` | Lookup original continuation key and destination; use a new same-principal Controller only as explicitly required by admission |
| `ObservationService.WatchRunEvents` | E | `EVENT_CURSOR_INVALID`, `SLOW_CONSUMER`, `SERVER_SHUTDOWN`, `RUN_STATE_CONFLICT` | Reconnect from committed delivered cursor; missing historical stamp requires snapshot rebase, never invented delivery |
| `ObservationService.ListRunTimelineItems` | R | `EVENT_CURSOR_INVALID`, `RUN_NOT_FOUND`, `ARTIFACT_INTEGRITY_FAILURE` | Preserve captured head and opaque page cursor; fetch complete oversized content through checked artifacts |
| `InteractionService.ListPendingInteractions` | R | `RUN_NOT_FOUND`, `RUN_STATE_CONFLICT` | Refresh pending summary independently of timeline degradation |
| `InteractionService.GetControllerInteraction` | R | `CONTROLLER_MISMATCH`, `INTERACTION_NOT_FOUND`, `INTERACTION_FULL_PAYLOAD_REQUIRES_CONTROLLER` | Verify current Controller and refetch current interaction; no observer fallback for protected payload |
| `InteractionService.ResolveInteraction` | K | `INTERACTION_STALE`, `INTERACTION_ALREADY_RESOLVED`, `INTERACTION_RESPONSE_INVALID`, `INTERACTION_RESPONSE_TOO_LARGE`, `INTERACTION_OUTCOME_UNKNOWN` | Lookup non-secret resolution receipt/refetch interaction; winning secret key returns receipt without body comparison; pending secret input requires explicit re-entry and MUST NOT be automatically replayed |
| `WriterService.GetWorkspaceWriterStatus` | R | `WORKSPACE_NOT_INITIALIZED`, `RUN_STATE_CONFLICT` | Refresh workspace and holder Run; report holder identity only under supported authorization |
| `WriterService.AcquireWriter` | S | `WRITER_BUSY`, `SHARED_RUN_WRITE_FORBIDDEN`, `ACCESS_TRANSITION_UNSUPPORTED`, `RUN_STATE_CONFLICT` | Read writer holder/generation and Run policy proof; fixed read-only pooled Runs remain non-promotable |
| `WriterService.ReleaseWriter` | S | `RUN_BUSY`, `CROSS_CONTROLLER_RELEASE_REQUIRED`, `RUN_STATE_CONFLICT`, `OUTCOME_UNKNOWN` | Read policy and writer guard; preserve reservation until read-only enforcement and descendant absence are proven |
| `WriterService.PrepareWriterHandoff` | S | `WRITER_HANDOFF_NOT_ALLOWED`, `STALE_WRITER_GENERATION`, `WRITER_BUSY` | Read current handoff and writer generation; no newly prepared transfer from an uncertain earlier response |
| `WriterService.CommitWriterHandoff` | S | `STALE_WRITER_GENERATION`, `WRITER_HANDOFF_NOT_ALLOWED`, `OUTCOME_UNKNOWN` | Read current holder/handoff and exact generation; reconcile uncertain authority before mutation |
| `WriterService.CancelWriterHandoff` | S | `STALE_WRITER_GENERATION`, `WRITER_HANDOFF_NOT_ALLOWED` | Read existing handoff and holder; cancellation response is not proof that another handoff did not commit |
| `ControllerService.VerifyController` | V | `CONTROLLER_MISMATCH`, `CONTROL_MODE_CONTROLLER_MISMATCH` | Revalidate the explicit carrier; no retry with inferred or model-supplied authority |
| `ArtifactService.GetArtifact` | R | `ARTIFACT_NOT_FOUND`, `INTERACTION_ARTIFACT_REQUIRES_CONTROLLER`, `ARTIFACT_INTEGRITY_FAILURE` | Verify authorized metadata and content availability; a retired body cannot erase retained identity |
| `ArtifactService.ReadArtifactChunk` | R | `ARTIFACT_RANGE_INVALID`, `ARTIFACT_INTEGRITY_FAILURE`, `ARTIFACT_NOT_FOUND` | Use bounded valid offsets, verify complete length/hash, and refuse partial bytes as a complete result |
| `OrchestrationService.GetOrchestratedSession` | R | `RUN_NOT_FOUND`, `CONTROLLER_MISMATCH`, `RUN_STATE_CONFLICT` | Refresh original root/session and retained close operation; never reinterpret it as an external Engagement |
| `OrchestrationService.ListOrchestratedSessionResults` | R | `RUN_NOT_FOUND`, `CONTROLLER_MISMATCH`, `ARTIFACT_INTEGRITY_FAILURE` | Read complete authorized results with source identities; summary availability does not prove full-result availability |

`INTERNAL_ERROR` and transport failure on a mutating method remain uncertain
unless a typed detail or authorized authoritative read proves `none`. The
presence of a code such as `RUN_BUSY` in a current refusal cannot resolve an
earlier ambiguous invocation. A `DATA_LOSS` condition requires supported
operator recovery and cannot be retried into a new generation automatically.

### New successor operations

TASK-068 MUST give each operation below a checked transport carrier, request
identity, response and error fixture. Names identify semantic operations here;
they do not claim that a particular RPC or model tool is already exposed.

| Operation | Effect and identity | Recovery and required failures |
| --- | --- | --- |
| Authorized operation lookup | R; exact Controller principal/generation, workspace or aggregate scope, operation kind and original key | Closed result union described below; authorization failure exposes no foreign receipt; corruption or unavailable index cannot return authoritative absence |
| Modern provisioning validation | R; selected Markdown Role/policy/Profile inputs | Missing, unsafe, raced or over-budget sources fail before install/allocation; no implicit installation |
| Explicit modern provisioning installation | K; accepted captured bytes and explicit authorizing operator/controller scope | Original-key receipt lookup; incompatible Profile or source drift rejects a new install without changing existing accepted snapshots |
| External hire / managed spawn | K; authenticated aggregate owner, accepted Role/configuration and original invocation identity | Lookup original member/admission; uncertain spawn preserves original member and cleanup owner; no replacement allocation |
| External assign / managed Broker task assignment | K; admitted member, exact task/context identity and caller scope | Lookup original accepted task; uncertain dispatch remains reserved and counts conservatively; known refusal resolves intent before exact-key retry |
| Await task/group | R; immutable task/group identity and `any`, `all`, or validated `k` predicate | Timeout ends the wait only; it never cancels tasks or changes unknown work into failure |
| Collect task/group / identity read-by-key | R; accepted identity and authorized scope | Return complete result references and coverage; unavailable content is typed and preserves its immutable identity; unresolved task remains explicit |
| Assign group | K; immutable bounded assignment manifest, original key and aggregate owner | Exact replay returns same group; partial acceptance or uncertainty recovers individual members without repeating uncertain tasks |
| Merge group results | K; immutable collected source set and `merge-exact-findings-v1` algorithm | Reproduce the same merged identity from the same sorted inputs; unknown/failed/missing sources remain coverage entries; invalid findings yield `FINDINGS_OUTPUT_INVALID` |
| Assign synthesis | K; ordinary explicitly accepted task referencing immutable merged input | Counts its own accepted round/result; no merge or collection call silently invokes a model |
| Cancel task / release member / close aggregate | S; authenticated current owner and current task/member/aggregate identity | Read same durable lifecycle owner and membership/workload proof; interrupt acknowledgement alone cannot establish `released` or terminal outcome |
| Operator maintenance / explicit result retention or GC | S; explicit operator scope, generation and dry-run plan | Preserve active/unknown references and identity receipts; interrupted operation resumes its recorded generation; predictable capacity refusal differs from corruption |

`PlanMaintenance.plan_id` is a UUIDv7 allocated for the returned dry-run plan;
planning creates no maintenance effect. The caller retains that ID, operator
identity, workspace and selected generation before `ApplyMaintenance`.
`maintenance_id` (facade `operation_ref`) MUST equal this original plan ID.
`GetMaintenance` uses it with the original authorized scope, even after the
selected generation changes. Apply durably binds at most one maintenance owner
to that plan under the lifecycle lock before any effect. Recovery and generation
retirement retain the binding. After reply loss, read this owner before any new
mutation; do not allocate a replacement ID or repeat Apply automatically.
`MAINTENANCE_NOT_APPLIED` proves no owner only after a complete, integrity-checked
read of the retained plan namespace. An unavailable or corrupt namespace requires
recovery and cannot return that refusal. The refusal grants no new mutation
authority. Facade Apply carries the complete retained dry-run plan and explicit
approval, matching public Apply; it cannot depend on a planning-side cache.
The facade getter retains `plan_ref`, workspace and selected generation;
operator authority remains in trusted transport, outside model arguments.

The public `OperationRef` consists of the retained `CallerScope`, operation kind
and original idempotency key. Consumers MUST retain these facts before the
initial mutation. The Controller kind, normalized principal and generation come
from the selected credential; the workspace and existing aggregate come from
the original request and prior authorized inspection. Pre-allocation operations
use fixed `provisioning_scope` values: `run-start` for `StartRun`,
`provisioning-install` for `InstallProvisioning` and `external-engagement-open`
for `OpenExternalEngagement`. Installation takes its workspace identity from
the retained `captured.workspace`. Run mutations use the original Run ID;
fork/continuation creation uses the source Run, with the destination Controller
for continuation admission. Other keyed delegation/group mutations use the
existing engagement or session in their original scope. A newly allocated
subject or the first mutation response MUST NOT be needed to construct lookup
identity; allocated subject IDs belong to the retained receipt.

`CallerScope.controller_generation` fences the public credential namespace.
There is no additional client-selected storage/namespace generation in
`OperationRef`. Physical storage generations and protected native-adapter
generations remain provider-owned recovery state. The provider proves its
retained namespace fence and durable watermark in an `absent` response;
unavailable or corrupt generation state cannot produce authoritative absence.

Lookup's normal result MUST be exactly one of:

| Result | Required facts and permitted next step |
| --- | --- |
| `accepted` | Original operation reference, accepted input digest, retained receipt, accepted subject identities, current authorized outcome/result reference; observe or collect that operation |
| `not_accepted` | Original key/scope/kind, durably recorded definitive refusal and input identity; exact-key retry is permitted only under that method's admission rules |
| `unresolved` | Original operation reference, known admitted identities, effect uncertainty and supported next action; reconcile without redispatch |
| `absent` | Complete authoritative lookup under the exact retained Controller/scope/kind/key namespace, with no matching operation or tombstone; permits admission of that original key |

Lookup authorization MUST NOT depend on current task files, attachment paths,
mutable prompt reconstruction or possession of an artifact ID. An unavailable,
corrupt or incompletely retained namespace MUST fail with a typed recovery
condition, never `absent`. No key TTL may erase an acceptance while its workspace
or aggregate remains retained. Content retirement MUST retain lookup metadata.
Protected interaction answers remain excluded from lookup and result content.

## Digest encoding and closed input objects

New digest domains use SHA-256. `D(domain, object)` means SHA-256 over the UTF-8
bytes of the exact ASCII domain, one LF byte, then the RFC 8785 JCS encoding of
the complete closed object. Its wire representation is lowercase
`sha256:` followed by 64 lowercase hexadecimal digits. The digest itself is
excluded from its own input. Domains below are exact and case-sensitive.

JCS reorders object keys and encodes JSON strings; it MUST NOT trim or normalize
the underlying source bytes, Unicode, line endings, opaque references or array
order. Invalid UTF-8, lone surrogates, duplicate keys, NUL-bearing instruction
sources, non-finite numbers and unknown fields are rejected. No optional field
is silently defaulted for digesting. Every listed field is required; nullable
fields use explicit `null`, and empty collections use `[]`. Integers in these
objects are unsigned JSON integers no larger than 2^53-1; lexical floats such
as `1.0` are rejected. Existing persisted counters outside that range require a
new schema/domain, never a lossy conversion.

Raw source/content hashes remain `SHA256(exact bytes)` without a domain prefix.
Existing captures using 64 hexadecimal digits retain that representation.
When placed in a new `BytesRef`, the same digest value gains the `sha256:`
representation prefix; its hash input does not change. TASK-068 vectors MUST
cover CRLF/LF, composed/decomposed Unicode, empty permitted content, ordering,
explicit null, duplicate-key rejection and one-byte source changes.

The following definitions are closed objects: additional fields are forbidden
at every level. Identifiers retain their owning contract's limits and exact
equality rules. Byte references require complete retained authorized content;
an uploader-supplied hash without validation and accepted linkage is insufficient.

### Shared values

| Type | Exact fields and constraints |
| --- | --- |
| `BytesRef` | `byte_length`: unsigned integer; `sha256`: lowercase prefixed digest of complete exact bytes. Zero length is allowed only where the owning content type permits it |
| `NamedSource` | `name`: 1..256 UTF-8 bytes without control characters, slash or backslash; `content`: `BytesRef`. Names are unique within their array; no path traversal, command expansion or source discovery follows from a name |
| `InstructionRef` | `schema`: exact snapshot schema ID; `digest`: its new domain-separated digest |
| `ContextRef` | `kind`: `accepted_context`, `artifact`, or `review_target`; `identity`: exact owning opaque identity; `content`: `BytesRef`; `media_type`: exact admitted media type |
| `ImageRef` | `content`: `BytesRef`; `media_type`: exact admitted image media type; `detail`: admitted detail selection |
| `ChangeRef` | `kind`: `add`, `modify`, `delete`, or `rename`; `path`: validated canonical workspace-relative path; `old_path`: path for rename, otherwise null; `before`: complete original `BytesRef` or null; `after`: complete accepted `BytesRef` or null; `before_mode`: accepted numeric mode or null; `after_mode`: accepted numeric mode or null |
| `ResultSource` | `scope_kind`, `scope_ref`, `worker_ref`, `task_ref`: accepted opaque identities; `role_snapshot_digest`, `agent_configuration_digest`, `accepted_task_digest`: their respective exact domain digests |

Source aliases and paths use UTF-8 bytes for validation and sorting. Arrays
declared sorted below use ascending bytewise UTF-8 order, with duplicate keys
rejected. A renamed file has both old and new identities; deletes have null
`after`, additions have null `before`. File mode values describe accepted
content provenance and confer no execution or write authority.

### Role, guidance and instruction composition

`dolgorae.role-snapshot/v1` hashes this exact object:

```text
{schema, role_body, dependencies, selected_guidance}
schema = "dolgorae.role-snapshot/v1"
role_body = BytesRef
dependencies = [NamedSource], sorted by name
selected_guidance = [NamedSource], in explicit accepted composition order
```

The Role reference/name is separate attribution. The Role snapshot binds exact
accepted content and dependencies, so equal display names cannot hide drift.
Dependencies and selected guidance each have at most 64 entries. Dependency
aliases are unique. Selected guidance aliases are unique and their array order
is meaningful. Common instructions remain a separate captured
layer. No source path, credential, runtime timestamp or current file content
enters this object. Inline Role admission follows the same capture and digest
rules as file-backed admission.

The raw Role body's SHA-256 is separately exposed as `role_body_sha256` with
its byte length. Podway's host-neutral contract requires complete resolved Role
content under the selected adapter revision; it does not prescribe a universal
raw-body or canonical-object hash. New bindings using the proposed mapping
declare `D("dolgorae.role-snapshot/v1", object)` as their exact Role requirement.
An existing Procedure that declares a raw-body hash or another snapshot domain
MUST NOT be reinterpreted under this mapping. Its selected separately reviewed
mapping must preserve that domain, or admission reports incompatible mapping.
Adding the textual `sha256:` prefix cannot convert one domain into another.

`dolgorae.instruction-identity/v1` hashes:

```text
{schema, core_version, core, common, role_snapshot_digest,
 dependency_order, guidance_order, composition_version, composition}
schema = "dolgorae.instruction-identity/v1"
core_version = accepted core integer version
core, common, composition = BytesRef
role_snapshot_digest = dolgorae.role-snapshot/v1 digest
dependency_order, guidance_order = [exact accepted source alias]
composition_version = "core-common-role-dependencies-guidance-lf2/v1"
```

Composition is the exact core bytes, common bytes, Role body, each dependency
body in `dependency_order`, then each guidance body in `guidance_order`, joined
by exactly two LF bytes with no added prefix/suffix or normalization.
`dependency_order` MUST enumerate the snapshot's dependency names exactly once
in bytewise name order. `guidance_order` MUST equal its selected-guidance array
order. The new aggregate source ceiling remains 65,536 UTF-8 bytes across
common, Role, dependencies and selected guidance. Core and separator bytes are
outside this ceiling. Every source is nonempty, valid UTF-8 and NUL-free.
Existing common-plus-Role composition/v2 is preserved for accepted old records.

Profile ambient guidance is disclosed separately and does not enter these
captured identities. Dynamic Turn access context and task content remain
separate. Reactivation reconstructs accepted bytes and never reloads sources.

### Admitted configuration and accepted task

`dolgorae.admitted-configuration/v1` hashes:

```text
{schema, profile_binding_schema, profile_binding_digest, model, effort,
 purpose_kind, purpose_label, required_capabilities, execution_lane,
 requested_access, required_assurance, native_subagent_policy,
 instruction_identity_digest}
schema = "dolgorae.admitted-configuration/v1"
profile_binding_schema = exact accepted immutable binding schema ID
profile_binding_digest = exact accepted binding digest under that schema
model, effort, purpose_kind = exact admitted values
purpose_label = exact admitted label or null
required_capabilities = sorted unique exact capability IDs
execution_lane = "dedicated" or "shared_readonly"
requested_access = "read_only" or "canonical_workspace_write"
required_assurance = exact admitted assurance enum
native_subagent_policy = "enabled" or "disabled"
instruction_identity_digest = dolgorae.instruction-identity/v1 digest
```

This object identifies the immutable admission, including the then-selected
effort. A later default-effort change applies to future Turn admission and does
not rewrite existing task/configuration digests. Current policy verification,
writer ownership, mutable capability observation, account secrets and process
IDs are excluded. The Profile binding remains immutable and version-qualified;
the configuration MUST NOT contain secret carrier bytes. Specialist native
subagents default to disabled; enabled admission requires a qualified dedicated
configuration. A shared read-only configuration cannot select writable access.

`dolgorae.accepted-task/v1` hashes:

```text
{schema, purpose, task_body_schema, task_body, contexts, images,
 expected_output_schema, execution_intent, admitted_model, admitted_effort,
 role_snapshot_digest, agent_configuration_digest}
schema = "dolgorae.accepted-task/v1"
purpose = exact accepted task purpose
task_body_schema = exact closed task-body schema ID (public structured tasks use
                   "dolgorae.structured-task/v1"; facade structured tasks use
                   "https://dolgorae.local/schema/external-specialist-facade/v5#/$defs/task_body")
task_body = BytesRef of JCS closed structured task body,
            or exact UTF-8 text under the explicitly selected text schema
contexts = [ContextRef], in accepted task order
images = [ImageRef], in accepted input order
expected_output_schema = exact accepted output schema or text-format ID
execution_intent = "read_only" or "canonical_workspace_write"
admitted_model, admitted_effort = exact Turn admission values
role_snapshot_digest = dolgorae.role-snapshot/v1 digest
agent_configuration_digest = dolgorae.admitted-configuration/v1 digest
```

The executor commits this object with the accepted task identity before
dispatch. Context references bind complete accepted content and provenance;
the Podway work-package digest may be retained as a context identity but cannot
substitute for the accepted task digest. Caller-local prepared bytes alone do
not prove admission. IDs, keys, Controller carriers, mutable deadlines and
transport request IDs are excluded from task content identity and belong to
the envelope/admission record. Context order and image order are preserved.

The existing brief/context/criterion bounds of the selected task schema apply;
the new mapping MUST NOT fall back to a legacy 65,536-byte objective carrier
when the accepted task uses the richer structured task schema. Large required
evidence uses immutable artifact references and supported complete reads.

### Mutation envelope

`dolgorae.mutation-envelope/v1` hashes:

```text
{schema, request_schema, operation_kind, workspace_ref, scope_kind, scope_ref,
 controller_principal, controller_generation, idempotency_key,
 trusted_invocation_ref, request_body}
schema = "dolgorae.mutation-envelope/v1"
request_schema = exact closed normalized request schema ID
workspace_ref = exact canonical workspace identity
scope_kind, scope_ref = exact accepted scope, or null for pre-allocation scope
controller_principal = normalized non-secret Controller principal
controller_generation = accepted Controller generation
idempotency_key = exact original key
trusted_invocation_ref = protected logical invocation ID or null
request_body = BytesRef of the JCS closed normalized request body
```

The normalized body excludes Controller secret bytes, current expected state
revision, transport correlation/connection/process IDs and the envelope's own
fields. It includes every method-specific semantic input: requested workspace
selection, Role/configuration/task digests, access/lane/assurance, source/fork
boundary, non-secret decision or lifecycle options as applicable. Omissions and
defaults MUST be resolved once by the owning request schema before digesting.
Unknown fields cannot disappear through normalization.

`ResolveInteraction` retains ordinary non-secret `answers` in question order.
Each closed answer contains `question_id`, ordered `selected_labels` and
explicit nullable `text`; changing any of them changes the normalized body and
mutation digest. The answer array has at most 64 entries, question IDs at most
128 UTF-8 bytes, each of at most 64 labels at most 256 UTF-8 bytes, and text at
most 65,536 UTF-8 bytes. Empty arrays and absent text (`null`) remain explicit.
For a secret-bearing interaction the normalized `answers` array is empty:
secret answer bytes and protected-input paths are never stored or hashed.
Authenticated winning-key retries use the original receipt without comparing
the supplied secret-bearing answer body. `protected_input_present` retains
only the non-secret carrier-presence fact; it never substitutes for ordinary
answers. Normalized image detail admits exactly public-v2's `auto`, `low` and
`high` values.

An existing-Run `SubmitTurn` mutation uses `scope_kind = "run"` and its original
Run ID as `scope_ref`; pre-allocation null scope is invalid for this operation.

Public-v2 `AssignSynthesis` uses
`https://dolgorae.local/schema/v014-identities/v1#/$defs/public_assign_synthesis_normalized_body`.
Its closed body contains `member_ref`, `merged_findings_digest`, `merged_input`
and `assignment`. The assignment retains `task_body`, `accepted_task_identity`
and `deadline_seconds`. Its complete public `StructuredTaskBody` contains
`schema`, `brief`, `context`, `criteria` and `constraints`; each criterion
retains `criterion_id`, `text` and `context_ids`. Repeated fields resolve to
explicit arrays before digesting, including empty arrays. Context and
constraint order is preserved. The accepted task's `task_body` BytesRef binds
these same complete JCS bytes, including every constraint. The existing rich
brief/context/criterion bounds apply. Ordered `constraints` has at most 64
strings, each at most 8,192 UTF-8 bytes (including JSON control characters).
The complete JCS public task body has a 16,777,216-byte encoded ceiling; the
finite worst-case proof includes every constraint, context and criterion.
An over-limit input is refused before acceptance and cannot be truncated.
The facade/v5 normalized synthesis schema owns its separate facade task-body
vocabulary and cannot name this public structured body. Normalized-body
validation follows nested schema references, including task and criterion
objects; matching the outer field names alone is insufficient.

Expected revision remains a new-operation admission fence and is persisted
alongside this identity. Original acceptance replay is checked before that
fence. The protected invocation reference is mandatory for the successor MCP
route; null is permitted only for a non-MCP keyed route that already supplies
its identity outside model arguments. ADR-031 or an accepted successor owns the
carrier and retention rules. No model field can select this reference.

Protected interaction answer bytes and their hashes are excluded from this
domain. Its body identifies the interaction, typed allowed decision and
protected-input presence only. First accepted secret resolution owns the key;
replay returns its non-secret receipt without retaining/comparing the body.

### Complete results and coverage

`dolgorae.complete-result/v1` hashes:

```text
{schema, source, terminal_outcome, final_response, structured_output_schema,
 structured_output, changes, attachments, safe_failure_code}
schema = "dolgorae.complete-result/v1"
source = ResultSource
terminal_outcome = "completed", "failed", "cancelled", or "expired"
final_response = complete exact UTF-8 BytesRef or null
structured_output_schema = exact validated output schema ID or null
structured_output = complete validated JCS output BytesRef or null
changes = [ChangeRef], sorted by path, old_path, then kind
attachments = [ContextRef], sorted by kind then identity
safe_failure_code = checked safe cause for failed/cancelled/expired outcomes,
                    or null when no checked cause exists; null on success
```

The result becomes immutable at its authoritative terminal acceptance. Failure,
cancellation and expiry are included only with terminal proof; unresolved work
has no fabricated terminal result. Checked structured findings must validate
before publication. `FINDINGS_OUTPUT_INVALID` is a failed-output result with
safe failure metadata and no accepted structured finding payload.

Every content reference binds complete bytes. A text preview, safe summary,
page, artifact ID, mutable working-tree observation or usage sample is excluded.
All accepted change components are included; a result cannot omit a change
because its preview is too large. Identical bytes may be stored once and
referenced repeatedly, while each provenance link remains explicit.

The established public bounds remain 1 MiB inline final response, 32 MiB final
response artifact, 1 MiB requested chunk, 8 MiB individual change/user-input
artifact and 256 MiB per-Run artifact quota. A large content body is read in
bounded chunks; the digest verifies complete bytes independent of presentation
limits. Retiring result content leaves its digest, length, source and typed
unavailability metadata intact. Automatic content deletion is not enabled.

`dolgorae.group-assignment/v1` hashes:

```text
{schema, scope_kind, scope_ref, entries}
schema = "dolgorae.group-assignment/v1"
entries = [{entry_key, role_snapshot_digest, agent_configuration_digest,
            accepted_task_digest}], sorted by unique entry_key
```

The identity's `scope_kind` is exactly `external_engagement` or
`orchestrated_session`, with the corresponding engagement or session ID as
`scope_ref`. Public `DelegationScope.external` and `.managed` select those
identities respectively; the transport labels themselves are not digest values.
Protobuf JSON field spelling does not change the closed identity vocabulary.

The group contains 1..64 entries and its complete encoded immutable manifest is
at most 1 MiB. Entries bind task/context identities through immutable references;
large task bodies are not duplicated inline. Group acceptance durably captures
the assignment set before member provisioning or dispatch. The durable group
record separately links each entry to its task/member identity and current
provisioning/dispatch state. Partial progress cannot change the manifest or
erase a member. Wait predicates and observation timestamps are excluded.
TASK-068 must prove worst-case size and include the mandatory 50-task case.

`AssignGroup.inputs` supplies exactly one `GroupTaskSource` per manifest entry,
sorted by the same unique entry key. Each source contains an explicitly selected
absolute file path and a `BytesRef` of its complete JCS
`dolgorae.group-task-input/v1` document. The checked facade definition
`group_task_input` owns that closed document: `{schema, provisioning_ref,
assignment}`, where `assignment` is the existing rich facade task assignment.
`provisioning_ref` selects the immutable installed provisioning snapshot returned
by Install (public `installed_policy_ref`); it does not hire or dispatch a member.
The snapshot supplies the accepted Role, configuration and instruction bytes.
Public-v2 and Machine/facade-v5 use this same file format. A digest alone or an
earlier individual Assign is not an input supply path.

The provider reads only these explicit regular files within the authorized
workspace, rejecting symlinks, path escape, source races, duplicate JSON keys,
unknown fields, unavailable snapshots, hash/length mismatch and invalid tasks
before group acceptance. A source is at most 128 MiB; the selected task schema's
existing rich context and criterion bounds still apply. This ceiling covers its
worst-case escaped JSON without replacing rich tasks with legacy objectives.
Referenced evidence remains subject to complete authorized content reads.
Validate each task body and accepted-task identity against actual captured bytes
and the installed provisioning snapshot, then match all three manifest digests.
Missing, duplicate, extra or mismatched entry input returns
`GROUP_MANIFEST_INVALID` with no acceptance or dispatch.

Capture the complete validated input bytes and provisioning snapshot references
durably before committing the group receipt, and commit that receipt before any
member provisioning or dispatch. Group state retains one content-addressed input
capture per entry; recovery never reopens a source file. Each stored capture
chunk is at most 8 MiB; the 256-MiB per-Run quota and reserved capacity apply.
Capture references identify the complete chunked snapshot, without increasing
existing individual artifact or read-chunk limits. Capacity is reserved before
acceptance; capacity refusal has no group effect.
The manifest and its digest remain unchanged. The normalized AssignGroup body
contains `{manifest, inputs}`, with each input reduced to `{entry_key, content}`:
paths locate captured bytes but are excluded from semantic identity. Same-key
replay of accepted work returns the retained receipt before opening input files;
changed content or manifest under that key conflicts. The input-reference list
is at most 2 MiB encoded and each path at most 4096 UTF-8 bytes. It travels with the at-most-1-MiB manifest; task bodies stay in the
captured snapshots.

`dolgorae.merged-findings/v1` hashes:

```text
{schema, algorithm, group_assignment_digest, sources, merged_output}
schema = "dolgorae.merged-findings/v1"
algorithm = "merge-exact-findings-v1"
group_assignment_digest = dolgorae.group-assignment/v1 digest
sources = [{entry_key, worker_ref, task_ref, role_snapshot_digest,
            agent_configuration_digest, accepted_task_digest, state,
            result_digest, safe_failure_code}], sorted by unique entry_key
merged_output = BytesRef of complete validated JCS merged findings
state = "completed", "failed", "cancelled", "expired", "unknown", or "missing"
```

Each source row retains its identity even when no complete result is available.
`worker_ref`, `task_ref` and digests that lack accepted evidence use explicit
null; they MUST NOT be guessed. `result_digest` is null for unknown/missing
sources and otherwise references a proven terminal complete result.
`safe_failure_code` is null where no checked safe cause exists.

Each source has at most 64 checked findings, preserving the existing Specialist
review output ceiling. Up to 64 group sources can therefore contribute 4096
findings before equivalence reduction. The complete merged artifact is at most
32 MiB and references complete immutable source artifacts. A normal 4 MiB page
returns bounded artifact/source references instead of all source bodies.
If accepted complete content exceeds the merge limit, a typed capacity refusal
preserves the source set and its coverage/provenance references; it MUST NOT
silently drop findings or claim a complete merged result. The existing 256 MiB
per-Run artifact quota still applies.

The merged output groups exact stable finding keys and removes only equivalent
checked finding entries. Equivalence requires equality of all finding content
fields, evidence, covered questions and limitations after the output schema's
specified canonicalization; source provenance remains separately preserved.
Different claims under the same key form explicit conflict sets. Failed,
expired, cancelled, unknown and missing sources remain in coverage. Free-text
similarity and a model call cannot establish equivalence. Synthesis is a
separate accepted task and receives its own result digest and round accounting.

## Podway export and authority boundary

The versioned mappings are `dolgorae.podway-external/v1` and
`dolgorae.podway-managed/v1`. These are the mapping contract IDs selected by
the accepted coordination decision. They do not identify an installed
adapter or change Podway's contract. Each mapping's qualified implementation
must supply its own exact `adapter_id` and `adapter_version`.

| Podway field | External Primary | Managed Primary |
| --- | --- | --- |
| `binding_ref` | Exact declared trusted host binding | Exact declared accepted session binding |
| `primary_host` | `external` | `managed` |
| `scope_kind`, `scope_ref` | `external_engagement`, accepted Engagement ID | `orchestrated_session`, accepted session ID |
| `supervisor_ref` | Authenticated external Controller's non-secret principal | Authenticated root Primary Controller's non-secret principal |
| `worker_ref`, `task_ref` | Accepted facade member/Run and task identities | Accepted Broker member/Run and task identities |
| `role_ref` | Exact declared Role reference | Exact declared Role reference |
| `role_snapshot_digest` | `dolgorae.role-snapshot/v1` | Same domain from accepted Broker identity |
| `agent_configuration_digest` | `dolgorae.admitted-configuration/v1` | Same domain from accepted Broker identity |
| `accepted_task_digest` | `dolgorae.accepted-task/v1` | Same domain from accepted Broker identity |
| `result_digest` | `dolgorae.complete-result/v1` | Same domain from complete Broker-delivered result |
| `rounds_used` | Durable accepted assignment count in the recorded caller scope | Equivalent durable count in the accepted managed scope |
| `worker_disposition` | Current membership/workload proof of `idle`, or retained successful release proof of `released` | Same vocabulary through the authorized Broker boundary |

Caller scope is the trusted binding's one Podway workspace/session/attempt and
delegated placement, stored explicitly outside model arguments. It differs
from the executor's workspace/session/task identity. Each accepted executor
assignment in this scope counts once. Uncertain dispatch counts conservatively;
proved non-dispatch does not. Replay, wait, collection and deterministic merge
do not add rounds. Explicit synthesis adds its own accepted assignment.
The Podway declared limit remains 1..32; there is no inferred default.
All assignments in one caller scope count, including group members and
synthesis. Exporting a member separately MUST NOT reset that count or relabel
earlier work into a new scope. The mandatory standalone 50-task group scenario
does not claim a receipt within one 32-round Podway Action.

`idle` requires a current accepted member with no running, queued, claimed,
dispatching or unresolved work in its owned scope and no unresolved interaction
or cleanup preventing disposition. Retention alone is insufficient. `released`
requires the member's successful durable release and settled owned workload.
The adapter MUST acquire the disposition proof after the final collected result
under the owning service's serialization; stale observation cannot authorize
receipt export. The final exported worker/task must identify the accepted result
that the Primary semantically accepted.

Full required Podway evidence is materialized through supported bounded reads;
package previews and paths are insufficient. The Primary records normal action
items and semantic acceptance first. Podway validates the structural receipt
and its current accepted-items digest, then controls its separate completion.
Dolgorae MUST NOT advance its cursor, calculate Podway's accepted-items digest,
access its private Store or settle a retired daemon job. Lost acceptance stdout
requires original-key Podway outcome lookup before exact-request replay.

External facade and managed run-bound Broker authorization remain distinct.
The adapter uses accepted/public response facts or authorized complete reads;
matching field names and private Store columns do not establish a mapping.
No managed Primary may switch to an external Engagement to bypass a gap.

Groups remain Dolgorae operations. The selected receipt scope is an
individual accepted task per delegated Action, including an explicitly assigned
synthesis task when appropriate. Full group/merge provenance remains available.
This scope does not claim that one group is one Podway Action or that Podway
supports fan-out, fan-in or child Actions. Consumer-owner scope confirmation
remains required before claiming accepted interoperability.

## Checked publication and runtime acceptance

TASK-068 MUST publish known answers for every domain and negative fixtures for
domain substitution, Role drift, reordered contexts, incomplete results,
changed requests under one key, protected-input leakage, wrong-scope lookup,
stale disposition and failed/unknown group sources. Method fixtures MUST cover
response loss before/after durable acceptance, definitive refusal, unresolved
effect, retained receipts after restart and corruption preventing absence.

The immutable H1 package pins source, descriptors, schemas, manifests and
generated clients. Code-generation pins must be selected before freeze.
TASK-068 cannot use an unbounded nested schema or a single small fixture as a
worst-case size proof.

Dolgorae owns the implementation plan for these identity, lookup, export,
maintenance and adapter contracts. Aquarium owns consuming and checking
admitted identities, candidate provenance and applicable workflow acceptance.
Podway owns its direct CLI, structural receipt and progression semantics.
The external replies establish technical agreement, not new runtime support.
Master accepted the source selection and Dolgorae-owned implementation plan in
the coordination decision. Dolgorae owns adapter installation instructions and
protected state; authorized consumer workflow agents check consumption, with
existing Primary/Judge and Master acceptance responsibilities. A selected
prepared Podway catalog does not attest a released CLI. Applicable contract
corrections require impact assessment; later actual artifact qualification
remains mandatory.

Both pinned native hosts and any trusted adapter must pass the required actual
pre-contract replay/conflict/concurrency/reconnect/restart/loss scenarios.
Static vectors cannot establish that qualification. ADR-031 or an explicitly
accepted successor must resolve protected logical invocation creation,
retention and carrier ownership before H1 can qualify.
