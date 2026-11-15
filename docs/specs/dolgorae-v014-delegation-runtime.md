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

## Public Primary native specialist preparation

New public caller-workspace Orchestrated Primaries initialize a protected
native-only specialist owner before starting their first Worker. The owner
binds the actual Root, current Controller and accepted Profile executable;
external MCP host qualification remains a separate authority. Bootstrap
accepts no Specialist or Task and creates no Podway caller scope. Existing
explicit managed connections take precedence and must validate; an invalid
connection never falls back to native-only. Accepted Threads retain their
original catalog. A Root with prior Worker or Thread evidence and no native owner
is not bootstrapped on replay. Repeating an unfinished original creation resumes
completed initialization steps, while conflicting or corrupt retained state fails closed.

The managed native tool adds private `prepare_provisioning` and
`prepare_assignment` operations for automatic native-only owners. Explicitly
connected delegated owners retain their existing preparation route. Inputs contain a policy role reference, the
eventual operation key and, for assignment, member and task content. The
producer derives all paths, scope, Controller and Profile fields. These
operations return the source or AcceptedTask identity used by the existing
install, spawn and assign operations. Preparation accepts no Task, consumes no
assignment round and dispatches no work. Original-key lookup and retained
preparation precede fresh capture; changed input under the same key conflicts.

Context status succeeds in native-only mode and returns the actual managed
scope plus requestable read-only roles from the accepted policy. This bounded
catalog allows 8 KiB of native metadata for up to 64 role references; other
native metadata retains its 4 KiB limit. Private preparation replies allow
256 KiB of identity metadata so legal context vectors and original-key lookup
receipts remain complete. They contain no original result bodies.
Eligibility requires fully delegated
approval, Primary request permission, automatic approval, activation other than never,
and read-only access. Disabled roles remain unavailable. Automatic native
owners enforce the same role and access conditions on install, spawn and
assignment, including policy instance/session limits. Every non-retired member
counts toward those limits, including provisioning, degraded and retiring members.
The Primary's legacy `request_specialist` path shares this count with native
spawn, and checks it again inside member reservation, including approved requests.
Legacy member views and reuse remain separate from native membership.
A fresh approval resolution commits with its member reservation; a capacity
refusal leaves the decision pending so it can still be rejected.
Ordinary Policy refusals carry `fix_request` and no effect; definitive refusals
inside acceptance resolve the original key as not accepted. Uncertain failures
retain their recovery floor. Existing delegated/external context-status replies
keep their original shape. Native-only accepted tasks
retain an authenticated Root/Controller origin, independent of Podway round
accounting. Delegated owners retain their original accounting and proof rules.

The [private managed preparation schema](../protocol/dolgorae-managed-preparation-v1.schema.json)
preserves a root object with an operation discriminator. Preparation replies use
`dolgorae.managed-preparation-response/v1`, including refusals; a private
preparation operation never claims the frozen external facade-v5 response
schema, including refusals before dispatch. A malformed or missing private operation
is reported as `operation:null`; successful replies always name one of the two
private operations. External facade-v5, H1 and public-v2 descriptors are
unchanged. Public Broker collection remains the complete-result owner. Native
JSON string presentation must be decoded before interpreting its result;
diagnostic campaign failures are not qualification passes.

### Supported public procedure

Create the Primary with the public-v2 StartRun provisioning and its accepted
Markdown Specialist Policy. No owner file or administrative specialist setup
is part of this procedure. Inside that Primary:

1. Call `dolgorae_specialist_consumer` with
   `{"schema":"dolgorae.consumer-control/v1","action":"status","target":{"kind":"context"}}`.
   Read `data.scope` and `data.allowed_role_refs`. An empty list means the
   accepted policy authorizes no native read-only role.
2. Call `dolgorae_specialists_native` with
   `{"schema":"dolgorae.managed-preparation/v1","operation":"prepare_provisioning","role_ref":"reviewer","idempotency_key":"install-1"}`.
   Use the returned `sources` in facade-v5 `install_provisioning`, with the
   same key and `expected_revision:null`.
3. Use the installed candidate's provisioning reference and configuration
   digest in facade-v5 `spawn_specialist`. Copy the returned managed scope
   from status/preparation, supply the admitted role and a distinct spawn key.
4. Call private `prepare_assignment` with `member_ref`, `task_body`,
   `deadline_seconds` and the eventual assignment key. Use its
   `accepted_task_identity` with the identical task, deadline and key in
   facade-v5 `assign_specialist_task`.
5. Await and collect through the native tool. The Controller can independently
   collect the same task through public-v2 BrokerService.CollectSpecialistTask.
   Read artifact representations through ArtifactService using their returned
   owner reference; check complete bytes, byte length and SHA-256.
6. Release the member with its current membership revision before Root Close.
   A result or successful Close does not itself prove physical cleanup.

Code-mode tool results can be JSON strings. Decode the correlated original
result before reading its fields; do not infer success from presentation alone.
An accepted or unresolved original-key lookup is recovery information, never
permission to issue a fresh key. These private preparation operations are not
available on the external MCP facade.

The accountless native fixture is opt-in: after runtime authorization, set
`DOLGORAE_RUN_NATIVE_CALLER_PIPELINE=1` and `DOLGORAE_CODEX_BIN` to the selected
qualified Codex executable, then run `make test-native-primary-specialists`.
It runs the allowed and disabled Policy cases separately, asserting the exact
conflict/refusal contract and native release plus member retirement. The default
repository gate exercises only offline checks and does not start this runtime.

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

## Delegation namespace and Engagement database generations

TASK-076 moved the workspace Engagement database from base generation 5 to 6
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
5→6 transaction, from the same DDL source. This established the generation-6
namespace; the current writer then applies the fence below.
`metadata.orchestration_schema_version` stays `3`; the
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
observed read-only without touching the database. Unsupported base and extension
generations are refused before any effect, with no sidecar, no maintenance lock,
no mode change and no `sqlite_master` change. The generation-6 implementation
refuses base generation 7 and extension 4. The current admission rules below
extend the supported base generations without changing extension 3.

### TASK-084 writer fence: generation 7

Generation 7 protects research-group capture charges, reserved Run births,
Controller group origins and result-component bindings from older writers that
do not account for those records. A writable open upgrades a verified generation
6 database in one `BEGIN IMMEDIATE` transaction. It rechecks the version, records
the generation-7 extension marker and advances `metadata.schema_version` to `7`.
The six generation-6 tables, existing rows, namespace birth ID and orchestration
extension 3 retain their identities and bytes. No result or capture is backfilled.
An interruption leaves either the complete generation-6 state or the committed
generation-7 state; retry never repeats an accepted group operation.

