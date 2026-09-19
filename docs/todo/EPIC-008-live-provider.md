# EPIC-008 Execution Dossier: Live Dolgorae Provider

This is the adopted implementation dossier for EPIC-008. The
[roadmap](../roadmap/README.md#epic-008-live-dolgorae-provider-and-brokered-hierarchy)
alone owns Task IDs, execution order, and status. Specifications own behavior;
this dossier supplies bounded implementation guidance and execution checklists.
Delete it at Epic closeout after promoting durable guidance to its owners.

## 1. Delivery decision

Deliver a live, independently testable Dolgorae provider for v0.1.3. A real
Primary must request Specialists, assign work, and consume their actual results.
An external client must be able to create, observe, approve, and recover this
operation through the existing public interface. Gul is not ready and is not a
prerequisite for EPIC-008 completion or v0.1.3 release eligibility.

The accepted implementation plan is eight Tasks, not the earlier sixteen-task
proposal. Integrate existing components, fix necessary live correctness gaps,
and avoid rebuilding the Broker, writer, artifact, or process ownership systems.

| Decision | Binding scope |
| --- | --- |
| Provider completion | `MILESTONE-BH1-P`; actual gateway and live Codex evidence |
| Gul integration | Separate deferred `MILESTONE-BH1`; never inferred from provider tests |
| Transport | Probe MCP and native candidates on the pinned Codex version, then support one proved path |
| Task granularity | One coherent implementation/test/doc result and one initial completion commit per Task |
| Busy member | No v0.1.3 queue; reject fresh busy assignments before effects, preserve exact accepted-task replay |
| Role/task | Preserve stable Role and exact accepted task content; do not force review criteria onto ordinary work |
| Public API | Existing frozen public v1 and minimum 24-method gateway, not the full TASK-029 surface |
| Release | Milestone Preview after separate release-candidate QA and explicit publication authority |

Authorities:

- [Provider slice](../specs/README.md#v013-live-provider-slice).
- [Provider versus Gul acceptance](../specs/README.md#provider-and-gul-acceptance-boundaries).
- [Live integration architecture](../architecture/README.md#live-provider-integration-boundary).
- [ADR-038](../architecture-decision-records/README.md#adr-038-deliver-the-live-provider-independently-of-gul).
- [Implementation tips](../implementation-tips/README.md).

## 2. Baseline and reuse map

Planning was based on committed tree `645e7e8` and the completed EPIC-007,
EPIC-014, and EPIC-015 foundations. Recheck the actual checkout before beginning
implementation; do not overwrite unrelated changes. These are code-inspection
checkpoints, not evidence that live provider acceptance already passed.

| Existing owner | Reuse and remaining integration concern |
| --- | --- |
| `src/orchestration.rs` | Durable Session/spawn/task/delivery state and fake adapter. Connect a production adapter; current dispatch returns `CompletedTask` synchronously, and tool waits currently inspect snapshots. |
| `src/semantic.rs`, `src/worker.rs`, `src/turn.rs` | Run/Worker/thread lifecycle and acceptance evidence. Preserve Controller, writer, pinned configuration, and unknown-outcome rules. |
| `src/gateway*.rs`, `src/gateway_service.rs` | Existing public gateway and Controller/observation/artifact paths. Route broker approvals without misrouting them to Codex pending requests. |
| `src/task_request.rs`, `src/specialist.rs` | Exact bounded task content and applicable result validation. Reuse helpers without importing external aggregate authority. |
| `src/engagement.rs`, `src/external_engagement.rs`, `src/review.rs` | Existing v1/v2/v3 behavior and accepted-request/deadline integrity patterns. Preserve compatibility; do not use the CLI/facade as a Broker backend. |
| `src/artifact.rs` and existing result storage | Publish actual immutable bytes and metadata, not an allocated but unreadable artifact ID. |
| `src/process_inventory.rs` | Existing detached-process ownership and cleanup. Register a bridge process only if the selected transport actually needs one. |
| `tests/gateway_native.rs`, `tests/gateway_semantic_native.rs`, `tests/support/gateway_native.rs` | Extend real gateway and generated-client fixtures rather than building a Gul imitation. |

Existing deterministic core tests are useful regressions but do not prove a
live model-facing tool, actual provider acceptance, or Gul compatibility.

## 3. Release slice and explicit limits

The required live path is:

```text
Trusted generated public client
  -> real dolgorae serve on a private Unix socket
  -> shared semantic service and durable orchestration Broker
  -> actual Primary tool call on the selected pinned Codex transport
  -> policy/approval-bound Specialist Run and accepted task
  -> checked immutable result
  -> actual Primary result consumption and authorized public result reads
```

In deterministic scenarios only the upstream Codex boundary may be faked;
the public gateway and semantic/Broker path are production code. Separately
required live evidence uses actual Codex and labels that distinction.

Both approval modes and all advertised access modes retain their safety
obligations. A live Primary that still owns an active canonical writer Turn
cannot silently yield, interrupt itself, or hand off atomically. Reject unsafe
transitions before task/writer effects and document the supported continuation
path using existing APIs. Do not claim that all requested write arrangements
are always possible.

v0.1.3 supports `never` and `reuse_idle_compatible` for live Session reuse.
Reject unsupported `reuse_any_compatible`, collaboration-enabled roles, and
automatic on-mail activation at live admission before Run allocation. Keep the
future policy enum values and historical fixtures/snapshots readable. A raw
core fixture proving future reuse selection is not a live capability promise.
A fresh busy assignment returns checked `RUN_STATE_CONFLICT` without accepting
a second task. Exact replay of an earlier accepted assignment instead returns
its original receipt. Multiple independent ready members are not limited to
one global active Specialist.

EPIC-009 owns busy-target queues, lateral collaboration, mailbox scheduling,
priority/aging/fairness, activation, and passivation. TASK-029 owns deferred
advanced public methods. No SDK, UI, new daemon, general artifact service,
whole-store rewrite, or multi-transport framework belongs to this Epic.

## 4. Contract closure before implementation

TASK-025 must leave a concrete checked contract, not a list of optional designs.
Implementation-dependent details are its owned engineering work, not reasons to
reopen the accepted product scope. If a transport cannot prove its required
behavior, stop at the roadmap's `BLOCKED` state rather than weaken the contract.

Until TASK-025 is `COMPLETE` under the ordinary completion gate, implementation
is limited to its probe, fixtures, and contract closure. Do not start later
production bridge, provisioning, dispatch, or result-path work in parallel.
Freezing a contract here does not move its downstream production implementation
into TASK-025.

| Contract item | Required closure in TASK-025 |
| --- | --- |
| Transport identity | Exact executable/version, registration mechanism, Run/Turn/call correlation and generation fence |
| Replay | Stable semantic identity across supported retries; original receipt versus new observation; different-input conflict |
| Request shape | Supported operations and exact text/context bounds; existing v1 preserved or a minimal checked private successor |
| Task content | Accepted bytes and context provenance; artifact authorization/resolution before dispatch; hiring rationale remains non-executable |
| Wait behavior | Define `blocking`, `any`/`all`, terminal sets, transport budget, cancellation, and durable deadline independently |
| Unsupported policies | Checked error mapping for live queue/collaboration/activation rejection, without removing future schema values |
| Result access | Bounded actual Primary consumption and authorized client access; no child Controller disclosure |
| Public compatibility | Byte-identical public Protobuf source and descriptor; unchanged minimum method inventory |
| Evidence | Executable probe/fixtures, sanitized version-specific conclusions, explicit live prerequisites and attack budget |

A useful result path is a Primary-owned projection through existing artifact
services plus a minimal checked private reader for the model. The Task must
select and specify one coherent contract. Do not tell the model to use a child
credential, private database/socket, or untrusted host path to fetch the result.
Any necessary private schema successor is scoped to actual live needs, not a
bulk copy of facade v3. Validate input and output schema conformance together.

## 5. Task execution checklists

The following checklist items do not independently change roadmap status.
Each Task also satisfies the ordinary completion gate and independent review.
Tests and recovery for a newly introduced effect belong to that same Task.

### TASK-025: Pinned transport probe and contract

Prerequisites: TASK-023, TASK-024, EPIC-014, EPIC-015 complete.

- [ ] Reconcile baseline code with the accepted slice; preserve unrelated work.
- [ ] Implement isolated probe/fixtures and obtain required live authorization.
- [ ] Prove one MCP or native path, including ambiguous-shared-identity handling.
- [ ] Limit the future-collaboration probe to source Run/Turn/call binding with
  an isolated inert stub; use temporary test-only registration only when needed
  for that proof.
- [ ] Freeze every contract item in section 4 and synchronize checked artifacts.
- [ ] Confirm source identity, retry, cancellation, wait, restart, and secret canaries.
- [ ] Complete independent review and one initial completion commit.

Production Specialist collaboration registration, advertisement, operation
handlers, mailbox, and scheduler implementation remain in EPIC-009. The inert
probe is not collaboration acceptance. This restriction does not reduce the
selected Primary transport's retry, cancellation, wait, disconnect, or restart
proof. Do not implicitly upgrade Codex or accept fake-only transport proof.
Next: TASK-047, only after TASK-025 is `COMPLETE`.

### TASK-047: Trusted live bridge

Input: the selected checked transport and contract.

- [ ] Wire only the Primary tool to the existing orchestration semantic service.
- [ ] Construct trusted call context outside model arguments; validate Run/Turn/call.
- [ ] Preserve semantic retry identity while fencing stale generations.
- [ ] Retain existing call/reuse receipts across lost replies and reconnect.
- [ ] Isolate registration without editing a shared global Profile.
- [ ] Keep incomplete operations unavailable before effects; test adversarial calls.
- [ ] Include required live evidence, independent review, and one completion commit.

Do not add a transport framework or change external-review MCP disposition.
Next: TASK-048.

### TASK-048: Provisioning and approvals

Input: the production bridge and existing spawn/Session state machine.

- [ ] Connect preallocated child IDs to existing Run/Worker/thread creation.
- [ ] Preserve immutable Role/Profile/access snapshots and protected credentials.
- [ ] Follow the v0.1.3 provider slice for live reuse: admit only `never` and
  `reuse_idle_compatible`, reject unsupported policies before Session allocation,
  and preserve durable reuse receipts without adding busy/mail-count selection.
- [ ] Connect broker approvals to the existing public Controller Interaction flow.
- [ ] Route broker approval responses to the spawn operation, not Codex requests.
- [ ] Verify both policies, denial, response loss, duplicate spawn, and publication crashes.
- [ ] Include tests/docs, independent review, and one completion commit.

Do not rebuild the policy registry or infer membership from raw managed Runs.
Next: TASK-049.

### TASK-049: Accepted task and actual dispatch

Input: an actual ready Specialist with an admitted immutable configuration.

- [ ] Compose separate bounded task content and authorized immutable context.
- [ ] Validate member/session/access and replay before fresh busy admission.
- [ ] Commit accepted bytes, identity, and deadline origin before execution effects.
- [ ] Split acceptance from completion and record authoritative target Turn evidence.
- [ ] Connect existing writer/isolated roots; refuse unsafe active-source writer yield.
- [ ] Test concurrent admission, Role/task invariance, busy rejection, and every dispatch crash boundary.
- [ ] Preserve unknown outcomes, compatibility, independent review, and one completion commit.

No general queue and no facade-as-backend shortcut. Next: TASK-050.

### TASK-050: Wait, deadline, and cancellation

Input: separately persisted task and Turn acceptance.

- [ ] Implement the frozen bounded wait and `blocking` semantics.
- [ ] Observe `any`/`all` terminal conditions without holding mutation ownership.
- [ ] Keep approval, cancellation, and other Run operations responsive.
- [ ] Derive remaining deadline from durable acceptance after every retry/restart.
- [ ] Distinguish wait timeout/disconnect from task cancellation or execution expiry.
- [ ] Test cancel/complete/expiry races and interrupt acknowledgement without terminal proof.
- [ ] Include checked errors, tests/docs, independent review, and one completion commit.

No mailbox/priority scheduler. Next: TASK-051.

### TASK-051: Result validation, readability, and delivery

Input: authoritative terminal observation and the accepted request.

- [ ] Recheck accepted-request identity before selecting the output validator.
- [ ] Preserve applicable structured v3 validation without requiring it for general tasks.
- [ ] Commit real immutable bytes and metadata before publishing readable completion.
- [ ] Reconcile artifact/SQLite publication without unsupported atomicity claims.
- [ ] Let the Primary consume actual content and its Controller read permitted result projections.
- [ ] Verify above-inline-bound results, chunk/digest/bounds/authorization and every receipt crash window.
- [ ] Preserve cursor replay, later pages, no repeated Turn, review, and one completion commit.

No new general artifact system or relaxed access to arbitrary child artifacts.
Next: TASK-052.

### TASK-052: Retirement and restart

Input: complete task and result paths with their own local recovery checks.

- [ ] Wire release and Session complete/abort to existing lifecycle semantics.
- [ ] Stop new admission without discarding accepted/unknown work or undelivered results.
- [ ] Reconstruct original Run/thread/Profile/access/working-root/deadline bindings.
- [ ] Preserve writer and Controller ownership across gateway/Worker replacement.
- [ ] Reuse TASK-046 process verification; keep healthy shared servers alive on disconnect.
- [ ] Test approval, dispatch, execution, result, and retirement restarts through connected paths.
- [ ] Verify cleanup, compatibility, independent review, and one completion commit.

No automatic replay, unsolicited Primary Turn, paused-Run activation, or new
cleanup subsystem. Next: TASK-026.

### TASK-026: Public provider acceptance and handoff

Input: all seven preceding Tasks complete.

- [ ] Extend existing native gateway/generated-client fixtures and implement the planned private-boundary driver.
- [ ] Execute the real public interface through production semantics in an isolated home.
- [ ] Prove actual pinned Codex Primary/Specialist execution and both approval modes with explicit authorization.
- [ ] Show actual Primary consumption of Specialist results, including an above-inline-bound result.
- [ ] Verify permitted client artifact reads, exact length/SHA-256, replay, typed errors, and secret canaries.
- [ ] Verify gateway/Worker restart and graceful retirement without duplicate work.
- [ ] Run the full deterministic gate and required independent read-only review; resolve blockers.
- [ ] Update canonical operations/tips, user entrypoint/source skill, and executable contract examples to actual behavior.
- [ ] Promote durable dossier content, remove this dossier/index entry, replace Detailed SOT with Canonical Outcomes, and make one completion commit.

No Gul dependency, real-Gul success claim, stable release, tag, publication, or
installation. Passing this Task establishes provider eligibility only.

## 6. Verification and adversarial budget

For each changed boundary, the independent review states the applicable attack
families and the empirical checks actually executed. Cover at least the named
families below; expand only when a finding or newly introduced behavior needs
more evidence, rather than repeating an unbounded broad review loop.

| Owner | Required adversarial families |
| --- | --- |
| TASK-025/047 | Cross-Run/Turn/call substitution; stale generation; same-call reconnect; different-input conflict; concurrent calls; cancellation; wait expiry; bridge loss; credential/source canaries |
| TASK-048 | Approve/reject replay; no allocation before approval; wrong Controller; immutable policy drift; pre/post child publication loss |
| TASK-049/050 | Privilege escalation; simultaneous busy admission; accepted receipt loss; pre/post Turn acceptance crash; independent-member interference; wait/cancel/complete/deadline races |
| TASK-051 | Request/discriminator corruption; incomplete artifact publication; forged ownership; bounds/digest failures; lost delivery receipt; later cursor pages |
| TASK-052/026 | Restart while approval/execution/delivery/retirement is in progress; stale process identity; pinned working-root loss; real public-client reconnect; retained unknown outcomes |

Use injectable clocks for deadline tests. Exercise pre-effect and post-effect
failure separately; a transport timeout cannot decide which occurred. Reuse
existing fake-Codex and process-identity helpers. Deterministic tests never use
production state or the user's normal Dolgorae home. Live campaigns require an
authorized test Profile/account and separate opt-in execution; do not infer
permission to consume credentials or tokens from documentation approval.
The approved Profile/account, isolated state, and execution scope are live-run
prerequisites, not substitutes for passing evidence. Without authorization,
limit work to permitted deterministic preparation. Missing required live
evidence prevents Task completion; do not mark fake-only results as live proof.

For each Task, identify the changed OS or external-runtime behavior and its
required evidence. Reuse earlier evidence only after checking that the tested
pin/build, configuration, and affected behavior still apply; identify that
coverage and its limits without copying sensitive runtime evidence into docs.
Changes that invalidate an earlier proof require a new focused check. Repeated
live wording does not require a full campaign for every Task or expand an
existing execution authorization. TASK-026 still verifies the assembled
provider path with actual pinned Codex; isolated earlier probes cannot replace
that end-to-end evidence.

Suggested focused commands, subject to the owning Task's affected surface:

```sh
cargo test --locked --lib orchestration::tests -- --test-threads=1
cargo test --locked --lib task_request::tests -- --test-threads=1
.venv/bin/python tools/validators/validate_json_schemas.py
.venv/bin/python tools/validators/validate_schema_examples.py
.venv/bin/python tools/validators/validate_public_descriptor.py
.venv/bin/python tools/validators/validate_markdown.py
.venv/bin/python tools/validators/validate_agent_skills.py
```

The complete implementation gate remains:

```sh
make PYTHON_BIN=.venv/bin/python test
```

It applies formatting and is not a read-only plan check. TASK-025 owns new
probe entrypoints and their documented live opt-in controls; do not assume an
existing review smoke proves internal Primary transport. TASK-026 owns the new
private-boundary test driver and any needed live acceptance wrapper. Planned
file names, commands, evidence labels, and checkboxes never count as executed
tests. Keep raw logs, provider prose, credentials, and local runtime identities
out of tracked docs; follow AGENTS.md evidence promotion rules where retention
is genuinely needed.

## 7. Provider handoff and release gates

The consumer guide belongs in canonical docs/ops and protocol examples, not in
this disposable dossier. It must explain verified call ordering, protected
Controller carriers and launch intent, capabilities, typed errors, idempotency,
wait versus task deadline, event cursor reconnect, authorized result artifacts,
and the explicit no-queue/no-collaboration v0.1.3 limits. Include executable
examples or fixtures from actual tests rather than invented wire payloads.

| Gate | Required evidence before v0.1.3 eligibility |
| --- | --- |
| Live behavior | Actual pinned Primary tool call, both approval modes, actual Specialist work, Primary consumes actual content |
| Provider API | Real UDS/gRPC client, production semantic path, unchanged minimum inventory and public descriptor |
| Failure safety | Named crash/retry/access/result/retirement cases and honest unknown outcomes |
| Regression | Complete deterministic gate, legacy external review/engagement and v3 behavior preserved |
| Review | Task and Epic acceptance reviews complete; no unresolved blocking finding |
| Documentation | Canonical contract, actual capabilities, user guidance, evidence provenance, and deferred Gul boundary agree |

After Epic completion, separate authorized release work verifies the exact
committed release candidate/build, installation/startup/capability smoke, and
applicable live evidence before assigning a release date, tag, or publication.
Eight completed Tasks do not themselves create a release. The reviewed default
plan retains v0.1.3 as a Milestone Preview; neither customer-supported Personal
Alpha nor actual Gul compatibility is claimed.

Actual Gul follow-up remains in the TODO owner. Its absence must not create an
ACTIVE/BLOCKED Task occupying the sequential slot or become a prerequisite of
EPIC-009 provider work.

## 8. Start and change-control rules

Start the repository's normal EPIC-008 workflow at TASK-025. The product choices
above are settled. Recheck baseline and available tool/skill instructions, then
implement only its probe and required contract closure until TASK-025 is
`COMPLETE`, then follow the roadmap sequentially. Do not reintroduce the
sixteen-task scope, reopen Gul readiness, or ask for those settled choices.
Required commit/live-operation approvals remain distinct.

Keep one Task active, include its own tests/docs/review, and keep incomplete
capabilities unavailable. The baseline is eight Tasks. If implementation proves
that one boundary cannot form a coherent safe completion commit, propose only
the bounded split with its reason before changing roadmap IDs; do not use the
split to add product scope or hide unresolved correctness work. Discovered
post-completion defects receive explicit corrective ownership, not a history
rewrite. This dossier and the roadmap are planning artifacts, not evidence of
implemented, released, or installed functionality.
