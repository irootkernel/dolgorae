# Dolgorae Deferred Feedback

This index owns small, independent, actionable findings intentionally postponed
from current work. It is not a second roadmap or status authority.

## DF-001: Consolidate external-facade validation helpers

- Owner: External Specialist Facade maintainers
- Finding: bounds, required-null deserialization, role-reference validation,
  and interrupt/terminal-proof plumbing still have small equivalent helpers in
  the facade, engagement store, and semantic service.
- Reason for deferral: the current layers intentionally revalidate at their own
  trust boundaries, and consolidating those helpers is independent of
  EPIC-006 correctness. A premature shared module could erase those boundary
  checks rather than merely deduplicate their representation.
- Revisit condition: the next external-facade schema revision or any change to
  a duplicated bound or terminal-proof budget.

## DF-002: Centralize external-engagement state predicates

- Owner: External Specialist Facade maintainers
- Finding: the SQLite schema, partial indexes, transition guards, recovery
  queries, and projections repeat the same active-task and active-engagement
  state sets as SQL literals.
- Reason for deferral: those literals currently agree and are covered by schema,
  migration, transition, and lifecycle tests. Replacing them safely requires a
  deliberate query-builder or generated-schema boundary and is independent of
  EPIC-006 behavior.
- Revisit condition: adding or renaming an engagement, member, or task state, or
  introducing the next orchestration schema migration.

## DF-003: Attest external-facade caller identity

- Owner: External Specialist Facade maintainers
- Finding: nested-hire defense in depth resolves `CODEX_THREAD_ID` from the
  caller environment, which a same-uid subprocess can unset or replace. The
  aggregate-owner Controller credential and profile isolation remain the
  authoritative capability boundary, so this does not grant a Specialist new
  authority under the current contract.
- Reason for deferral: an independently authoritative caller identity requires
  a descriptor-passed or worker-attested channel across the host, CLI, and Run
  boundary. That trust-boundary change is independent of EPIC-006 correctness
  while the owner credential remains unavailable to Specialist Runs.
- Revisit condition: extending the threat model to hostile same-uid callers,
  exposing the External Specialist Facade to a Specialist Runtime Profile, or
  adding an attested caller-identity carrier.

## DF-004: Keep membership checksum fields in one representation

- Owner: Global Profile membership maintainers
- Finding: append and replay construct equivalent checksum bodies separately.
- Reason for deferral: the fields currently agree and journal replay tests
  verify their compatibility; consolidation is independent of current behavior.
- Revisit condition: the next membership schema revision or checksum-field change.

## DF-005: Enforce complete launch-snapshot projection during schema evolution

- Owner: Global Profile runtime maintainers
- Finding: the name-neutral launch snapshot explicitly copies the named
  snapshot's fields, so a future field needs a matching projection change.
- Reason for deferral: all current launch fields are present and binding tests
  cover current equality; this is a future schema-maintenance constraint.
- Revisit condition: adding a Profile snapshot field or changing launch identity.

## DF-006: Centralize the Profile lifecycle home path

- Owner: Global Profile lifecycle maintainers
- Finding: lifecycle operations derive the home from the server root at several
  call sites instead of retaining it in their shared path context.
- Reason for deferral: the fixed current layout is consistent at those sites;
  changing the path representation is independent of current lifecycle safety.
- Revisit condition: changing the Profile server directory layout.

## DF-007: Coordinate orphan verification with Run manifest successors

- Owner: Global Profile and Run persistence maintainers
- Finding: the Profile layer's bounded orphan check explicitly reads v2 Run
  identity fields to preserve the approved dependency boundary.
- Reason for deferral: current v2 fields agree with the Run owner; a shared
  verifier requires a separate boundary design when the contract evolves.
- Revisit condition: introducing a Run manifest successor or changing its
  workspace, Run, or global Profile identity fields.

## DF-008: Keep global Profile digest predicates consistent

- Owner: Global Profile runtime maintainers
- Finding: registry digest validation and runtime server-key validation repeat
  the same lowercase SHA-256 predicate.
- Reason for deferral: their current accepted formats agree; centralization is
  independent of current binding and membership correctness.
- Revisit condition: changing a digest format or its accepted representation.

