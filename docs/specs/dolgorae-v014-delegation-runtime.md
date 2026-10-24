# v0.1.4 Delegation Runtime

This document owns TASK-076's runtime choices where the frozen H1
specifications leave room. It implements the
[method and digest contract](dolgorae-v014-method-and-digest-contract.md)
(MDC), the [release-proof correction](dolgorae-v014-release-proof-correction.md)
(RPC), the caller-workspace capture rules
([CW](caller-workspaces-v1.md)) and the generation selection of the
[successor requirements](dolgorae-v014-successor-requirements.md) as shared
Rust semantic services. TASK-076 adds no transport: Machine v5, the trusted
facade, MCP, public v2 and capability activation belong to TASK-077/078, and
the runtime checks this document records are the ones the
[release-proof dossier](../todo/TODO-DOLGORAE-RELEASE-PROOF-CORRECTION.md)
(RPD:299-307) assigns to the shared runtime.

## Master decisions

Two bounds are Master decisions recorded verbatim from the accepted TASK-076
plan; every other rule below is derived from a canonical clause.

**MD1** — the delegated change set excludes Git-ignored paths **except** paths
that this task's executor explicitly edited through its `fileChange` items in
the task's Turn(s), which are included. Build outputs and caches are therefore
not result content. The ignored side of this bound is the set of paths named
by the task's change items that are in neither the dispatch-index domain nor
the current non-ignored listing; its before image is the content before the
executor's explicit edits, reconstructed from the items, not a dispatch
capture.

**MD2** — non-Git workspaces refuse writable delegation in v0.1.4.
`canonical_workspace_write` delegation is refused with a typed error
**before acceptance** — before any intent row, so there is no receipt and
lookup never reports `not_accepted` for it. Read-only delegation and the
existing writable start are unchanged. The recorded reason is the cost of a
per-task full byte capture, deferred by Master — not "no baseline is
possible", because a bounded non-Git reader exists. The support limit is
asymmetric with the writable start: a non-Git workspace can still be written
by a started Run, but its changes cannot be captured as a delegated complete
result.

## Delegation namespace and Engagement database generation 6

The workspace Engagement database moves from base generation 5 to 6
(RPC:466-524) in one transaction per database: `BEGIN IMMEDIATE`, a re-read of
`schema_version` inside the transaction, the generation-6 DDL plus the
`metadata` update and the `namespace_birth_id` row for `"5"`, a no-op for
`"6"`, and a typed refusal for anything else. An interruption before the
commit leaves generation 5 byte-intact with no generation-6 object; after the
commit the database is completely generation 6 and the next open resumes
normally. There is no backfill (RPC:168-171) and no downgrade. A generation-5
database that already carries a generation-6 object is refused inside the
transaction — there is no runtime schema comparator; the strict
no-`IF NOT EXISTS` DDL is the comparator. A fresh database is created by
the historical generation-5 bootstrap followed immediately by the same single
5→6 transaction, from the same DDL source, so a new workspace is born at
generation 6. `metadata.orchestration_schema_version` stays `3`; the
orchestration extension SQL is unchanged.

Generation 6 adds six tables, each retained because a clause needs it:
`delegation_operations` persists the H1 `operation_intent` v2 shape plus
lookup-index columns for every successor keyed operation (MDC:59-72,
MDC:132-145); `caller_scope_assignments` owns round accounting separately from
receipt state (MDC:615-623); `scope_membership_revisions` and
`member_workload_revisions` carry the scoped membership counter and per-member
workload counter RPC:284-290 requires and RPC:291-295 forbids deriving from
other revisions; `member_release_operations` retains both release owners'
durable operation (RPC:219-325); `dispatch_indexes` anchors each delegated
write task's immutable dispatch index (MDC:455-459). Immutable-identity and
state-transition triggers enforce the per-state rules declaratively,
including receipt immutability (a receipt is written only on
`prepared|unresolved → accepted` and never rewritten) and the terminal freeze
of a settled release row.

Every opener checks the versions before any write, chmod, WAL pragma or DDL,
in all four openers RPC:508-517 names: the writable engagement open, the
read-only engagement open, and both orchestration opens, including the
observer that previously validated neither version. The check is an
effect-free probe: it creates no file, no lock and no sidecar, never chmods
and never runs DDL. An empty schema (no `sqlite_master` object, including a
zero-byte file) means "not yet created"; an immutable read is used only when
no `-wal`/`-shm`/`-journal` sidecar exists; a one-sided sidecar pair is
re-observed up to five times over 10 ms before it is reported, because SQLite
creates and unlinks the pair in sequence at open; `-wal` without `-shm`, or
`-shm` without `-wal`, is a typed `RECOVERY_REQUIRED`. A live WAL pair is
observed read-only without touching the database. Generation 7 and extension 4
are refused typed before any effect, with no sidecar, no maintenance lock, no
mode change and no `sqlite_master` change. Only verified legacy shapes — the
recorded birth schemas of generations 1-5 and a completed 6 — are admitted;
everything else fails closed.

