# Durable Submit, startup reconciliation and operation lookup

This specification owns TASK-070's implementation of the frozen
[method and digest contract](dolgorae-v014-method-and-digest-contract.md).
It preserves the published H1 artifacts and accepted legacy records. Public-v2
projection and capability activation remain TASK-078's obligations.

Before Submit dispatch, the worker durably retains the original operation,
Controller-scoped namespace and mutation-envelope identity. Native Controller
principals use the SHA-256 of the JCS public identity excluding generation.
Generation is a separate namespace field. The public identity includes the
Controller ID, kind, instance and subject; credentials and capability bytes
never enter the digest.
Normalized Submit inputs include the original Run, requested write intent,
message bytes, ordered captured images, admitted model and effort. Expected
revisions and transport identifiers are excluded.

A correlated definitive rejection resolves only its matching reserved
operation. The worker syncs this resolution before releasing the reservation.
Retry may use the exact original key and input; changed input conflicts even
after restart. A startup persistence barrier failure after creation of a
provisional caller Thread remains unresolved. Transport loss, malformed
acceptance and failed journal writes never grant redispatch authority.

Before this durable resolution existed, a definitive refusal could leave a
chain of reservations followed by an exact-key retry and its accepted Turn.
Restoration preserves the whole chain only when every retry has the same digest
and worker identity, and a later retained acceptance has a prior Thread binding.
No operation in that chain may carry successor identity. A chain without a
final acceptance remains fenced, as does any reservation after acceptance;
this compatibility read never grants redispatch or rewrites accepted history.
An older acceptance without effort keeps an unknown result until retained
terminal evidence supplies the original receipt; restoration never guesses it.

Lookup authenticates the current Controller against the Run binding and reads
verified retained ledger records. It neither starts a worker nor captures
attachments. The original TurnStarted record supplies the immutable receipt
revision and chain hash; its prefix binds the original operation, envelope and
accepted Controller. Later terminal records update only current outcome.
The receipt digest is the prefixed SHA-256 chain hash of that original record.
An accepted Turn without terminal proof reports unknown when its current worker
generation cannot be verified. Run-level uncertainty cannot erase its receipt,
and unrelated later uncertainty cannot change an already terminal outcome.

Lookup distinguishes accepted, definitively not accepted, unresolved and
authoritatively absent. Absence requires a complete verified Run history and
retained namespace, with no matching operation or tombstone. Missing state,
corruption, incomplete history and legacy operations without successor identity
must produce a typed recovery or compatibility refusal rather than absence.
An unpublished ledger tail, including a synced reservation whose projection
publication failed, makes lookup inconclusive and requires recovery. Ordinary
event observation may still read its published prefix without classifying that
tail; it does not establish operation absence.
History is not retired automatically. Controller replacement cannot expose
another principal's operation or reuse its receipt.
When the same key exists outside the requested Controller namespace, lookup
refuses it without classifying a modern operation as a legacy receipt.

After confirmed terminal Run deletion, the original Start allocation remains
durable. When the accepted public Start outcome was verified before deletion,
its original authenticated receipt and final Start outcome remain readable.
The tombstone does not expose or reconstruct deleted Turn history. Lookup of
a deleted Run-scoped operation cannot be treated as proof of non-acceptance.

An Operator force deletion of an eligible Primary records session abandonment
and preserves the original Start receipt and bootstrap outcome. The session
has state `aborted`. The abandonment prevents Primary startup and new Turns
while resource cleanup is pending.

The native `run lookup-operation <run-id> --idempotency-key <key>` diagnostic
uses the ordinary protected Controller carrier and returns H1's
operation-lookup shape within the Machine v5 lookup response. Its typed
failure classifies the current lookup; it never resolves an earlier unknown
mutation. It supports Submit in that Run. Full public-v2 projection remains
TASK-078's responsibility.

## Pre-allocation Start reconciliation

`OperationService.ReconcileStartOperation` authenticates the original
Controller and exact `start_run` OperationRef, including workspace, generation
and the fixed `run-start` provisioning scope. It is an explicit state-convergent
mutation, not a lookup or a new Start. It never starts a Profile, Run, worker,
Thread or Turn, rereads provisioning inputs, or reconstructs an input digest.

Reconciliation holds the permanent allocation lock used by every Start under
the original key. An existing accepted reservation is never replaced: its
ordinary lookup state and known Run are returned. A corrupt or incompatible
record fails closed. Before closing an unreserved key, the producer validates
the complete retained start/fork allocation inventory and verifies that every
published Run is represented by that inventory. Missing, unreadable or
inconsistent retained evidence prevents closure. It also observes the retained
orchestration database without setup or migration. An aggregate bootstrap or
policy-session acceptance under the original workspace/key prevents closure,
even if its allocation record or Run is missing. An inconsistent delegation
namespace likewise requires recovery. A valid base database with no orchestration
extension marker and no extension tables is an uninitialized extension, not a
missing acceptance index. Reconciliation may observe that state without creating
or migrating the extension. A partial extension remains a recovery condition.

Only after those checks may the producer atomically create and sync a
Controller-scoped `dolgorae.start-operation-closure/v1` record in the original
allocation slot. Publication must be create-only. The record and its namespace
remain immutable for the workspace lifetime. Its receipt digest binds the
original namespace and a provider-generated UUIDv7 closure identity. The slot
also fences delayed or queued Starts: current readers reject the closed key,
and older readers cannot reinterpret its closed shape as an allocation.
Closure cannot erase an existing acceptance or a namespace tombstone.
The retained workspace record must match the canonical workspace identity and
physical state/lock directory identities. A separate matching completion witness
is published only after the allocation slot and its ancestor directories are
synced. Lookup returns unresolved while that witness is missing; only explicit
reconciliation may complete an interrupted publication, retaining its original
closure identity. A witness with a missing or inconsistent allocation slot
requires recovery and never authorizes admission.

Reconciliation and subsequent `LookupOperation` return
`closed_without_acceptance` with the original operation, immutable closure
receipt digest, durable watermark and `original_key_permanently_closed=true`.
This is a terminal reconciliation result for that key, distinct from
`not_accepted` (which requires the original input identity) and `absent`
(which permits original-key admission). It records no fabricated input digest,
Run or historical refusal cause. Repeating reconciliation returns the same
closure. Another Controller, generation, workspace or key cannot consume it.
The original key can never be admitted again. A separate new request still
requires normal user authorization and admission; closure does not execute it.

The current public-v2 contract adds this method and result without changing
the immutable historical H1 selection or public-v1. Consumers must negotiate
the current descriptor and method before use. Older consumers that cannot
decode the new result must fail closed rather than treat it as absence.

A Controller binding that advanced past an uncommitted reset token returns
`RecoveryNeeded` with `restore_state`. Because Controller authority has not
been established, that response omits subjects and operation identity.