Fresh databases use the same historical bootstrap and 5→6 migration, followed by
6→7 before writable use. Supported older databases retain their existing upgrade
path through generation 6. Read-only opens accept verified generation 6 and 7
without upgrading either; writable opens finish the upgrade before accepting
new work. Unknown versions, incomplete markers and inconsistent shapes fail
closed through the existing effect-free admission probe. Generation-6 writers
refuse a generation-7 database before effects. There is no downgrade.

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

### TASK-085 writer fence: generation 8

Generation 8 adds immutable merged-artifact charges and synthesis input bindings.
A writable open upgrades a verified generation 7 database atomically, retaining
its group marker, base-six tables, namespace birth and orchestration extension 3.
It records `research_merge_schema_version = 1` before accepting merge or
synthesis work. Read-only opens support verified generations 6, 7 and 8.
Generation-7 writers reject generation 8 before effects; incomplete or unknown
markers remain typed schema refusals. No source content is backfilled.
Historical migration steps keep their literal target generations and are no-ops
when already applied to a verified generation 6, 7 or 8 database. They preserve
existing rows, namespace birth and extension markers. A historical generation
carrying a later merge marker still fails admission; fixtures representing old
databases must remove that marker with the other later-generation metadata.

The first new merge upgrades the internal merge marker to `2` within its normal
`IMMEDIATE` acceptance transaction. It retains a fixed
`research_merge_origin_cutoff` bound to the database namespace birth and the
operation AUTOINCREMENT high-water. An accepted original-key replay or read
never upgrades state. The live Primary retains its shared workspace lease and
Profile connection; this metadata/trigger transition requires no exclusive
workspace lease, process stop or physical generation replacement.

A verified accepted operation at or below the cutoff retains its historical
Controller provenance. Above it, every accepted merge requires its compact
invocation record in the same transaction as publication and receipt. Missing
new provenance requires recovery, never a legacy-origin fallback. Marker,
cutoff and guard shapes are checked together. New writers read markers `1` and
`2`; marker-1-only fresh opens reject `2`. Persistent SQL acceptance guards also
refuse a pre-opened old producer that omits the required record. The original
operation/receipt bytes and historical input/result identities are unchanged;
there is no backfill or new public digest domain.

### Protected host setup and invocation ownership

The private administrative entry point `__trusted-specialist-host` separates
`owner-config-create`, `namespace-setup` and `host-state-init`, in that order.
Each step authenticates the current Operator and retains its authorization hold
through publication. It also verifies the configured Controller carrier and
canonical workspace. Managed configuration additionally names an existing
session and its root Run Controller. The Root manifest must carry that session's
Primary aggregate binding, bootstrap operation and policy digest, in the same
workspace. External native calls cannot use managed configuration. Setup does
not install a host or edit host configuration.

Owner configuration lives outside the workspace in an existing private root.
It pins the historical route identifier separately from the current executable
version/hash, workspace, Controller generation/carrier, allowed aggregate scopes,
the five caller references and budget, and adapter provenance. Its credential
and isolated native-host journal root remain private. Configuration is immutable:
repeating creation must match the original canonical bytes. Host state binds its generation
to that configuration and the existing delegation namespace birth. Normal hook,
delivery and lookup paths neither initialize nor repair missing state. A partial
or mismatched host database requires recovery; initialization never resets it.
Only a verified complete setup can supply a host launch recipe. Administrative
configuration requests and stored owner configuration each have a 128 KiB raw
JSON limit.

Public requests are limited to 16 MiB in both raw UTF-8 and canonical JSON.
The frozen per-field bounds also apply; the aggregate limit does not enlarge
individual fields. Private native records include additional identity and
linkage metadata and are not subject to the public-request byte limit.

The host store keeps a native-event map for every external call. It preserves
the canonical public request, checked trusted-invocation context, native session,
optional native Turn, tool-use, server/tool and a separate random 256-bit
capability. Equal native identity/input retains its UUID and capability; changed
input conflicts. The native key includes route, session, optional Turn,
tool-use, server and tool.
A retained session/call cannot be rebound to a different Turn/server/tool.
Codex delivery must select exactly one session/call binding.
Claude delivery must match the dynamic `dolgorae` origin and private capability;
the capability is removed before public-request processing. Neither route can
recreate a missing binding. Raw capabilities have no Debug/ordinary-log or
export representation. Claude's native journal is protected identity state,
inside the configured isolated host root, never public evidence.

Only K methods have a product linkage in the checked
`dolgorae.protected-invocation-state/v1` shape: its namespace contains the real
request key, and receipt/operation reference remain null before acceptance.
The product Engagement transaction is the sole acceptance authority. The native
mirror can lag that transaction and is restored only from authorized original-key
lookup. Checked persisted UUIDs require canonical lowercase, hyphenated UUIDv7
text. This also applies to the original serialized mutation envelope before
typed decoding; alternate encodings are invalid. The storage API preserves accepted,
unresolved, definitively refused and absent lookup outcomes separately. It cannot authorize redispatch. Product
intents retain the safe context beside the envelope's matching `trusted_invocation_ref`; non-host callers keep
null context. No Engagement schema migration is added.

R methods retain only the native binding and perform a fresh observation on
each delivery; no cached result or timeout is authoritative. S methods retain
the original target/input and commit a delivery marker before effect. Repeated
or ambiguous delivery requires the method's authoritative getter, never a
transparent mutation retry. Neither R nor S receives a synthetic key, K intent,
receipt or checked K invocation envelope. Full facade decoding, transport and
getter/service dispatch remain separate adapter boundaries.

Caller/member Writer release retains the original external opening owner or
managed Root/Broker authorization. On the exact qualified caller-workspace
binding it retires the whole old physical generation, verifies the original
thread/catalog/model on a reserved Reader successor, then retires Writer
atomically. This applies to both Primary and Specialist Runs. Logical Run,
Role/configuration, task acceptance, original result/receipt and caller-scope
accounting remain unchanged. This physical transition adds no native Turn;
Paused remains Paused. Existing cancellation proves only its accepted-before-
reservation no-dispatch case, not native quiescence. Member settlement continues
through its original release owner and borrows that owner's lifecycle lease.

State access uses current-user 0700 directories and 0600 regular single-link
files, descriptor-relative no-follow traversal and physical-identity checks.
Journal verification permits at most 10,000 descendant entries and refuses an
entry encountered at nesting depth 64; it neither truncates nor deletes history.
The protected root serializes initialization and native-map updates; SQLite
commits use full synchronization. An unpublished interrupted database is retained
as recovery evidence, not silently overwritten. No delivery creates a missing
lock, database, namespace or owner configuration.
The private database publishes from an exclusive staging file only after its
metadata transaction is durable. Initialization refuses retained staging files and SQLite sidecars for either
the final or staging database name, even when the final database is absent. Initialization checks the original namespace birth, owner
configuration digest and random safe-integer generation. Ordinary access checks
the same metadata and physical database identity, without CREATE or migration.
K mirror reconciliation accepts only an authorized lookup result for its exact
original namespace. Reconciliation never authorizes dispatch. A separate durable
K delivery marker, like the S marker, makes an ambiguous first delivery require
lookup before any later dispatch decision. R has no delivery result cache.

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
| `spawn_specialist` | the same derivation over the Broker member row and its Run residency; an original spawn still marked `recovery_required` remains `unknown`, including retained degraded/unavailable members |
| `assign_*_task` | derived from the original Task and its validated assignment claim: `pending`, `running`, `completed`, `failed`, `cancelled`, `expired`, else `unknown` |