The delegation namespace is explicit setup, never a side effect. Any
creation of the engagement database is a namespace birth: the creating
transaction writes a fresh `namespace_birth_id` and the birth record is
published after the commit — legacy lazy creation is such a birth. Successor
keyed operations and lookups never create the database: they require an
existing set-up namespace and otherwise return the typed
`DELEGATION_NAMESPACE_UNSET` precondition (`effect: none`,
`restore_state`), or `RECOVERY_REQUIRED` for an inconsistent one — never
authoritative absence. `ensure_delegation_namespace` is the explicit setup
step for a legacy workspace, and `dolgorae init` does not perform it: the
side effect broke artifact reads, maintenance garbage collection and the
invariant that ordinary Runs create no orchestration state, so readers
of the extension tables treat a base-only database as "no orchestration data".
The operator-facing trigger belongs to TASK-077's adapter. The production
birth record is `<state_root>/orchestration/namespace-birth.json`; isolated
and fixture databases carry `<database file name>.namespace-birth.json` beside
the database. Publication is create-exclusive and never overwrites a record
with a different birth ID. A birth record without its database, or a
birth-ID mismatch, is `RECOVERY_REQUIRED`. A database whose birth ID is set
but whose record is missing (the crash-between state) is a set-up namespace:
read-only lookups answer from the database's birth ID without publishing, and
the next writable open republishes the record.

**Residual risk.** If both the birth record and the database are
deleted and a legacy path later recreates the database, the recreated
database is a new namespace with a new birth ID. An old key then looks absent
in the new namespace and could be admitted again; the prior receipts are not
misreported, but a lookup finds a different namespace rather than the
original one.

## Keyed intents, receipts and lookup

All six successor keyed kinds — `install_provisioning`,
`open_external_engagement`, `hire_external_specialist`,
`assign_external_specialist_task`, `spawn_specialist`,
`assign_specialist_task` — persist one `operation_intent` v2 row per key in
`delegation_operations`, admitted `prepared` before the acceptance
transaction and `accepted` inside it, so the receipt precedes the
acknowledgement (MDC:60-61). Legacy facade-v4 paths keep the legacy
`operations` table; there is no dual-write in either direction. Replay
precedes every revision fence and returns the retained receipt byte-for-byte
(MDC:66-68); the same key with a changed accepted-input digest is
`IDEMPOTENCY_CONFLICT`. A definitively refused key re-enters as a new
operation only with the latest refused input digest (MDC:69-70); lookup
selects the latest durable refusal. A key that already exists in the legacy
`operations` table for the same scope and operation — or in the managed
Broker's key columns for the same session — is a typed compatibility refusal
inside `intent_prepare`, `intent_accept` and lookup, never `absent` and never
admitted as new.

`lookup_operation` (MDC:186-200) runs the namespace check and the effect-free
probe first and never opens the store for creation. Its result is exactly one
of:

- **accepted** — the retained receipt, plus `current_outcome` derived at
  lookup time from the task and member stores; only `receipt_json` is
  immutable and it is never rewritten.
- **not_accepted** — the latest `resolved_not_accepted` refusal with
  `durable_refusal_revision` and `exact_key_retry_permitted = true`.
- **unresolved** — a `prepared` or `unresolved` intent; an exact-key retry of
  a prepared managed intent reports outcome unknown without a second
  allocation or a counted round.
- **absent** — only from a complete read of a set-up namespace, carrying
  `retained_namespace_generation` (the controller-generation fence, not the
  schema generation) and `durable_watermark` = the observed maximum
  `delegation_operations.sequence` (MDC:179-184, MDC:195-198).

A corrupt or unavailable namespace never yields `absent`; corrupt retained
intents, envelopes and receipts are typed recovery conditions. Managed lookup
authenticates the workspace and the session's root Run Controller before any
Broker or collision read, with no active-session precondition, so a foreign
Controller cannot distinguish a used key from an unused one and the retained
receipt stays readable after a close intent (MDC:134, MDC:186-200).

| Operation kind | `current_outcome` |
| --- | --- |
| `install_provisioning`, `open_external_engagement` | `completed` |
| `hire_external_specialist` | derived from the external member row and residency: `pending` while provisioning, `unknown` while recovering or degraded-unproven, `failed` when unavailable, else `completed` |
| `spawn_specialist` | the same derivation over the Broker member row and its Run residency |
| `assign_*_task` | derived from the task row: `pending`, `running`, `completed`, `failed`, `cancelled`, `expired`, else `unknown` |

`result_ref` and `result_digest` are populated for task kinds from the
settled task; `install`/`open` carry none. `adapter_id` and `adapter_version`
stay provider-owned and are never derived here (MDC:594-597).

Replay recomputes the identity from the presented inputs and compares it with
the retained envelope; lookup is the source-independent recovery path.
Every error after a durable prepare carries the known `operation_ref` with the
unresolved effect floor (MDC:39). A definitive refusal before acceptance —
including an admission whose acceptance closure itself refuses late — rolls
the acceptance back and records `resolved_not_accepted` in its own
transaction with the real refusal code. This is a Case A refusal: a refusal
decided before keyed acceptance, recorded `not_accepted` with no effect, and
nothing durable remains when the refusal precedes the prepare.

## Capture and identity

Capture happens before allocation and dispatch: no Run, allocation or
dispatch exists before `delegation_identity::InstructionSnapshot::from_capture`
has accepted the captured bytes. The Role snapshot binds an inline body or an
instruction-home file Role verbatim — UTF-8, non-empty, NUL-free, with named
dependencies sorted by alias bytes and explicitly selected guidance in the
caller's order (MDC:250-302, CW:210-262). The aggregate ceiling is 65,536
UTF-8 bytes across common, Role, dependencies and guidance, excluding the
core sections and separators (MDC:299-301); capture performs no
normalization, and each alias is at most 64 bytes rejecting only C0 controls
and DEL. Changing a same-named Role's bytes, one dependency or one
configuration field changes the identity; display names are attribution only.

