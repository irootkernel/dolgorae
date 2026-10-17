# Delegation release proof correction

Status: supplementary planning dossier for EPIC-023 (`EPIC-023` `ACTIVE`;
`TASK-090` `COMPLETE`; `TASK-091` and `TASK-092` `PLANNED`). Adopted normative
behavior is owned by the
[release-proof contract correction specification](../specs/dolgorae-v014-release-proof-correction.md),
and accepted decision rationale is recorded in
[ADR-042](../architecture-decision-records/README.md#adr-042-replace-keyed-release-receipts-with-state-based-member-release-proofs).
The [roadmap](../roadmap/README.md#epic-023-release-proof-contract-correction-and-consumer-reacceptance)
owns registration, status and the active-slot handoff. This supplements the
[v0.1.4 alignment dossier](TODO-DOLGORAE-V014-RELEASE-ALIGNMENT.md).
`TASK-090` contract adoption does not amend H1 bytes or accept a consumer
package. EPIC-020 and TASK-076 remain parked at `PLANNED` with preserved
partial uncommitted work until TASK-092 completes.

## Recommendation

Keep member release as an S lifecycle operation. Give settled release its own
small completion proof, backed by the original durable operation. Keep keyed
acceptance receipts unchanged. Deliver the correction as an explicitly selected
new H1 package with affected consumer verification.

Public-v2 RPC names, requests and response fields, Machine/facade-v5 outer
envelopes, the Podway acceptance input and its `idle`/`released` vocabulary can
remain. The nested JSON `disposition_proof.release_receipt` type changes: this
is an interface change even where its referring envelope needs no byte edit.
Package selection and the new proof's schema identify that change.

## Defect and limits of the existing checks

The [identity schema](../protocol/dolgorae-v014-identities-v1.schema.json)
requires an `operation_receipt` for released disposition. That type admits only
14 keyed acceptance operations. Both successor release methods are S operations
without an idempotency key, as defined by the
[method contract](../specs/dolgorae-v014-method-and-digest-contract.md).
A real release cannot produce that K receipt. A hire or assignment receipt may
fit the old schema but cannot establish release.

The disposition handler in `tests/successor_contract.rs` checks synthetic
`state`, `fresh_after_result` and `outstanding` fields. Full checked external and
managed delegation examples exercise idle with a null receipt. Those checks
miss the invalid release contract.

Current external release in `src/engagement.rs::release_external_member` still
uses the older keyed contract. It is not an implementation of successor S
release. Managed `src/orchestration.rs::release_specialist` has a state-based
release path, but does not retain the proposed member release operation and
completion proof. This proposal describes work still to implement.

## Completion proof and durable owner

Keep the field name `release_receipt`, with the following closed type. All
listed fields are required; existing opaque-reference, UUIDv7 and safe-integer
bounds apply. This is an S completion fact, with no key, accepted-input digest,
exact-replay flag or newly invented digest domain.

| Field | Meaning |
| --- | --- |
| `schema` | `dolgorae.member-release-proof/v1` |
| `operation_ref` | Original server-allocated release operation UUIDv7 |
| `operation_kind` | Exactly `release_external_specialist` or `release_specialist` |
| `workspace_ref`, `scope_kind`, `scope_ref` | Original authorized workspace and external Engagement or managed Session |
| `member_ref`, `run_ref` | Exact released membership and its bound Run |
| `controller_principal`, `controller_generation` | Non-secret Controller authority that initiated this release, retained across recovery |
| `terminal_membership_revision` | Committed revision that records this member's settled release |
| `settled_workload_revision` | Corresponding durable workload revision at settlement |

For these adapter mappings, `worker_ref` identifies the accepted member;
`run_ref` remains its separate Run binding. Validate this relationship through
the authoritative membership record. Similar-looking identifiers alone grant
no authority. A scoped member identity is never rebound to another Run or reused
after terminal release. These identities identify the membership incarnation;
no ambiguous public `run_generation` field is added. Exact runtime-generation
and process checks remain with the existing lifecycle owner. The Controller
principal uses the existing non-secret principal type, never a credential or a
credential-derived value. The Podway `caller_scope` binds its
workspace/session/attempt and placement; it is not the release's Controller
authority namespace.

Retain a `dolgorae.member-release-operation/v1` record in a new
`member_release_operations` table in the existing workspace Engagement database,
which the Broker already opens through `EngagementStore`. Both release owners
use this bounded shared repository. Propose base database generation 6 from the
current generation 5; the orchestration extension stays at 3 because its tables
need no shape change. Recheck generation allocation before implementation.
The supported new reader migrates the old database transactionally, preserving
old records and their meanings. Older binaries must refuse generation 6 before
effects, and downgrade is unsupported. Test interrupted migration and refusal;
never backfill invented S proofs for historical keyed releases.

The closed new record contains the immutable operation identity and subject/
initiating-authority bindings above, the admitted revision, a separately mutable
`owner_ref`, progress and the last safe failure/required recovery action.
Progress is `requested`, `settling`, `terminal`, `cleanup_failed` or
`recovery_needed`. A held lifecycle lock and its verified witness establish the
active owner; a stored owner reference alone does not. Reassignment of recovery
ownership preserves initiating authority. Completion revisions are required
fields with explicit null until terminal settlement, then immutable integers.
Add this individually versioned record to successor-state's checked selection
and the new H1 package impact list; it is not accepted by the old closed union.

Admit one release operation for the exact scoped membership before effects;
fence further assignment and recover that operation through completion. For a
repeat, authenticate current access first and inspect the original record before
attempting new admission. A terminal record returns its original operation
reference and member state. Another active owner yields a typed busy/conflict
result with no new effect. An abandoned unfinished operation requires recovery;
a stale expected revision never authorizes a second release. Preserve the
existing revision checks for genuinely new admissions.

Reuse the existing lifecycle lock, owner witness, revision fences, transactional
commit and recovery patterns. This needs bounded member-release state, not a new
generic operation service. The existing closed
[`dolgorae.lifecycle-operation/v1`](../protocol/dolgorae-successor-state-v1.schema.json)
has Run-oriented fields and lacks these member/scope/authority bindings. Do not
store a different shape under its identifier. New records get their own schema;
any changed existing persisted record gets a separate generation with its old
reader preserved.

The proof is projected only from a committed successful release. Settlement
requires terminal membership, no owned running, queued, claimed, dispatching or
unresolved work, no blocking interaction or cleanup, and settled writer and Run
activity. The closed Run's global Profile membership must also be retired
through the Profile authority before either external or managed release reports
completion. Unknown retirement or cleanup blocks the proof. Apply these checks
to the owned workload. A shared Profile server can continue serving other
members; release does not require shutting it down.
An interrupt acknowledgement, model assertion or a set of caller-supplied
booleans cannot establish these facts.

## Observation, response loss and recovery

After the final complete result is collected, the service acquires disposition
under its owning serialization and binds
`observed_after_result_digest == result_digest` to that exact task and result.
It checks current workload and membership before serializing the identity.
Release can have completed before collection; collection does not create a new
release. Preserve the original completion fact and operation reference.

The existing disposition counters remain zero and interaction/cleanup flags
remain false. Idle requires a null release proof and current accepted membership.
Released requires the non-null completion proof and terminal membership. Reject
mismatches between outer disposition, proof state, subject and result.

JSON Schema enforces closed shapes, constants and finite conditional branches:
idle versus released, and external versus managed release kinds. Rust enforces
dynamic subject/authority bindings, digest equality, revision ordering and
serialized observation against durable facts. Static schema/fixture checks
cannot establish those runtime facts. Keep product semantics in Rust; the H1
validator checks schema conformance and the selected fixture inventory.

Compare revisions within their declared owning domain:
`terminal_membership_revision <= membership_revision` and
`settled_workload_revision <= workload_revision`. Other members may advance an
aggregate revision after release. Such activity does not rewrite the original
completion or invalidate this member's terminal fact. A stale snapshot acquired
before final result collection is insufficient. An idle snapshot is not a
promise that no future work will ever be admitted.

On response loss, read the same member and retained lifecycle through supported
authorized reads, including the existing delegation identity getters. S recovery
does not use the K operation-lookup namespace or fabricate a key. Resume a
retained operation only after its former active owner has stopped. Unknown
effects, failed cleanup, missing/corrupt records and unverifiable activity remain
unresolved and block released export. Do not blindly issue a fresh release.

A terminal reread returns the original release operation. The facade response's
`operation_ref` names that durable operation; a transport/request correlation ID
does not substitute for it. An authorized Controller successor may read or
recover the retained operation through existing handoff rules. Keep initiating
authority unchanged and verify handoff continuity; do not require the historical
principal/generation to equal every later reader or silently relabel it.

The adapter uses authenticated service facts, not arbitrary model-supplied proof
JSON. It verifies complete result, scope, rounds and fresh disposition, then
exports the existing Podway fields. Primary/Judge semantic acceptance, Podway's
structural acceptance/progression and user acceptance remain separate.

## Version and package boundaries

Use a controlled H1 erratum with a new package commit and lock checksum. Retain
the existing identity schema URI and outer Machine/facade-v5 generations for
this package-bound correction, and give the new completion and retained release
records the distinct schema identifiers above. Preserve all eight digest domains
and their accepted bytes, K receipts, mapping IDs and Podway fields. Consumers
must select the exact qualified package; a schema/version string alone does not
establish compatibility. The changed JSON proof is not transparent to an old
reader.

This choice corrects an unimplementable release branch without introducing
parallel facade or public API families. If an actual accepted reader or retained
record requires the old nested shape in operation, stop and schedule an explicit
versioned transition before implementation. Do not coerce its data into the new
proof. Ordinary accepted historical records retain their bytes and meaning.

The [H1 reproduction rules](../specs/dolgorae-v014-h1-package.md) already require
a different package, impact assessment and affected verification for changed
locked artifacts. Preserve reproduction of original H1
`55ba623fce231192226d29ba718153e18b6644a3` with its original lock and historical
consumer selection, including its exact external source pins; do not repin them
to the currently inspected checkouts. H3 separately binds actual qualified
consumer/adapter revisions. The old H2 and
[Gate A acceptance](../specs/dolgorae-v014-gate-a-acceptance.md) remain valid for
that package; they do not qualify this corrected package.

Before a corrected H3 selection is handed onward, obtain Gul's explicit package
selection, affected foundation checks and a delta H2 record, then independently
accept the corresponding Gate A delta. Preserve the historical E15/Task closeout
instead of rewriting it as approval of new bytes. Whole-product runtime
qualification remains at the
[existing consumer gates](../specs/dolgorae-consumer-acceptance-v014.md).

EPIC-023 owns the contract correction. TASK-090 defines the corrected contract;
TASK-091 synchronizes its selected artifacts and produces the qualified H1;
TASK-092 independently accepts Gul's completed delta H2 and records the Gate A
addendum. Do not reopen completed TASK-068/069 or E15. TASK-087 later consumes
the accepted selection when producing H3.

Resume EPIC-020 only after TASK-092/EPIC-023 is complete. Gul's correction work
is a separately registered, owner-authorized unit before E16 entry, not an early
E16 Task. TASK-091 ends before waiting for it; TASK-092 starts only when its
completed delta H2 is available. No cross-repository active Task overlap or
postponement of Gate A to TASK-087 is introduced.

## Consumer impact

The investigation compared current source with the original H1-selected external
sources. These are source conclusions, not installed-runtime qualification.

| Consumer | Confirmed boundary | Required response to this correction |
| --- | --- | --- |
| Aquarium | `plugins/aquarium/references/native-agent-mappings.md` exports disposition; `dolgorae-review-contract.md` declares its Dolgorae review backend dormant. General orchestration now has a separate ordinary v2 path. | No current parser or ordinary-workflow interface change identified. Dolgorae's formal executor/adapter must acquire and validate the corrected proof. Requalify applicable AQ cases with the selected host and adapter; do not activate a review backend as part of this fix. |
| Podway | `crates/podway-store/src/v2_delegation.rs` and delegation schemas consume closed host-neutral input with `worker_disposition`; they do not consume this proof. | Keep acceptance/receipt schemas and `release/dolgorae-adapter-contract-v3.json`. Dolgorae owns proof validation before projection. Requalify retain/release, loss and stale-acceptance cases. Do not add the proof to Podway's closed input. |
| Gul | `contract/h1/pin.json` and `contract/scripts/h1.mjs` bind and verify the full H1 package. The generated public-v2 `DelegationIdentity` carries disposition, not nested JSON proof. | A selected corrected package needs an owner-authorized mirror/pin/check update and affected contract/foundation verification. No RPC, generated wire shape or UI change is required by this JSON correction. Actual release projections still need their existing runtime qualification. |

Current inspected source revisions were Aquarium `c0e2b25`, Podway `25be7dd`,
Gul `0064099` and Dolgorae `923142a`, including their existing uncommitted work.
Historical external selections were Aquarium `1e1e929`, Podway `4101a01` and
Gul `04ef1e5`. New implementation must recheck affected source applicability.
Consumer maintainers own changes and acceptance in their repositories. Advice
from this design review does not authorize cross-repository edits or substitute
for that acceptance.

## Alternatives considered

| Option | Assessment |
| --- | --- |
| S completion proof and qualified H1 erratum | Recommended. Keeps lifecycle semantics and consumer outer interfaces while making genuine release representable. |
| Parallel identity/facade/public API generations | Reserve for a demonstrated incompatible active reader. Current findings do not justify a full parallel API family. |
| Convert release to K or weaken generic K receipts | Adds or changes keys, replay and acceptance semantics; accepting a request would still not prove settled release. Keep the K/S boundary. |
| Idle-only, nullable released proof or interrupt acknowledgement | Fails mandatory settled-release behavior and cannot complete TASK-076/077. |

## Sequential delivery and verification

| Task | Deliverable and completion boundary |
| --- | --- |
| TASK-090 | Canonical correction specification and accepted design decision, including closed proof/state definitions, compatibility and migration contract. Keep the original selected H1 artifacts unchanged so its package checks remain valid. |
| TASK-091 | Update selected owning documents and derived schemas, record selection, fixtures, validators and lock together. Reproduce the new committed package, preserve the original package, compare all changed artifacts and deliver exact consumer inputs. No runtime implementation is required to construct contract fixtures. |
| TASK-092 | Independently consume Gul's completed package-adoption delta H2, verify exact candidate and check evidence, and record Gate A delta acceptance. Include the later foundation changes beyond the originally accepted baseline. |

TASK-091's contract fixture coverage includes both external and managed
idle/null and released/non-null positives; released/null, idle/non-null,
K-receipt substitution, malformed proof and schema-level wrong scope/kind
refusals; unchanged K receipts; and complete Machine/facade response validation.
Use the selected qualified JSON Schema dependencies and bounded fixture
inventory. Schema validity is not proof of runtime settlement.

Run applicable documentation, generator, H1/schema/fixture and repository gates
for each Task's actual scope. Deliver a committed package only after its approved
local commit and reproduction checks. Gul compares exact locked/generated
inventories and runs its contract-only Python fixture check plus affected
foundation checks through its normal pipeline. No mutable producer checkout
substitutes for the selected package. Unexpected wire, policy, digest or host
changes require impact assessment before adoption.

Runtime implementation stays outside EPIC-023. TASK-076 implements shared Rust
validation/projection, retained member-release state, database migration and
both durable owners. TASK-077 supplies MCP/Podway transport and supported paths;
TASK-086 owns integrated qualification. Their runtime checks include
release-before-collect, response loss/restart, a stopped owner and authorized
handoff, concurrent assignment/release, unrelated aggregate revision changes,
wrong subject/kind/generation, K-receipt substitution, null proof for released,
stale/pre-result observation, corrupt records, pending interactions, failed
cleanup, unknown Profile retirement, writer uncertainty and unresolved dispatch.
Include migration preservation/refusal and repeated-release owner conflicts.
Verify old K receipt and accepted-generation behavior still holds.

Runtime/host/account operations retain separate explicit authorization.
Contract completion does not establish runtime success, consumer product
acceptance, publication or completion of TASK-076/077. Preserve the existing
uncommitted instruction/configuration/startup work and its tests.

## Fresh-session entry

Begin with EPIC-023/TASK-090, not resumed TASK-076. Re-observe HEAD, index and
working changes, this dossier, the canonical roadmap and Gul's current plan.
The existing TASK-076 implementation is incomplete and uncommitted; this planning
commit intentionally excludes its source, tests, runtime document and activation
bookkeeping. Do not reset, clean, stash, stage or commit that work implicitly.

The working roadmap may still carry EPIC-020/TASK-076 `ACTIVE` from that work.
Before activating EPIC-023/TASK-090, complete the roadmap's explicit entry
handoff: stop former writing activity, preserve partial work and park those
unfinished items at `PLANNED`. Keep that record distinct from completion, then
activate only the correction Epic and its first Task. Do not use `BLOCKED` as a
way to bypass the occupied slot.

Continue the direct Herdr collaboration workflow with supported native
permissions, one active Writer and independent review. Do not invoke
epic-handler or make Podway a prerequisite. Verify live caller context and
agent capabilities afresh; helper agents use separate tabs. Obtain the new
session's bounded execution envelope before implementation. This planning
commit starts no Task or runtime and authorizes no push, publication or
cross-repository write. No broker transmission is implied.

## Design review decisions

Independent Claude Opus and AGY Gemini 4 Argon groups used native subagents to
inspect all three consumers, then cross-reviewed the proposal. They converged
on the package-bound correction and existing consumer outer interfaces. The
discussion rejected immediate reuse of the closed Run lifecycle record,
principal equality across Controller handoff, equality with a later aggregate
revision and a four-field proof without explicit subject binding. Review added
Profile retirement, repeated-request behavior and explicit storage/migration
ownership. The later planning decision separates contract correction into
EPIC-023 and requires delta Gate A before EPIC-020 resumes, matching Gul's
2026-10-16 plan. Implementation and package acceptance remain pending.
