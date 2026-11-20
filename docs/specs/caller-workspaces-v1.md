# Caller-workspace successor contract

Status: Accepted contract for EPIC-017. The integrated interface requires a
qualified Profile and per-guarantee readiness. The [roadmap](../roadmap/README.md#epic-017-caller-owned-workspaces-and-explicit-agent-roles)
owns delivery status. This document is part of the [product specification](README.md).
Uppercase normative words have the meaning defined there.

## Version and activation map

TASK-061 MUST establish the following contracts before product behavior changes.
An implementation MUST advertise support only after its required deterministic
and empirical checks pass. TASK-066 activates the integrated interface when all
six Tasks meet their completion gates. Earlier Tasks MAY implement internal
support; they MUST NOT advertise an incomplete integrated guarantee.

| Surface | Existing contract and retained meaning | Successor contract |
| --- | --- | --- |
| Public Run and Gul clients | TASK-053 public-v1 source, descriptor and TASK-056 clients; explicit lanes and lazy write activation | Byte-identical public-v1; no new Protobuf generation |
| Machine/private startup | Existing low-level start input and threadless allocation | `dolgorae-run-start-request/v2`, resolved writable/general defaults, dedicated lane |
| Instructions and Agent Configuration | `dolgorae.instructions/v1`, Agent Configuration v1/v2 | `dolgorae.instructions/v2` and Agent Configuration v3 |
| Policy authoring and installation | JSON Role v1 and policy input/snapshot v2 | Policy input/snapshot v3 with named Markdown Role capture |
| Run manifest | Manifest v1/v2 | Manifest v3 containing resolved startup selection and admitted instruction snapshot |
| Review | Capture target v1 and review v1/v2/v3, including accepted receipts | Direct target v2 and review v4; six unchanged source-scope meanings |
| External Specialist facade | Existing open/observe/close and accepted v1/v2/v3 task history | Facade v4 hire/assignment and direct structured results; unchanged aggregate authority |
| Machine output | Existing command-tagged v1/v2/v3 envelopes | Modern command-tagged v4 envelopes and safe access/review projections |
| Capabilities and errors | Existing capability v1 and checked errors v1/v2/v3 | Capability v2 with per-guarantee readiness; closed successor errors v4 |
| New isolated launch | Legacy `copy`/`isolated_write`, capture creation and managed Git worktree creation | Typed retirement before effects; caller prepares any separate workspace |

The [workspace execution schema](../protocol/dolgorae-workspace-execution-v1.schema.json)
owns the concrete startup, instructions, Agent Configuration, policy and manifest
successor shapes. The [direct target](../protocol/dolgorae-review-target-v2.schema.json),
[review v4](../protocol/dolgorae-specialist-review-tool-v4.schema.json),
[facade v4](../protocol/dolgorae-external-specialist-facade-v4.schema.json),
[Machine v4](../protocol/dolgorae-machine-v4.schema.json),
[capability v2](../protocol/dolgorae-capabilities-v2.schema.json) and
[error v4](../protocol/dolgorae-error-contract-v4.json) schemas own the remaining shapes.
Rust admission MUST enforce the semantic checks identified in schema
annotations. JSON Schema validation alone does not prove byte lengths, digests,
JCS identity, scope provenance or effective runtime policy.

Policy v3 resources are embedded in the workspace execution schema. Their
canonical document pointers are `#/$defs/policy_input` and
`#/$defs/policy_snapshot`; offline registries MUST resolve their embedded IDs.
The capability `caller_workspace` block describes the successor surface.
Its `source_materialization: retired` field does not claim that retirement of
new legacy requests has activated while `integrated` is false. The cutover rule
below continues to govern those requests.
Inherited `lane_capabilities` describe frozen public-v1 launch semantics,
including dedicated `lazy_first_input`. Modern starts use the separate
`caller_workspace.physical_start: initialized_before_success` contract; that
field does not advertise availability before startup qualification and cutover.
Policy and facade `read_only` maps to modern `read-only`, and
`canonical_workspace_write` maps to `writable`; both refer to the caller
workspace and grant no authority by themselves.

At cutover, a new legacy request requiring source materialization or an isolated
launch MUST return `SOURCE_ISOLATION_RETIRED` before Run, capture or worktree
allocation, source publication, provider dispatch or caller-root mutation.
It MUST NOT be reinterpreted as access to the caller workspace. A legacy input
that requires JSON Role resolution MUST return `ROLE_MIGRATION_REQUIRED`.
Existing public-v1 starts retain their explicit lane/access semantics and do
not acquire modern defaults merely because the executable has been upgraded.

Already accepted Runs, tasks, policies, results, receipts and captures MUST retain
their original bytes, identities and meaning. Their readers and supported
inspection, collection, closure and settlement operations remain available.
Recovery MUST NOT reconstruct a missing source copy or worktree, replay uncertain
work, rewrite a snapshot from current Role files, or grant new cleanup ownership.
An accepted old Run MAY finish using its existing proven resources; a new launch
from an isolated legacy policy requires migration.

## Startup and access

**CW-START**: Modern `run start --access read-only|writable --role <name>` MUST
resolve omitted access to `writable` and omitted Role to `general` at the modern
adapter. The private resolved request MUST carry both selections explicitly.
Startup selection is immutable provenance; current effective access is a
separate mutable fact. The modern lane is `dedicated` for both selections.
Workspace discovery, canonical byte identity, Git/non-Git admission, global
Profile binding, Controller carriers and idempotency retain their existing rules.
An externally prepared Git worktree is a distinct workspace. Shared Git metadata
and editors outside Dolgorae remain outside its one-writer guarantee.

**CW-WRITER**: Writable startup MUST reserve the canonical workspace writer
authority before provider effects. `WRITER_BUSY` MUST fail without dispatch,
silent access downgrade, source copy, or a second writer. Success MUST require
active matching authority and positively verified writable policy for the same
Run, process generation and thread. This includes startup without a task Turn.
An internal thread initialization is allowed; a synthetic user Turn is not.
When a writable Turn is prepared for a Run that already holds the Writer,
policy verification MUST use that Turn's exact durable admission and Controller.
It MUST preserve the pending Turn admission until actual Turn execution and
MUST reject a foreign, completed or read-only admission. Reusing the Writer
MUST retain the existing idle, quiescence and current-policy checks.
Machine v4 writer-busy diagnostics MUST expose only the requested Run identity,
nullable holder Run and Profile, authority state, writer generation and handoff
eligibility. Controller kind remains null when it is not carried by the Writer
record. These diagnostics MUST NOT include instruction bodies, source paths or
credentials, and MUST retain the existing retryable `WRITER_BUSY` code.
An internal failure after caller Run allocation MUST retain `INTERNAL_ERROR`
and the original Run identity in a closed Machine v4 diagnostic. It MUST use
a bounded startup invariant rather than raw storage or provider diagnostics.
Before publishing WorkerReady for each new physical Worker connection,
initial runtime qualification, configuration
readback and turnless Thread binding or original-Thread rebinding MAY wait for
same-user Dedicated Server startup descendants to exit. These checks MUST share
one monotonic budget for that connection and remain within the session
transport budget. The cap is sixty seconds for the first dedicated startup of
a public-v2 caller Run or Orchestrated Session Primary, fifty seconds for its
replacement Reader during generation release, and thirty seconds elsewhere.
The public Start response deadline is separate. After it expires, the caller
can look up the original Start key. Every observed child MUST retain its exact
PID and birth identity until the kernel confirms absence. Identity drift,
scope escape, inaccessible census, and Worker workload MUST fail closed.
Success still requires five complete empty samples at 100-ms intervals.
An initially different group or session MAY be admitted only after two complete
kernel observations establish the stable PID/birth/UID ancestry to the exact
owned Dedicated Server. Its admitted UID, birth, group and session MUST then
remain fixed. Shared UID or group membership alone grants no ownership.
New caller Worker generations MUST persist a version-2 runtime ownership
witness before Dedicated launch and every child admission. Incomplete startup,
including Worker death before any child observation, MUST NOT prove generation
absence or retire Writer authority. Completed evidence MUST survive parent
exit, Worker exit and normal control-socket replacement. Missing, corrupt,
cross-generation or regressing evidence MUST fail closed. Version-1 runtime
records retain their original shape and meaning; old readers reject version 2.
An initial qualification timeout before native Thread dispatch MAY transfer
cleanup to the startup owner through a separate immutable witness for the
same Worker, Dedicated Server and observed descendants. The startup owner MUST
verify the original Start, durable dispatch history, and exact process identities.
It MUST prove five complete empty samples before sealing `start_failed` and
releasing any matching initial Writer reservation. An unknown dispatch,
unverified process, or incomplete census retains the recovery fence. The
original key remains allocated to that Run. After cleanup, lookup reports an
accepted Start with a failed outcome. Provider initialization can modify Codex
home before this boundary.
The existing personal-alpha limit for deliberate escape wholly between census
polls remains unchanged.
A startup group/census mismatch MAY discard a sample only after every differing
nonleader PID is either verified as a matching startup child or kernel-absent.
All other identity and workload checks MUST still run. The discarded sample
MUST reset the empty count and MUST NOT count toward success.
The wait MUST end permanently on binding success or failure and before
WorkerReady, including connections that do not eagerly bind a Thread. Diagnostic
phase changes and later resumes MUST NOT recreate it. No task Turn or provider
dispatch is permitted in this window. Failure MUST preserve the original or
provisional Thread, unknown-state retry fence and Writer guard semantics.
Policy proofs and later access transitions MUST NOT use this startup wait.

After startup, a serialized caller policy proof and its authoritative
Controller/Writer revalidation MAY distinguish one qualified resident Code Mode
host from workload. This exception is limited to Codex
0.164.0 executable SHA-256
`d8990e3ee7acfbc26f91265b12a1a83f0fee40d0e0f8f1f40a37d4ec21f499cf`
and its sibling host SHA-256
`416721e33af8822041dc650793512b3e58d43e0be4f3a5c6187eb4dfaad3148a`.
Qualification MUST establish that completed native Turns leave no executing
Code Mode isolate, including yielded cells and unawaited timers or promises.
Idle Thread metadata alone is insufficient: yielded native tool commands can
outlive their Turn. The caller MUST retain its idle/no-pending-interaction,
exclusive original Thread, current policy and durable admission checks.
The companion MUST have the exact sibling executable path, regular executable
image and digest, the same UID and session, and direct ancestry to the matching
owned Dedicated Server. A proof MUST retain the same PID, birth, parent,
group, session and executable device/inode throughout all five samples.
Every additional descendant and any identity or census uncertainty MUST refuse
the proof. The coordinator MUST validate the received proof against current Controller,
Writer, runtime, socket, policy, original Thread and committed projection before
using this same policy-specific census. Generic workload/absence inspection and
startup remain strict.
Writer retirement MUST capture and stop the companion with the complete owned
generation and prove physical absence before releasing authority. This bounded
trusted-runtime qualification does not remove the existing between-poll escape
limit; it grants no exception to other executable pairs or configurations.

Missing instruction sources and known unsupported required capabilities MUST
fail before Run allocation. Post-allocation failures retain the original Run
and idempotency receipt with truthful failure or unknown state; retries MUST
reconcile it and MUST NOT allocate a replacement. Ambiguous policy/process
effects MUST retain a reserved or blocked guard until existing recovery proves
safe absence or a completed transition.
An unqualified turnless startup/effective-policy mechanism MUST return
`WORKSPACE_STARTUP_UNSUPPORTED` before allocation. Qualification cannot infer
write access from a reservation, thread sandbox label or synthetic task.
New caller and Specialist admission MUST prepare a qualified launch snapshot
with final `--disable shell_snapshot` arguments and `shell_snapshot` in its
disabled features before computing the launch contract, server key and derived
cwd. The registered definition and observed configuration remain unchanged.
Qualified preparation MUST NOT record history until current-runtime admission
passes; recording revalidates the definition under the existing registry lock.
Read-only configuration and capability inspection MUST NOT start, repair or
replace a server or record a qualified binding. Generic Profile preparation,
observation and lifecycle retain their existing behavior. Qualified readiness
MUST refuse conflicting active or uncertain contracts for the same CODEX_HOME;
a distinct key does not authorize automatic rollover or concurrent contracts.
Accepted pre-policy 0.164.0 and historical 0.159.3 bindings retain their original
bytes, hashes, keys and executable checks during replay and recovery. They MUST
NOT qualify genuinely new work through that recovery permission.

The existing shared Profile readiness/catalog check MAY precede allocation and
Writer reservation. It MUST NOT create the caller Run's Dedicated Server,
Thread or task Turn. The final Writer reservation remains authoritative after
any earlier busy observation and MUST precede caller execution effects.

**CW-TRANSITION**: Dedicated read-only startup MUST establish a verified reader
thread. Controller-authorized acquire/release MUST retain the same Run and
Codex thread, enforce idle/no-pending-interaction and process quiescence, and
commit only after effective-policy proof. Unsupported transitions MUST fail
before mutation. Release MUST preserve the guard until read-only policy and
absence of writable descendants are proven. A released reader MAY reacquire.
Fixed legacy `shared_readonly` remains non-promotable. Review Runs and restricted
Specialists retain their existing prohibitions on writer acquisition. Role text,
task wording, Profile selection and peer mail MUST NOT grant writer authority.

The successor policy proof MUST bind its operation, Workspace and Run, original
Thread, current Worker and Dedicated Server identities, socket and server epochs,
accepted Profile binding and effective policy. Writer commit MUST compare the
proof with the durable policy and its prepared transaction. A legacy writer
acknowledgement or an unverified or mismatched proof MUST NOT authorize commit.

Generation recovery MUST preserve accepted instructions and resolved startup
selection. A successor generation follows the existing conservative authority
reconciliation; a stored writable selection MUST NOT cause automatic acquisition
or semantic replay. Readers MAY coexist with a writer and observe intermediate
files. Effective policy, its verification and epoch, writer state/generation,
and startup selection MUST be projected independently.
If a turnless native thread is lost and cannot be resumed, recovery MUST report
its unavailable binding rather than inventing a replacement or replaying a
task. Unproven writer authority remains guarded. Qualification MUST exercise
this boundary and state the selected runtime's persistence limit.

Explicit recovery MAY resume the original Thread as a reader and retire a
pending writer guard only after current Controller, generation, original
Thread, verified read-only policy and workload absence checks succeed. This
MUST NOT acknowledge the interrupted writable startup as successful or
automatically reacquire writer authority.

This Reader-resume restriction does not prohibit confirming an already applied
Write transition in the same live Worker generation. Explicit recovery of that
generation MAY commit the original pending Writer transaction only after a fresh
qualified Write proof verifies the original Controller, Thread, accepted launch,
generation and workload absence. It MUST NOT send another Write update, replace
the Thread or allocate another Run. After that confirmation, explicit replay of
the identical original start input and key MAY publish its missing receipt.
The initial uncertain start response remains an error; recovery and receipt
publication are separate observations. A Reader proof cannot complete writable
startup, and generation recovery retains its conservative Reader semantics.

If an idle Worker generation is lost, explicit recovery MAY acknowledge its
verified reader attachment without changing logical Idle or replaying history.
Retiring the verified absent generation MUST inspect its committed policy and
guard under Writer serialization without requiring active Writer authority.
It MUST retain the original guarded transaction, validate generation and
server bindings, and compare the complete committed audit head before release.
A qualified authenticated acquire with an original Thread and protected recorded
Read policy MUST use the retained generation-retirement owner with an explicit
Acquire purpose before its Write update. Active Writer replay, threadless first
write and legacy shared startup retain their existing paths. The original acquire
transaction, Reserved holder, writer generation, Controller, Thread and source
bindings MUST remain protected. Retirement MAY stop checked owned descendants
without proving them inert; active Turns, pending interactions, foreign actors,
unreadable identity and scope drift MUST still refuse. Persist intent before
stop, prove full owned-generation absence, and verify a normal read-only
successor on the original Thread and history before atomically rebinding the
reservation. Only the subsequent authorized Write update and fresh proof MAY
commit Active. After retirement intent is persisted, uncertainty MUST retain
Reserved or BlockedUnknown authority.
Explicit recovery MUST remain read-conservative and MUST NOT acquire Write or
replay a Turn. Missing purpose retains historical Release meaning and encoding;
Acquire purpose MUST be explicit and rejected by unsupported readers. The
successor uses the normal one-shot connection startup window defined above,
without reset or retry. Descriptor and readiness timeouts, stop deadlines and
the five-sample absence proof remain unchanged.

A lost policy response MAY be reconciled using fresh proof of the already
durable policy for the same Controller, Thread and guarded transaction. It
MUST NOT reissue the native policy operation to reconstruct that response.

## Instructions and Role admission

**CW-SOURCE**: The common source MUST be `~/.dolgorae/home/AGENTS.md`, and the
selected Role MUST be `~/.dolgorae/roles/<name>/AGENTS.md`. Names MUST match
`^[a-z0-9][a-z0-9._-]{0,63}$`. These managed sources have no project, JSON,
ancestor, include or environment fallback. Source text supplies instructions;
execution configuration and policy display metadata remain separate. Ordinary
omission selects `general`; standalone review omission selects `reviewer`.
Engagement and broker policy
configuration MUST bind a Role explicitly.

**CW-INIT**: Only explicit `init` or migration MAY create sources. Init MUST
install common, general and reviewer templates using create-exclusive,
descriptor-relative publication and the existing owner-only home protections.
It MUST preserve existing files byte-for-byte and never repair invalid inputs
silently. Startup, validation, discovery and recovery MUST NOT create sources.
Existing files take precedence and still require admission.

**CW-CAPTURE**: Admission MUST traverse validated current-uid-owned 0700
directories without following symlinks and read each current-uid-owned 0600
regular file once through its validated descriptor. It MUST reject missing,
unsafe, non-UTF-8, NUL-bearing, empty, raced or oversized sources before a new
Run is allocated or policy installed. The combined raw common and Role bodies
MUST be at most 65,536 UTF-8 bytes. Descriptor identity and content stability
MUST be checked across the bounded read; replacement or in-place changes
detected during admission fail closed. Accepted raw bytes, their individual
lengths and SHA-256 digests MUST be preserved without trimming, newline or
Unicode normalization. Exact captured bytes, not a reopened pathname, become
the execution input. Existing core, dynamic-context and task bounds remain
independent of the source-body ceiling.

**CW-COMPOSE**: The immutable developer instruction prefix MUST compose core
governance, admitted common body and selected Role body, in that order with
two LF bytes between layers. Its versioned composition identity MUST bind the
core version and exact source digests. Dynamic access context remains Turn-scoped.
The current task MUST be submitted separately as user input; it MUST NOT change
Role identity. Conflicting instructions cannot remove enforced Controller,
writer, tool or lifecycle boundaries. The command and thread cwd MUST be the
selected canonical workspace. The instruction home and Profile CODEX_HOME MUST
remain separate from cwd and from each other.

**CW-SNAPSHOT**: Standalone admission MUST capture both sources. Policy
installation MUST capture each selected Role once and store its raw body and
digest independently of live sources. Aggregate bootstrap MUST capture common
instructions once, combine them with the installed policy's Role captures, and
retain them in its accepted session snapshot for children and reactivation.
It MUST recheck the combined ceiling before publishing bootstrap. Editing or
removing source files after acceptance MUST NOT alter Run, policy or session
bytes, trigger a source reload on retry, or invalidate accepted snapshots.

**CW-AMBIENT**: The selected Profile's native `CODEX_HOME/AGENTS.override.md` or
`CODEX_HOME/AGENTS.md` MAY contribute user instructions. This guidance is outside
the captured common/Role bodies and their byte-immutability guarantee. The
managed developer prefix takes precedence over both Profile guidance and task
user input; neither grants Controller or writer authority. User-facing guidance
MUST warn that Profile instruction edits can affect later runtime input without
changing the captured Role.

Modern admission MUST suppress automatic project and ancestor AGENTS discovery
using supported runtime configuration. Capability proof MUST include the actual
pinned Codex thread with conflicting project and ancestor files, an ambient
positive control, and a positive control for the explicitly supplied bodies.
Profile contributions MUST be observed separately and MUST NOT be treated as a
failure of project/ancestor suppression. Shell cwd changes or an instruction to
ignore files do not establish suppression.
If the selected runtime cannot establish this guarantee, admission MUST return
`INSTRUCTION_DISCOVERY_UNSUPPORTED` before allocation. Explicit project guidance
MAY be supplied as task context; it cannot replace the admitted Role or grant
authority. The common prefix does not disable supported skills, plugins, apps
or MCP tools globally; review candidate-reading restrictions are scoped below.

## Direct review and candidate reading

**CW-NOCOPY**: Every new one-shot, review-target, Engagement and brokered launch
path MUST use the caller workspace without preparing a source tree or Git
worktree. Durable instruction, configuration, result and bounded target-metadata
snapshots remain required. Close, failure, recovery, export and deletion MUST
preserve caller-owned roots. Historical cleanup MUST use original proven
capture/worktree ownership and MUST NOT recreate absent resources.

**CW-SCOPE**: Direct review MUST retain all six source meanings:

| Scope | Candidate source and comparison |
| --- | --- |
| `workspace` | Eligible non-ignored current files, including eligible untracked files |
| `staged` | Admitted HEAD/empty-tree to index transition; index bytes supply after context |
| `dirty` | Admitted HEAD/empty-tree to combined staged, unstaged and untracked state; current worktree wins and absence deletes |
| `head` | Tree of the commit resolved once from HEAD |
| `commit` | First-parent transition into the resolved commit; root uses the empty tree |
| `range` | `A..B` compares resolved A and B; `A...B` compares their merge base and resolved B |

Non-Git workspaces MUST support only `workspace`. Conflicts, unsafe paths,
escaping links, special files and recognized secrets retain existing fail-closed
admission. Renames remain deletion plus addition. Each side MUST retain the
existing 10,000-file, 8-MiB-per-file and 64-MiB-total bounds; transition metadata
MAY contain up to 20,000 entries. Candidate paths MUST be safe relative paths.
Review input and structured output retain their independent 1-MiB bounds and
the v3 brief/context/criterion limits.

**CW-PIN**: Before dispatch, Git scopes MUST resolve requested refs once to full
object IDs. Range admission MUST bind both endpoint IDs, two-dot/three-dot mode,
and the effective comparison base. A ref moving later MUST NOT change that
selection. Historical candidate file contents and diffs MUST be read from those
objects without checkout. Missing objects MUST fail as `REVIEW_TARGET_UNAVAILABLE`;
current workspace contents cannot substitute. Staged admission MUST bind selected
index entries, their blob IDs and modes, including deleted and absent sides.

**CW-READ**: Candidate full-file reads and diff reads for staged/head/commit/range
MUST pass through a checked target-reader bound to the admitted index or pinned
objects. Each candidate read MUST produce an authoritative bounded receipt
binding the review identity, target digest, side, relative path, content digest
and returned range. Candidate evidence MUST bind a matching receipt and location;
the coordinator MUST validate it against the admitted source. Live-workspace
reads MUST be labelled separate context and cannot supply candidate evidence.
Model-generated labels or claims that a read used Git are insufficient.
Numbered candidate locations name lines in an admitted file side, not diff
headers or diff row numbers. Transition evidence MAY cite a diff receipt with
null line locations; numbered locations MUST cite a current/before/after file
receipt covering those source lines.

Before dispatch, the selected runtime/tool configuration MUST establish that
unmediated native or plugin tool reads cannot enter candidate provenance and
that all candidate evidence is verifiable through the reader. This includes
full-file context outside changed hunks. If it cannot establish that boundary,
it MUST return `REVIEW_READING_BOUNDARY_UNSUPPORTED` before Reviewer allocation
or source-bearing dispatch. No prompt-only or cwd-only implementation qualifies.
Results with a violated or unverifiable boundary MUST return
`REVIEW_CANDIDATE_PROVENANCE_INVALID`, even when refs and fingerprints are unchanged.
The integration MUST NOT advertise checked object/index review until adversarial
native-tool worktree reads and forged/missing receipts have been exercised.

**CW-DRIFT**: Workspace, staged and dirty admission MUST capture bounded
fingerprints before dispatch and recheck the same eligible scope before result
acceptance. Staged checks MUST cover the admitted index and comparison base;
irrelevant worktree edits cannot replace candidate bytes. Detected changes MUST
return `REVIEW_TARGET_DRIFTED` with an inconclusive outcome, never an accepted
clean verdict. Source-read instability also fails closed. These sampled checks
can miss intervening edits. External writers also remain outside Dolgorae's
control, so review does not provide snapshot isolation. Accepted mutable-scope
results MUST disclose these limits. Object-backed review MAY retain separate live context;
that context cannot inherit object-backed assurance.

**CW-RESULT**: Review MUST use a fresh explicitly read-only Reviewer, separate
task input and admitted Role, stable request/Run/thread/Turn identities, bounded
ordered findings and exact criterion coverage. Unknown acceptance/outcome MUST
preserve the original identities and forbid automatic replay. Validated technical
verdict, criterion completion, source assurance, Reviewer/engagement closure and
server cleanup remain separate facts. Direct review has no capture-settlement
identity or immutable root. Read-only access protects source mutation; it does
not establish hostile-process containment or repository secrecy.

Direct review lifecycle and output errors retain their existing codes with
capture-free checked details identifying the original request, review and
Reviewer Run. One-shot inspection retains legacy results and admits direct v4
results without synthesizing a capture or settlement identity.
One-shot and reusable membership MUST be distinguished by their durable
authority kind. Missing reusable configuration MUST NOT reinterpret a member
as a one-shot Reviewer or change accepted access.

## Migration, projections and implementation handoffs

The Machine adapter selects modern startup when `run start` omits
`--execution-lane` and `--required-assurance`. It accepts `--access` and `--role`
on that surface and resolves their defaults before calling the shared service.
The two explicit legacy lane/assurance flags select the existing startup
contract and must appear together; they cannot be combined with modern access
or Role flags. This preserves explicit legacy caller semantics without applying
modern defaults to them. Frozen public-v1 requests continue to use their
original service entrypoint.

Modern startup, status and access transitions emit the closed Machine-v4
access projection. Historical Run observations retain their original output.
The adapter derives policy verification from the captured durable policy;
writer ownership alone is insufficient evidence of effective access.

`specialist review --request-stdin` accepts only the checked v4 request for new
reviews. `--target-kind` constructs a v4 change review; `--role` selects its
Markdown Role, defaulting to `reviewer`. A Role supplied both in argv and in a
request must match. Legacy `--scope working-tree` and older request schemas
require migration, rather than silently changing their isolation contract.
Existing request references remain available through review inspect/recover.

**CW-MIGRATE**: Explicit migration MUST enumerate legacy common/project JSON
Role inputs, destination names and collisions. Any collision MUST require an
explicit source/name choice; migration MUST NOT choose a winner, merge bodies,
overwrite destinations or rewrite accepted snapshots. It MUST retain original
source bytes and generate successor authoring input separately. New source
resolution MUST NOT fall back to JSON or project paths after cutover.

`instructions migrate --workspace <root>` reports a read-only plan of legacy
Role references, their canonical content digests, suggested destination names,
existing destinations and duplicate-name collisions. `--file <policy-input>`
also includes a legacy policy authoring input and its raw-byte digest.
`--request-stdin` applies an explicit bounded selection request. Every Role
selection binds its common/project reference and content digest to a destination
name. Policy selections bind the input digest, a new output path and explicit
per-role successor access selections. Referenced Roles must be selected or
explicitly mapped to an existing valid Markdown Role. Outputs are created
exclusively; source files and installed/accepted snapshots are never modified.
Publication may leave completed outputs if a later publication fails. The
caller must inspect and replan rather than replaying an uncertain batch.

Policy add/validate accepts v3 authoring input. New v2 input receives
`ROLE_MIGRATION_REQUIRED`; existing installed v2 snapshots remain readable and
removable alongside v3 snapshots without consulting current Role files.
Modern policy output contains authoring metadata, the snapshot digest and
per-role capture digests. It omits captured instruction bodies.

A modern parentless direct-interactive Run may admit the named Markdown
policy carried by its authenticated orchestration launch. Its accepted start
request binds that policy name. Session acceptance retains the policy, child
instruction captures, configurations and Profile bindings before physical
Primary startup. The broker uses those retained captures for later child
publication and recovery, with every child starting in the caller's root.
Legacy session snapshots keep their original representation and entrypoint;
the frozen public-v1 adapter does not project modern session captures.

Ordinary writable caller Runs MUST complete the same verified read-only
transition before terminal close. Controller and revision checks precede the
transition; concurrent mutations must invalidate close admission. Status and
repeated close MUST remain readable after Writer release.
Lists containing caller access projections MUST use Machine v4; empty and
legacy-only lists retain Machine v2. Mixed lists retain each accepted item
representation. Recovery and
reconciliation of a closed ordinary Run retain their typed lifecycle refusal.

Modern Orchestrated roots MUST support normal aggregate completion and abort
through checked Machine `run close` and its explicit `--interrupt` choice.
The authenticated Controller and current Run state revision authorize the
durable close intent before effects. Admission of new Broker work stops at
that boundary. Successful closure requires all owned Specialists retired,
the Primary closed and its Writer released. A writable modern Primary MUST
complete its verified same-Thread read-only transition before terminal close;
interrupting live work must establish terminal evidence before that transition.
Closure MUST NOT leave an active write-policy observation after Writer release.
An uncertain effect MUST retain
the original close operation identity and typed reconciliation state rather
than report closure or replay the effect. `run recover` and `run reconcile`
resume that intent after authoritative effect reconciliation; repeated close
uses the retained interrupt choice and operation identity. Modern output
exposes the safe close operation identity, progress and terminal Run state
without publishing captured policy bodies or credentials. Accepted legacy
sessions retain public `CloseRun` and its frozen projection.

Reusable facade-v4 Specialists bind Agent Configuration v3 and an explicit
Markdown Role at hire. A configuration with purpose `review` uses a read-only
candidate-tool session from initial Thread creation. Structured v4 review tasks
require that session, a qualified reading boundary and an explicit target;
an existing general-purpose Thread cannot be upgraded implicitly. Each task
owns an immutable reader and receipt history. The authenticated assignment
binds that reader while the actor is idle, before Turn input, and subsequent
tasks cannot reassign earlier receipts or histories. Initial candidate-tool
availability does not require a checked reader for generic or structured v3
tasks. Their authenticated dispatch clears any preceding task's reader;
restart never restores an earlier task's reader for them. They retain the
bounded generic task/result contract.

**CW-PROJECT**: Observer-safe successor output MUST expose resolved requested
access, current effective access/verification, transition and writer state,
instruction identity and selected Role, and review target/drift/provenance
assurance. It MUST omit source paths, instruction bodies, Controller credentials
and private tool state. Controller-only persisted captures are not observer
data. Capability output MUST distinguish `supported`, `unverified` and
`unavailable` for managed instructions, modern startup and checked target reads;
failure to qualify one guarantee cannot silently downgrade it. A schema's
presence MUST NOT imply runtime support.
Workspace Writer status MUST remain readable when a modern Run holds the
writer slot. Its frozen status shape projects only the shared lifecycle,
identity, cursor, and writer fields; it MUST NOT expose a successor Run
projection through a public-v1 Run method.

| Successor error | Phase/outcome and required action |
| --- | --- |
| `SOURCE_ISOLATION_RETIRED` | Admission/not dispatched; migrate the request |
| `ROLE_MIGRATION_REQUIRED` | Admission/not dispatched; explicitly migrate the Role input |
| `INSTRUCTION_SOURCE_INVALID` | Admission/not dispatched; fix the selected source |
| `INSTRUCTION_DISCOVERY_UNSUPPORTED` | Admission/not dispatched; select a qualified runtime |
| `WORKSPACE_STARTUP_UNSUPPORTED` | Admission/not dispatched; select a qualified startup/policy mechanism |
| `REVIEW_READING_BOUNDARY_UNSUPPORTED` | Admission/not dispatched; select a qualified runtime/tool boundary |
| `REVIEW_CANDIDATE_PROVENANCE_INVALID` | Inconclusive; inspect the original review |
| `REVIEW_TARGET_DRIFTED` | Inconclusive; explicitly submit a new review if desired |
| `REVIEW_TARGET_UNAVAILABLE` | Failed or not dispatched; restore the pinned target without substituting live bytes |

These errors MUST use the closed safe details in error v4 and exit status 4.
Provenance-invalid and drift errors MUST bind the existing review identity so
their inspect or resubmit action can refer to the original review.
They are non-retryable for the same accepted operation. Existing writer,
Controller, transition and idempotency errors retain their current semantics.

The following cases are implementation obligations, not executed evidence.
Each Task MUST map its actual production path and focused Rust/black-box tests
to these IDs and obtain its independent read-only review. OS and external-runtime
claims require empirical evidence in addition to deterministic fakes.

| Task | Cases and success/failure checks |
| --- | --- |
| TASK-062 | CW-SOURCE/INIT/CAPTURE: exact bytes/digests; combined ceiling and UTF-8/NUL; missing and unsafe inputs; symlink ancestry/leaf, owner/mode, replacement and in-place race; no allocation on failure; init preserves existing files |
| TASK-062 | CW-COMPOSE/SNAPSHOT/AMBIENT: separate task/Role; cwd versus CODEX_HOME; edited/deleted sources on retry/recovery; installed-policy and session captures; conflicting Profile/project/ancestor files plus explicit-body positive controls in separately approved pinned Codex probes |
| TASK-063 | CW-START/WRITER/TRANSITION: defaults and explicit read-only; startup without Turn; competing controllers/Profiles; writer conflict before provider effects; no downgrade; same-Run acquire/release; pending interaction and descendants; crash before/after reservation, policy proof and publication; original receipt on retry/restart |
| TASK-064 | CW-NOCOPY and cutover: every facade; retired copy/isolation pre-effect; accepted legacy completion/inspection/settlement; missing-resource recovery; failure/close/export/delete never remove caller roots; reassess shared launch-preparation responsibilities |
| TASK-065 | CW-SCOPE/PIN: six scopes; non-Git rejection; root/first-parent and two-dot/three-dot ranges; moved refs; differing worktree/index/commit bytes; missing object; bounds, secrets and unsafe paths; no checkout |
| TASK-065 | CW-READ/DRIFT/RESULT: full-file and diff receipts; deliberate native worktree read, forged/missing receipts and unsupported boundary; unchanged refs/fingerprints still reject bad provenance; index/dirty drift; fresh reviewer and original unknown-outcome recovery |
| TASK-066 | CW-MIGRATE/PROJECT and activation: common/project/name collisions with explicit choice; no overwrite or snapshot rewrite; old receipts/readers; unchanged TASK-056 clients; synchronized help, examples, operations and packaged skill; complete pinned gate and separately approved integrated live probes |

EPIC-009/TASK-027 consumes captured Role selectors and caller-workspace
activation. EPIC-010/TASK-028 through TASK-031 consume safe access/review
projections, deletion ownership and composition. EPIC-011/TASK-032 through
TASK-034 extend deterministic, crash and separately approved live conformance.
Their existing IDs, status and completion gates remain unchanged.