The closed domains are `role-snapshot/v1`, `instruction-identity/v1`,
`admitted-configuration/v1`, `accepted-task/v1`, `complete-result/v1` and the
reused `mutation-envelope/v1` (MDC:202-591). The successor configuration is
generation 4, constructed directly from the admitted configuration —
`native_subagent_policy` defaults to `disabled`, `enabled` requires a
dedicated qualified configuration, and `shared_readonly` implies `read_only`
plus `disabled` (MDC:310-337). The public `agent_configuration_digest` is the
admitted-configuration domain digest; the Run's aggregate binding keeps the
bare runtime hash. The Run manifest is generation 4 with its aggregate
binding; **Run state 2 is not implemented** — a manifest-4 delegated start is
admitted and read through the manifest/configuration pair and the aggregate
acceptance rows while its Run-state projection stays generation 1, a
supported mixed-generation read until TASK-078 (MDC:88, MDC:415-416). The
accepted task keeps one `ContextRef` per caller context in accepted order,
with the direct review target as its own slot, within 64 context slots.
Both assignment owners admit a requested direct target from the authenticated
workspace before preparing an intent. The last `review_target` slot uses the
admitted `DirectTarget.target_sha256` as its identity and references the exact
`metadata_bytes()` output as `application/json`. The target counts toward the
64-slot bound: 63 caller contexts plus a target fit; 64 plus a target do not.
The manifest bytes, accepted identity and receipt commit together. This retains
metadata without copying candidate files or creating a worktree; staged after
entries name the index, while dirty after entries name the worktree.

Accepted replay reads the original task and target manifest under the keyed
namespace before any new target admission. It compares the original requested
selection and retained caller input, including member, task, execution intent
and deadline. Moving a ref or editing source files cannot change that receipt;
changed inputs conflict. Dispatch and restart read the retained manifest and
verify its content reference and owning target digest without resolving refs
again. Mutable candidate reads retain the existing drift checks.
There is no policy-session delegation path: both `hire` and `spawn` start
only from a verified installed `provisioning_ref`, so no prepared policy
session carries delegation data.

Profile ambient guidance is disclosed separately and never enters any digest
or closed object (MDC:304-306, CW:242-262). Each admission and identity read
returns a successor-only wrapper — `MemberAdmissionView` for `hire`/`spawn`,
`DelegationIdentityView` for the identity exports — carrying the closed
identity once and an `AmbientGuidanceDisclosure`. The disclosure is a
call-time observation: each ambient source reports `Absent`, `Present`, or
`Unobservable` with a typed reason, and `Unobservable` is never projected as
`Absent`. It may change between calls without changing any digest, and
TASK-077 must not project it into the closed identity. Project and ancestor
discovery stays suppressed through the existing discovery control
(CD-10); recovery reconstructs accepted bytes and never reloads sources
(CW:233-241), so captured bytes survive source edits and deletion after
install.

## Complete results and the changes producer

A complete result binds complete bytes, never a preview or summary
(MDC:450-486). Every final-response source the Turn coordinator resolves is a
complete-result source — inline text, history fallback, or an artifact within
32 MiB whose length and digest verify; a failed or cancelled terminal without
a response is a valid result with a null final response. Checked
`structured_review_v3` output is validated before publication; invalid
findings settle as the failed-output result with `FINDINGS_OUTPUT_INVALID`
and no payload (MDC:471-473). Direct-target manifest capture is supported for
both assignment owners. Checked `structured_review_v4` tasks require an
explicit target and a manifest-4 Specialist admitted with read-only Review
purpose and a qualified checked-review Profile. The initial Thread has the
candidate tool enabled with read-only sandbox and approval never; a general
Thread cannot be upgraded by an assignment.

Each checked task owns a reader identified by its task reference, bound to the
member, workspace/mode and retained target digest. Dispatch creates or opens
that reader from admitted metadata before Turn input. Restart selects the
active task's reader; once a Turn is recorded, a missing or mismatched reader
is a recovery/provenance failure and is never recreated. Earlier task readers
and receipt histories remain intact. A supported non-v4 assignment clears the
actor's previous checked reader before Turn input, including after restart.
The session's candidate-tool availability grants no access to that old reader
or its history. An unbound checked task still cannot start a Turn.

Settlement checks the retained acceptance, reader/history and source assurance
before parsing v4 structure and task semantics, including criterion order,
context scope, locations and overall assessment. Candidate evidence must cite
host-issued receipts for that task. Successful publication retains the original
v4 fields and canonical payload, with a second acceptance check before commit.
Malformed output settles with `FINDINGS_OUTPUT_INVALID` and no checked payload.
Drift preserves `REVIEW_TARGET_DRIFTED`, a reading-boundary or forged/foreign
receipt violation preserves `REVIEW_CANDIDATE_PROVENANCE_INVALID`, and pinned
source loss preserves `REVIEW_TARGET_UNAVAILABLE`. These are failed findings
adoptions with null structured output, not rewritten execution terminals or
new CompleteResult outcomes. Collect and identity preserve the safe review
cause through the existing successor-error shape. Reader/history storage or
integrity failures propagate without settlement; restoring the original state
allows retry against the same task and durable terminal, without reexecution.
Failure to read or parse a present host receipt during item or history
processing propagates as a storage failure and never writes a provenance
violation marker. Receipt absence alone is not proof of unmediated use. A
completed candidate call's recorded host response is checked against its
arguments and task binding. A failed host call retains its original error
classification; a lost issued receipt or insufficient issuance evidence is a
recoverable storage/integrity error. Genuine unmediated use and forged or
foreign receipts retain their provenance rejection. Recovery never adopts
unverified evidence.