## DF-009: Centralize quiescence refusal construction

- Owner: Global Profile membership maintainers
- Finding: the two quiescence lock-entry paths construct the same busy error.
- Reason for deferral: current codes, details, and retryability agree and both
  paths reject live members; consolidation addresses future contract drift.
- Revisit condition: changing the Profile busy error or quiescence diagnostics.

## DF-010: Exercise concurrent initial home creation across processes

- Owner: Global Profile home maintainers
- Finding: registry writes and membership appends have concurrency coverage,
  while initial generation creation is covered by sequential lifecycle tests.
- Reason for deferral: initial creation holds the parent-directory lock and
  rechecks the generation under that lock; an additional process-race campaign
  strengthens evidence without correcting a known current race.
- Revisit condition: changing initialization locking, staging recovery, or the
  atomic home-publication sequence.

## DF-011: Define recovery of abandoned atomic-write temporaries

- Owner: Global Profile persistence maintainers
- Finding: a crash can leave private atomic-write temporary files beside the
  live registry or membership state.
- Reason for deferral: those mode-0600 files are not read as live authority;
  automatic cleanup needs its own safe ownership and active-writer rules.
- Revisit condition: adding storage maintenance or investigating repeated
  crash-related accumulation in the global home.

## DF-012: Construct reviewer configurations at their final version boundary

- Owner: Specialist planning and semantic-service maintainers
- Finding: the reviewer planner produces a historical configuration that its
  production caller converts to the current version and binding digest.
- Reason for deferral: the sole production caller performs both conversions
  before publishing the configuration; direct current-version construction
  would make future consumers less dependent on that caller convention.
- Revisit condition: adding a reviewer-plan consumer or a configuration version.

## DF-013: Make snapshot binding persistence explicit to future callers

- Owner: Global Profile launch maintainers
- Finding: snapshot preparation also records the name-to-launch binding history.
- Reason for deferral: current callers authorize preparation and its durable
  binding write; an explicit return-and-record boundary would help future
  read-only consumers avoid accidentally adopting a mutating helper.
- Revisit condition: introducing a read-only snapshot consumer or changing launch
  preparation ownership.

## DF-014: Consolidate compatibility checks while preserving entry contracts

- Owner: Run and External Specialist semantic-service maintainers
- Finding: Run admission and prepared Specialist admission repeat model, effort,
  and capability checks with entry-specific error construction.
- Reason for deferral: both paths reject incompatible inputs under their current
  contracts; sharing validation facts is independent of that behavior and must
  retain each public error surface and its boundary revalidation.
- Revisit condition: changing compatibility requirements or adding a consumer.

## DF-015: Add a checked Run manifest successor fixture

- Owner: Run persistence and protocol-validation maintainers
- Finding: the v2 manifest has Rust publication and recovery coverage but no
  positive fixture routed through the checked JSON Schema example validator.
- Reason for deferral: no manifest/schema divergence has been established;
  a retained representative fixture would supplement existing semantic tests
  with direct schema-instance coverage.
- Revisit condition: changing the manifest schema or expanding checked examples.

## DF-016: Define a shared Run manifest size budget

- Owner: Run persistence and Global Profile membership maintainers
- Finding: Run recovery and membership orphan verification read complete
  manifests without a shared byte ceiling, so an oversized user-owned manifest
  can consume excess memory during inspection.
- Reason for deferral: the current publisher has no total-manifest byte budget;
  a limit only in orphan verification could reject a valid published Run.
  Establish matching publication, recovery, and inspection limits together,
  including compatibility with existing immutable snapshots. Hostile same-uid
  mutation is outside the current threat model.
- Revisit condition: adding a persisted Run size budget, changing snapshot
  bounds, or extending the local-state threat model.

## DF-017: Distinguish binding-history failures in home diagnostics

- Owner: Global Profile home and Machine error-contract maintainers
- Finding: malformed, oversized, or inconsistent binding history is reported
  under the existing `malformed_marker` classification, which does not identify
  the failing artifact precisely.
- Reason for deferral: the current classification correctly blocks admission;
  a more specific classification needs coordinated error-contract and operator
  runbook changes rather than silently altering a checked response.
- Revisit condition: introducing a home-diagnostic contract successor or the
  journal-maintenance candidate in the TODO index.

