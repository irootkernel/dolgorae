# Dolgorae v0.1.4 successor requirements

Status: TASK-067 requirements reconciliation accepted. This document records
adopted requirements from the [alignment dossier](../todo/TODO-DOLGORAE-V014-RELEASE-ALIGNMENT.md).
The source selection and implementation/acceptance responsibility split are
accepted. Independent review accepted the bounded host feasibility result.
No checked successor or runtime capability is published here.
The [roadmap](../roadmap/README.md) owns delivery status.

Uppercase normative words have the meaning defined in the [product specification](README.md).
The [three-consumer acceptance specification](dolgorae-consumer-acceptance-v014.md)
owns AQ/PW/GU requirements, evidence delivery and release gates. All rows remain
mandatory. The [coordination requests](dolgorae-v014-contract-coordination.md)
record the selected sources and accepted implementation/qualification ownership.

## Retained and successor behavior

| Surface | Retained meaning | v0.1.4 requirement and owner |
| --- | --- | --- |
| Public API | TASK-053 public-v1 source and descriptor remain byte-identical; accepted v1 records retain supported reads, collection, close and recovery | Public v2 projects caller-workspace Runs and Markdown Roles directly; TASK-068 freezes shapes, TASK-078 implements |
| Modern startup | Caller-workspace identity, default writable/general selection, explicit lane/access and one writer remain in force | Gul creation mode is immutable provenance, separate from effective access/verification; fixed read-only pooling cannot become writable |
| Instructions | Accepted snapshots retain exact bytes; legacy JSON Role authoring is not restored | Capture Role, named dependencies, selected guidance and admitted configuration before dispatch; TASK-076 defines execution identity |
| Capabilities | A schema or descriptor alone grants no runtime availability | Negotiate method subset, credential identity, bounds and descriptor digest; renegotiate on server-instance change; isolated candidate and production admission remain distinct |
| Submission | Durable intent precedes effects, durable receipt precedes acknowledgement | Original-key replay and authorized operation lookup resolve loss; TASK-070 implements without reconstructing attachments |
| Close and generation recovery | Interrupt acknowledgement is not terminal proof; exact-build mutation refusal remains | One durable close owner resumes after restart; only proved quiescent old generations can hand off; TASK-071 implements |
| Observation | Existing stamped MINIMAL/OPERATIONAL projections and old interaction history retain meaning | Bounded forward/tail pages, artifact-backed large content, separate stream budgets and safe operational events; TASK-075/079 implement |
| Delegation | Semantic acceptance belongs to Primary/Judge; Podway owns structural acceptance and progression | Versioned accepted instruction/task/result identity, rounds and current disposition through facade/Broker/lookup; TASK-076/077/086 implement |
| Findings and groups | No automatic semantic judgment or model synthesis | Checked findings, immutable groups and deterministic provenance-preserving merge; synthesis is an explicit accepted task; TASK-084/085 implement |
| Pool and queues | One active Turn per Run; no shared writable workers | Qualified fixed read-only pool, bounded queues/reuse/deadlines/passivation/activation, native subagents disabled by default; TASK-078/080/082 implement |
| Collaboration | External Engagements have no lateral Specialist messaging | Complete internal lateral collaboration and wait-cycle rules within v0.1.4; TASK-081/083/027 implement |
| Storage | Accepted identity/audit bytes remain immutable; no downgrade guarantee or automatic result deletion | Capacity reserve, crash-safe checkpoints/compaction, explicit operator maintenance and result retention; TASK-072/073/075 implement |
| Release | v0.1.3 history and supported regression paths remain version-qualified | Qualified H1 precedes Gul E15; accepted H2 precedes EPIC-019; final three-consumer evidence and both products' QA precede Gate D/publication |

## Failure, effect and recovery rules

The following cases define acceptance obligations. Their results remain pending.