Recorded unsuccessful calls separate deterministic request/content refusals
from host faults. Invalid arguments, unsafe paths/files, source limits and
secret screening remain failed reads. A recorded reader-state, I/O or Git
fault blocks adoption until the original reader lock, binding, receipt
directory and existing receipts verify again, together with the admitted
target content. An unresolved fault keeps its original code without a marker
or settlement. Missing pinned content keeps `REVIEW_TARGET_UNAVAILABLE`.
An oversized or corrupt stored receipt remains an integrity fault even when
its lower error shares the deterministic source-limit code. Healthy storage
allows the recorded unsuccessful call to remain a failed read and the same
task to settle once; recovery never reissues that call.

When an issued receipt is unavailable after an actual native terminal arrives, the
worker automatically preserves the verified history response in the existing
resolution store. A failed checked history read preserves the actual terminal's
status and usage in the journal, without fixing a notification fallback as
the response or final resolution. History remains pending and the task stays
unsettled until its original receipts can be verified. If a reader fault
prevented the pending-history write, startup restores that existing marker
only from the bound unfinished checked task and its actual terminal record.
Startup hydration reuses the retained
response reference under the quota lease, including for an artifact response.
An unsuccessful host read recorded in history needs no issued receipt; its
source rejection keeps the original safe cause and publishes no checked payload.

History-only recovery with a missing worker locator uses the ordinary complete
absence proof under its already-held startup range: no owned worker range or
remaining registration, the highest generation evidenced by the projection
and both owner slots, and an exact-generation absence marker bound to any
surviving worker-slot identity. Observer-only startup holds preserve every
owner-slot byte on success and failure. Root history proof accepts only the
exact JSON `null` parent token and rejects duplicate JSON keys, including
escaped spellings. Every streamed `thread/read` result must contain exactly
one complete JSON root; only JSON whitespace may follow its closing boundary.
This rule applies to recovery and ordinary bound or unbound history reads.
The sink validates the complete JSON grammar before accepting proof: object
and array separators, keys and values, exact literals, number syntax, string
escapes and Unicode, including fields it discards. Balanced delimiters alone
do not establish a valid result. Validation is incremental and retains no
complete history buffer.

Execution-side refusals retain the existing workspace review-error phase,
outcome and required action. The closed successor error exposes the retained
code through its existing classification/action fields.

| Acceptance condition | Review code and outcome | Settlement |
| --- | --- | --- |
| Mutable index, base or workspace drift | `REVIEW_TARGET_DRIFTED`, inconclusive | Failed findings adoption; no checked payload or automatic reexecution. |
| Native/plugin history, boundary violation or forged/foreign receipt | `REVIEW_CANDIDATE_PROVENANCE_INVALID`, inconclusive | Preserve the safe cause; no checked payload or automatic reexecution. |
| Pinned source needed by a receipt is missing | `REVIEW_TARGET_UNAVAILABLE`, failed | Preserve the safe cause; never substitute worktree bytes. |
| Reader/history storage or integrity failure | `RUNTIME_STATE_INVALID` or the existing lower error | Roll back settlement; recover the original state and retry the same terminal. |

For `canonical_workspace_write` tasks the change set is produced from an
immutable per-task dispatch index recorded at dispatch inside the member
Run's writer authority: the whole non-ignored domain
(`path → sha256, length, mode, kind`) plus the recorded `HEAD`, with the
complete bytes of every dirty and untracked baseline file retained in the
content store. A path clean at dispatch reconstructs its before image from
the worktree-filtered `git cat-file --filters` form and must match the index
digest; a filter mismatch fails closed. The index bounds are the existing
workspace-enumeration bounds: at most 10,000 files, a 64 MiB index-only
ceiling, an 8 MiB per-side bound, and a 256 MiB per-Run aggregate over
retained dispatch baselines, checked inside the recording transaction;
the current listing's paths that are new to the index are bounded by the same
10,000-file enumeration bound. A workspace with a submodule or gitlink fails
the dispatch capture with `REVIEW_TARGET_UNSAFE_FILE` (v0.1.4 support
limit).

At terminal publication, candidate paths are every dispatch-index path,
every baseline path, and every current non-ignored listing path not in the
index — never `git status`, whose clean-looking output would hide a file
created and committed during the task. Ignored-side candidates are MD1's
executor-edited paths only. Each path's before side is the retained capture,
the filtered `HEAD` blob, the reconstructed item, or absent; the after side
is the current worktree bytes or absent; a `ChangeRef` is emitted only when
bytes or mode differ, sorted by `(path, old_path, kind)` (MDC:239-241,
MDC:465). Renames are not detected. A move arrives as one executor
`fileChange` item whose `path` names the source and whose `move_path` names
the destination; the closed `ChangeRef` carries no `move_path`, so that one
item is emitted as a `delete` `ChangeRef` at the source path plus an `add`
`ChangeRef` at the destination path, and `old_path` is null on both in
v0.1.4 — no rename relationship is claimed. Modes are `st_mode & 0o7777` on
both sides; a
clean path's `before_mode` comes from the dispatch index, never from
Git-normalized tree modes, and an ignored-but-edited path's `before_mode` is
an explicit null unless a verified value exists. Mode-only changes — both
`chmod +x` and `0644 → 0600` — are detected by index comparison.

