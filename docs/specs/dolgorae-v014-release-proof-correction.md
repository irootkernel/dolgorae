# Release-proof contract correction

Status: Normative contract correction specification for `EPIC-023` (`TASK-090`).

This specification owns the `v0.1.4` contract correction for state-based (`S`)
Specialist member release completion proof, retained release operation state,
durable release admission and assignment fencing, membership admission revision
semantics, settlement and recovery rules, and workspace Engagement database
generation advancement. The
[roadmap](../roadmap/README.md#epic-023-release-proof-contract-correction-and-consumer-reacceptance)
owns delivery ordering and Task status. Decision rationale is recorded in
[ADR-042](../architecture-decision-records/README.md#adr-042-replace-keyed-release-receipts-with-state-based-member-release-proofs).
The [release-proof correction dossier](../todo/TODO-DOLGORAE-RELEASE-PROOF-CORRECTION.md)
and [v0.1.4 alignment dossier](../todo/TODO-DOLGORAE-V014-RELEASE-ALIGNMENT.md)
provide supplementary planning context and grant no independent authority.

During `TASK-090`, all files bound by the original `H1` lock
([`dolgorae-gul-consumer-v2.lock.json`](../protocol/dolgorae-gul-consumer-v2.lock.json))
retain their exact bytes. `TASK-091` synchronizes the affected owning
specifications, JSON Schemas, fixtures, validators, and corrected `H1` lock.
`TASK-076`, `TASK-077`, and `TASK-086` own the runtime and storage
implementations after `TASK-092` accepts Gul's delta `H2` handoff.

## 1. Closed member release proof (`dolgorae.member-release-proof/v1`)

Both member release operations, `release_external_specialist` and
`release_specialist`, are state-based (`S`) lifecycle operations without an
`idempotency_key`. Keyed operation receipts (`$defs/operation_receipt` with
schema `dolgorae.operation-receipt/v1` in
[`dolgorae-v014-identities-v1.schema.json`](../protocol/dolgorae-v014-identities-v1.schema.json))
admit only the 14 keyed (`K`) operations (`start_run`, `submit_turn`,
`fork_run`, `create_write_continuation`, `resolve_interaction`,
`install_provisioning`, `open_external_engagement`, `hire_external_specialist`,
`assign_external_specialist_task`, `spawn_specialist`, `assign_specialist_task`,
`assign_group`, `merge_group_results`, and `assign_synthesis`). That `K` receipt
contract **MUST** remain unchanged.

A settled member release is proved by a closed JSON object with schema constant
`dolgorae.member-release-proof/v1` (`additionalProperties: false`). In
`$defs/disposition_proof`, the `release_receipt` field **MUST** accept only
`dolgorae.member-release-proof/v1` or `null`.

| Field | Type and constraints | Normative meaning |
| --- | --- | --- |
| `schema` | `"dolgorae.member-release-proof/v1"` | Closed proof schema identifier. |
| `operation_ref` | `uuid7` | Stable UUIDv7 identity of the durable `S` release operation that settled this member. |
| `operation_kind` | `"release_external_specialist"` \| `"release_specialist"` | Exact `S` release operation kind from [`dolgorae-external-specialist-facade-v5.schema.json`](../protocol/dolgorae-external-specialist-facade-v5.schema.json). |
| `workspace_ref` | `opaque_ref` | Canonical workspace identity owning the scope and member. |
| `scope_kind` | `"external_engagement"` \| `"orchestrated_session"` | Aggregate scope kind owning the member. |
| `scope_ref` | `opaque_ref` | Engagement ID (`external_engagement`) or orchestrated session ID (`orchestrated_session`). |
| `member_ref` | `opaque_ref` | Released member identity; **MUST** equal the enclosing delegation identity's `worker_ref`. |
| `run_ref` | `opaque_ref` | Specialist Run bound to `member_ref` at admission. |
| `controller_principal` | `opaque_ref` | Authenticated Controller principal that initiated the release operation. |
| `controller_generation` | integer `1..9007199254740991` | Authenticated Controller generation that initiated the release operation. |
| `terminal_membership_revision` | integer `0..9007199254740991` | Scoped membership revision at which `member_ref` became terminally `released`. |
| `settled_workload_revision` | integer `0..9007199254740991` | Member workload revision at which all tasks, interactions, and cleanup settled for release. |

In `$defs/external_delegation_identity` and `$defs/managed_delegation_identity`,
`worker_ref` identifies the accepted member (`member_ref`), while `run_ref` in
`dolgorae.member-release-proof/v1` records its separate Run binding. A
Specialist membership **MUST NOT** be rebound to another Run or reused after
terminal release. No public `run_generation` field is added to
`dolgorae.member-release-proof/v1`.

### 1.1 Finite schema branches and external versus managed pairing

Inspection of [`dolgorae-v014-identities-v1.schema.json`](../protocol/dolgorae-v014-identities-v1.schema.json)
(`$defs/disposition_proof`, `$defs/external_delegation_identity`, and
`$defs/managed_delegation_identity`) confirms that `external_delegation_identity`
and `managed_delegation_identity` currently reference shared
`$defs/disposition_proof` without binding outer `worker_disposition` to
`disposition_proof.state` or binding the enclosing identity kind to
`release_receipt.operation_kind` and `release_receipt.scope_kind`. `TASK-091`
**MUST** encode the following finite schema branches:

1. When `worker_disposition` is `"idle"`, `disposition_proof.state` **MUST** be
   `"idle"`, `disposition_proof.release_receipt` **MUST** be `null`, and the
   member's scoped membership remains accepted and non-terminal.
2. When `worker_disposition` is `"released"`, `disposition_proof.state` **MUST**
   be `"released"`, `disposition_proof.release_receipt` **MUST** be a valid
   non-null `dolgorae.member-release-proof/v1` object, and the member's scoped
   membership is terminal.
3. In `$defs/external_delegation_identity` (`primary_host: "external"`,
   `mapping_contract: "dolgorae.podway-external/v1"`,
   `scope_kind: "external_engagement"`), a non-null `release_receipt` **MUST**
   have `operation_kind: "release_external_specialist"` and
   `scope_kind: "external_engagement"`, matching
   `$defs/release_external_specialist_request` in
   [`dolgorae-external-specialist-facade-v5.schema.json`](../protocol/dolgorae-external-specialist-facade-v5.schema.json).
4. In `$defs/managed_delegation_identity` (`primary_host: "managed"`,
   `mapping_contract: "dolgorae.podway-managed/v1"`,
   `scope_kind: "orchestrated_session"`), a non-null `release_receipt` **MUST**
   have `operation_kind: "release_specialist"` and
   `scope_kind: "orchestrated_session"`, matching
   `$defs/release_specialist_request` in
   [`dolgorae-external-specialist-facade-v5.schema.json`](../protocol/dolgorae-external-specialist-facade-v5.schema.json).
5. Across all `$defs/disposition_proof` instances, the quiescence counters and
   flags remain fixed constants: `running: 0`, `queued: 0`, `claimed: 0`,
   `dispatching: 0`, `unresolved: 0`, `pending_interaction: false`, and
   `cleanup_pending: false`.

### 1.2 Disposition proof refusal matrix

Schema validation, executable semantic validators, and the runtime **MUST**
reject every invalid combination below:

| Invalid case | Condition | Required refusal |
| --- | --- | --- |
| Released without proof | `worker_disposition == "released"` or `disposition_proof.state == "released"` with `release_receipt == null` | Schema and semantic rejection. |
| Idle with proof | `worker_disposition == "idle"` or `disposition_proof.state == "idle"` with non-null `release_receipt` | Schema and semantic rejection. |
| Outer/inner disposition mismatch | `worker_disposition != disposition_proof.state` | Schema and semantic rejection. |
| Non-zero workload or pending flags | Any of `running`, `queued`, `claimed`, `dispatching`, or `unresolved` is non-zero, or `pending_interaction` or `cleanup_pending` is `true` in `disposition_proof` | Schema and semantic/runtime rejection. |
| Keyed receipt substitution | `release_receipt.schema == "dolgorae.operation-receipt/v1"` (such as hire, spawn, or task assignment receipt) | Schema and semantic rejection. |
| Malformed proof object | Missing required field, unknown property, non-UUIDv7 `operation_ref`, or out-of-range integer | Schema rejection. |
| Wrong `operation_kind` | Any value other than `"release_external_specialist"` or `"release_specialist"` | Schema rejection. |
| Wrong `scope_kind` | Any value other than `"external_engagement"` or `"orchestrated_session"` | Schema rejection. |
| Kind/scope cross-mismatch | `"release_external_specialist"` paired with `"orchestrated_session"`, or `"release_specialist"` paired with `"external_engagement"` | Schema and semantic rejection. |
| Identity/proof cross-mismatch | `external_delegation_identity` carrying a `"release_specialist"` proof, or `managed_delegation_identity` carrying a `"release_external_specialist"` proof | Schema and semantic rejection. |
| Subject binding mismatch | `release_receipt.scope_ref != identity.scope_ref` or `release_receipt.member_ref != identity.worker_ref` | Semantic validator and runtime rejection. |
| Post-result digest mismatch | `disposition_proof.observed_after_result_digest != identity.result_digest` | Semantic validator and runtime rejection. |
| Within-domain revision order violation | `admitted_membership_revision >= terminal_membership_revision`, `terminal_membership_revision > disposition_proof.membership_revision`, or `settled_workload_revision > disposition_proof.workload_revision` | Semantic validator and runtime rejection. |

## 2. Retained release operation record (`dolgorae.member-release-operation/v1`) and database generations

Inspection of [`dolgorae-successor-state-v1.schema.json`](../protocol/dolgorae-successor-state-v1.schema.json)
confirms that `$defs/operation_intent_v2` (`schema_version: 2`) covers only
keyed (`K`) operations with a `mutation_envelope` and `operation_receipt`, while
`$defs/lifecycle_state` (`dolgorae.lifecycle-operation/v1`) models whole-Run
close and interruption. Inspection of `$defs/membership_record` in
[`dolgorae-orchestration-state-v2.schema.json`](../protocol/dolgorae-orchestration-state-v2.schema.json)
and `$defs/membership_record` in
[`dolgorae-profile-membership-v2.schema.json`](../protocol/dolgorae-profile-membership-v2.schema.json)
confirms that neither membership record stores workspace `S` release operation
or completion-proof state.

To back `dolgorae.member-release-proof/v1` durably, `TASK-091` **MUST** add a
closed `$defs/member_release_operation` definition
(`dolgorae.member-release-operation/v1`) to
[`dolgorae-successor-state-v1.schema.json`](../protocol/dolgorae-successor-state-v1.schema.json),
and `TASK-076` **MUST** persist it in a new `member_release_operations` table in
the workspace Engagement SQLite database (`orchestration/orchestration.sqlite3`).

### 2.1 Verified store generations and scoped membership counter ownership

Inspection of `SCHEMA_VERSION` in [`src/engagement.rs`](../../src/engagement.rs)
and `ORCHESTRATION_SCHEMA_VERSION` and `OrchestrationStore::open` in
[`src/orchestration.rs`](../../src/orchestration.rs) establishes the current
database version metadata in `orchestration/orchestration.sqlite3`:

- `EngagementStore` owns `metadata.key = 'schema_version'`, currently `"5"`
  (`SCHEMA_VERSION = 5`).
- `OrchestrationStore` opens the same database file through
  `EngagementStore::open` and owns `metadata.key = 'orchestration_schema_version'`,
  currently `"3"` (`ORCHESTRATION_SCHEMA_VERSION = 3`).

Base workspace Engagement database generation `6` (`metadata.schema_version = "6"`)
owns both the `member_release_operations` table and a durable monotonic
membership revision counter keyed by `(workspace_ref, scope_kind, scope_ref)`.
That scoped membership revision counter is distinct from per-member record
revisions and from generic aggregate `append_event` revisions, and **MUST NOT**
be derived from `orchestrated_sessions.revision`. Because both external
engagement release and managed orchestrated-session specialist release share the
base Engagement generation `6` tables and no existing orchestration extension
table changes shape, `metadata.orchestration_schema_version` **MUST** remain
`"3"` unchanged. Detailed SQLite DDL for the generation `6` base tables belongs
to `TASK-076`.

Historical records from `v0.1.3` and Engagement database generations `1` through
`5` **MUST** keep their exact stored bytes and meaning. The runtime **MUST NOT**
fabricate or backfill `dolgorae.member-release-operation/v1` or
`dolgorae.member-release-proof/v1` records for historical keyed releases.

### 2.2 Closed `dolgorae.member-release-operation/v1` shape

Each retained record in `member_release_operations` conforms to the closed
`dolgorae.member-release-operation/v1` object (`additionalProperties: false`):

| Field | Type and constraints | Mutability and meaning |
| --- | --- | --- |
| `schema` | `"dolgorae.member-release-operation/v1"` | Immutable schema constant. |
| `operation_ref` | `uuid7` | Immutable UUIDv7 allocated when the release operation is first admitted. |
| `operation_kind` | `"release_external_specialist"` \| `"release_specialist"` | Immutable `S` operation kind. |
| `workspace_ref` | `opaque_ref` | Immutable canonical workspace ID. |
| `scope_kind` | `"external_engagement"` \| `"orchestrated_session"` | Immutable scope kind paired with `operation_kind`. |
| `scope_ref` | `opaque_ref` | Immutable engagement ID or orchestrated session ID. |
| `member_ref` | `opaque_ref` | Immutable target member ID (at most one release operation per `(workspace_ref, scope_kind, scope_ref, member_ref)`). |
| `run_ref` | `opaque_ref` | Immutable Specialist Run ID bound to `member_ref`. |
| `controller_principal` | `opaque_ref` | Immutable initiating Controller principal. |
| `controller_generation` | integer `1..9007199254740991` | Immutable initiating Controller generation. |
| `admitted_membership_revision` | integer `0..9007199254740991` | Immutable membership revision of `(workspace_ref, scope_kind, scope_ref)` checked at initial release admission. |
| `owner_ref` | `opaque_ref` \| `null` | Separately mutable coordinator owner reference; active only while backed by the held lifecycle lock and verified witness. |
| `progress` | `"requested"` \| `"settling"` \| `"terminal"` \| `"cleanup_failed"` \| `"recovery_needed"` | Current durable lifecycle progress of the release operation. |
| `safe_failure_code` | `safe_code` \| `null` | Sanitized failure code when `progress` is `"cleanup_failed"` or `"recovery_needed"`; `null` when `"terminal"`. |
| `required_action` | `"none"` \| `"reconcile_operation"` \| `"recover_run"` \| `"restore_state"` | Required recovery action; `"none"` when `progress` is `"requested"`, `"settling"`, or `"terminal"`. |
| `terminal_membership_revision` | integer `0..9007199254740991` \| `null` | `null` until `progress == "terminal"`; once set, immutable terminal membership revision copied to `dolgorae.member-release-proof/v1`. |
| `settled_workload_revision` | integer `0..9007199254740991` \| `null` | `null` until `progress == "terminal"`; once set, immutable settled workload revision copied to `dolgorae.member-release-proof/v1`. |

Finite schema branches on `dolgorae.member-release-operation/v1` **MUST**
enforce:

1. `operation_kind == "release_external_specialist"` requires
   `scope_kind == "external_engagement"`, and
   `operation_kind == "release_specialist"` requires
   `scope_kind == "orchestrated_session"`.
2. When `progress == "terminal"`, `terminal_membership_revision` and
   `settled_workload_revision` **MUST** be non-null integers,
   `safe_failure_code` **MUST** be `null`, and `required_action` **MUST** be
   `"none"`.
3. When `progress` is `"requested"` or `"settling"`,
   `terminal_membership_revision`, `settled_workload_revision`, and
   `safe_failure_code` **MUST** be `null`, and `required_action` **MUST** be
   `"none"`.
4. When `progress` is `"cleanup_failed"` or `"recovery_needed"`,
   `terminal_membership_revision` and `settled_workload_revision` **MUST** be
   `null`, `safe_failure_code` **MUST** be non-null, and `required_action`
   **MUST** be one of `"reconcile_operation"`, `"recover_run"`, or
   `"restore_state"`.

## 3. Durable release admission, assignment fencing, and separated revision domains

Both `$defs/release_external_specialist_request` and
`$defs/release_specialist_request` in
[`dolgorae-external-specialist-facade-v5.schema.json`](../protocol/dolgorae-external-specialist-facade-v5.schema.json),
as well as `ReleaseExternalSpecialistRequest` and `ReleaseSpecialistRequest` in
[`dolgorae.proto`](../protocol/dolgorae/public/v2/dolgorae.proto), carry a
required `expected_revision` integer, while current Rust implementations in
`EngagementStore::release_external_member`
([`src/engagement.rs`](../../src/engagement.rs)) and
`OrchestrationStore::release_specialist`
([`src/orchestration.rs`](../../src/orchestration.rs)) do not yet implement the
successor `S` release `expected_revision` fence. Prior `v0.1.4` specifications
did not define which revision domain `expected_revision` checks on member
release.

This specification defines that domain and the admission boundary as a new
normative clarification:

1. On `release_external_specialist` and `release_specialist`,
   `expected_revision` **MUST** check the authoritative membership revision
   counter of `(workspace_ref, scope_kind, scope_ref)` owned by Engagement
   database generation `6` (Section 2.1).
2. Durable release admission **MUST** occur before any settlement effect (before
   Run close or interruption, runtime cleanup, global Profile membership
   retirement, or terminal membership state transition) and **MUST** execute
   under the same owning serialization boundary as member task and group
   assignment (`assign_external_specialist_task`, `assign_specialist_task`,
   `assign_group`, and `assign_synthesis`).
3. When a genuinely new release request is admitted (no existing row in
   `member_release_operations` for `(workspace_ref, scope_kind, scope_ref, member_ref)`),
   the runtime **MUST** verify under that owning serialization boundary that
   `expected_revision` equals the current membership revision of
   `(workspace_ref, scope_kind, scope_ref)` and durably persist the
   `dolgorae.member-release-operation/v1` row with that checked value recorded
   as `admitted_membership_revision`. If `expected_revision` does not match and
   no prior release operation exists for that member, admission fails with no
   state effect.
4. Once release admission is durably recorded for `member_ref` (whether the
   member is releasing with `progress` in `"requested"`, `"settling"`,
   `"cleanup_failed"`, or `"recovery_needed"`, or already `"terminal"`), the
   runtime **MUST** fence and reject any additional task or group assignment to
   `member_ref`. This assignment fence protects quiescence from release
   admission through atomic terminal commit; all previously admitted work for
   `member_ref` **MUST** settle before the release operation can transition to
   `progress = "terminal"`.
5. Existing wire fields carry this `(workspace_ref, scope_kind, scope_ref)`
   membership revision domain as a new normative clarification without adding
   wire fields:
   - In [`dolgorae-external-specialist-facade-v5.schema.json`](../protocol/dolgorae-external-specialist-facade-v5.schema.json),
     `$defs/member.properties.revision` carries the observed scoped membership
     revision.
   - In [`dolgorae.proto`](../protocol/dolgorae/public/v2/dolgorae.proto),
     `DelegationIdentity.source_revision` (embedded in
     `MemberProjection.identity`, `ExternalDelegationIdentity.facts`, and
     `ManagedDelegationIdentity.facts`) carries the same scoped membership
     revision observation, whereas `OrchestratedSessionProjection.source_revision`
     and `ListOrchestratedSessionResultsResponse.source_revision` retain their
     aggregate orchestrated-session revision meaning.
   - In [`dolgorae-v014-identities-v1.schema.json`](../protocol/dolgorae-v014-identities-v1.schema.json),
     `$defs/disposition_proof.properties.membership_revision` records the scoped
     membership revision observed when constructing the disposition proof.
6. Revision domains **MUST** remain strictly separated and **MUST** be compared
   only within the same domain:
   - In the membership revision domain (`(workspace_ref, scope_kind, scope_ref)`),
     every released disposition proof **MUST** satisfy
     `admitted_membership_revision < terminal_membership_revision <= disposition_proof.membership_revision`,
     because the same scoped membership revision counter checked at release
     admission advances on atomic terminal commit (Section 5).
   - In the workload revision domain (per-member task, interaction, and
     execution quiescence), every released disposition proof **MUST** satisfy
     `settled_workload_revision <= disposition_proof.workload_revision`.
   - Run state revisions, global Profile membership index revisions
     (`dolgorae-profile-membership-v2.schema.json`), queue sequence numbers, and
     event stream cursors belong to separate domains and **MUST NOT** be
     substituted into `expected_revision`, `admitted_membership_revision`,
     `terminal_membership_revision`, or `settled_workload_revision`.
7. Once a member release reaches `progress = "terminal"`, its
   `terminal_membership_revision` and `settled_workload_revision` are frozen.
   Later activity by other members in the same engagement or orchestrated
   session may advance `disposition_proof.membership_revision` or aggregate
   revisions, but **MUST NOT** rewrite the released member's completion
   revisions or invalidate its `dolgorae.member-release-proof/v1`.

## 4. Initiating authority, Controller handoff, and active owner proof

`controller_principal` and `controller_generation` in
`dolgorae.member-release-operation/v1` and `dolgorae.member-release-proof/v1`
record the authenticated Controller that initiated the release operation. Once
persisted at initial admission, both fields **MUST** remain immutable across
retries, coordinator restarts, recovery, and later Controller handoffs.

`owner_ref` in `dolgorae.member-release-operation/v1` identifies the runtime
coordinator attempt currently driving settlement. `owner_ref` is mutable across
recovery attempts and **MUST NOT** appear in `dolgorae.member-release-proof/v1`.
An active release owner is established only when the coordinator holds the
scoped kernel lifecycle lock and a verified synced owner witness matching
`owner_ref`. A stored `progress` value of `"requested"` or `"settling"` or a
non-null `owner_ref` without the held lock and verified witness **MUST NOT** be
treated as active progress.

When an incomplete release operation has no live owner, a successor coordinator
or a validly handed-off Controller with current authority over
`(workspace_ref, scope_kind, scope_ref)` **MAY** acquire the lifecycle lock,
replace `owner_ref`, and complete settlement. Reassignment **MUST** preserve
the original `operation_ref`, `controller_principal`, `controller_generation`,
and `admitted_membership_revision`.

Subsequent calls to `collect_external_specialist_task`,
`collect_specialist_task`, `get_external_delegation_identity`,
`get_managed_delegation_identity`, or release recovery authenticate the caller's
current access to the scope and verify that
`release_receipt.controller_principal` and
`release_receipt.controller_generation` equal the retained initiating fields in
`member_release_operations`. Readers and validators **MUST NOT** require the
current Controller's principal or generation to equal the historical initiating
Controller after a valid Controller handoff, and **MUST NOT** relabel the
initiating Controller in the operation record or proof.

## 5. Settlement conditions, Run closure cleanup, and global Profile membership retirement

After durable release admission and assignment fencing (Section 3), a member
release operation (`release_external_specialist` or `release_specialist`)
**MUST NOT** transition to `progress = "terminal"` or produce
`dolgorae.member-release-proof/v1` until all of the following settlement
conditions hold in order:

1. All previously admitted tasks and assignments for `member_ref` are terminal,
   with `running == 0`, `queued == 0`, `claimed == 0`, `dispatching == 0`, and
   `unresolved == 0`, and additional assignments to `member_ref` remain fenced.
2. The bound Specialist Run (`run_ref`) has reached `RunLifecycle::Closed` and
   has verified settlement of that Run's exact owned runtime generation,
   including termination and cleanup of its Worker, any Run-owned Dedicated Lane
   Server generation and descendants, and all owned Turn and background
   execution activity, with `pending_interaction == false` and
   `cleanup_pending == false`. Its private connection to a shared Profile Server
   **MUST** settle, whereas the shared Profile Server itself is not Run-owned
   and **MUST NOT** be terminated, restarted, or replaced for member release.
   Any pending interaction, pending cleanup, or unverifiable owned execution
   state blocks completion. Interrupt acknowledgement, model-reported flags or
   booleans, or global Profile membership retirement alone **MUST NOT**
   substitute for `RunLifecycle::Closed` and verified settlement of the Run's
   exact owned runtime generation and execution cleanup. These Run closure and
   owned-cleanup checks **MUST** be verified before global Profile membership
   retirement and before atomic terminal workspace commit, including on retry or
   recovery when the Run projection is already `RunLifecycle::Closed`.
3. Any workspace writer authority or writer handoff associated with `run_ref`
   is settled.
4. For both external (`release_external_specialist`) and managed
   (`release_specialist`) Specialists, after conditions 1 through 3 hold, the
   member Run's entry `(workspace_id, run_id)` in the global Profile membership
   index ([`dolgorae-profile-membership-v2.schema.json`](../protocol/dolgorae-profile-membership-v2.schema.json))
   **MUST** be verified and retired through the Profile authority to
   `disposition = "released"`, including on retry or recovery after the member
   Run projection has already reached `RunLifecycle::Closed`. Inspection of
   [`src/semantic.rs`](../../src/semantic.rs) confirms that while external
   `finish_external_run_close` calls `release_global_run_membership`, the
   current managed adapter `release_specialist` returns `Ok(())` immediately
   when `snapshot.projection.lifecycle == RunLifecycle::Closed` and after
   `mutate_bound_run` without checking `closed_membership_released` or
   completing `release_global_run_membership`. Closing that managed runtime gap
   is assigned to `TASK-076`. If global Profile membership remains `"active"` or
   `"unknown"`, or if updating the Profile membership journal/index fails,
   terminal release proof emission is blocked: the operation **MUST** record
   `progress = "cleanup_failed"` or `progress = "recovery_needed"` with a
   non-null `safe_failure_code` and `required_action`.
5. Retiring a Specialist Run's global Profile membership entry **MUST NOT**
   stop, restart, or replace a shared Profile Server; the shared server
   continues serving other members.
6. In the workspace Engagement database, the member's scoped state transition
   to `"released"` (advancing the scoped membership revision counter of
   `(workspace_ref, scope_kind, scope_ref)`) and the `member_release_operations`
   row transition to `progress = "terminal"` (with non-null
   `terminal_membership_revision` and `settled_workload_revision`) **MUST**
   commit atomically in one SQLite transaction after Run closure/cleanup
   verification and global Profile membership retirement succeed.

## 6. Serialized post-result observation and release-before-collect

Each `$defs/external_delegation_identity` and
`$defs/managed_delegation_identity` is bound to a specific accepted task
(`task_ref`, `accepted_task_digest`) and its already durably committed terminal
result (`result_digest`, `complete_result`). `collect_external_specialist_task`,
`collect_specialist_task`, `get_external_delegation_identity`, and
`get_managed_delegation_identity` remain read-only (`R`) methods that do not
mutate durable state. After final collection of the already committed
`complete_result` and `result_digest`, the runtime **MUST** acquire a fresh
disposition observation under the owning scope's serialization boundary against
current membership and workload state and bind the exact `result_digest` so that
`disposition_proof.observed_after_result_digest` equals the enclosing identity's
`result_digest`. A cached pre-collection disposition snapshot is insufficient
and **MUST NOT** be reused to populate `disposition_proof`.

When a member completes a task (durably committing `complete_result` and
`result_digest`) and is subsequently released before the caller invokes
`collect_external_specialist_task`, `collect_specialist_task`,
`get_external_delegation_identity`, or `get_managed_delegation_identity`, the
fresh post-collection observation under the owning serialization boundary reads
the settled release state and **MUST** return `worker_disposition = "released"`,
`disposition_proof.state = "released"`, and the original
`dolgorae.member-release-proof/v1` backed by the original
`dolgorae.member-release-operation/v1` (preserving the original `operation_ref`,
`terminal_membership_revision`, and `settled_workload_revision`). Collecting or
reading identity after release **MUST NOT** allocate a new release operation or
rewrite the settled proof.

## 7. Repeat requests, response loss, and recovery

Because `release_external_specialist` and `release_specialist` are `S`
operations without an `idempotency_key`, durable convergence is anchored by the
retained `member_release_operations` record for
`(workspace_ref, scope_kind, scope_ref, member_ref)` created under the owning
serialization boundary (Section 3):

1. Every release call **MUST** first authenticate current Controller authority
   for the target scope and then inspect `member_release_operations` for
   `(workspace_ref, scope_kind, scope_ref, member_ref)` under the owning
   serialization boundary before evaluating `expected_revision` as a new
   admission fence or performing any settlement effect.
2. If an existing record has `progress == "terminal"`, the runtime **MUST**
   return the settled `released` member projection and the original
   `operation_ref` without mutating state, even when the repeat request supplies
   a stale `expected_revision`.
3. If an existing record is non-terminal and its `owner_ref` is backed by a
   currently held lifecycle lock and verified owner witness from another active
   coordinator attempt, the runtime **MUST** refuse the call with a typed
   busy/conflict error and no state mutation while keeping additional task
   assignments to `member_ref` fenced.
4. If an existing record has `progress` in `"requested"`, `"settling"`,
   `"cleanup_failed"`, or `"recovery_needed"` and no live owner holds the
   lifecycle lock and witness (including after client response loss or
   coordinator crash), the runtime **MUST** acquire the lifecycle lock, update
   `owner_ref`, preserve the original `operation_ref`, `controller_principal`,
   `controller_generation`, and `admitted_membership_revision`, keep additional
   assignments to `member_ref` fenced, and resume settlement from the durable
   state (including full Section 5 Run closure/cleanup and Profile retirement
   verification) regardless of whether the caller's `expected_revision` matches
   the post-admission revision.
5. A stale `expected_revision` **MUST NEVER** authorize a second release
   operation for an already releasing or released member.
6. Recovery and status inspection of `S` member release **MUST** use the scope
   and delegation identity surfaces (`get_external_engagement`,
   `get_orchestrated_session`, `get_external_delegation_identity`,
   `get_managed_delegation_identity`, and repeat `release_external_specialist` /
   `release_specialist` calls) and **MUST NOT** use
   `OperationService.LookupOperation` (`K`-only lookup).

## 8. Atomic generation `6` migration and legacy-reader limits

Inspection of `EngagementStore::open_with_faults` and
`EngagementStore::open_read_only` in [`src/engagement.rs`](../../src/engagement.rs)
and `OrchestrationStore::open_observer` in
[`src/orchestration.rs`](../../src/orchestration.rs) establishes the exact
behavior of current pre-`TASK-076` binaries when opening
`orchestration/orchestration.sqlite3`:

1. In existing binaries, when `path.exists()`,
   `EngagementStore::open_with_faults` first calls
   `storage_observation::CheckedConnection::open(path, false)`, which acquires
   `database_admission`, opens SQLite with `SQLITE_OPEN_READ_WRITE`, and
   executes `PRAGMA foreign_keys = ON` and `PRAGMA synchronous = FULL` before
   reading and comparing `metadata.schema_version` against `"5"`. When
   `metadata.schema_version` is `"6"`, execution does not return early at the
   `"5"` check; instead it falls through to directory creation and `0o700`
   permission setting, a second writable SQLite open, `0o600` permission setting
   on the database file, and a `BEGIN IMMEDIATE` batch executing
   `PRAGMA journal_mode = WAL`, `PRAGMA foreign_keys = ON`,
   `PRAGMA synchronous = FULL`, `CREATE TABLE IF NOT EXISTS ...`, and
   `INSERT OR IGNORE INTO metadata(key, value) VALUES('schema_version', '5')`
   before rejecting `"6"` with `ORCHESTRATION_SCHEMA_UNSUPPORTED`. Existing
   writable openers therefore do not safely refuse generation `6` without first
   opening the database read-write and mutating permissions, WAL state, and
   DDL.
2. In existing binaries, `EngagementStore::open_read_only` opens SQLite with
   `CheckedConnection::open(path, true)` (`SQLITE_OPEN_READ_ONLY`) and checks
   `metadata.schema_version` against `"1"` through `"5"`, rejecting `"6"` with
   `ORCHESTRATION_SCHEMA_UNSUPPORTED`. By contrast,
   `OrchestrationStore::open_observer` opens read-only via
   `CheckedConnection::open(&path, true)` but does not validate either
   `metadata.schema_version` or `metadata.orchestration_schema_version` today.
3. In `TASK-076`, the `5 -> 6` database migration **MUST** atomically create the
   new base tables (`member_release_operations` and the scoped
   `(workspace_ref, scope_kind, scope_ref)` membership revision counter state)
   and update `metadata.schema_version` from `"5"` to `"6"` within a single
   atomic SQLite transaction (keeping `metadata.orchestration_schema_version` at
   `"3"`). An interrupted migration **MUST** leave the previous valid generation
   `5` database completely untouched or complete generation `6` in full, never
   leaving half-updated metadata or partial schema state, and **MUST** preserve
   all existing records from generations `1` through `5` untouched.
4. Starting in `TASK-076`, all supported Engagement and Orchestration database
   openers (both writable and read-only, including
   `EngagementStore::open_with_faults`, `EngagementStore::open_read_only`,
   `OrchestrationStore::open`, and `OrchestrationStore::open_observer`) **MUST**
   validate `metadata.schema_version` and
   `metadata.orchestration_schema_version` on an existing database before
   performing any writable open, `chmod`, `WAL` pragma, or DDL statement, and
   **MUST** fail closed with `ORCHESTRATION_SCHEMA_UNSUPPORTED` and no
   filesystem or database write effects when encountering an unsupported
   generation.
5. Downgrading a workspace whose `orchestration/orchestration.sqlite3` has been
   migrated to generation `6` back to pre-`TASK-076` binaries is unsupported and
   **MUST NOT** be performed. `TASK-076` owns the atomic `5 -> 6` migration, the
   pre-effect version gate across all openers, and verification tests for
   interrupted migration rollback, historical-record preservation, and
   unsupported-reader refusal.

## 9. Package, compatibility, and verification boundaries

### 9.1 Controlled H1 erratum and preserved invariants

This correction is delivered as a controlled `H1` erratum under `EPIC-023`.
[`dolgorae-v014-identities-v1.schema.json`](../protocol/dolgorae-v014-identities-v1.schema.json)
retains `$id: "https://dolgorae.local/schema/v014-identities/v1"` and
`dolgorae.delegation-identity/v1`.
[`dolgorae-machine-v5.schema.json`](../protocol/dolgorae-machine-v5.schema.json)
and [`dolgorae-external-specialist-facade-v5.schema.json`](../protocol/dolgorae-external-specialist-facade-v5.schema.json)
retain their outer schema `$id` URIs, operation names, and request/response
envelope shapes.
[`dolgorae.proto`](../protocol/dolgorae/public/v2/dolgorae.proto),
`dolgorae-public-v2.descriptor.pb`, all 64 gRPC methods, mutation and error
mapping policies, and generated Go consumer stubs remain byte-for-byte
unchanged in semantic surface. All eight `v0.1.4` JCS SHA-256 digest domains
(`dolgorae.role-snapshot/v1`, `dolgorae.instruction-identity/v1`,
`dolgorae.admitted-configuration/v1`, `dolgorae.accepted-task/v1`,
`dolgorae.complete-result/v1`, `dolgorae.group-assignment/v1`,
`dolgorae.merged-findings/v1`, and `dolgorae.mutation-envelope/v1`) and their
checked vectors in [`dolgorae-v014-digest-vectors-v1.json`](../protocol/dolgorae-v014-digest-vectors-v1.json)
remain unchanged, as do `dolgorae.podway-external/v1`,
`dolgorae.podway-managed/v1`, `caller_scope`, and the `"idle"` / `"released"`
disposition vocabulary.

Replacing `disposition_proof.release_receipt`'s non-null target from
`dolgorae.operation-receipt/v1` to `dolgorae.member-release-proof/v1` is a
normative interface change even where referring `Machine v5` and `facade-v5`
envelopes require no outer field rename. Consumers select this change through
the corrected `H1` package lock and the `dolgorae.member-release-proof/v1`
schema discriminator.

The original `H1` baseline at commit
`55ba623fce231192226d29ba718153e18b6644a3` and all 11 selected external
consumer source pins in
[`dolgorae-v014-contract-coordination.md`](dolgorae-v014-contract-coordination.md)
remain preserved. Completed `TASK-068`, `TASK-069` (`Gate A`), and Gul
`E15`/`H2` remain valid only for that original
`55ba623fce231192226d29ba718153e18b6644a3` package and **MUST NOT** be cited as
qualifying the corrected `H1` package until `TASK-092` independently accepts
Gul's delta `H2` and records the `Gate A` addendum.

### 9.2 Separation of JSON Schema, Rust runtime, and consumer acceptance

| Layer | Owner | Enforced guarantees |
| --- | --- | --- |
| Checked JSON Schema | `TASK-091` (`dolgorae-v014-identities-v1.schema.json`, `dolgorae-successor-state-v1.schema.json`) | Closed object shapes, required fields, integer and UTF-8 bounds, constant quiescence counters/flags, and finite conditional branches (`idle`/`null` vs `released`/`dolgorae.member-release-proof/v1`, external vs managed `operation_kind`/`scope_kind` pairing, and `member_release_operation` progress branches). |
| Executable contract and runtime checks | `TASK-091` (offline validators/fixtures) and `TASK-076`/`TASK-077`/`TASK-086` (Rust runtime) | Dynamic cross-field and cross-record settlement invariants: durable admission before settlement under assignment serialization, assignment fencing for releasing/released members, fresh post-collection disposition observation of the already committed result (`observed_after_result_digest == result_digest`), `member_ref == worker_ref`, `admitted_membership_revision < terminal_membership_revision <= membership_revision`, `settled_workload_revision <= workload_revision`, `RunLifecycle::Closed` plus verified settlement of that Run's exact owned runtime generation (Worker, any Run-owned Dedicated Lane Server generation and descendants, owned Turn/background execution, and its private connection to a shared Profile Server, without terminating shared Profile Servers) before global Profile membership retirement to `disposition == "released"`, lifecycle lock plus witness ownership, and repeat/recovery convergence. |
| Consumer acceptance | `TASK-092` (Gul delta `H2` / `Gate A` addendum) and `TASK-077`/`TASK-087`/`TASK-088`/`TASK-089` (runtime qualification) | Independent consumer verification of the corrected `H1` package and subsequent end-to-end runtime qualification across Aquarium, Podway, and Gul. |

## 10. Handoff to `TASK-091` and runtime implementation Tasks

### 10.1 Exact `TASK-091` artifact change list

`TASK-091` **MUST** synchronize the following artifacts and regenerate the
reproducible `H1` package without implementing production release runtime or
modifying host behavior:

1. Owning specifications:
   - [`docs/specs/dolgorae-v014-method-and-digest-contract.md`](dolgorae-v014-method-and-digest-contract.md):
     update `release_external_specialist`, `release_specialist`, delegation
     identity disposition proof, and persisted-state references to use
     `dolgorae.member-release-proof/v1`, `dolgorae.member-release-operation/v1`,
     and `admitted_membership_revision`.
   - [`docs/specs/dolgorae-v014-successor-requirements.md`](dolgorae-v014-successor-requirements.md):
     synchronize `DELEG` disposition proof, Engagement store generation `5 -> 6`
     (with orchestration extension `3` unchanged), and `H1` erratum package
     rules.
   - [`docs/specs/dolgorae-v014-h1-package.md`](dolgorae-v014-h1-package.md) and
     [`docs/specs/dolgorae-consumer-acceptance-v014.md`](dolgorae-consumer-acceptance-v014.md):
     record the `EPIC-023`/`TASK-091` corrected `H1` package binding while
     preserving the historical `55ba623fce231192226d29ba718153e18b6644a3`
     reproduction reference and the 11 external consumer source pins in
     [`docs/specs/dolgorae-v014-contract-coordination.md`](dolgorae-v014-contract-coordination.md).
2. Checked protocol schemas:
   - [`docs/protocol/dolgorae-v014-identities-v1.schema.json`](../protocol/dolgorae-v014-identities-v1.schema.json):
     add `$defs/member_release_proof` (`dolgorae.member-release-proof/v1`),
     update `$defs/disposition_proof.properties.release_receipt` and its
     `allOf` branch to reference `$defs/member_release_proof`, and add finite
     `allOf` branches on `$defs/external_delegation_identity` and
     `$defs/managed_delegation_identity` binding `worker_disposition` to
     `disposition_proof.state` and binding released proofs to
     `"release_external_specialist"` + `"external_engagement"` (external) and
     `"release_specialist"` + `"orchestrated_session"` (managed).
   - [`docs/protocol/dolgorae-successor-state-v1.schema.json`](../protocol/dolgorae-successor-state-v1.schema.json):
     add `$defs/member_release_operation` (`dolgorae.member-release-operation/v1`)
     and include it in the top-level `oneOf` selection; update
     `x-generationSelection` to record workspace Engagement store generation
     `"5 -> 6"` and orchestration extension generation `"3 unchanged"`.
3. Fixture strategy and pre-existing fixture defect disposition:
   - Keep [`docs/protocol/examples/successor-facade-v5-methods.valid.json`](../protocol/examples/successor-facade-v5-methods.valid.json)
     at its exact 28-operation `$defs/contract_fixture_suite` structure
     (`minItems: 28`, `maxItems: 28` in
     [`dolgorae-external-specialist-facade-v5.schema.json`](../protocol/dolgorae-external-specialist-facade-v5.schema.json)).
   - Add a separate bounded release-proof fixture inventory under
     `docs/protocol/examples/` (including positive and negative `v014-identity-`,
     `v014-state-`, `successor-facade-v5-`, and `successor-machine-v5-` examples)
     covering full external and managed `idle` and `released` delegation
     identities, their `Machine v5` and `facade-v5` wrappers,
     `member_release_operation` states, and every refusal branch in Section 1.2.
   - Inspection of
     [`docs/protocol/examples/successor-facade-v5-methods.valid.json`](../protocol/examples/successor-facade-v5-methods.valid.json)
     confirms that the managed `spawn_specialist` and `assign_specialist_task`
     example receipts currently specify
     `namespace.scope_kind: "external_engagement"` and
     `namespace.scope_ref: "engagement:fixture"` even though their requests use
     `scope_kind: "orchestrated_session"` and `scope_ref: "session:fixture"`,
     and `validate_facade_method_suite_semantics` in
     [`tools/validators/validate_schema_examples.py`](../../tools/validators/validate_schema_examples.py)
     checks only `operation_kind` and `idempotency_key`. `TASK-091` **MUST**
     repair those two managed example receipt namespaces in
     `successor-facade-v5-methods.valid.json` and extend
     `validate_schema_examples.py` to enforce request/receipt `scope_kind` and
     `scope_ref` equality. No new digest algorithm or digest domain is
     introduced, `dolgorae.operation-receipt/v1` and `K` receipt semantics
     remain unchanged, `TASK-091` respects the existing closed receipt contract
     and qualified generator/validator where applicable, and no fixture files
     are modified during `TASK-090`.
4. Acceptance fixtures and executable validators:
   - [`docs/protocol/dolgorae-v014-acceptance-fixtures-v1.json`](../protocol/dolgorae-v014-acceptance-fixtures-v1.json)
     (`"disposition"` contract cases) and `validate_disposition_case` in
     [`tests/successor_contract.rs`](../../tests/successor_contract.rs): extend
     the `"disposition"` contract cases beyond synthetic
     `state`/`fresh_after_result`/`outstanding` flags to check
     `dolgorae.member-release-proof/v1` versus `null`, external versus managed
     `operation_kind`/`scope_kind` pairing, `K`-receipt substitution refusal,
     non-zero quiescence counter/flag refusal,
     `observed_after_result_digest == result_digest`, within-domain revision
     ordering (including an explicit equality-refusal fixture for
     `admitted_membership_revision == terminal_membership_revision`), and
     global Profile membership retirement.
   - [`tools/validators/validate_schema_examples.py`](../../tools/validators/validate_schema_examples.py):
     extend `schema_for_example` routing, negative-example repair rules, and
     semantic checks for the new release-proof and `member_release_operation`
     fixtures.
   - [`tools/validators/generate_h1_lock.py`](../../tools/validators/generate_h1_lock.py),
     [`tools/validators/validate_h1_contract.py`](../../tools/validators/validate_h1_contract.py),
     [`tools/validators/package_h1.py`](../../tools/validators/package_h1.py),
     and [`docs/protocol/dolgorae-gul-consumer-v2.lock.json`](../protocol/dolgorae-gul-consumer-v2.lock.json):
     include `docs/specs/dolgorae-v014-release-proof-correction.md` and the new
     fixture files in the `H1` inventory, record `EPIC-023`/`TASK-091` erratum
     producer metadata while preserving the original `H1`
     `55ba623fce231192226d29ba718153e18b6644a3` baseline reference and the 11
     external consumer source pins, and regenerate the lock.

### 10.2 Runtime responsibilities assigned to `TASK-076`, `TASK-077`, and `TASK-086`

| Roadmap Task | Assigned runtime and verification scope |
| --- | --- |
| `TASK-076` | Implement atomic Engagement database migration `5 -> 6` (`member_release_operations` table, durable `(workspace_ref, scope_kind, scope_ref)` membership revision counter state, and `metadata.schema_version = "6"`, keeping `metadata.orchestration_schema_version = "3"` unchanged); enforce pre-effect `schema_version` and `orchestration_schema_version` validation across all supported openers (`EngagementStore::open_with_faults`, `EngagementStore::open_read_only`, `OrchestrationStore::open`, and `OrchestrationStore::open_observer`) before any writable open, `chmod`, `WAL` pragma, or DDL; synchronize the preserved uncommitted `src/delegation_identity.rs` (`DispositionProof` and `MemberRecord`) with `dolgorae.member-release-proof/v1` and `dolgorae.member-release-operation/v1` after `TASK-092` (without editing `src/delegation_identity.rs` during `EPIC-023`); implement durable release admission before any settlement effect under the same owning serialization boundary as member task assignment, fencing additional assignments to releasing/released members; require `RunLifecycle::Closed` and verified settlement of that Run's exact owned runtime generation (termination and cleanup of its Worker, any Run-owned Dedicated Lane Server generation and descendants, owned Turn and background execution activity, and its private connection to a shared Profile Server) before global Profile membership retirement (`disposition = "released"`) and atomic terminal workspace commit on both `release_external_specialist` and `release_specialist` (closing the `src/semantic.rs` `release_specialist` post-`Closed` early-return gap via `closed_membership_released` / `release_global_run_membership` on initial release, retry, and recovery) without terminating, restarting, or replacing shared Profile Servers; enforce fresh post-collection disposition observation of the already committed result under owning serialization (`observed_after_result_digest == result_digest`), Controller handoff preservation of initiating authority, repeat/recovery convergence, and tests for interrupted migration rollback, historical-record preservation, and unsupported-reader refusal. |
| `TASK-077` | Project `dolgorae.member-release-proof/v1` and `idle`/`released` delegation identities through the trusted External Specialist MCP facade and `dolgorae.podway-external/v1` / `dolgorae.podway-managed/v1` exports, including repeat release, response-loss recovery, and release-before-collect verification. |
| `TASK-086` | Verify that retention, passivation, pool management, and maintenance operations preserve `member_release_operations` records and `dolgorae.member-release-proof/v1` evidence without deleting or rewriting settled release proofs or stopping shared Profile Servers during single-member release. |