An error starting an already accepted external hire or managed spawn may precede
the member Run's manifest. It returns `Unknown` with the schema's `unresolved`
effect floor and `lookup_original_key`, retaining the original accepted receipt,
member and cleanup owner. The response preserves only a closed safe cause code;
runtime messages and private details are not copied. Original-key replay returns
the same admission without starting another Run. A missing manifest is neither
proof of no effect nor a reason to promise `recover_run`, whose entrypoint requires
that manifest. Unresolved members cannot supply idle or released disposition.

While an accepted member start remains unsettled, admitting its release preserves
that uncertainty: original-key lookup remains `unknown` after release admission.
A late managed start publication records the original spawn outcome without
changing `retiring` or `retired` membership, the release proof, membership revision
or credential retirement. A start already settled successfully remains
`completed` while its separate release is nonterminal.

An accepted assignment remains `accepted` when its dispatch outcome is unknown.
In one read snapshot, the provider validates the original receipt namespace,
Controller, workspace, scope, member, Task identity and execution intent. A
`dispatching` Task with no bound Turn yields `current_outcome = unknown` only
when its own valid assignment claim has phase `unknown`, no Turn and blocker
`dispatch_outcome_unknown`. The detailed Task phase and immutable receipt remain
unchanged. Terminal Task facts and a known positive Turn retain precedence;
foreign or contradictory claim metadata is a typed recovery refusal. A claim
for another Task on the same member cannot taint the original operation. Lease
expiry neither changes the outcome nor permits dispatch. This metadata lookup
does not reopen retained source payloads or mutate scheduler state.

`result_ref` and `result_digest` are populated for task kinds from the
settled CompleteResult. Ordinary task lookup returns a reference to its exact
canonical JSON bytes. The digest uses the `dolgorae.complete-result/v1` domain and differs from the
content hash. Lookup joins the original receipt, accepted task, member, scope
and terminal outcome in one read snapshot. It uses retained publication identity
and content-reference metadata after body retirement, without reopening task
inputs or performing recovery. Historical live result identities may be
validated from their retained result JSON. A historical ordinary result without
separately retained publication identity stays protected from retirement, so
maintenance cannot erase its last lookup identity. Planning and eligibility
checks remain read-only and mint no artifact identities. If a historical retired
identity cannot be recovered, or retained identity and task facts disagree, lookup
returns a typed recovery error. A typed-unavailable publication supplies no
CompleteResult reference. `install`/`open` carry none. `adapter_id` and
`adapter_version` stay provider-owned and are never derived here (MDC:594-597).

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

## Protected source preparation

Private host administration initializes preparation state, registers sources
and projects their identities. It requires the current Operator and the
configured Controller, an initialized host store and the exact workspace
namespace birth. Managed preparation also verifies the accepted Root/session,
bootstrap and policy binding. Preparation creates no product intent, receipt,
Run, task or round and grants no delivery authority.

Immutable registration binds the owner configuration, physical workspace,
namespace birth, Controller principal/generation, caller references and budget,
adapter generation and managed owner to explicit instruction or target
selection and its complete capture. Equal public identities with different
selection provenance conflict. Existing records and interrupted publication
remnants are preserved; ordinary reads never initialize or repair preparation
state. Public identity/request limits remain 16 MiB raw and canonical. Private
selection, snapshot and manifest components have separate finite producer-based
bounds and descriptor-checked private storage.

Provisioning registration retains explicit Inline/File selections, common and
Role bytes, dependencies, ordered guidance, Profile binding and admission.
Fresh validation/install captures the registered selection once and compares the
complete supplied sources identity and registered snapshot before any intent.
Accepted install replay verifies the retained snapshot, normalized body and
receipt without registration, source files or Current qualification.

Both assignment owners authenticate and resolve original replay, conflict or
unresolved state first. For new assignments, owner/member/runtime agreement,
Current qualification and checked-review admission precede live target
resolution and task capture. The exact registered selector is re-admitted in
the authenticated workspace. Its complete manifest and the generated accepted
task identity must match registration and the supplied identity before intent
creation. The checked capture enters the existing atomic content, identity,
round and receipt transaction without a second capture. Facade replay verifies
all supplied identity fields against retained bytes and the original receipt;
it needs no live registration or current Git ref. Typed callers retain original
target-presence, kind and requested-revision comparisons. Changed input
conflicts; corrupt retained proof requires recovery. Private preparation fields
never enter the public normalized mutation body.