The ignored-side before image is reconstructed from the executor's change
items by reverse application: context lines and added (`+`) lines are matched
against the after bytes, the removed (`-`) lines reconstruct the before
bytes, and applying the diff forward to the reconstructed before must
reproduce the after bytes exactly. The `---`/`+++` header pair is optional —
a diff may start directly with an `@@` hunk — but at least one hunk, exact
hunk counts, ordered hunks, exact context matching and the no-newline marker
are enforced; zero-count hunk ranges index at the named position; hunk ends
use checked arithmetic; each step stays within the 8 MiB side bound. An empty
after file reconstructs from the removed lines alone; a delete is one
whole-file `@@ -1,N +0,0 @@` hunk covering the file from line 1 — a partial
delete hunk is refused. Several changes on one path chain newest first in
journal first-appearance order with the latest revision per item; an item
that did not take effect contributes nothing; a diff carrying the
`Moved to:` suffix is rejected as format-unverified (a v0.1.4 bound). A diff
the reverse applier accepts is at most 65,536 bytes and must end with a
newline. The executor's items are themselves bounded per item by the Turn
coordinator's normalization (HEAD behaviour): at most 4,096 changes in one
item, each diff at most 64 KiB, one item's diffs at most 8 MiB in total, and
the item's normalized payload at most 8 MiB. An over-bound item is rejected
there and leaves the Run OutcomeUnknown, rather than proving a failed Turn.
After an authenticated recovery freshly verifies the original history
terminal, the task converges without redispatch. A lost write stream cannot
prove its complete changes and publishes the existing unavailable marker. At
settlement, each published file-change payload is read with an 8 MiB bound,
and the payload bytes read for one task are bounded in aggregate by the same
64 MiB workspace target bound the dispatch index uses; a recorded payload
digest without its bytes, or an over-bound total, is the typed unavailable
condition. Domain paths have no `fileChange` cross-check — the index is the
sole authority — and an ignored move destination's prior absence is asserted,
not verified (the MD1 bound).

Every `ChangeRef` side's exact bytes are retained in the content store inside
the settlement transaction, with an explicit provenance link per task, path
and side (`owner_kind` `successor_change`, representation `before:<path>` /
`after:<path>`); identical bytes are stored once, and retained sides survive
later worktree edits and deletion. One 256 MiB per-Run budget covers dispatch
baseline bytes **and** retained change content; exceeding it rolls the
settlement back and re-settles the task as the typed unavailable result. The
producer itself holds at most 64 MiB of retained sides per half plus a 64 MiB
merged check; exceeding the retained aggregate is a permanent typed
unavailable result, and a dirty-at-dispatch path's baseline and before
reference conservatively count twice. More than 20,000 changes, a path a
`ChangeRef` cannot carry, or a non-UTF-8 path is the typed unavailable
result; a failed Git listing fails the settlement attempt (retried) rather
than publishing an incomplete set, and settlement-side worktree read errors
are classified — links, special files, `ELOOP`, `ENOTDIR`, `EACCES`/`EPERM`
are deterministic content conditions, other errno retried.

History-terminal recovery binds the accepted task's recorded Turn to its
original TurnStarted operation, Thread and effort in the member ledger. It
requires the original root Thread with null parent, one matching terminal
Turn, and the retained Profile binding. A status-only Reconciliation or a
private response preparation is not fresh terminal proof. Without a final
resolution, every retry reads history again; foreign roots, descendants,
duplicate Turns, nonterminal status and read errors leave the task unsettled.
Full `itemsView` and exact response bytes are additionally required for a
complete read result; only a full view can prove an absent response. Lost
writes publish unavailable without reconstructing changes from a partial
stream or the current worktree. Failed/interrupted terminal status remains
failed/cancelled, with its existing safe cause.

Recovery observes the durable journal under its owning serialization after
the fresh read. It appends the terminal Reconciliation once, then completes
ReconciliationRequired → Paused before publishing a response or task result.
A crash after the first sync adds only the missing Paused transition; an
already Paused retry adds neither record. Conflicting status, unrelated
lifecycle progression and invalid ledger history remain recovery errors.
Recovery never synthesizes OutcomeUnknown or TurnTerminal evidence. A worker
that died before abandon can recover from its recorded execution and verified
generation recovery without an earlier OutcomeUnknown record.

Large read responses retain one stable artifact reference under the existing
per-Run quota lease. The existing terminal representation is first saved as a
private preparation, then its exact body, checked-review history response and
final resolution are published with the same reference. Retries verify and
reuse existing references and bodies; conflicting metadata or bytes remain
integrity errors. The preparation is never settlement evidence. Storage and
publication errors propagate; they do not turn a recoverable response into an
unavailable result.

Explicit member recovery is an internal Rust service/port operation under the
original aggregate owner and member Controller. It borrows or acquires the
member lifecycle lease once. A definitively lost checked reader with a proved
absent worker takes a history-only path before normal worker construction.
That path validates an already-running retained Profile Server and sends only
initialize/initialized and the original Thread's solicited history read. It
starts no server, worker, Thread or Turn, recreates no reader and performs no
writer transition. A valid terminal permits unavailable publication when the
original checked evidence is definitively lost; storage or binding errors
remain retryable errors. Normal checked-reader startup refusal remains intact.