## DF-018: Extend defensive home and Machine projection regressions

- Owner: Global Profile and CLI validation maintainers
- Finding: explicit negative fixtures could additionally pin non-v1 registry
  removal and symlinked workspace-entry rejection; schema assertions could
  also be extended to remaining non-engagement Machine test helpers.
- Reason for deferral: existing production guards reject those filesystem and
  schema states, and current command/schema tests establish no divergence.
  These cases are independent regression expansion, beyond the exercised
  marker encoding/size, registry limits, and home-admission classifications.
- Revisit condition: changing registry removal, workspace-entry traversal, or
  the affected Machine test helpers.

## DF-019: Retire the legacy Orchestrated Session finish path

- Owner: Orchestration persistence maintainers
- Finding: `finish_session` remains a public store method used only by unit
  tests. Production close uses the durable begin, settle, and complete protocol.
- Reason for deferral: no production caller can create a session closure without
  the close record through this method. Removing it requires migrating older
  store tests and is independent of current close and recovery behavior.
- Revisit condition: changing the session close protocol or its store tests.

## DF-020: Cover artifact-backed final responses in the timeline

- Owner: Controller timeline maintainers
- Finding: tests cover artifact storage for large final responses and inline
  timeline projection, but do not combine them in a timeline assertion.
- Reason for deferral: the timeline uses the same observer-visible artifact
  reference as the shared artifact reader, and no projection mismatch was
  found. This is independent regression coverage.
- Revisit condition: changing final-response artifact classification or the
  timeline item projection.

## DF-021: Fence legacy Broker admission at the close commit boundary

- Owner: Orchestration Broker maintainers
- Finding: the legacy `request_specialist` and managed assign paths check the
  session only before their reservation transaction. The legacy session close
  settles — terminates — the owned work it finds at settle time, so the gap is
  only a legacy admission that lands after a completed close: it enters with
  no later fence and its own settle finds no owned work to terminate.
- Reason for deferral: pre-existing at the TASK-076 baseline and unchanged by
  it — the successor spawn/assign acceptance transactions fence themselves
  inside the close commit boundary, but reworking the legacy admission
  transactions belongs to their owning completed Tasks.
- Revisit condition: changing legacy Broker admission or the session close
  protocol.

## DF-022: Reclaim the crash-orphaned successor member credential

- Owner: Orchestration Broker maintainers
- Finding: a crash between a successor spawn's member-credential mint and its
  acceptance commit leaves one inert mode-0600 credential file with no member
  row and no Run, and v0.1.4 does not reclaim it.
- Reason for deferral: a race-free reclaim needs a durable mint record neither
  spawn path writes; the workspace-wide sweep that would reclaim it could
  delete another session's in-flight credential and was removed
  (design check during TASK-076, 2026-10-23).
- Revisit condition: adding a durable credential mint record or a credential
  maintenance owner.

## DF-023: Converge an uncertain successor dispatch with no Turn id

- Owner: Delegation runtime maintainers
- Finding: a successor dispatch without a scheduler assignment claim can remain
  in the typed recovery row when its transport effect is uncertain and no Turn
  id was recorded. Its Run ledger may already retain `IdempotencyReserved`
  mapping the Submit key to an operation id and `TurnStarted` mapping that
  operation to a Turn id.
- Scope boundary: TASK-082 repairs positively verified original acceptance for
  managed and external assignments with a retained scheduler claim. This entry
  retains successor recovery without that claim and genuinely unresolved
  effects without positive acceptance evidence.
- Reason for deferral: positive acceptance recovery is not integrated with the
  successor path that has no scheduler claim. A complete-journal non-dispatch
  proof plus a late-admission block would be required before redispatching an
  unresolved request; missing positive evidence alone is insufficient. The
  weaker "no Turn id plus generation absent means proved non-dispatch" rule
  was rejected by a design check (2026-10-23) because a worker can die after
  turn/start and before `TurnAccepted` is journaled.
- Revisit condition: a complete-journal proof owner or the next delegation
  recovery revision.

## DF-024: Settle a terminal-without-resolution window with the existing unavailable marker

