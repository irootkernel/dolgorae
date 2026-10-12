# Durable Submit and operation lookup

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

The native `run lookup-operation <run-id> --idempotency-key <key>` diagnostic
uses the ordinary protected Controller carrier and returns H1's
operation-lookup shape within the Machine v5 lookup response. Its typed
failure classifies the current lookup; it never resolves an earlier unknown
mutation. It supports Submit in that Run. Full public-v2 projection remains
TASK-078's responsibility.

A Controller binding that advanced past an uncommitted reset token returns
`RecoveryNeeded` with `restore_state`. Because Controller authority has not
been established, that response omits subjects and operation identity.