Preparation records selections for the native adapters; it does not establish
delivery or consumer acceptance. Results and remaining qualification are recorded
under [Current host and Profile runtime qualification](#current-host-and-profile-runtime-qualification).

## Native delivery and managed Worker dispatch

The stdio server supports MCP `2025-06-18`. An otherwise valid initialize proposing another version receives that supported version; the client decides whether to continue. Required initialize fields remain mandatory. Request params may carry object `_meta`; its optional `progressToken` is a string or integer and does not require progress notifications. Initialized notifications use their separate object metadata definition. Explicit null params are invalid. Inert protocol dictionary extensions are accepted where the selected method and common request definitions permit them. Tool listing returns one complete page and issues no cursor. Public facade arguments keep their closed grammar. Ignored protocol metadata supplies no native identity, caller scope or product authority.

Incoming MCP requests require string or integer IDs; a present null ID is invalid and cannot initialize or dispatch. Valid notifications omit the ID. Parse and invalid-message error envelopes may use null when the request ID is unknown.

Managed native completion preserves the complete successor response or failure body. A failure sets the native success flag to false, including an unresolved post-admission outcome with its original operation reference. Legacy Primary result/error projection remains unchanged. Full valid collect metadata must cross the native JSON-string wrapper without truncation or smaller producer limits; large outgoing messages use bounded WebSocket frames whose entire message is serialized against other senders. Input and per-frame limits remain independent.

Native Root registration emits the selected function-tool wire tag `"type":"function"` for both orchestration and successor tools. MCP tool-list metadata keeps its separate shape. Specialist and consumer-control input schemas declare the root `"type":"object"` and require their discriminator. The checked alternatives are its `dependentSchemas` constraint, so host APIs receive no top-level union while the complete operation constraints remain intact. The legacy orchestration request schema and behavior are unchanged.

New Root Threads advertise `dolgorae_specialists` separately from the legacy orchestration tool. `managed-connect` binds an explicit protected managed owner configuration to its existing Root Run under Operator and current Root Controller authorization. The Worker resolves this connection at each call, verifies the original Root, generation, Thread, Turn and tool call, and passes the public operation key with the authenticated native context. It never derives authority from a public scope reference. Connection setup does not admit work. A missing connection refuses successor calls; legacy calls keep their existing behavior. Selected Codex `thread/resume` has no dynamic-tool registration field, so reconnect retains the Thread's original tool set. A Thread created before successor-tool registration remains a legacy Thread; it is not silently replaced. Managed calls retain normal non-host null invocation context and never fabricate an external UUID or carrier. Paths and caller configuration come only from the protected connection.

The private `host-hook` and `host-mcp` entrypoints consume an explicitly configured
owner file. The synchronous native hook commits its binding before allowing a
tool call. Codex selects it by native thread/call metadata. Claude uses only the
hook-injected private capability, removed before public processing. Model schemas
contain only the selected facade-v5 request branches. The adapter negotiates
`2025-06-18`, the selected native protocol, and exposes neither future methods nor
credentials, configuration paths or private carriers in ordinary output.

Every external delivery authenticates its exact native input, server/tool, owner,
scope and generation before product dispatch. K services receive the sealed
trusted context; their existing intent and receipt transaction remains the sole
acceptance authority. A delivery marker commits before a mutator runs. Repeated
ambiguous delivery performs authorized original-key or state observation only;
it cannot retry a mutation transparently. Read methods always observe afresh.
Native group assignment is K: its protected linkage names the original
`assign_group` namespace and the immutable group receipt. Native `await_group`
and `collect_group` are R and authenticate each observation without admitting an
operation, charging a round or resuming a child. Both external host carriers and
the managed Worker use the existing group service and its aggregate owner proof.
Original-key lookup verifies the accepted group receipt, captured manifest and
normalized input in one read snapshot. It returns the group identity with
`pending` and null result metadata. The group has no single Task or CompleteResult;
current child outcomes belong to await/collect coverage.
Lookup never reopens assignment files or provisions or dispatches a member.
Re-delivery of the original protected logical invocation retains its complete
accepted envelope and native proof. A fresh native K invocation with a different
`trusted_invocation_ref` under an accepted key returns `IDEMPOTENCY_CONFLICT`,
even when its normalized assignment and caller scope match. Fresh authorized R
lookup reads the original receipt without rebinding that mutation.
Controller-origin replay retains its non-native origin. Managed calls retain
their actual Root, Thread, Turn and tool-call checks and original child provenance;
they have no external invocation UUID.

Host locks cover authentication, marker publication and mirror reconciliation,
then release before service operations or awaits. Reconnection and response loss
retain original identities and receipts. Stdio EOF has no cancellation, release
or Profile-stop meaning.

The initial engagement getter projects a complete member set of at most 64 entries, as the frozen response requires. A larger set returns a typed capacity refusal before a close is admitted; it is never truncated or given an unusable cursor. Complete bounded observation beyond that set remains with the consistent-observation boundary. This transport limit does not impose a member-admission limit.

Member views retain the membership/residency projection. Identity export derives
its Worker-disposition proof through [bounded consistent observation](#bounded-consistent-identity-observation); member-view state alone supplies no such proof.

A cancellation or close admitted once is observed on ordinary repeats, including a new native call for the same target. A partially completed close remains `closing`; its remaining effectful continuation belongs to explicit recovery. Errors after state admission retain the original operation reference and an unresolved effect, including a failed commit or a lost post-effect read.

Each fresh member release during close reads the current scoped membership revision before admission. A preceding member's terminal release may advance that shared counter. Already released members are skipped. Close observes a release owner visible at its per-member precheck without resuming that owner. An owner admitted after this read may be joined by the actual release call under the existing RPC §7 convergence rules, preserving the original operation and live-owner busy protection. Ordinary close repeats remain observations. Explicit member-task/history recovery is a separate boundary.

Each private state-owner record checksums its original operation reference, namespace, target, input digest and terminal flag. Reads reject an altered or malformed record without rewriting it. Only the explicit close owner may update its terminal flag and checksum.

These joins belong to `delegation_service`; the legacy close path in `external_engagement` keeps its existing successor-member refusal.

Cancellation fences the scoped membership revision exposed by the member view;
engagement close fences that engagement's revision. Their non-keyed owners use
private versioned records in the existing Engagement metadata table, without a
schema migration, caller key or K receipt. For an accepted task, the authenticated
cancellation owner may prove no Submit by winning a transaction against dispatch
reservation: the retained acceptance matches, no dispatch index or content
reference exists, no task Turn or original Submit admission is recorded in the
Dolgorae Run audit ledger, and its round remains `counted_unresolved`. The
transaction commits `cancelled`, `proved_nondispatch`, the task event and member
workload revision together. Once
reservation or submission may have occurred, cancellation addresses the exact
recorded task Turn at the Worker serialization boundary, so a later task cannot
be interrupted by a stale cancellation. Close first commits its original owner
and the closing fence, then uses the existing member release owners; completion
commits only after every member is released. Lost or repeated delivery observes
the original owner and target without starting another effect.

State controls retain real non-keyed operation owners in the product store and
report their references, never synthetic keyed receipts. Repeated delivery reads
those owners without invoking a mutator. Cancellation acknowledgement is not a
Turn terminal. Missing execution after dispatch reservation, inconsistent
no-dispatch evidence or unresolved ownership requires the original recovery path.
An admission whose effect was not completed stays with its original owner;
ordinary repeats never resubmit it. A closed/released immutable proof can be
projected; active idle proof requires the bounded consistent observation below.
An incomplete interaction read never proves idle or qualifies export.

## Complete results and the changes producer

A complete result binds complete bytes, never a preview or summary
(MDC:450-486). Every final-response source the Turn coordinator resolves is a
complete-result source — inline text, history fallback, or an artifact within
32 MiB whose length and digest verify; a failed or cancelled terminal without
a response is a valid result with a null final response. Checked
`structured_review_v3` output is validated before publication; invalid
findings settle as the failed-output result with `FINDINGS_OUTPUT_INVALID`
and no payload (MDC:471-473). A reader reconstructs the v3 canonical output
from the verified original response and accepted task when that representation
was not retained separately, then verifies its complete-result length and
digest. Missing or inconsistent source bytes remain unavailable. Direct-target manifest capture is supported for
both assignment owners. Checked `structured_review_v4` tasks require an
explicit target and a manifest-4 Specialist admitted with read-only Review
purpose and a qualified checked-review Profile. The initial Thread has the
candidate tool enabled with read-only sandbox and approval never; a general
Thread cannot be upgraded by an assignment.

Terminal publication allocates UUIDv7 identities for the immutable result and
its component artifact bindings in the complete-result transaction. Each
binding names the actual execution Run and retained source without copying
component bytes. Public collection limits inline bodies to 1 MiB. Larger
components and results that require manifest discovery use Controller-only
final-response artifact references.

A `dolgorae.result-component-manifest/v1` JSON root artifact references the
original identity as ordered binary artifact chunks and bounded
`dolgorae.result-component-manifest-page/v1` component-reference pages. The
[result component manifest schema](../protocol/dolgorae-result-component-manifest-v1.schema.json)
defines the closed root and page shapes. Each artifact fits within 32 MiB and
is charged to the actual Run's shared 256 MiB quota.

Queued successor assignment expiry publishes its body-free `expired` complete
result in the same transaction as the terminal task state. Artifact quota is
locked before that transaction. If no artifact capacity remains, a bounded
immutable identity binding retains the real result UUID and digest without
publishing artifact references or charging fictitious artifact bytes. Collection
still returns the terminal identity; artifact-manifest discovery is unavailable.
Repeated expiry preserves the original binding. Observation never backfills
results for historical terminal tasks.

Artifact reads authenticate the current aggregate Controller, verify the
complete source identity and bytes, and return at most 1 MiB. They leave
publication and source state unchanged. Normal maintenance preserves bound
source content.

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

Explicit recovery on an attached Worker must obtain acknowledged exclusive read
ownership before sending its authoritative history request. A quiet native peer
must not prevent that handoff. The existing 120-second operation budget includes
the handoff and history processing; no retry, new execution or buffered
whole-history fallback may replace the original Thread/Turn proof. A failed
history command preserves the existing command-versus-active-Turn loss rules.

Explicit member recovery has a private local administrative entry:
`dolgorae __trusted-specialist-host member-recover --owner-config PATH --scope-ref SCOPE --member-ref MEMBER --task-ref TASK`,
with inherited `--operator-fd` and `--controller-fd`. It requires current Operator
authority, the protected exact owner/workspace/scope and current Controller before
effect. It calls the original external or managed reconciliation service and
semantic port. Successor external native recovery verifies the accepted open,
original assignment receipt, member configuration and execution intent, then
joins the retained trusted invocation to the task's caller accounting. A native
group child instead joins its exact accepted group entry and parent native
invocation, because internal child acceptance has no separate native call.
Missing or foreign proof refuses before native-history effects. Controller
groups keep their distinct retained-origin authorization; successor native work
never acquires legacy external provenance or a Controller-group origin.
An already-terminal assignment does not bypass recovery of its retained pending
Writer generation release. A completed task does not prove that this release
finished. Failure to resume the original release remains a typed recovery
failure; recovery never submits another task or creates a new key.
Managed recovery reads the original accepted assignment's key,
Root, Turn and tool-call provenance; missing or inconsistent provenance is a
closed refusal. The semantic port reauthenticates the actual owner. For managed recovery,
it obtains the member's Broker-held carrier internally. No carrier is returned to the administrator or
model, and the Root carrier is never substituted for the member carrier.

This entry is not an MCP/public operation, new acceptance or receipt. A successful
exit means the explicit call completed; existing collect and identity getters
establish subsequent result/disposition facts. A lost reply is observed through
those getters, without an automatic retry. Lookup, collect, identity and export
never invoke this administrative entry. Recovery retains the member's originally accepted qualification and does not
perform a new Current admission.

The internal Rust service/port operates under the original aggregate owner and
member Controller. It borrows or acquires the
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
(MDC:625-627, RPC:396-423). The service combines the store facts with a fresh,
complete Run interaction observation. The store-only flag cannot establish
absence. The observation folds the existing pending-at-context rules without
copying protected payload lists. An asynchronous write terminal leaves the writer held as pending owned cleanup,
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

Both managed and external R services fold the complete Run interaction verdict
into the consistent database observation. The store-only quiescence flag has no
interaction authority. TASK-079 retains its later answer-routing and disclosure
work.

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

If the member's durable Run state cannot be read, the retained release directs
the owner to `reconcile_operation` through the same authenticated release
surface (RPC §7.6). That entrypoint reads the original release before requiring
Run state; it can preserve the unresolved operation even without a manifest.
It does not promise successful Run repair or terminal release. Completion
revisions stay null until actual Closed, cleanup and retirement proofs exist.

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
only). Legacy `source_turn_id` and `source_tool_call_id` remain provenance
without ledger verification; modern managed Worker dispatch supplies
authenticated source references. `ACCEPTED_ASSIGNMENT_MISMATCH` remains an
uncertain dispatch, not a proved non-dispatch.

## Current host and Profile runtime qualification

The current Profile candidate is Codex `0.167.2`
`97bc82fd46f117cf10efb95e6ff0c81dc4784f5797bcdc2cb75093ba09e4def0`
and companion `45acdb759c3ac0a6b30a5107329b364b7ac1579f2cdf270997f03bc8d6365ef6`.
New qualification work covers this pair. Accountless scripted-loopback checks
establish only the mechanisms they exercise; real-account and model behavior
remain unverified.
Generic `tested` registration requires its own compatibility, access-safety and
Profile launch evidence; a qualified caller candidate is not that evidence.
The earlier results below retain their original images and applicability.
The current candidate's ordinary schema-4 accountless session fixture passed
first and sequential WRITE, typed permission/protected input, original Run/Thread
recovery after idle Worker loss, gateway replacement, idle and consented interrupt
Close, Writer release and separate owned-generation absence. It used nine
scripted loopback POSTs and no external provider or account credentials. This
mechanism evidence does not register a generic tested runtime or qualify the
external Codex host, Gul browser creation or a final replacement H3.

Status: the selected runtime and individual-task paths have the scoped native
results below. Repository verification and Task acceptance are recorded by their
delivery owners.

TASK-077 selects Codex CLI `0.164.0`
(`d8990e3ee7acfbc26f91265b12a1a83f0fee40d0e0f8f1f40a37d4ec21f499cf`) for the MCP
host and caller-workspace Profile runtime, and Claude Code `2.1.310`
(`ffb64331bc0ee1b69efb7c2c428bd3ab516bc31b75bf60886b356c97c1c25ffd`) for the
Claude MCP host. The results below apply to these selected executables;
checksums alone do not establish runtime behavior. The H1 host qualification
remains the historical design-time record. Its route identifiers retain their
checked encoding; actual executable versions and checksums remain explicit.

New qualified caller-workspace startup/policy, instruction-discovery and
checked-review admissions, and member/task acceptances depending on those
mechanisms, require the current Codex pin before their acceptance commits or
launches. An installed provisioning snapshot, accepted policy or pending approval does not
qualify a new member or task. Authenticate the owner and check the original key
and request digest before applying current admission. Exact accepted replay and
recovery use the original binding and preserve its bytes, launch contract,
receipts and supported release, closure and collection paths. Accepted Codex
0.159.3 bindings retain their original qualification and never inherit the new
executable. Missing or changed executables refuse restart without substitution
or re-execution.

An approval resolution that would admit a new member performs the current-pin
check after owner authentication and original-resolution lookup, before storing
the resolution receipt. Refusal leaves the pending approval unresolved so the
owner can still deny it. Denial does not require runtime admission. A resolution
replay or recovery of an already reserved member retains its original receipt
and accepted binding.

Profile registry, binding-history observation and server lifecycle retain generic
compatibility admission: the tested minimum and compatible unverified runtimes
remain usable. This includes doctor probes, start/restart/stop, operator and
automatic migration, compensation and diagnostics. The exact current pin applies
only at qualified-mechanism admission callers; shared snapshot, bind, history
and Profile launch helpers do not apply that pin.

Earlier TASK-077 consumer qualification selected Aquarium commit
`db132845624c24bb4d0a889503b35a2a59b2571d` and Podway commit
`7c4a4c1ec98fd4e6eb143852419977180755d0df` (0.2.15), with Podway contract
generation 1. Aquarium owns finding severity and local-correction policy;
Dolgorae preserves supplied severity and reports round facts only for work it
dispatched.

The full caller pipeline passed 17 native Turns and 34 loopback POSTs through
whole-session close. Its separate caller-startup check used zero Turns and POSTs.
All four startup-only default-feature forms passed: omission, configuration,
raw argv and ordered enable/disable. They used zero Turns and POSTs and retained
the full process-quiescence proof. The separate native workspace-policy campaign
passed six Turns and twelve POSTs.

The successor pipeline passed five Turns, 23 POSTs and four dispatch checks. It
exercised managed and external one-file tasks, actual Primary Record/Accept and
Podway Action completion, and the ordinary v3 handoff. The unqualified native
review-reader fixture passed two Turns and four POSTs, preserving original index
receipts across reopen and rejecting deliberate native worktree access. This
fixture does not establish production checked-review support.

Production-host evidence combines 32 passing rows from the earlier 36-row
campaign with four later passing rows: the quote-heavy 20,000-change backend
restart and ordinary 20,000-change fresh-result cases on each selected host.
These records span source changes, including ledger observation, collection,
legacy review routing and Writer/Worker retirement. They are not a single
36-row run on the final binary; reuse applicability belongs to the completion
assessment. The 64 MiB full-output receiver limitation remains, and
these results change none of the producer or retained-content budgets.

These scoped results cover individual-task paths. TASK-086 retains its broader
receipt-mapping/handoff matrix; TASK-087 owns artifact requalification after
cleanup. TASK-078's modern Run/pool work and TASK-079's activity/interaction
routing retain their existing scope.

## Research group admission and observation

Research groups use the existing protected delegation namespace and its
transactional immutable content store. Group acceptance binds the complete
assignment manifest, installed provisioning references and captured input bytes
to one original-key receipt before any member provisioning or task dispatch.
Input captures are split into chunks of at most 8 MiB. Existing input, manifest,
artifact and reserved-capacity limits remain in force; a group never enlarges a
member's execution or result budgets.

Each entry's input capture is charged to its selected execution member Run,
using the artifact owner's existing logical-byte and deduplication rules.
Several entries may select the same compatible Run; admission includes their
combined charge and that Run's existing usage. Group size does not require an
equal number of new Runs or live generations. When a new member is needed,
the provider allocator reserves its real future Run identity before acceptance.
The caller cannot select that identity, and a group ID is never a Run identity.
The immutable captures, required member-birth reservations, quota reservations
and group receipt commit atomically before provisioning or dispatch. Run birth
consumes the same reservation and preserves its logical charge without copying
the capture, double charging it or resetting its budget. Concurrent artifact
publication and reuse share the owning quota serialization. Unknown effects
retain their reservations and captures; failure alone permits no release or
deletion. Only the explicit retention or cleanup owner may release a reservation
after proving ownership and that its last consumer has finished.

Per-entry provisioning and assignment retain their own authoritative receipts.
Group observation derives progress from these receipts and the owning task
records. Caller-supplied task IDs cannot establish membership. Failed,
expired, interrupted-unknown and not-yet-provisioned entries remain visible.
An accepted replay returns the original group identity without reopening input
files or dispatching uncertain work. Waiting and collection are observations:
neither starts a task, cancels a member nor advances a consumer cursor.

Public groups authenticate the existing aggregate Controller. Their retained
origin names the accepted group, entry and real member Run; it carries no
native Turn/tool claim or Podway caller scope. Managed and external recovery
check that origin, the current Controller, member configuration and original
assignment receipt against the accepted group. Checked native facade groups retain
their actual source-call checks and configured caller budget.

## Bounded consistent identity observation

Both owning services allow at most three attempts under one
cumulative 512 MiB, 1,000,000-record, `MAX_AUDIT_LINE_BYTES` and 300,000 ms
ledger allowance. Source selection, immutable segment verification, parsing
and interaction reduction share that allowance; retries do not reset it.
Display caches and prefix-only histories cannot prove interaction absence.

Each attempt first captures the Run manifest, current Controller, projection,
runtime record and exact selected ledger sources, followed by the writer fact.
Fresh verified history at the captured head/hash must reproduce the Run
projection and supply the interaction revision and pending verdict. After
rechecking those Run/source facts, the service opens a fresh pinned Engagement
read transaction for task/result linkage, caller counts, membership and workload
revisions, write work, cleanup and original terminal release proof. The current
task, accepted identity and typed complete result are materialized on that same
connection. The service checks the writer and Run/source facts again before the
transaction ends; only the confirmed task/result and disposition leave together. A proof
requires all three Run/source captures and both writer captures to match, with
the collected result/digest matching the database facts. The database snapshot
is their common observation instant; it does not fence future operations.

The stamp includes generation, lifecycle, active Turn, Controller and manifest
fingerprints, control/App Server epochs, runtime fingerprint, projection and
ledger generation/head/hash/source identities. Unpublished suffixes, replacement
sources and open/resolve ABA changes invalidate the attempt. After dropping the
failed transaction, a retry may wait at most the existing 100-ms group-commit
interval within the same remaining allowance; no wait follows the third attempt.
Torn, incomplete,
unclassified or unreadable history never defaults to pending=false. Motion
through all three attempts or cumulative exhaustion returns `RECOVERY_REQUIRED`,
`RecoveryNeeded`, `restore_state`, retaining a safe underlying classification.
Interaction correlation follows ledger order: a decision closes only an already
observed request, and a reused native request ID opens a new request. An
unresolved raw open also prevents idle during interrupted normalization or
automatic refusal. To prove absence, the interaction reducer must validate resolution fields,
kind/status linkage and date-time values for decisions and initially resolved
or stale requests. It validates normalized interaction arms separately from
raw journal decisions. The Turn producer's `terminal_turn` stale reason and
nonsecret answered receipt remain valid, and historical nonsecret decisions
may lack the outer receipt. The proof never invents receipts or rewrites
journal metadata. Malformed resolution metadata requires recovery.
Pending interactions prevent idle. An original released
proof still requires coherent settled work. Reads never start, recover, cancel or release work.

External and managed exports preserve their existing mapping IDs, caller scope,
accepted authority, complete result and digest. This observation does not resolve
native full-output receiver compatibility or establish actual consumer acceptance.

### Deterministic findings merge and explicit synthesis

`research_merge` authenticates the aggregate and derives every source from the
complete accepted group manifest and original task/result owners. Caller source
rows must name that entire collected set. It sorts them by unique `entry_key`
before normalization, so permutations have identical accepted input and result
bytes. An accepted original key returns its frozen source set before observing
later group progress.

`merge-exact-findings-v1` validates each completed source's original
`structured_findings_v1` bytes. It groups exact stable keys, orders variants by
complete JCS finding bytes, and collapses only byte-equivalent checked findings.
Each variant retains every sorted unique source entry key. Different variants
remain explicit conflicts. Coverage retains failed, cancelled, expired, unknown
and missing sources. A key with more than 64 inequivalent variants or a complete
output over 32 MiB is a typed capacity refusal with no accepted merge; sources
are retained intact. Merge and collection never invoke a model or resolve claims.
The merged identity and digest use the existing immutable MDC domain and schema.

The merged output is retained atomically with its genuine UUIDv7 merge and
artifact identities and keyed receipt. Managed publication charges the actual
Session root Run; external publication charges the first proven existing source
Run with output capacity in entry-key order. An external group without a genuine
owner returns `COMPLETE_RESULT_UNAVAILABLE`. Exhausted eligible owners return
`ARTIFACT_CAPACITY_EXCEEDED`; both refusals retain complete source coverage. Charges
share the existing 256 MiB Run quota and serialization lease. Public result
references and Artifact metadata/chunks read these exact bytes under current
aggregate Controller authorization, with no observation-time publication.

The existing native `merge_group_results` is K under both owner paths. External
admission retains the full original protected context in its existing operation
column/envelope; managed admission retains its genuine Root/Turn/tool-call with
null external context. Merge does not acquire assignment caller scope or spend a
round. Original-key lookup authenticates the aggregate before revealing key
state and verifies receipt, publication and origin in one read snapshot.
Ordinary external and managed native lookups return the exact merged-output
BytesRef and separate merged-domain digest. The Controller gateway retains the
merge UUID in `result_ref`. A completed merge can contain unknown sources; it does
not complete those Tasks. Lost replies
recover the original operation, with no replacement invocation or redispatch.

`assign_synthesis` is an explicit ordinary task acceptance, with its own actual
Task, caller round and complete result. The request binds a retained merged digest
and exact merged-output BytesRef. Public task-body syntax retains its 16 MiB
JCS ceiling; the complete rendered synthesis input must also satisfy the shared
ordinary Turn 8 MiB validator. Capacity is checked on that exact rendered input
before task acceptance, caller-round reservation or dispatch. No input is
truncated, and an accepted original key replays before fresh source/capacity
checks. Acceptance retains that dependency before dispatch. Dispatch and recovery
render the same immutable input as data.
The facade task body retains its existing schema. Public structured task bodies
retain their original complete JCS bytes, ordered contexts, criteria and
constraints under `dolgorae.structured-task/v1`; an internal execution projection
never replaces their accepted bytes or identity. Controller synthesis uses its
own authenticated acceptance origin, without manufacturing a native source call,
Podway round scope or group receipt.

The protected native host classifies the existing `assign_synthesis` method as
K and permits its original namespace for lookup, preserving its checked native
binding and owner/scope fences.

Original-key synthesis lookup authenticates the current aggregate owner before
reading a key, including the managed Root and existing Broker compatibility
fence. It verifies the actual accepted `assign_synthesis` receipt and retained
normalized input in the same read snapshot as the Task and result. Pending,
running and uncertain execution remain distinct; an exact original unknown
Submit claim without a known Turn is unknown, while proven terminal facts take
precedence over an old unknown claim. Settled lookup returns the complete result
BytesRef and its separate CompleteResult domain digest after body retirement.
It never recaptures input, invokes recovery or changes caller accounting.

Explicit native external synthesis recovery verifies its own `assign_synthesis`
namespace, immutable input and accepted body, current owner and member, and
original native invocation and caller accounting before the existing history
owner runs. A missing proof cannot acquire ordinary-assignment, group-child,
Controller or legacy provenance. Controller synthesis retains its separate
origin. This authorization performs no new acceptance or redispatch.

Queued synthesis and restart reconstruction resolve the original accepted
`assign_synthesis` operation by its actual Task binding. They verify the complete
original public or facade task bytes before constructing the internal execution
projection. The scheduler and worker use the same retained merged-input renderer
and original deadline; recovery neither accepts another task nor spends another
caller round.

A body-free capacity-finalized source keeps its genuine Result UUID and complete
identity. Its source manifest is null with the explicit artifact-capacity reason;
merge provenance retains that identity without inventing an artifact. Selecting
another eligible genuine Run places only the newly merged artifact. Existing
source and capture charges remain with their original Run.

Synthesis context references must resolve to retained, authenticated bytes. The
public adapter resolves its exact merged input with JSON media type and exact
`accepted_context` references already retained by authenticated source Tasks in
that immutable merge. Source context identity, kind, media type, complete hash
and length must all match the original accepted Task and bytes. Other references
fail before acceptance when this bounded source set has no original data owner;
there is no file discovery or global hash lookup. Ordered aliases remain part of
the exact task identity. Their complete rendered input and capture size are
bounded before body replication, including repeated aliases, and actual context
bodies enter the rendered input in accepted order. This preserves the public task
syntax and the merge deduplication rules.

External Submit binding selects the actual dispatching Task and compares its
exact ordinary key or Task-bound synthesis key. An unrelated key supplies no
assignment binding; a generation-4 Worker returns
`ACCEPTED_ASSIGNMENT_NOT_FOUND` before submitting a Turn. An exact accepted key
still requires its original Run, dispatch claim and complete rendered bytes.

## TASK-086 consumer qualification

Fresh qualification selects the official immutable Podway 0.2.15 distribution,
source `9a3f94c8752298f689832d05aa827a0f6496c1af`, executable SHA-256
`dece7fecd90eaf6130b330e2cb70e98532f3e6b73950227cc2ff935a8be8e5ae`,
and contract generation 1. Its manifest digest is
`sha256:293cfcf7bcaebb55b55b9e7cabb5842df228dbd9afacea5d1ae78474c25ae01b`.
This replaces the earlier unshipped qualification build as a fresh runtime
selection; it does not rewrite those earlier results. The source delta preserves
schemas, catalog, Store and delegated APIs; the changed generation-2 writer-name
diagnostic is outside this generation-1 selection. Actual DOLV3 and native
consumer acceptance remain required. H1's eleven contract-source pins stay fixed.

The Aquarium input remains the complete source archive of
`db132845624c24bb4d0a889503b35a2a59b2571d`, SHA-256
`542a7836dee2d412b06f6c78e16712124cce8f89c14413d943c1bd3fab5454e9`.
The historical TASK-077 scope selected six instruction/helper inputs. Current
TASK-086 qualification also includes the selected source's Goal Evaluator body
and its applicable dependencies under the
[current consumer qualification](dolgorae-v014-current-consumer-qualification.md#operative-source-closure).
Current neighboring Aquarium changes and Podway's later default-disabled
integration permission feature are separate applicability boundaries. The selected
Podway contract has no `integration.show`; it does not qualify a toggle-aware adapter.

Qualification composes the existing production host and consumer handlers and
public Podway CLI. External Codex and Claude hosts and the genuine managed Codex
Primary are distinct supported paths. Each due AQ/PW row requires its own exact
input, actual invocation/result observations, source byte identities, original
recovery keys and cleanup. Full result/component inspection precedes explicit
semantic item decisions, structural receipt acceptance and separate Primary
completion. Group/merge provenance stays executor-owned; only an individual or
separately accepted synthesis Task supplies an Action receipt and real caller
rounds. A row cannot pass from a trace, metadata-only probe, fixture ledger,
preview, model acknowledgement or a different row's success. Final packaged H3
and post-cleanup requalification remain TASK-087's obligations.

The external `consumer-complete` qualification case adds a separate bounded
budget to the original eighteen host scenarios. It admits explicit Dolgorae
producer Role, dependency and guidance bytes and uses one genuine writable Task.
These producer instructions do not claim an Aquarium formal-v1 Role. The current
Aquarium Role bodies and ordinary helpers belong to the separate native consumer
composition. The native
Primary reads the complete result, final response, both change sides and work
package through byte-bound continuation pages. It then inspects the prepared
Record and Accept inputs, returned outcomes and structural receipt. Public
receipt observation must match the accepted Task; acceptance must leave the
Action running until the Primary issues the separate completion command.
Original loss-case budgets and evidence remain unchanged.

The separate `consumer-rounds` case reuses one genuine member for three accepted
Tasks with explicit feedback. It replays and polls original assignments without
adding rounds, then requires a fourth assignment to fail as `ROUNDS_EXHAUSTED`
without an extra executor Turn. The final Task has its own complete result and
three-round receipt. `consumer-release` uses the same full-read sequence after a
settled member release and requires the released disposition proof; an interrupt
acknowledgement cannot substitute. These cases retain their own original keys
and declare their additional executor POSTs independently of the old rows.

The `consumer-group` case accepts two research entries under the real caller
scope, merges their immutable complete source identities with the exact merge
algorithm, and retains an explicit conflict between their claims. It reads the
published merged artifact through the authorized artifact/chunk owner, then
accepts one explicit synthesis Task over those exact bytes. The synthesis is
the third caller round and the only Task mapped to the Action receipt. Group
references cannot be used as consumer evidence selections. Neither the merge
nor the receipt resolves the source conflict or supplies a Judge verdict.

The distinct `consumer-fences` case preserves a genuine accepted receipt while
checking cosmetic edit/revert staleness, exact original-key replay and conflict,
a fresh attempt, historical receipt lookup and late old-attempt refusal. It
leaves that explicit fresh attempt unaccepted for owned cleanup; other complete
consumer cases prove separate Action completion. Invalid Role and round claims
must fail before acceptance without changing current items or session state.
All public fixture commands use the fixture's isolated HOME and configuration.

The selected-distribution check verifies the release archive and executable,
the manifest's canonical self-digest, every manifest-bound asset, the packaged
adapter catalog and the closed schema reference set. Runtime cases validate
public success and refusal bodies with those actual packaged schemas. Artifact
verification alone covers no route execution, migration or native row.
The separate direct-contract case exercises genuine Direct progression and
completed-to-running reactivation, retired-command refusal and a generation-2
workspace rejected by the generation-1 production client. The adapter observes
that unsupported workspace without migrating it. This does not qualify the
later generation-2 diagnostic contract or a toggle-aware consumer.

`consumer-group-gaps` repeats the small group path with one physical worker loss
while its original native response is held. Signalling requires a fresh match
of Run, generation, frozen image digest, UID, birth, process group and session.
Its original accepted operation remains unknown without rewriting the Task
phase or redispatching; the merge retains that explicit source gap. A separate
genuine synthesis Task may report the gap but cannot relabel it as success. Cleanup must reconcile the original Task from
its native history before retiring the owned Profile; an unresolved history
keeps the failed receipt and requires explicit recovery.

The original six-input Aquarium scope does not qualify an operative Judge Role.
AQ-03 must also bind the selected source's actual Goal Evaluator body and its
applicable resolved dependencies. Current ordinary-v3 Evaluator documents and
retained formal-v1 Judge documents keep their own schemas and Role semantics.
A qualifier must establish the applicable route before admitting that Role;
reading the formal reference does not convert a current v3 body into a retained
v1 snapshot. Formal semantic progression requires separate Worker delivery and
Judge assessment Actions followed by the Primary's Decision. A single delivery
receipt case does not satisfy that workflow. A test oracle may check observed
native decisions but cannot supply the missing consumer authority.

Complete-consumer cases also capture a real upstream Direct Action's declared
text evidence. The executor receives its exact bytes as accepted context, and
the Primary reads the full typed value through `package_evidence` continuation
pages. Those pages remain bound to the current package's original source
attempt, item revision and value digest. The worker-loss case runs the independent
Direct compatibility flow while the original research Task is still unknown;
it cannot recover that Task by issuing new work.

Current AQ-02/AQ-03 consumer qualification follows the explicit
[ordinary-v3 source/application selection](dolgorae-v014-current-consumer-qualification.md).
Dolgorae supplies authentic producer facts; actual same-host collection Executor
and Goal Evaluator roles produce current v3 documents, followed by the Primary's
Decision. These roles do not turn a managed producer Root into a native consumer
or claim the unresolved Aquarium formal-v1 hybrid. Individual Podway structural
receipt domains and authentic historical formal meanings remain separate.
