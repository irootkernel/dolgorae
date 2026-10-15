# v0.1.4 Ledger Capacity and Maintenance Runtime

This document owns TASK-072's native implementation of the frozen successor
storage requirements. Public-v2 and facade activation remain TASK-078 work;
actual consumer qualification retains its assigned owners. H1 and Gul E15
establish contract/foundation availability only. Gul E16 remains Planned until
Gate A and all Dolgorae Phase 3/H3 prerequisites are satisfied.

## Admission and retained finalization

A selected append generation admits ordinary records below its 512 MiB and
1,000,000-record ceilings minus a fixed finalization reserve. The reserve holds
32 maximum-size represented audit lines and 256 records. Each pending
interaction reserves two additional maximum-size lines and records for its
decision and resolution. A new interaction without an adjacent, matching
accepted approval intent uses ordinary admission; only an already admitted
interaction may use protected opening/resolution capacity. Repeated policy-denied
requests cannot consume the terminal reserve.

Submit/Send admission reserves its remaining pre-effect records as one group.
A prepared `MutationAdmitted` reserves three following record slots for read
admission, or four for writer preparation. A direct `IdempotencyReserved`
reserves two. These cover the input reservation, successor identity and
applicable caller-native intent or writer-policy update. The records share
the existing byte allowance of two maximum-size audit lines. Each matching
continuation consumes its actual slot and bytes; unrelated records cannot use
the remainder. Acceptance or definitive resolution releases the reservation,
and ledger reopen reconstructs it from durable records.

A Run admits only one active Turn. Other Turn admission, new mutations and
ordinary streaming cannot consume the fixed finalization reserve.
Terminal response/usage, terminal and unknown outcomes,
Close, writer release, cleanup and authoritative recovery may consume it.
These writes still validate record shape, chain and projection and remain
inside the absolute generation ceilings. Exhaustion is
`STORAGE_CAPACITY_EXCEEDED`, independently of audit integrity failure. Capacity
refusal precedes external dispatch; it never means that a corrupt ledger is
usable. Native successor diagnostics retain `STORAGE_CAPACITY_EXCEEDED`. Frozen
Run Machine-v2/v4 and public-v1 transports cannot represent that code and report
`CAPABILITY_UNSUPPORTED` for `dolgorae.storage-capacity/v014` instead. This is an
explicit reporting-version boundary, distinct from internal or integrity failure.
Inspect the original operation and use explicitly approved native maintenance
before an exact-key retry; this does not activate public-v2. Repeated recovery
attempts cannot manufacture unlimited reserve.

## Workspace selection and per-Run history

Maintenance is workspace-scoped, with independently chained Run ledgers.
A dry-run plan pins the workspace generation and every retained Run's manifest,
head and selected ledger generation. Applying that exact plan requires explicit
operator approval and the same operator identity and generation. Operator
rotation invalidates a new apply; an already retained owner can resume under
fresh explicit approval by the same operator identity. Original-scope observation remains authorized
against the original operator identity. The original plan ID is the maintenance
operation ID, retained before any file effect; reply loss is resolved by
observing this ID. An incomplete or unreadable operation namespace cannot
establish absence.

The workspace lifecycle gate excludes new ledger writers. Maintenance holds
it exclusively before taking Run lifecycle and audit writer locks, proves all
workloads absent and all Runs quiescent, and rejects pending or unknown work.
Ordinary ledger writers hold its shared admission lease for their lifetime.
A retained incomplete checkpoint owner keeps shared admission closed after its
process locks are released. Ledger writers, database leases and new Run
publication cannot change its frozen source before the original operation
resumes. Sidecar inspection and exclusive original-operation Apply remain
available; completion reopens ordinary admission.
While holding shared admission, a process may reuse a successful complete
namespace inspection only while freshly checked workspace and maintenance
directory identities remain unchanged. Owner retention and every progress or
completion publication change a directory entry atomically, invalidating this
reuse even for older writers. The first admission and every changed identity
require the complete inspection. Pending and failed inspections are never
cached. Original-operation Plan/Get/Apply still validate their retained files;
this optimization does not prove history or authorize maintenance effects.
All locks are nonblocking; a busy workspace requires stopping its writers
through their existing lifecycle owners before applying maintenance.

Each Run preserves `audit.jsonl` as its permanent writer-lock anchor. Legacy
history remains unchanged. A selected immutable manifest identifies a checked
checkpoint, immutable historical segments, an accepted-receipt index, the
checkpoint chain hash/watermark and the new append segment. A reader pins that
manifest once and reads its exact files; it never reselects during an operation.
The checkpoint retains the complete history reference set. Historical segments
belong to retained generations; the selected append generation has its own
byte and record ceilings. The selected checkpoint, receipt index, manifest, selector and activation witness
bytes count against its byte ceiling; retained historical segments remain
owned by their earlier generations. Chain validation covers the historical prefix and
append suffix together, including Run identity and generation monotonicity.
Original audit sequences, record hashes, receipts, event cursors and export
bytes are unchanged. History replay verifies checkpoint state and preserves every integrity check.

## Crash ordering and retention

Apply records a separate immutable owner binding before updating progress,
then creates new generation files exclusively,
syncs each file, syncs the new generation directory and its parent, then
atomically replaces the selected manifest reference and syncs its parent.
Before selection, readers use the old generation. After selection, readers
validate the new checkpoint and files. A repeated apply resumes the original
owner and recorded target, including partial multi-Run selection; it does not
allocate another operation or rewrite accepted history. Unexpected selectors,
changed history or damaged prepared files require recovery. Each activated Run
generation retains an immutable selection witness before any new append. Missing
or older selectors cannot fall back below these witnesses. Prepared generations
without activation witnesses do not invalidate a still-selected old generation.
Workspace selection must match its retained owner, original source generation
and selected Run bases, and cannot precede the latest retained completed generation.
The explicit terminal integrity-failure deletion escape pins the selected append
or, when it is empty, the checkpoint's final historical segment. It verifies the
independent terminal seal and selection identity without accepting a damaged
historical prefix. Retained namespace security/access failures require recovery
with unresolved effect. A trusted original owner or applied progress record
preserves the original operation reference; untrusted metadata cannot establish
absence. Caller-provided plan/carrier refusal remains separate. Deletion holds
shared workspace admission through Run removal and directory sync, excluding maintenance for the complete operation.

No automatic deletion or pruning occurs. Old append files, manifests,
checkpoints, receipt indexes and recovery references remain available to old
reader pins and interrupted maintenance. Checkpoint rollover performs no garbage collection. Content retirement and explicit protected-reference
GC remain their assigned later scope. Unsupported maintenance kinds are refused.