Startup and pre-Turn sweeps retry only verifiable final resolutions. Explicit
recovery resumes Paused reconciliation debt in the same call and propagates
settlement errors. Collect and identity remain observations. Task terminal,
retained result or marker, round acceptance and workload revision commit in
one guarded transaction; a racing loser rolls back and verifies the same
committed task. Counted rounds are never returned by convergence. Writer
cleanup and release retain their existing owners: release returns blocked or
recovery while work is unsettled, drops its lease, and can resume its original
operation after separate owner recovery. Convergence does not retire Profiles
or publish release completion. A task without a recorded Turn remains DF-023;
terminal-only recovery without reconciliation or fresh history remains DF-024.

Only deterministic content conditions become the immutable typed-unavailable
result: `COMPLETE_RESULT_UNAVAILABLE`, `STORAGE_CAPACITY_EXCEEDED`,
`REVIEW_TARGET_UNSAFE_PATH`, `REVIEW_TARGET_UNSAFE_FILE`,
`REVIEW_TARGET_LIMIT_EXCEEDED`, and a corrupt or unverifiable published
payload. Every other fault propagates, the task stays running, and the
worker's terminal funnel or the restart sweep retries the settlement. The
unavailable marker is one `dolgorae.complete-result-unavailable/v1` object
whose reason is `complete_result_unavailable`. An unavailable result blocks
identity export for that task only: `collect_*` and
`get_*_delegation_identity` return the typed unavailable condition instead of
an identity with a null or substitute digest, while `idle` remains defined
over running, queued, claimed, dispatching and unresolved work, and a member
whose result is unavailable can still reach `released` with its
`member-release-proof/v1` (MDC:459-472, MDC:625-633).

## Round accounting and disposition

Round accounting lives in `caller_scope_assignments`, separate from receipt
state. One row per accepted assignment, keyed by the complete
five-reference caller scope, with one immutable `max_rounds` per scope
(between 1 and 32). `rounds_used` = `counted_unresolved` + `counted_accepted`
over the entire caller scope — every member and aggregate under it, never per
executor aggregate. The only transitions are
`counted_unresolved → counted_accepted` and
`counted_unresolved → proved_nondispatch`; `counted_accepted` is final for
counting. Replay, wait, collect and lookup never transition and never change
the count. A proved non-dispatch after acceptance (Case B: a dispatch
refusal proved after keyed acceptance, which leaves the receipt untouched)
preserves the
accepted intent and its receipt byte-for-byte and moves only the accounting
row; a refusal before acceptance (Case A) creates no accounting row. When
`rounds_used >= max_rounds`, admission refuses with a typed denial and
neither a task nor an accounting row exists. A settlement from the task's own
durable Turn terminal proves the dispatch and resolves
`counted_unresolved → counted_accepted`, so a member with a once-uncertain
dispatch can become idle again; uncertain dispatches that never produce a
Turn remain the release path's recovery obligation.

`idle` requires an active member with zero running, queued, claimed,
dispatching and unresolved work, no pending interaction and no pending
cleanup, observed after the collected result —
`observed_after_result_digest` equals the collected `result_digest`
(MDC:625-627, RPC:396-423). The read-side idle computation cannot see a
Run-side pending interaction today: the store-level quiescence flag stays
`false` (the v0.1.4 limit stated under managed services), and the release
gate is the only reader of the Run's own durable interactions. An
asynchronous write terminal leaves the writer held as pending owned cleanup,
which blocks `idle` until the member's next authorized mutation or its
release settles it.

## Managed successor services

Both owners apply the same checked-v4 qualification, task-reader binding and
source-assurance settlement rules described above.

The managed Broker services — `spawn_specialist`, `assign_specialist_task`,
`await_specialist_tasks`, `collect_specialist_task`,
`get_managed_delegation_identity` — authenticate the session scope: the root
Run is the caller's source Run and its Controller binding is the authority.
`spawn` accepts only a verified installed snapshot as its Role and
configuration source; there is no session approval step and no session
Specialist Policy limit on the successor spawn, because the installed
provisioning under explicit authorizing scope supplies the accepted Role and
configuration (MDC:136, MDC:604). Broker execution rows use internal keys
(`successor:<operation_ref>`, `successor-task:<task_ref>`) and the
`dolgorae.successor-spawn/v1` / `dolgorae.successor-task/v1` markers; the
caller's key lives only in the successor intent store, and a caller key
already used by the session's Broker rows is refused at admission and lookup.
Successor members are marked by `brokered_members.activation_policy =
'successor'`.

Assignment gates membership state inside the acceptance transaction and
records the accounting row in the same transaction. The assignment fence is
the scoped membership counter; assign advances only the member workload
revision. The one-active-task wall maps to `RUN_STATE_CONFLICT` (Case A).
Successor spawn and assign acceptance transactions re-read the session and
refuse Case A `RUN_STATE_CONFLICT` after a close intent has committed — new
Broker admission stops at that commit boundary
([README](README.md), close contract) — discarding any pre-acceptance member
credential.

Legacy readers keep their shape: `members()` and `session()` exclude
successor members; the legacy task readers, `collect_results` and the
observation task counts filter successor rows; a legacy read that names a
successor task gets `SPECIALIST_TASK_NOT_FOUND`, and a legacy key replay that
hits a successor row is `IDEMPOTENCY_CONFLICT` without decoding it. Both
legacy session close paths, `begin_session_close` and
`close_external_engagement` for successor hire members, refuse
`SUCCESSOR_MEMBER_UNRELEASED` before any read-for-mutation or write while an
unreleased successor member exists; a keyed legacy member still closes and
releases exactly as before.