- Owner: Delegation runtime maintainers
- Finding: a bound Turn with a durable terminal but no resolution record (the
  window between the terminal and its resolution) ends in the typed recovery
  row; once the bound terminal and
  whole-generation absence are both proved, the existing
  typed-unavailable marker could settle it with no new unknown outcome and no
  fabricated response.
- Reason for deferral: optional extension explicitly deferred by a design
  check (2026-10-23); the recovery row is correct and a
  repeat release resumes from it.
- Scope boundary: authenticated recovery that freshly verifies the original
  history terminal and its reconciliation prefix is handled by the delegation
  runtime. This entry retains the ordinary TurnTerminal-only window with no
  final resolution and no fresh history proof; it does not authorize inferring
  a task Turn ID or publishing a fabricated response. DF-023 remains separate.
- Revisit condition: the next revision of member release recovery.

## DF-025: Isolate the load-sensitive fake-app-server timing tests

- Owner: Repository test infrastructure maintainers
- Finding: three tests failed only under heavy concurrent load (concurrent
  reviewers plus an e2e clone plus the Writer) and passed three of three when
  re-run alone during TASK-076 verification —
  `semantic::tests::model_list_shape_faults_are_compatibility_rejections`,
  `semantic::tests::model_resolution_walks_every_page_and_keeps_the_advertised_order`
  and
  `worker_turn_drain::an_interaction_pauses_a_blocked_caller_and_its_answer_resumes_the_turn`;
  every failure was the fake app-server never listening or the interaction
  answer never reaching it.
- Reason for deferral: environmental timing flakes of one class, not
  regressions; each passed on isolated re-run.
- Revisit condition: the next occurrence of the flake or a change to the fake
  app-server harness.

## DF-026: Promote TASK-076 decisions into the ADR index

- Owner: Architecture decision record maintainers
- Finding: the accepted TASK-076 decisions — the namespace birth authority and
  its residual risk, the strict-DDL generation gate, the dispatch-index
  bounds, the reverse-diff before-image reconstruction, the
  deterministic-unavailable code set and the two Master bounds — are recorded
  in the delegation runtime specification because the ADR index is H1-locked.
- Reason for deferral: the ADR index cannot be regenerated while H1 stays
  locked.
- Revisit condition: the next regeneration of the ADR index.

## DF-027: Two optional successor-capture coverage items

- Owner: Successor capture maintainers
- Finding: the retained-content quota test covers only the pre-insertion
  refusal, so a settlement rollback after content insertion has no test (an
  optional review finding during TASK-076 verification);
  and an optional `run_ref` index over `dispatch_indexes` was noted in review
  and not built.
- Reason for deferral: both are independent regression and index coverage; the
  existing bounds and behaviour are covered by the current tests and the
  per-Run aggregate check.
- Revisit condition: changing retained-content quota accounting or
  dispatch-index access patterns.

## DF-028: Diagnose intermittent gateway snapshot conflict after interrupt response loss

- Owner: Gateway semantic integration test maintainers
- Finding: `gateway_semantic_native::native_pending_interaction_parity_after_lost_interrupt_response`
  intermittently fails when the GetRun helper unwraps `RUN_STATE_CONFLICT` at
  `tests/gateway_semantic_native.rs:5444`. TASK-076 verification observed one
  failure in 65 candidate runs and none in 60 runs at the Epic starting commit.
  These observations do not establish the defect's cause.
- Reason for deferral: Master explicitly accepted this exact intermittent
  failure during TASK-076 completion. Keep its recorded failure distinct from
  passing evidence; do not extend the allowance to another test or signature.
- Revisit condition: the next occurrence, a change to the exercised
  snapshot/interrupt/recovery path, or a change to this test's synchronization.

## DF-029: Separate the legacy Controller reset journal from Ledger recovery

- Owner: Controller reset and Ledger persistence maintainers
- Finding: `DurableResetJournal` writes `recovery/controller-reset.jsonl`, while
  authoritative Ledger reopen accepts only torn-tail evidence in `recovery/`.
  A retained reset journal therefore conflicts with Ledger's entry grammar.
  This finding comes from source inspection; the TASK-077 native campaign did
  not exercise Controller reset.