| Case family / behavioral owner | Failure and effect rule | Recovery and acceptance obligation |
| --- | --- | --- |
| ID / this document and caller-workspace contract | Unsafe, raced or over-budget source admission MUST fail closed before allocation/dispatch; accepted bytes MUST NOT be reconstructed from mutable files | Known answers distinguish instruction, request envelope, accepted task and complete result; drift cannot change a retained acceptance |
| OP / product specification, successor operation contract | Rejected, RetrySameKey, Unknown and RecoveryNeeded MUST be separate from effect. Current-request rejection MUST NOT erase a prior uncertain effect | Scoped Controller/operation-kind lookup returns receipt, definitive non-acceptance, unresolved effect or authoritative absence; response loss never authorizes a new key |
| LIFE / product specification and architecture | Busy/unknown generations MUST NOT be silently replaced. SETTLING MUST identify a live advancing close owner; release failure MUST remain visible even after retained close success | Restart/reconcile continues the same close; exact old-generation absence, no active Turn/interaction and writer settlement precede handoff |
| OBS / product specification and successor timeline contract | Heartbeat head MUST NOT advance delivered cursor or prove a gap; oversized content MUST NOT stall a one-item page | Normal timeline pages obey a 4 MiB encoded response budget and compatible receive limits; opaque cursors, stable head, chunk integrity and page-level ingest permit complete recovery |
| INT / product specification and interaction contracts | Responses MUST check current Controller/request/Run/Turn/generation; absent answer owner MUST produce a bounded explicit decline/interruption | Typed permission/MCP decisions, protected-content canaries, safe opened/resolved unsupported history and continued session availability are required |
| DELEG / consumer acceptance specification | Model arguments MUST NOT supply invocation authority; unresolved work MUST NOT be redispatched or exported idle; local prepared digests MUST NOT stand in for accepted executor facts | Both host routes pass the pre-contract matrix; authorized complete result/receipt lookup, conservative rounds and current idle/released proof survive restart/loss |
| FIND / this document and successor findings contract | Invalid structured output MUST yield FINDINGS_OUTPUT_INVALID; unknown/failed sources MUST remain in coverage | Stable keys, evidence/coverage/limitations and complete per-source provenance survive merge; explicit conflicts are preserved and algorithm/input identity is reproducible |
| SCHED / product specification and collaboration policy | Lease expiry MUST NOT revoke an accepted/unknown Turn; a fixed read-only member MUST NOT acquire writer authority or stop another member through passivation | Bounded fair queues, compatible pool configuration, whole-generation shutdown proof and crash-safe activation are required; internal wait cycles are rejected |
| STORE / architecture and persistence contracts | Predictable capacity refusal MUST remain distinct from integrity failure; maintenance MUST NOT bypass a live writer, drop active/unknown references or change accepted identities | Reserved finalization capacity, locked generation selection, checkpoint continuity, crash recovery and explicit dry-run retention/GC preserve reader compatibility and auditability |
| REL / consumer acceptance specification | Schema existence, version text, report acknowledgement or provider-only tests MUST NOT establish actual consumer acceptance | Reproducible locks, exact final artifact bindings, independent acceptance and all mandatory gates are required; substantive candidate changes trigger impact assessment and affected requalification |

## Feedback coverage and acceptance cases

Each case below belongs to the family above and to its named implementing
Task. TASK-067 reconciles the requirement; TASK-068 publishes applicable
shapes/fixtures. Runtime implementation and measured performance remain with
their later owners. Advisory diagnoses are hypotheses until reproduced; an
invalid/non-applicable disposition needs evidence. Estimated CPU, memory,
fsync, duplication and code-size costs are not adopted as measured defects.