Managed `canonical_workspace_write` assignments in Git workspaces acquire
the member's writer after acceptance, using its own Broker Controller carrier
and the ordinary Controller and revision checks. MD2 still refuses non-Git
writable delegation before any intent. A foreign holder's `WRITER_BUSY` is
proved non-dispatch: the receipt stays byte-identical and only the round is
released. A pending member transition, an unobservable writer or a failed
release remains uncertain; the Primary's writer is never taken over.

Both owners record the dispatch index and baseline together under writer
authority before Submit. Their manifest-4 Specialist workers publish the
normalized MD1 file-change payloads. Complete change sides are retained in the
same transaction as the task terminal, result, workload revision and round
accounting. Deterministic content and quota conditions produce the
typed-unavailable result; storage and integrity failures roll the attempt back
and propagate for retry. Writer release follows that commit. An asynchronous
terminal can leave pending cleanup, which the next authorized member mutation
or member release settles. Managed collect and identity read active write
counts in the same observation snapshot.

Normal manifest-4 close for both owners verifies the Reader transition before
the terminal seal. Live external close prepares that transition through the
engagement's writer-release authority. A releasing external member accepts
release and writer-release commands only from its authenticated owner while
the settling release row names the verified current lifecycle-lease witness.
Other active-member commands remain refused during release.
Already-Closed managed writer cleanup is a separate release-only path:
it requires the lifecycle lease, authenticated Broker authority, and exact
owned-generation cleanup verification. Ordinary Closed-Run writer operations
retain their lifecycle refusal. Absent-generation retirement uses the caller
workspace rules for both manifest generations 3 and 4.

Managed R services report `pending_interaction = false`
today — the same current limit as the external side — until TASK-079 makes
the field reflect actual state (RPC:288).

## Member release owners

Both `S` release owners — external `release_external_specialist` and managed
`release_specialist` — share one settlement. The serialized order is:
authenticate the scope authority (the engagement's owner binding, or the
workspace and the session's root Primary Controller binding) → read the
retained release row before any lease (a `terminal` row answers with the
settled projection and the original `operation_ref`, no mutation, a stale
`expected_revision` ignored) → for a missing row, one admission transaction
that checks the existing row before the revision fence, fences the scoped
membership counter, inserts the `requested` row, moves the member to
`releasing`/`retiring` and appends the event → acquire the member Run's
lifecycle lease → re-read the row under the owning serialization.

The lease result decides (RPC:314-318, RPC:433-456). A successful acquire
means this attempt owns the freshly written witness: it records `owner_ref`
as the digest of its own witness and runs the settlement. A busy acquire
evaluates the held-owner verdict — `HeldVerified` with a digest matching the
row's current `owner_ref` is a verified active owner answered as typed busy
(`RELEASE_IN_PROGRESS`, no mutation); a mismatching digest or an unverifiable
witness (`RELEASE_OWNER_UNPROVEN`) is busy-or-recovery with
`restore_state`, and a lock that became free is retried once. A live owner is
never taken over; a stopped owner's acquire succeeds, so the takeover
replaces `owner_ref` with the new witness while `operation_ref`, the
initiating principal and generation and the admitted revision are preserved.
The Controller handoff that exists in v0.1.4 is the managed one: a controller
reset of the session's root Run leaves a later-generation binding that is the
current scope authority, whose Controller may take over a stopped release,
and the readers accept the proof's historical initiating fields (RPC §4,
RPC:303-336). The external owner has no handoff — the engagement's owner
binding is fixed at open and digest-bound, so only the opening Controller can
release, collect or export.

The settlement runs while the lease is held, in one order (RPC:338-394):
quiescence — any running, queued, claimed, dispatching or unresolved
work and any pending interaction blocks the terminal and keeps the row
settling; the non-retiring close (managed runs close through the inner
close path under the member's own Broker credential; external runs answer an
`ExternalClose` control request or, without a runtime record, the same
offline close — neither calls a retiring wrapper); the shared owned-cleanup
verifier, run even when the Run is already `Closed`, classifying a present,
absent, mismatching or generation-absent runtime record (never a silent
skip); writer verification (a guard the member still holds, including a
blocked-unknown one, is `WRITER_CLEANUP_UNSETTLED`); explicit Profile
retirement to `released` without terminating, restarting or replacing a
shared Profile Server; then one atomic terminal transaction — the member's
terminal state and event, the scoped counter +1 and the release row frozen
`terminal` with both completion revisions, with the quiescence proof retaken
inside the transaction. Only then does the lease drop, and only a terminal
row cleans the external member's isolated root.

Every classified failure records its `member_release_operations` failure row
in its own small transaction with the completion revisions left null
(RPC:198-217). The seven new safe codes are `RELEASE_LEASE_UNVERIFIED`,
`RELEASE_OWNER_UNRECORDED`, `MEMBER_RUN_CLOSE_UNVERIFIED`,
`OWNED_CLEANUP_UNVERIFIED`, `WRITER_CLEANUP_UNSETTLED`,
`PROFILE_RETIREMENT_FAILED` and `RELEASE_TERMINAL_UNSETTLED`, with
`recover_run`, `restore_state` or `reconcile_operation` as their required
actions. Errors after admission keep the `operation_ref` and the unresolved
effect floor.