- Reason for deferral: the Controller journal and Ledger grammar predate
  TASK-077 and are unchanged by it. TASK-077's ThreadRegistration and generation
  release evidence use separate namespaces. Moving the legacy journal needs
  its own existing-state recovery decision and must preserve the unresolved
  prepare fence without deleting journals or weakening Ledger validation.
- Revisit condition: the next Controller reset or persistence change, or an
  observed Ledger reopen failure naming `controller-reset.jsonl`.

## DF-030: Consolidate native successor component selection

- Owner: Native successor acceptance fixture maintainers
- Finding: the example entrypoint selects collaboration and scheduler components,
  while its support campaign selects successor startup and supplies mode-specific
  instructions and access settings.
- Reason for deferral: the bounded startup branch and the default campaign use
  their intended settings. A shared selection boundary would reduce the work
  needed to add or rename modes, independently of current startup correctness.
- Revisit condition: adding a native successor component or changing campaign
  mode selection, including scheduler acceptance.

## DF-031: Keep Session Role catalog predicates with their policy owner

- Owner: Session policy and collaboration maintainers
- Finding: collaboration checks catalog membership by matching the Legacy and
  Caller variants directly before calling the policy owner's fallible Role
  resolver. Both catalog predicates currently match their owning lookups.
- Reason for deferral: the exhaustive match preserves current absence and
  registered-Role failure semantics. Moving the presence predicate to the policy
  owner would reduce future maintenance across consumers.
- Revisit condition: adding a policy variant, changing catalog membership, or
  introducing another consumer that needs to distinguish absence from invalid
  registered captures.

## DF-032: Extend optional collaboration membership regressions

- Owner: Collaboration regression maintainers
- Finding: owning-service unit tests cover Legacy catalog absence and registered
  Role corruption. Caller absence is covered by the opt-in native successor
  startup campaign, without an equivalent cargo-level case. The suite also lacks
  a mixed registered/unlisted successor case that checks registered-peer tool
  retention and denial of the unlisted Role as a consultation target.
- Reason for deferral: current catalog filtering preserves registered members
  and restricts targets to the filtered set. The Caller startup campaign has
  exercised actual Worker readiness with no optional tool. These additional
  cases would strengthen regression coverage without correcting a demonstrated
  current failure.
- Revisit condition: changing optional-tool membership filtering, Role target
  resolution, or Caller policy capture handling.

## DF-034: Extend optional scheduler boundary regressions

- Owner: Scheduler runtime regression maintainers
- Finding: additional default-suite central-entry, passivation-refusal,
  retained activation attempt limits, immutable-policy refusal and durable
  fairness-history cases can strengthen the existing behavior checks.
- Reason for deferral: these additional combinations do not identify another
  current production defect. Mandatory recovery, compensation, expiry and
  fairness-correction verification is outside this deferred item. Unavailable
  residency stops automatic retries; an attempt-bound case must preserve that rule.
- Revisit condition: changing central scheduling, passivation fences,
  activation attempt policy, policy capture or persistent fairness selection.

Additional offline scheduler-entry assertions can cover forwarding a waiter
hint and notifying a source after queued expiry. Existing physical notification
proof and required reserved-dispatch uncertainty tests remain outside this
optional coverage item. Revisit this residual coverage when central hint or
expiry notification wiring changes.

## DF-035: Improve safe scheduler diagnostics

- Owner: Scheduler diagnostics maintainers
- Finding: distinct closed scheduler reasons share a generic top-level message,
  and invalid workspace registration is skipped without a trusted diagnostic.
- Reason for deferral: machine reasons remain available for actor failures.
  Workspace identity failure must prohibit opening or writing that workspace's
  store; diagnostic placement needs an already trusted boundary.
- Revisit condition: changing scheduler operator diagnostics or adding a trusted
  control-plane diagnostic surface.

## DF-036: Share fixed scheduler default constants

- Owner: Collaboration policy and scheduler maintainers
- Finding: immutable scheduling defaults and matching runtime timer values have
  equivalent literals in their current owners. Retained policy snapshots are
  checked against those fixed values.
- Reason for deferral: the fixed values currently agree. Sharing constants is
  independent of the current recovery and expiry corrections and must preserve
  admission-time immutable capture.