| Feedback | Family / acceptance case | Implementing owner |
| --- | --- | --- |
| CD-01 | LIFE: rebuild while idle/busy retains cross-build mutation refusal and typed recovery | TASK-071 |
| CD-02 | OBS: count executable/schema probes with config/file invalidation | TASK-075 |
| CD-03 | OBS: bounded durable waits reuse executing-build evidence | TASK-075 |
| CD-04 | STORE: capacity reserve and interrupted segment/checkpoint maintenance | TASK-072 |
| CD-05 | STORE: Profile compaction and durable cleanup failure at crash boundaries | TASK-073 |
| CD-06 | LIFE: worker failure/poisoned journal retains intent and recoverability | TASK-074 |
| CD-07 | STORE: indexed reads, content deduplication and explicit retention integrity | TASK-075 |
| CD-08 | OBS: steady calls spawn no Git; identity changes invalidate observation | TASK-075 |
| CD-09 | ID: modern public projection coexists with frozen accepted legacy records | TASK-067, TASK-078 |
| CD-10 | ID: selected guidance is captured; ambient discovery stays suppressed | TASK-076 |
| CD-11 | SCHED: pool isolation, generation cap/warning and subagent policy | TASK-078 |
| CD-12 | REL: consumer inventory/migration precedes retirement; frozen regressions remain | TASK-087 |
| CD-13 | REL: recursive architecture guards enforce justified component boundaries | TASK-087 |
| CD-14 | REL: protoc/build/CI identities and exact released artifacts are reproducible | TASK-067, TASK-087, TASK-089 |
| GI-01 | LIFE: close resumes with current Primary revision, retained cause and actions | TASK-071 |
| GI-02 | INT: unsupported variants produce safe opened/resolved timeline history | TASK-079 |
| GI-03 | ID: modern Runs do not break v1 listing or shared writer status | TASK-078 |
| GI-04 | ID: fresh modern Role/policy provisioning and Profile rebinding | TASK-078 |
| GI-05 | OP: definitive refusal resolves intent; original-key restart retry admits once | TASK-070 |
| GI-06 | OBS: ledger tailing and idle wake/backoff isolate stream work budgets | TASK-075 |
| GI-07 | OBS: cheap shared display sampling retains fresh mutation safety proof | TASK-075 |
| GI-08 | OBS: Profile lookup invalidates cached executable/schema on configuration drift | TASK-075 |
| GI-09 | OP: method-specific effects, writer-holder diagnostics and response-loss lookup | TASK-068, TASK-070 |
| GI-10 | OBS: redacted command/completion/change/error events reject secret canaries | TASK-079 |
| GI-11 | INT: versioned permission/MCP routing preserves old unsupported history | TASK-079 |
| GI-12 | LIFE: typed build blockers, bounded server work and safe diagnostics | TASK-071, TASK-075 |
| GI-13 | OP: authorized lookup needs neither attachment paths nor reconstructed request | TASK-070 |
| GI-14 | OBS: bounded forward/tail pages recover complete artifact-backed large content | TASK-068, TASK-075 |
| GI-15 | REL: tested intended release artifact is distinct from development identity | TASK-087, TASK-089 |
| GI-16 | OBS: aggregate reads reuse observations and derive availability from source facts | TASK-075, TASK-078 |
| EN-A1 | DELEG: accepted identity, full result, rounds, disposition and lookup agree | TASK-076 |
| EN-A2 | ID: bounded inline Role captures exact dependency bytes | TASK-076 |
| EN-A3 | DELEG: both pinned trusted host routes pass replay/conflict/restart/loss matrix | TASK-067, TASK-068, TASK-077 |
| EN-A4 | DELEG: versioned Podway exports pass external/managed known answers | TASK-077, TASK-086 |
| EN-A5 | ID: selected guidance participates in captured instruction identity | TASK-076 |
| EN-A6 | INT: ownerless interaction declines/interrupts within bounded policy | TASK-079 |
| EN-B1 | FIND: checked finding evidence, coverage and limitations precede publication | TASK-084 |
| EN-B2 | FIND: immutable group assignment and any/all/k wait recover terminal/unknown members | TASK-084 |
| EN-B3 | FIND: deterministic merge preserves conflicts/gaps and algorithm/source identity | TASK-085 |
| EN-B4 | FIND: explicit synthesis adds its own round/result; Podway receipt scope is accepted | TASK-085, TASK-086 |
| EN-C1 | STORE: capacity/observation prerequisites hold under larger fan-out | TASK-072, TASK-073, TASK-075 |
| EN-C2 | SCHED: configured K isolates fixed read-only members and suppresses native subagents | TASK-078 |
| EN-C3 | SCHED: external assignments retain queue/reuse/deadline/activation recovery | TASK-080, TASK-082 |
| EN-C4 | SCHED: internal lateral delivery and wait-cycle rules pass integrated acceptance | TASK-081, TASK-083, TASK-027 |
| EN-D | INT: current Controller owns command/permission/MCP answers or explicit decline | TASK-079 |