`member_release_proof/v1` is re-derived from the frozen terminal row on every
read and validated against the revision separation —
`admitted_membership_revision < terminal_membership_revision`, the terminal
at most the proof's membership revision, the settled workload at most the
proof's workload revision (RPC:283-290); a violation is
`RECOVERY_REQUIRED` / `reconcile_operation`. A keyed operation receipt is not
a release proof, and a released member requires one — a genuine
K-receipt-substitution attempt is refused by the schema branches. After a
terminal release, `collect_*` and the identity exports answer `released` with
the original proof and no durable write (release-before-collect,
RPC:396-423); a terminal row with residual work or pending cleanup is a typed
contradiction. The managed R reads and the managed release itself
authenticate the root Primary, the workspace and the root Controller binding
without the active-session precondition (RPC §7.1-7.4), so a repeat release
answers after the session closed and a retained close intent with an
unreleased member is reconciled by releasing that member. Every terminal
answer — including the busy branch's — re-runs the idempotent post-terminal
cleanup: the external member's isolated root and the managed member's retired
Broker credential.

Two owned-work states can never settle on their own and end in the typed
recovery row `recovery_needed` / `OWNED_CLEANUP_UNVERIFIED` / `recover_run`
instead of an endless settling answer: a dispatching task whose Turn id was
never recorded, and a bound Turn whose durable terminal exists without its
resolution record — that window, decided per task Turn id against every
recorded `turn_terminal` payload, not only the Run's last terminal. A
still-running Turn keeps the settling answer, and a repeat release resumes
from the durable state once the dispatch's response path records the Turn id
or the worker writes the resolution.

## Generation-absence proof

The runtime locator has exactly one remover, and that remover durably records
a verified-absence marker — write-once per Run and generation, bound to the
recorded worker identity digest, atomically renamed and fsynced with private
modes — for exactly the generation it verified absent **before** removing the
locator. A missing locator is therefore explained only by that verified
cleanup, a Run that never started a worker generation, or an out-of-band loss.

The successor settlement's locator-missing proof (RPC:349-366) holds the
Run's startup range, requires the worker range unowned and no remaining
process-inventory registration of the Run in any state, and binds the marker
to `G`, the highest generation evidenced by the durable facts that exist
before any process of a generation runs: the projection's `run_generation`
and the generations in the slot-0 and slot-1 startup owner records. `G == 0`
(no evidence of any attempt) is proven; otherwise a verified-absence marker
for exactly `G` — identity-bound to the slot-1 record of `G` when one exists
— is proven. An unreadable slot record is not "absent": it blocks. Anything
else is `OWNED_CLEANUP_UNVERIFIED` / `recover_run`, never terminal. The proof
mutates no evidence: the startup range is released with the slot-preserving
byte-range unlock only, so a slot record that blocks one attempt blocks them
all. Legacy external close callers keep HEAD's behaviour — a missing runtime
record is skipped — through the shared verifier's `MissingLocatorRule::Skip`;
only the successor settlement uses the strict rule.

**Residual.** No census of recorded process groups is claimed: detached
descendants in other sessions or groups cannot be enumerated without the
locator's recorded scopes, and unregistered detached grandchildren outside the
two recorded process groups are outside this guarantee on both the
record-present and the locator-missing paths. A generation whose only durable
trace was its runtime locator — it shut down cleanly before any ledger
append and its locator was then removed out of band — cannot be detected;
out-of-band deletion of Dolgorae's private runtime state is outside this
guarantee, and the in-band remover always leaves the marker.

## Preserved legacy behaviour and current limits

Legacy caller orders are preserved by construction. `run_control_admitted`
still calls its inner close path and then the Profile retirement in the
previous order for every existing caller; `mutate_run` and `mutate_bound_run`
keep the retiring order; `finish_external_run_close` still runs the
owned-cleanup verifier and then the global membership release for its four
external callers; `lifecycle_owner_active` keeps returning its boolean for its
current callers; `member_task_state` keeps its behaviour and callers; the
legacy keyed close receipt, the legacy `operations` replay helpers and the
legacy facade-v4 CLI surface are not rerouted; the legacy managed
`request_specialist`/`assign` admission keeps its pre-check-only shape; and a
keyed legacy member still closes and releases exactly as before. The legacy
external close skips a missing runtime record without requiring a marker.

Current limits and residuals: a crash between the successor spawn's
credential mint and its acceptance commit leaves one inert mode-0600 member
credential file with no member row and no Run — the successor path starts no
Run before acceptance — and v0.1.4 does not reclaim it, because a race-free
reclaim needs a durable mint record neither spawn path records (the
workspace-wide sweep that would reclaim it could delete another session's
in-flight credential and was removed). Legacy Broker admission checks the
session only before its reservation transaction, so a legacy member admitted
after a completed close is uncovered — pre-existing, unchanged by TASK-076.
A session with only successor members reports the legacy composition state
`standalone_primary`; the legacy session observation still includes successor
rows in its member, nonterminal-spawn and pending-approval counts while its
task counts filter successor rows like every other legacy reader (projection
only); `source_turn_id` and
`source_tool_call_id` are recorded as provenance without ledger verification
until TASK-077 supplies authenticated values; and an
`ACCEPTED_ASSIGNMENT_MISMATCH` is classified as an uncertain dispatch, not a
proved non-dispatch.