- Revisit condition: changing a scheduling default or introducing another
  consumer of the captured policy. A default or version change needs an explicit
  retained-snapshot compatibility and migration decision that preserves original
  acceptance values.

## DF-037: Centralize accepted-work identity constructors

- Owner: Accepted-work and Submit identity maintainers
- Finding: accepted-work schema names and task Submit key construction have
  matching literals across their producers and consumers.
- Reason for deferral: no current identity mismatch is established. A shared
  constructor should preserve each owner and validation boundary.
- Revisit condition: changing an accepted-work schema or Submit key, or adding
  another producer or consumer.

## DF-038: Revisit scheduler extension bootstrap ownership

- Owner: Scheduler persistence maintainers
- Finding: admission and runtime recovery both install or validate the private
  scheduler extension through its current sanctioned dependency boundary.
- Reason for deferral: admission-time policy capture and recovery from original
  durable queues must remain available. Simplifying bootstrap ownership is
  independent of current correctness.
- Revisit condition: changing extension migration or initialization ownership.

## DF-039: Distinguish collaboration expiry in safe errors

- Owner: Collaboration tool contract maintainers
- Finding: expired root admission uses the authority-refusal code, while queued
  expiry has an honest `expired` state with a generic transport safe code.
- Reason for deferral: refusal and execution state are correct. The closed
  collaboration vocabulary has no timeout code; improving actionability needs
  a deliberate contract revision.
- Revisit condition: revising the collaboration safe-error vocabulary or its
  model-facing recovery guidance.

## DF-040: Keep collaboration state-root derivation with its storage owner

- Owner: Collaboration storage maintainers
- Finding: dependency boost, source-wait and call-thread helpers derive the
  state root from the same fixed SQLite path layout.
- Reason for deferral: the current derivations agree. Consolidating them is
  independent of reserved-dispatch uncertainty and result-delivery correctness.
- Revisit condition: changing the orchestration database layout or the
  connection-to-state-root boundary.

## DF-041: Consolidate collaboration transition vocabulary when it changes

- Owner: Collaboration and scheduler state maintainers
- Finding: collaboration owns the exchange vocabulary while scheduler SQL also
  names dispatch, recovery and terminal states.
- Reason for deferral: the current state sets agree. Guarded unknown
  classification and its regressions preserve execution authority; shared
  transition helpers would serve future vocabulary changes.
- Revisit condition: adding or removing an exchange state, or changing which
  component owns an existing transition.

## DF-042: Revisit collaboration result privacy backstops when read scope changes

- Owner: Collaboration result privacy maintainers
- Finding: the result validator rejects a bounded set of private-output markers;
  this backstop does not classify every possible credential or private payload.
  No concrete exposure chain is established by that limitation alone.
- Reason for deferral: broader classification requires a defined output scope
  and evidence of which additional payloads it must recognize. Existing required
  privacy checks and the prohibition on exposing protected output still apply.
- Coverage: the Source-bound model reader rejects whole immutable
  bodies for selected credential assignments, private paths, raw frames and
  known structural reasoning items. Exact-byte and foreign-source regressions
  cover that bounded vocabulary; they do not certify arbitrary unlabeled text.
- Revisit condition: further changes to model-visible result reads, result
  formats or credential handling. Qualify each changed output path for privacy
  before relying on an additional backstop.

## DF-043: Improve immediate policy-task settlement diagnostics

- Finding: the Worker terminal hook relies on owned reconciliation when direct
  policy-task settlement fails. Repeated refusal has limited immediate
  diagnostic detail.
- Owner: policy-task runtime diagnostics.
- Reason for deferral: the original terminal and accepted execution authority
  remain durable, and central reconciliation retries their settlement. No lost
  original result or required recovery failure is established. Diagnostic work
  must preserve safe errors and must not log raw runtime failures or authorize
  execution replay.
- Revisit condition: changes to policy-task settlement or operator diagnostics.

## DF-044: Bound repeated collaboration artifact validation cost

- Owner: Collaboration storage maintainers
- Finding: each Source result page rechecks the complete bounded artifact and
  privacy vocabulary inside the receipt transaction. Paging a large artifact
  repeats that work while other workspace writes wait.