Gul G4-1 through G4-7 remain Gul E15-T21 foundation obligations against its
completed T4. Their provider dependencies are TASK-068/070/071/075/078/079.
H1 supplies frozen correction requirements; completed H2 supplies Gul's scoped
verification. TASK-088 later verifies corrected actual-client behavior.
Gul CAS, drafts, immutable unknown material, shared attachments, six-stream
fairness, bounded local queries and additive migration remain mandatory.

## Dolgorae-owned contract decisions

These decisions select the provider-side design. Production implementation and
independent acceptance remain separate gates. They do not accept a consumer work plan.

### Successor generations and public admission

Public v2 MUST be a separate `dolgorae.public.v2` package and descriptor with
its own Gul consumer-v2 lock and generated-client module. Frozen public-v1
bytes and generated clients MUST remain unchanged. Machine and External
Specialist facade are already v4; changed envelopes MUST use v5, not reinterpret
v2. A persisted shape MUST advance from its own current generation only when
its meaning changes. Existing v2/v3 shapes MUST NOT be overwritten.

The v2 package MUST include modern start/provisioning, scoped operation lookup,
forward/latest timeline paging, typed interactions, result/group/artifact
discovery and capability negotiation. Creation mode is immutable and separate
from effective access, writer state and policy verification. Provisioning MUST
use accepted Markdown Role/policy/configuration snapshots through supported
Machine admission and public discovery; Gul MUST NOT author legacy JSON Roles.

All advertised methods MUST have one explicit mutation-policy row. Pure reads
have `no_effect`; absence of a read response grants no mutation authority.
Start, submit, close and interaction answers use durable operation identity.
Accepted replay returns the original receipt; definitive non-acceptance may
permit original-key retry; uncertain admission permits lookup only. Remaining
mutations MUST declare either keyed replay or state-based reconciliation and
its authoritative getter. Unknown errors MUST NOT imply `no_effect`.
TASK-068 MUST freeze each method's exact classification and lookup availability.

### Trusted host adapter selected for qualification

Dolgorae owns the trusted host hook, stdio adapter, installation/configuration
recipe, protected state and operational qualification harness. Aquarium owns
consumer-side collection, provenance/applicability checks and semantic
integration. Authorized consumer-side workflow agents perform independent
consumption checks; existing Primary/Judge and Master retain acceptance roles.

The selected candidate uses a synchronous native `PreToolUse` hook before MCP
dispatch. Its protected state maps the configured host route, native session,
native Turn when supplied, tool-use ID and exact server/tool binding to one
UUIDv7 and accepted public-input digest. A fresh native invocation with equal
arguments gets a distinct identity. A repeated native invocation retains its
identity; changed input conflicts before effect. Neither public tool arguments
nor JSON-RPC IDs can choose that identity.

The hook MUST durably commit the identity before allowing dispatch. The Codex
route leaves public arguments unchanged: native `_meta.threadId` and
`_meta.callId` select the precommitted native-event binding. Both fields MUST
match exactly one retained binding. They do not replace its UUIDv7. The Claude
route returns a private opaque binding in `_dolgorae_host_binding` through
`updatedInput`; the adapter verifies and removes it before processing public
input. The public tool schema MUST exclude that property. A model-supplied
private binding MUST fail closed. Both adapters check the accepted input digest
and configured route/server/tool. Missing/failed/timed-out hooks MUST NOT bypass
server-side validation. Host arguments cannot supply account credentials.

The binding is a protected per-invocation random capability with at least 256
bits of entropy, separate from the public UUID. It MUST NOT appear in tool
results, model requests, ordinary diagnostic logs, exported receipts or consumer
artifacts. Claude retains hook stdout in its native conversation journal; that
journal is explicitly part of this route's protected identity state, under the
installation/state owner's isolated host configuration root. It MUST remain
current-user-owned and inaccessible outside that protected root, MUST NOT be
exported as review/evidence, and follows the same retention and uncertainty
rules as the adapter state. Host launch uses umask 077; state directories/files
are current-user-owned 0700/0600, without symlink traversal.
Initialization is explicit; normal hook, delivery and lookup MUST NOT recreate
missing state. Missing/corrupted state and failed lookup retain uncertainty or
refuse before effects. No automatic reset, new identity or redispatch is allowed.

