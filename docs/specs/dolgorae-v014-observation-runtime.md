# v0.1.4 Observation, Wait and Result Storage Runtime

This document owns TASK-075's native component implementation. Its acceptance
uses the frozen [measurement definitions](dolgorae-v014-successor-requirements.md#measurement-and-real-account-campaign-definitions).
Component evidence does not establish actual Gul, account, device, Aquarium or
Podway runtime qualification. Those obligations retain their assigned owners.
H1 remains unchanged; Gul E16 remains Planned until Gate A and all Phase 3/H3
prerequisites are satisfied.

## Authority and observation

Observation reuse applies only to display and event sampling. Mutation admission,
controller and operator authentication, process and complete-history absence
proof, writer release and generation replacement always use fresh authoritative
sources. A cache entry cannot authorize an external effect or prove an accepted
operation absent. Replacement, truncation, changed configuration, symlink target,
selected generation, executable or database identity invalidates relevant reuse.
An invalid source fails through its existing typed integrity or configuration
error; a previously valid observation cannot mask that error.

## Public-v2 Observation adapter

Public-v2 transport uses the existing public event projection and Controller
timeline version 1. These versions are independent of persisted event schema
versions and the transport's protocol version 2. Runtime capabilities advertise
those implemented public versions. Public-v1 behavior and frozen H1 bytes remain
unchanged. Both Observation methods require the checked v2 request context and
return v2 typed error details, including errors after stream admission.

Event sampling reuses the verified history, original historical stamps and safe
MINIMAL/OPERATIONAL projections. Public projection version 1 describes the public
payload even when its source record uses persisted schema 2. Streams retain the
existing independent queue, byte, stall, heartbeat, replay and shutdown bounds.
Dropping a stream does not alter its Run or another stream.

Timeline pages revalidate the current Controller before and after reading
protected material. Forward pages use an exclusive initial ledger cursor and
then opaque continuation tokens bound to the Run, captured head and Controller.
Latest requires an empty initial cursor and selects a chronological suffix at
one captured head; independent forward backfill can start at cursor `0`.
Unspecified direction selects forward. The default count is 100, with a maximum
of 500. A zero byte budget selects the normal 4 MiB bound; larger budgets are
refused. Returned continuation tokens preserve the original capture across
appends and cannot be used for another Run or Controller binding. Controller
replacement invalidates prior continuations independently of any change to the
Run ledger head or aggregate stamp. Each page's Run/Writer/Interaction stamp
comes from its fresh authoritative snapshot, independently of the retained
timeline head. Continuations carry no aggregate
stamp supplied by the client; event replay retains its original historical
stamps. A workspace Writer revision is independent of the Run ledger sequence.

The complete encoded response, including context, stamp, continuation and its
own encoded-length field, fits the selected byte budget. `complete_page` means
that the returned page is complete, not that all history has been returned.
Public-v2 artifact reads resolve the existing immutable accepted-input storage
when a byte-bound page uses its artifact reference, including input normally
presented inline. Those reads require the current Controller before and after
access, retain the complete length/hash and leave public-v1 lookup unchanged.
Content retains its existing complete inline or immutable artifact form; reads
do not create artifacts. A budget too small for the next complete item receives
a typed capacity refusal without advancing its cursor. No page truncates content
or returns a continuation that cannot make progress.

## Executable and workspace observations

Checked executable and schema evidence may be reused only with unchanged file
identity and relevant configuration. Profile definitions and canonical homes
remain part of the observation boundary. One Profile list captures one freshly
checked registry and observes its definitions using a shared context. Registry,
home generation or root identity changes during that pass refuse the list;
readers do not combine definitions from different source captures. Every entry
retains executable/configuration invalidation and fresh server state, process,
socket and model-catalog checks. A list observes at most four Profiles concurrently
and returns them in registry order. It joins every observation in the current
batch before returning an error, selecting the first error in registry order;
an observation panic is an internal error. Named Profile reads and mutation preparation
continue to resolve their own fresh definitions. Executing-build evidence
describes the running image, while process compatibility checks retain exact-build semantics.
Workspace display sampling may reuse discovery with validated repository and
worktree identity. Steady idle sampling must spawn no repeated Git commands.
Fresh lifecycle and writer operations continue to discover their own baseline.
Run display sampling reuses only executable digests whose fresh no-follow
path and descriptor identities still agree, including owner, mode, link count,
length, modification and change times. Each sample retains fresh boot, PID birth,
group, session, live executable path and exit-watch checks. Authoritative process
classification and absence proof continue to read complete executable bytes;
display reuse cannot authorize a mutation, writer release or generation change.

## Verified history sampling

Display readers retain a verified prefix for one selected ledger generation and
extend it only with a validated, published suffix. Replacement, changed prefix,
truncation or a changed generation invalidates the prefix. Event cursors and
captured durable heads remain unchanged, and readers never combine generations.
Terminal response decoding, interaction revision and policy inputs belong to
that exact verified captured head and may be shared with its display readers.
The display cache retains up to 16 Runs within a shared 512 MiB budget of
canonical record bytes, with 120-second idle eviction. Larger Runs use a larger
share of that budget; inserting or extending a capture evicts older entries as
needed. Captures larger than the whole budget bypass retention without evicting
other entries and remain readable through full verification. The budget covers
idle cache ownership; parsed JSON allocation and active reader snapshots have
separate lifetimes. Suffix extension reuses owned records, copying only while
an older snapshot shares them, and extends the freshly verified prefix hasher
with the checked suffix bytes.
An older captured head or changed prefix receives its own derived views.
A valid concurrent append or generation publication may prevent a coherent
capture from completing within its fixed budget. That bounded source-motion
condition is a capture conflict, not audit corruption. Snapshot capture may
retry only its explicit transient signal within the existing capture budget.
Every returned prefix still passes the complete source and history fences.
Before reporting exhausted motion, the reader freshly verifies the current
selected prefix and confirms that its captured head hash is preserved.
Damaged prefixes, truncation, invalid published records and source-security
failures retain their integrity errors; they are never retried as source motion.
Complete authoritative history and absence checks remain separate fresh reads.
Shared sampling must preserve the coherent source boundary of Run snapshots.
Each source pass resolves the current Controller binding against that pass's
freshly validated manifest. Display socket epochs come from the same captured
runtime record, without a separate pre-capture read. Before/after comparison
still covers every manifest, Controller, writer, projection and runtime field.

## Database access and durable waits

Schema migration is versioned and atomic. Ordinary access reuses checked
connections and avoids repeated DDL, migration scans and complete event replay.
Database identity and durable revision, including WAL commits, determine
invalidation. Read-only observation does not create, migrate or repair a store.
A verified SQLite event history may be reused only while its checked main/WAL
source, schema, connection-specific durable version and own-write count are
unchanged. Any source change requires full fresh chain verification; mutable
SQLite rows cannot be treated as an immutable prefix. The Run ledger alone uses
verified published suffix extension. Useful indexes and bounded query plans
must be measured against actual queries.
The store retains at most 32 idle connections; active leases retain their caller work admission. Engagement
and orchestration access share the same checked connection. The native
orchestration extension carries a persisted migration version, changed within
one immediate transaction. WAL `data_version` participates in verified-history
invalidation. SQLite statement callbacks record actual query and DDL counts without allocating, retaining, or emitting statement text, bound values, or bodies. Registration uses the bundled SQLite FFI in the Darwin boundary with a static count-only context and does not require a manifest feature change.

No connection-cache lock, database transaction or exclusive Run/controller
ownership lock spans a wait or upstream dispatch. Checked live database leases
and result readers retain their shared workspace maintenance admission pin; idle
pooled connections retain no pin.

Durable state remains authoritative. A commit notification wakes local waiters,
which recheck the predicate; an epoch/recheck fence prevents lost wakeups.
Cross-process or restart recovery uses a bounded fallback. Existing worker
completion reconciliation must remain reachable without eight serial long waits.
Deadline expiration never turns accepted or uncertain effects into absence,
replay eligibility or cancellation success.

## Immutable result content and explicit retention

Equal result bytes may share immutable storage while artifact, task, publication,
receipt and provenance identities remain distinct. Raw bytes and structured values
retain their original representation. Publication preserves prepare, sync,
atomic visibility and retained completion ordering. Orchestration extension
version 3 retains the original structured JSON digest separately from the raw
result digest. Task, collection and publication recovery reads check that digest
before reading the selected reference, including retired references. Migration
hashes retained legacy inline JSON without changing its bytes; intermediate
normalized rows lacking an original JSON digest are refused rather than rebound
to whichever content reference is currently selected. Legacy collect and
one-shot inspection also check the original artifact and engagement JSON digests.
Broker collection materializes and validates every selected result within its
delivery transaction before recording any new delivery receipt, state or event.
An integrity refusal leaves all selected delivery evidence unchanged.
Missing published content is
an integrity failure, never an invitation to regenerate an accepted result.
Run-scoped publication and startup recovery expose exhausted quota-lock
contention as retryable `RUN_BUSY` with the original Run and `attachment` owner,
including when recovery runs while opening the orchestration store.
Logical content limits still apply to each owning Run. A shared per-Run artifact
quota lease serializes ordinary response storage and brokered publication from
quota check through publication. Contending publishers wait for this brief critical section before opening their publication transaction, with the same five-second bound as database writer admission. Lock identity and security are rechecked while waiting and after acquisition; exhausted contention reports `RUN_BUSY`, while an untrusted lock remains an integrity failure. The 256 MiB bound counts each retained artifact
identity at its logical byte length even when bodies share storage, includes
prepared publication references and ordinary files, and excludes explicitly
retired bodies. Pointer file length cannot reduce the logical count. Final Turn
quota exhaustion preserves terminal status and records the existing
`quota_exceeded` unavailable response; it does not truncate the body. A known
brokered completion whose publication exceeds capacity retains its original
accepted task, target Turn and receipt binding, plus observed output digests and
lengths in a bounded native `brokered_result_capacity_failures` row, committed
with the failure event and task transition. An event payload hash alone cannot
retain those facts. Its terminal local publication failure carries
`STORAGE_CAPACITY_EXCEEDED`; this does not declare provider output semantically
invalid or authorize replay. Failed publication remains visible in collection
and accounting. The native result-content extension version 3 atomically adds retained capacity
failure metadata to version 2 chunk receipt bindings and version 1 content
storage. Legacy inline responses remain
unchanged. The extension stores one immutable digest-addressed byte value and per-publication references
to the original JSON and raw representations. Secure shared content files have
one link; checked artifact references retain the original artifact identity.
New successful cached `read_specialist_result` chunks retain a separate UUIDv7
content-reference identity without replacing the arbitrary original source-call
ID. The private cached-result row retains a nullable UUIDv7
`normalized_chunk_reference_id` independently of the chunk binding. Non-null
classification requires that exact binding on replay, including after retirement;
missing binding is corruption rather than legacy success. Orchestration extension
version 2 adds this column and classifies existing intact normalized bindings in
one atomic migration without changing stored response or provenance bytes.
Legacy inline rows remain null; an empty non-EOF chunk without classification is
corrupt. One transaction stores this classification, the original response SHA,
checked session/task/call
and range binding, body-free response skeleton and exact chunk-byte reference.
Replay rehydrates and verifies the original response or reports its retirement
tombstone; it cannot bypass content availability. Chunk bindings participate in
the exact maintenance source and target digest. Delivered source-task and chunk
references are retired together. Legacy inline cached chunks remain immutable
and protect their source body; this Task does not rewrite historical accepted
responses or silently migrate that provenance. Legacy published byte files
remain readable and verified. Result-bearing external task snapshots require the
original task/artifact binding, including its engagement and specialist owners.
Missing, malformed or changed bindings are corruption; normal tasks without a
result retain their existing states. Legacy artifacts retain their historical
owner fields. A normalized artifact with empty stored JSON requires its content
reference; absence is corruption. Recorded task and artifact result digests bind
the selected content reference and legacy inline JSON. A valid shared byte value
with a different digest cannot replace an accepted result, including after
retirement. Normalized bytes are read from that same checked reference capture.
A
validated retirement tombstone preserves task/artifact identity with unavailable
result content. Retention tombstones
are durable native metadata; they do not rewrite historical accepted rows.

Retention and garbage collection are explicit operator-approved maintenance,
with a retained original operation and read-back after response loss. Plans bind
scope, operator, selected generation and content references and are revalidated
before effects. Active, unknown, prepared, undelivered and live-reader references
remain protected. Writers participate in maintenance exclusion. Retiring content
preserves identity and audit receipts, records a durable tombstone and reports
`RESULT_CONTENT_UNAVAILABLE`; it never claims the operation did not exist.
Native content maintenance uses the existing `retire_result_content` and
`garbage_collection` plan kinds. The frozen plan contains only checked
`bytes_ref` values in `retire_content`, whose digests use `sha256:<64 lowercase hex>`; native content references and completion progress retain bare hex digests. The maintenance boundary validates the checked byte references before comparing or executing private content operations. Private owner/representation references
are bound by the plan digest and retained in an additive native content-operation
sidecar, with a separate immutable owner binding. Retirement also retains the
exact delivered owner/representation targets selected at planning, separately
from the complete reference snapshot; both are digest-bound. Newly delivered
equal-byte references cannot expand an already approved retirement. Garbage
collection has no retirement targets and removes only unreferenced content. A native result-content
reference at the original artifact pathname binds artifact identity, byte length
and SHA-256 to its shared file; it is not a hard link. These shapes are checked by
[native content storage](../protocol/dolgorae-native-result-content-v1.schema.json).
Retained content operations and owners live in the separate secure
`result-maintenance` namespace. The native CLI projects their read-back into the
frozen maintenance observation: active retirement/collection is
`recovery_needed` with `resume_maintenance`; completion is `completed` with
`none`. `old_generation` is the source workspace checkpoint generation and
`new_generation` is null because content maintenance does not select a new Run
ledger generation. `selected_manifest_digest` retains the exact immutable plan
source digest. Native sidecars retain the separate content state and effect.

Database writers, content readers and new Run publication hold a shared workspace
maintenance lease; content maintenance holds the existing exclusive workspace
boundary. A retained incomplete checkpoint or content owner keeps ordinary admission closed
after a crash or error releases the process lock. Original-operation inspection
and maintenance resumption remain available; completion reopens admission.

The native semantic error retains `RESULT_CONTENT_UNAVAILABLE` with the original
retirement operation reference. Frozen public-v1 and Machine-v2/v4 error carriers
report `CAPABILITY_UNSUPPORTED` for `dolgorae.result-retention/v014`, rather than
inventing an unregistered wire code. This reporting boundary does not activate
public-v2; its result-content projection retains the later TASK-078 owner.

Ordinary Run publication reports maintenance exclusion using the registered
`RUN_BUSY` startup details for its known Run identity. Database admission has no
Run identity and reports the existing `CAPABILITY_UNSUPPORTED` shape with
`dolgorae.workspace-maintenance/v014` and `recognized_unsupported`. Unsafe paths
and inconsistent maintenance namespaces remain path refusals; capacity retains
its separate reporting boundary. Native Plan/Get/Apply retain their original
operation and recovery diagnostics.

Automatic deletion is disabled. Shared bytes are removed only after their final
protected reference is absent, with crash-safe retained progress.

## Stream and RPC work

The server admits eight concurrent unary jobs and four concurrent stream sampling
jobs, with twelve blocking executor slots. Each RPC has a 30-second maximum
response budget; a valid shorter `grpc-timeout` bounds admission and response
waiting. Already admitted blocking work retains its slot until it exits. A
response timeout reports the existing `TRANSPORT_FAILURE` policy for the named
method, including uncertain mutation recovery, and cannot cancel or replay the
accepted operation. Idle stream sampling backs off from 100 to 500 ms and returns
to 100 ms when its durable head changes.

Queue, encoded-byte,
stalled-reader and reconnect/resume semantics remain enforced. A transport
response deadline cannot erase accepted work. Diagnostics use checked structured
fields and redact credentials, prompts, result bodies and private paths.

## Component acceptance

Use one recorded supported-toolchain build/configuration and isolated state.
Measure every warm-read repetition and the six-stream and eight-task campaigns
exactly as defined by the owning measurement contract. Record executable/schema,
Git, ledger replay/tail, connection, DDL, query, migration, wait and notification
counters, database/WAL/content growth and actual query plans in local evidence.
Tests also cover replacement/invalidation, lost notifications, crash boundaries,
protected retention references, deadline uncertainty and diagnostic canaries.
The default repository gate and selected independent review remain required.
The gate runs snapshot consistency cases in its existing Run contract target
and native observation/content cases in its existing semantic gateway target.
The existing E2E wrapper invokes those prebuilt cases; frozen H1 build inputs
remain unchanged. Long measurement campaigns retain separate explicit opt-in.