- Reason for deferral: every page must retain current integrity, privacy and
  exact-replay checks. No required liveness failure is established; moving or
  caching validation needs a separate design for changed artifact bytes and
  receipt races, supported by contention measurements.
- Revisit condition: changing artifact size limits, page-read concurrency or
  the collaboration storage transaction boundary.

## DF-045: Check private state and public schema evolution together

- Owner: Orchestration state contract maintainers
- Finding: checked materialization uses a private Rust representation alongside
  the public orchestration-state-v2 JSON schema. Future field changes must keep
  those representations consistent.
- Reason for deferral: no current field mismatch is established. Existing
  materialization, refusal and schema-example checks cover their current
  contracts; a produced-document schema check would strengthen future changes.
- Revisit condition: changing the orchestration-state-v2 fields or their owners.

## DF-046: Refine inbound orchestration child dependency checks

- Owner: Architecture contract maintainers
- Finding: per-child dependency checks distinguish outbound imports, while
  inbound graph entries name the orchestration parent. New crate-visible child
  consumers can therefore retain the same parent-level graph entry.
- Reason for deferral: no forbidden current caller is established. A finer
  graph needs explicit allowed consumers for each crate-visible child and must
  preserve existing approved calls and the private checked-state boundary.
- Revisit condition: adding an orchestration child or changing its consumers.

## DF-047: Keep captured scheduler policy and recovery checks aligned

- Owner: Scheduler and collaboration policy maintainers
- Finding: managed and External policy defaults are pinned separately, while
  the control-plane reconciliation interval is also fixed in its timer. Direct
  negative assertions for changed retained policy rows and post-queue source or
  priority changes would strengthen future regression coverage.
- Reason for deferral: current values and guards match the fixed policy, and
  existing bounds, ordering, fairness and recovery checks cover their present
  behavior. No current wrong policy value or unsafe claim is established.
- Revisit condition: changing captured policy values, policy integrity checks or
  restart-time candidate reconstruction. Add owner-specific corruption and
  reopening checks with that change.

## DF-048: Keep queue claims and lifecycle state sets synchronized

- Owner: Assignment queue, scheduler and aggregate storage maintainers
- Finding: foundation claim helpers used by tests overlap the live scheduler's
  claim path, while task and exchange state sets appear in several store owners.
  A later transition change could leave a helper or active-state set behind.
- Reason for deferral: no omitted current state or unsafe transition is
  established. Existing owning transactions, admission guards and activation
  fences retain their present contracts; a broad helper refactor is not needed
  for current correctness.
- Revisit condition: adding a task or exchange state, or changing reservation,
  requeue or claim ownership. Review the affected sets together and retire or
  narrow obsolete helpers as part of that change.

## DF-049: Bind scheduler wake-record reads to the verified file

- Owner: Scheduler wake transport maintainers
- Finding: wake-path lookup verifies the gateway record and then reads its path
  again. A same-user replacement between those operations could change the
  non-authoritative wake endpoint.
- Reason for deferral: the state home is within the trusted user boundary, wake
  hints grant no authority, and global reconciliation repairs missed hints. No
  exploit across an untrusted boundary is established.
- Revisit condition: changing the state-home trust boundary or wake protocol.
  Reconsider reading and checking one open file descriptor at that boundary.

## DF-050: Preserve immutable Submit receipts across live approval gaps

- Owner: Gateway acceptance and approval projection maintainers
- Finding: live Run projections use checked Broker approvals. SubmitAccepted
  keeps an immutable acceptance snapshot, and LookupSubmit reads retained
  operation facts without that projection. A combined regression scenario would
  protect this separation during an approval publication gap.
- Reason for deferral: the current paths preserve their separate contracts.
  Existing checks cover live projection refusal, lookup recovery and accepted
  receipts; no incorrect current receipt or unsafe replay is established.
- Revisit condition: changing acceptance capture, either converter or approval
  recovery. Check that the accepted snapshot stays unchanged and the original
  operation facts remain readable when a live projection refuses the gap.
  Preserve LookupSubmit's separate current-outcome observation.

Record a future entry only with a concrete finding, owner, reason for deferral,
and revisit condition. Promote epic-sized work to the
[TODO owner](../todo/README.md) or adopt it in the canonical
[roadmap](../roadmap/README.md).