The adapter stores `prepared`, `accepted` and `resolved_not_accepted` separately.
Receipt and acceptance commit atomically before acknowledgement. A prepared
delivery with unknown effect resolves only through authorized lookup. Concurrent
replay admits once; retained acceptance survives host reconnect and MCP process
replacement. State generation, caller scope and operation kind fence lookup.
Retention MUST preserve identity, conflict and receipt metadata while any
delivery is uncertain; explicit retirement MUST NOT turn forgotten history into
authoritative absence.

This carrier is the explicitly scoped successor to ADR-031 described in
[ADR-041](../architecture-decision-records/README.md#adr-041-qualify-a-protected-native-host-hook-binding-for-external-specialist-mcp).
The initial qualification pins remain Codex CLI 0.160.0 and Claude Code
2.1.295. Both actual isolated hosts passed the
[prototype qualification scenarios](dolgorae-v014-host-contract-qualification.md),
including physical host loss before acknowledgement and supported receipt
recovery. Independent review accepted this bounded pre-contract qualification.
Aquarium's native Codex 0.162.0/serialized-child 0.162.1 and Claude
Agent SDK 0.3.295 evidence remains scoped to those native paths; it neither
changes these pins nor qualifies this adapter. Both actual host routes MUST
pass before freeze. A prototype receipt is not Phase 3 facade implementation.

### Exact digest and Podway export domains

Raw captured bytes retain plain SHA-256 hashes and exact UTF-8 lengths. New
structured identities use `sha256:` plus lowercase hex of
`SHA256(UTF8(domain + "\n") || JCS(closed_object))`. Each domain includes its
schema version. JCS MUST reject duplicate keys and unrepresentable numbers.
The [method and digest contract](dolgorae-v014-method-and-digest-contract.md)
defines the closed objects and method-specific recovery decisions. TASK-068
MUST publish their checked shapes and independent known-answer vectors.

| Domain | Included immutable inputs | Excluded inputs |
| --- | --- | --- |
| `dolgorae.role-snapshot/v1` | Body length/hash; dependencies sorted by unique name with length/hash; selected guidance identities | Live paths, current rereads, Role name as a substitute for content |
| `dolgorae.instruction-identity/v1` | Core version/hash, common capture, Role snapshot, ordered selected guidance and composition separators | Ambient guidance claimed as captured, dynamic Turn access context |
| `dolgorae.admitted-configuration/v1` | Resolved Profile binding, model/effort, lane/access/assurance and subagent policy | Credential secrets, mutable observations/timestamps, display names |
| `dolgorae.accepted-task/v1` | Executor-accepted task bytes, ordered immutable context identities and output contract | Envelope/key/transport identity and merely prepared requests |
| `dolgorae.mutation-envelope/v1` | Authenticated caller scope, operation kind and exact admitted public input | Transport ID and claim of accepted task identity |
| `dolgorae.complete-result/v1` | Terminal outcome, complete content hashes/lengths, accepted change components and checked structured provenance | Preview, summary, artifact ID and live workspace content |
| `dolgorae.group-assignment/v1` | Ordered immutable assignment manifest and bounded member/task identities | Mutable scheduling order |
| `dolgorae.merged-findings/v1` | Algorithm/version, sorted source task/result identities, conflicts and missing/failed/unknown coverage | Similarity-based deduplication and omitted uncollected sources |

`dolgorae.delegation-identity/v1` MUST expose these accepted facts through hire,
assign, collect, Broker delivery and authorized read-by-key lookup. Both export
mappings consume this same fact record through different authorization paths:
`dolgorae.podway-external/v1` uses External Engagement scope and external
Controller provenance; `dolgorae.podway-managed/v1` uses Orchestrated Session
scope and root Primary provenance. Neither mapping may impersonate the other.

The receipt maps actual accepted member/task IDs, admitted Role/configuration
digests and the complete Primary-accepted result digest. It binds explicit
adapter ID/version, configured binding, supervisor and caller scope. Rounds
count accepted assignments and uncertain dispatches conservatively, exclude
proved non-dispatch/replay/collection, and obey Podway's explicit `max_rounds`
1..32. `after_accept=retain` requires current proved `idle`;
`after_accept=release` requires successful `released` settlement. Podway computes
its own accepted-items digest; the adapter cannot choose it or advance a cursor.

One individual task maps to one delegated Action. A synthesis task, if requested,
maps to its own Action. Groups retain full provenance as Dolgorae operations;
this contract makes no group-as-one-Action, fan-out or child-Action claim. This
selection requires no new Podway group implementation. Consumer mapping
acceptance and actual-path qualification remain explicit later gates.

### Bounds and maintenance

All captured common, Role, dependency and selected-guidance bytes together
MUST fit the existing 65,536-byte raw-source ceiling. Core prefix, dynamic
context and task bounds remain separate. Existing task context/deadline/key,
collaboration-policy, stream and artifact bounds remain unchanged. Normal
timeline responses are at most 4 MiB encoded with complete artifact-backed
oversized content; stream queues remain 32 envelopes/4 MiB with five-second
stalled delivery handling. Existing inline result 1 MiB, artifact 32 MiB,
chunk 1 MiB and per-Run artifact quota 256 MiB remain in force.

The operator pool configuration selects K in 1..16, default 1; dedicated live
generation cap is 1..32, default 8. The sixth live dedicated generation emits
a warning. Lowering a cap does not terminate or reassign accepted work; it
refuses additional admission until current usage fits. A 16-member fixed
read-only qualification uses at most configured K compatible servers. Account,
Profile and process-static configuration mismatches require separate pools.
An explicitly qualified native-subagent configuration uses a dedicated lane.

Groups admit 1..64 tasks, preserving the required 50-task qualification case.
The immutable assignment manifest is at most 1 MiB encoded and references
accepted task/context identities instead of embedding unbounded content.
Existing structured findings remain limited to 64 per source. Merged content
must fit the existing 32 MiB artifact limit; larger inputs yield a typed capacity
refusal that preserves complete source/coverage references. Bounded collection
pages use artifacts rather than embedding every source result in one response.

Ledger maintenance keeps the 512 MiB/1,000,000-record replay bounds per selected
generation. Admission MUST reserve finalization/cleanup capacity for every
accepted operation before admitting it. Capacity refusal is distinct from
integrity failure. Recovery/finalization may use retained reserve over the
ordinary admission limit; it cannot bypass corruption checks.

Maintenance takes the workspace lifecycle lock before the ledger writer lock,
closes new admission and proves no live writer/unknown workload before mutation.
A reader selects and pins one immutable generation manifest containing schema,
checkpoint digest, segment identities, chain hash and durable watermark. It
MUST NOT combine generations. New checkpoint/segment files and their directory
entries are synced before an atomic manifest switch. Pre-switch crash selects
the old generation; post-switch crash validates and selects the new one.
Old generations remain while reader pins or recovery references require them.

Profile/membership compaction preserves active/unknown members, immutable
bindings, accepted identity bytes and audit continuity. Retention/GC is explicit
operator work with dry-run scope, authority checks and protected-reference
checks. Retired content gets a durable tombstone and typed unavailable result;
identity/receipt metadata remains. Automatic deletion is disabled. Cleanup
failure after a successful close preserves its receipt and exposes recovery.

## H1 publication inventory and reproduction

TASK-068 publishes and verifies the following artifacts. The immutable lock
MUST enumerate each required file, its raw-byte
SHA-256, schema/contract generation and generator inputs. A lock cannot contain
an unresolved required file or an unaccepted prerequisite disguised as a note.

| Artifact family | Required contents and checks |
| --- | --- |
| Public v2 | Separate `dolgorae.public.v2` source, complete descriptor and supported-method/capability policy; modern Run/provisioning, scoped operation lookup, timeline, interactions, results/groups/artifacts; descriptor-to-policy coverage and breaking checks |
| Gul consumer v2 | Named method subset, source/descriptor bindings, generated Go module, client/error/effect/mutation policies, examples and immutable consumer lock; client compilation and fixture decoding against the selected descriptor |
| Machine and facade v5 | Separate checked envelopes, commands/tool definitions, admission/error/effect/recovery policies and capability examples; no reinterpretation of accepted v4 envelopes |
| Identity and delegation | Closed Role/instruction/configuration/task/mutation/result digest shapes; protected invocation binding and accepted receipt/lookup shapes; separate external/managed Podway mappings, rounds, disposition and stale-receipt examples |
| Findings and groups | Checked findings, immutable group assignment, wait/coverage results and deterministic merged findings; explicit synthesis task identity; bounded artifact-backed source/result discovery |
| Persisted state | Versioned modern Run/configuration, invocation/operation intent and receipt, group/merge, pool/queue, lifecycle and maintenance shapes where their meaning changes; explicit old/new readers, supported recovery and migration rules |
| Fixtures | Known answers for every digest domain; equal-text/distinct-call, replay/conflict, original-key response loss, prepared/unknown/absence, wrong scope/generation, stale revisions and malformed/unsupported variants; exact-limit and limit-plus-one UTF-8/encoded/artifact cases |
| Consumer qualification contract | All AQ/PW/GU rows, exact selected source pins and accepted external decisions/work plans; complete case inventory, producer/acceptance owners, H2/H3/H4 delivery obligations and candidate-change applicability rules |
| Native feasibility binding | Reviewed prototype/harness source and tested executable hashes, both host pins, bounded scenario results and assurance limits; retained structured evidence only under the approved promotion policy |

The initial new-v2 reproduction pins are Rust 1.97.1, Buf 1.69.0, Go 1.26.6,
protoc 36.2, protoc-gen-go 1.36.11 and protoc-gen-go-grpc 1.6.1. Python uses
the repository validation requirements. TASK-068 MUST record exact generator
commands, input paths and output hashes and reproduce them without rewriting
the frozen v1 package. Its historical protoc 35.1 pin and all frozen source,
descriptor, policy, fixture and generated-client bytes remain unchanged.

Persisted generation selection starts from the actual current shapes: modern
Run manifest v3, agent configuration v3, instructions v2, Run state v1,
idempotency intent v1, audit record v2, Profile membership v2, workspace record
v1 and Profile server state v2. A changed shape receives a separate successor
generation; an unchanged shape retains its existing generation and meaning.
The lock MUST identify each selected shape individually. A product-wide version
increment does not authorize rewriting accepted records.

Fixture coverage MUST include the ID/OP/LIFE/OBS/INT/DELEG/FIND/SCHED/STORE/REL
families and every feedback row above. Gul's G4-1..7 cases cover advisory
heartbeat heads, immutable creation mode, contextual error/effect recovery,
independent timeline degradation, refresh masks/page-level ingest, receive/page
bounds and settling attention, and checked command/status/timing projections.
These fixture obligations do not replace their later actual-runtime cases.

## Later qualification records and delivery

Every record binds immutable H1 package/manifest digests and applicable matrix
rows. It names the producing work unit, source/artifact identities, supported
runtime/adapter/configuration pins, scenario results/limits and independent
acceptance responsibility. Secret state, raw host journals and runtime session
identities are excluded from portable evidence. Future implementation hashes
and measured results remain pending until their scheduled producer completes.

| Record | Producer and required contents | Delivery and acceptance owner |
| --- | --- | --- |
| H2 foundation | Gul E15 closeout: exact H1 pin, completed E15-T21 G4-1..7 corrections, scoped fixture/generation checks, CAS/draft/unknown-material/shared-attachment protections, six-stream allocator, bounded queries, additive migration and assigned E16 runtime cases | Gul returns its completed Epic output; TASK-069 in EPIC-019 independently validates the pin, completed foundation and Gate A. Neither Task starts during EPIC-018 |
| H3 provider candidate | TASK-087 after EPIC-019/020/009/021: intended v0.1.4 source and binary/package hashes; actual Aquarium/Podway consumer and adapter implementation identities; accepted route/mapping/receipt/interaction evidence; runtime/configuration pins, setup/harness instructions and complete repository gate | Dolgorae delivers the fixed candidate after EPIC-021 closeout; Gul admits that exact candidate for E16. Consumer owners accept their applicable paths; TASK-088 later checks final-artifact applicability |
| H4 consumer campaign | Gul E16 closeout and E16-T16: intended v0.1.0 source/package hashes bound to unchanged H3/H1; own gates/QA, all deterministic native fault/load/recovery cases, real-account functionality, device usability, 24-hour three-client soak and packaging/recovery evidence | Gul returns completed product/qualification output; TASK-088 independently verifies/reproduces necessary cases and accepts combined Gate C, then TASK-089 verifies Gate D provenance for separate release owners |

Aquarium's host/integration acceptance owner and Podway's structural-contract/
candidate-admission acceptance owner are the EXT-AQ-01 and EXT-PW-01/02
decisions. Dolgorae's TASK-077/086 owners produce executor/adapter facts;
TASK-088 independently verifies consumer acceptance and TASK-089 checks the
final combination. Provider implementation cannot self-certify consumer
semantic or structural acceptance. The accepted coordination decision names
these accountable workflow roles before H1 freeze.

## Measurement and real-account campaign definitions

Component measurements use isolated state, the supported pinned toolchain and
one recorded build/configuration. Record hardware/runtime and instrumentation
in bounded local evidence. For each warm-read case, create a 30-Turn Run and
a 500-item timeline, warm with 20 calls and measure 200 sequential calls in
three repetitions with a monotonic clock; report each repetition's nearest-rank
p95 and counts, without averaging away a failed bound. Include five Profiles
for Profile reads and eight Runs for aggregate reads. The dossier's 50 ms
GetRun/Profile and 100 ms aggregate limits apply to each repetition.

The six-stream component case lasts ten minutes, including an injected
five-second stalled reader and reconnect/resume. Counters MUST establish zero
repeated Git spawns and zero repeated full-ledger replay during steady idle
observation. Eight-task wait uses eight independent existing core tasks, a
60-second idle baseline and 60-second waiting interval, recording CPU/process/
poll/read counters; one scripted completion MUST be observed within one second.
Report the baseline and waiting delta for the dossier's near-idle assessment.
This case requires no future pool/group implementation.

TASK-078 tests 16 fixed read-only members at configured K=1 and K=4, including
incompatible Profile/account/static configurations and default-disabled native
subagents. TASK-084 tests 50 tasks with K=4 and bounded queues, including failed,
expired and unknown members; every source remains in result/coverage accounting.
TASK-073 uses 10,000 synthetic membership cycles. TASK-072 tests record-count
and byte capacity independently, then finalizes accepted work, Close and writer
cleanup using reserved capacity. Record actual growth/counters, query plans,
backpressure and crash-boundary results against the bounds above. TASK-075 owns
component baselines; Gul E16-T16 owns integrated measurements and TASK-088
independently checks the final pair. Earlier component results do not certify Gul.

The account-backed functionality scenario selects the Codex CLI 0.160.0 pin in
the native qualification contract, an explicit `gpt-5.6-luna` / `low` Profile and
a private disposable workspace. H3 MUST bind the qualified execution runtime;
a replacement requires impact assessment and affected requalification.
The first task reads a fixture containing `alpha`, requests an actual supported
approval to run a single bounded local command that writes `beta` to a designated
scratch output, and returns both values. Gul supplies that approval through its
real UI; provider decision and continuation MUST agree. Collect and verify the
complete result, then send one follow-up in the same Session to read and report
`beta`, and close normally with proved terminal/writer settlement. Missing actual
approval or follow-up support fails the campaign; a fabricated card cannot pass.

The scenario permits one initial task and one follow-up, at most three minutes
per task and ten minutes overall, with no network command or automatic retry of
uncertain work. Gul E16-T16 owns its actual-client harness and approval UI;
TASK-079 owns provider approval routing and TASK-088 independently checks the
completed campaign. H2 records Gul's scenario/harness allocation against H1;
Gul acknowledgement is not a prerequisite for EPIC-018 closeout. Account use
and its financial/token ceiling require separate explicit authorization at
execution; this scenario grants neither. A changed selected runtime/model/effort/approval
configuration requires impact assessment and affected requalification. Real
account functionality, deterministic faults/load and device/soak results remain
separate required records.

## Accepted TASK-067 decisions and remaining handoff

Independent completion review accepted the selected native host prototype and
its exact source/artifact binding. This accepts bounded pre-contract feasibility;
it does not establish production runtime support or actual consumer-runtime
acceptance. The selected sources, mapping and implementation/acceptance
responsibilities are adopted in the specification, architecture and ADR owners.
The historical Codex 0.149.0 negative MCP disposition and frozen public-v1
artifacts remain unchanged. At TASK-067 acceptance, TASK-068 freeze, Epic
validation and H1 delivery were pending. The roadmap owns their current status.
