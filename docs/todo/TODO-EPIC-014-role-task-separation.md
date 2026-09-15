# EPIC-014 execution dossier: Role/Task separation and structured completion review

## 1. Purpose, restoration context, and authority

Dolgorae must load the selected Role into a Specialist or Run's stable Agent Configuration, then accept the actual work as a separate task request. The Role describes the specialist's expertise and durable behavior. The task describes what the caller wants that specialist to do now.

Apply this separation to reusable External Specialist Engagements and one-shot Specialist Review. Preserve the existing runtime, controller authorization, immutable-target, persistence, and recovery foundations. Reuse the existing task-assignment model rather than introducing another task engine.

This temporary dossier restores the earlier development handoff and serves as EPIC-014's active execution SOT. It integrates canonical documentation, roadmap reconciliation, implementation, and acceptance without becoming a new specification authority, runtime state owner, or roadmap. Requirement labels R01-R12 are local traceability labels, not newly allocated canonical requirement or Task IDs.

### Current adoption notice

At restoration, checkout `6ff1ef4` already contains **EPIC-014: Role/Task Separation and Structured Completion Review**. Its roadmap records TASK-039 through TASK-044 as `COMPLETE` and TASK-045 as `PLANNED`; EPIC-014 remains `ACTIVE`.

These are recorded roadmap states, not an independent implementation-verification result. Do not create a duplicate Epic, reset completed Tasks, or replace accepted v3 decisions with the earlier proposal's open alternatives. Section 7 maps this handoff to the existing work. Recheck the canonical roadmap before subsequent action.

The original gap analysis below concerns historical checkout `130a046`. It explains why the change was requested; it must not be presented as proof that those defects still exist in the restored checkout.

Aquarium's companion `NEW-REVIEW.md` is a separate consumer-side handoff. Aquarium's enabled Mulgae/Orca improvements and temporary Independent Review disablement do not depend on Dolgorae repair. Producer completion must not automatically restore Aquarium routing.

Restoring this file authorizes no code changes, canonical-document edits, runtime migration, installation, live review, staging, commits, or publication. Those remain separately scoped work.

## 2. Original problem and existing foundations

| Historical evidence | Original limitation or useful foundation | Required direction |
| --- | --- | --- |
| [Architecture](../architecture/README.md), Profile and Agent Configuration ownership | Codex Profile and Role-bearing Agent Configuration were already separate. | Preserve the distinction; do not make one shared Profile server select one Role for all Runs. |
| [External engagement implementation](../../src/external_engagement.rs), `AssignExternalSpecialistTask` | General assignment already accepted an objective, context references, expected output, execution intent, deadline, and idempotency identity. | Reuse its durable acceptance and execution foundations. |
| [External engagement implementation](../../src/external_engagement.rs), `task_prompt` | Rendering an artifact identifier into a prompt did not itself establish model-readable context. | Verify actual authorized content delivery, not only metadata presence. |
| [Legacy review v2 schema](../protocol/dolgorae-specialist-review-tool-v2.schema.json), `review_request` | The checked input admitted a target and deadline but no substantive caller-authored brief. | Provide a versioned intent-aware carrier without silently changing legacy semantics. |
| [Review implementation](../../src/review.rs), the original `execute_scoped` path | The adapter generated `ReviewRequest::fixed()` instead of receiving the caller's detailed task. | Dispatch explicitly supplied work through the established lifecycle. |
| [Reviewer configuration](../../src/specialist.rs), the original `ReviewerRuntimePlan::resolve` | The individual objective was appended to stable Reviewer instructions; review objective validation rejected ordinary newlines. | Separate stable behavior from task content and support bounded multiline input. |
| [Reviewer output](../../src/specialist.rs), legacy `ReviewerOutput` | Output was summary/findings, without a dedicated criterion-assessment contract. Finding locations were already nullable. | Preserve full requested assessment without inventing source locations or overloading native status. |

The original problem was not that Dolgorae had no task dispatch. Creation and execution were already separate internally. The correction concerns input contracts, instruction composition, context delivery, and result preservation.

## 3. Required conceptual model

| Concept | Responsibility | Lifetime and boundary |
| --- | --- | --- |
| Codex Profile | Executable, `CODEX_HOME`, deterministic environment, process-static configuration, and verified runtime capabilities. | Existing Profile lifecycle and snapshot rules. Multiple Roles may share a Profile. |
| Agent Configuration and Role | Expertise, stable instructions, model, effort defaults, descriptive purpose, and Profile binding. | Stable for the relevant Run generation under existing snapshot and lineage rules. |
| Task Request | Current work, constraints, authorized context, expected result, execution intent, and task identity. | Accepted independently of specialist creation and bound to its accepted basis. |
| Execution policy | Authenticated controller, lane, writer ownership, network, approval, and capability authority. | Enforced through native state and policy, not granted by task prose. |
| Run and native Turn | The conversation/execution session and its actual runtime operations. | A logical task is associated with its execution; transport retries are not new work. |
| Task Result | Output for an accepted task, operational outcome, and evidence limitations. | Persisted, collected, and recovered without losing its task, candidate, or provenance. |

Keep Run `purpose`, such as `review`, separate from task-level `change` or `completion`. Neither is Aquarium's remediation or confirmation review mode, and none grants additional authority.

A retained hire-level `objective` is non-executable hiring rationale. It must not contain an obligatory first task, become an implicit task, or replace a later assignment. Do not add several synonymous mission/objective fields when clarifying the existing contract is sufficient.

## 4. Functional requirements

### R01. Configure the specialist without inventing work

- [ ] Select a Profile and Role and create or hire a specialist without requiring the detailed first task.
- [ ] Do not infer executable work from a Role name, hiring rationale, recent roadmap item, previous task, or default generic objective.
- [ ] Keep task text out of generation-stable Role instructions and their configuration digest.
- [ ] Different tasks and hiring rationales under the same configuration must not require different Role snapshots.
- [ ] Preserve bound Role/Profile configuration if its source files later change. Follow existing new-Run or lineage rules for intentional changes.
- [ ] Keep startup and capability probes distinguishable from assigned work. Preserve lazy worker startup where already supported.

A Reviewer Role may require independent judgment, evidence-based findings, and explicit uncertainty. It must not silently select a project Task, candidate, or acceptance criteria.

### R02. Accept substantive task requests as data

A caller must be able to submit the real work, not only a short label or project Task ID.

| Information | Meaning |
| --- | --- |
| Objective and intended outcome | What to do now and why. |
| Constraints and non-goals | Relevant invariants, exclusions, and action boundaries. |
| Context and provenance | Readable material and its declared authority/basis. |
| Target | Exact native candidate or workspace scope where applicable. |
| Expected result | Requested deliverable and supported report contract. |
| Lifecycle metadata | External request identity, execution intent, deadline, idempotency, and native lineage. |

- [ ] Support multiline UTF-8, including Korean, indentation, Markdown, quotes, and code examples.
- [ ] Define size accounting and invalid input. Preserve exact accepted strings under the adopted v3 contract, including line endings.
- [ ] Do not truncate, summarize, or silently discard substantive input. Reject invalid or oversized requests before dispatch.
- [ ] Provide the adopted script-friendly carrier so callers need not manually escape a long brief as one shell argument.
- [ ] Treat payloads as data. Shell metacharacters and example commands must not be evaluated by the local adapter.
- [ ] Do not reinterpret task text as configuration, controller identity, or policy changes.

The adopted roadmap selects checked v3 request input through `specialist review --request-stdin --format json` and reusable facade v3 assignment. This statement identifies the adopted design, not an installed-runtime guarantee. Use canonical schemas and capability evidence before invoking a particular binary.

### R03. Preserve reusable and one-shot usage

| Presentation | Required behavior |
| --- | --- |
| Reusable specialist | Configure or hire once, then submit separate tasks through the existing engagement lifecycle. |
| One-shot operation | Accept configuration selection and an explicit task in one user-facing operation; internally create, assign, collect, and close. |

Semantic separation does not require two external calls. Conversely, changing the task must not require reconstructing a reusable specialist solely to replace its objective.

- [ ] Preserve native task concurrency, continuation, and release rules.
- [ ] Use a fresh Reviewer Run for a new independent one-shot review.
- [ ] Do not import an implementer's or previous review's conversation into an independent review silently.
- [ ] Document intentional persistent-session reuse without representing it as fresh independence.
- [ ] Do not introduce a mandatory new conversation manager.

### R04. Reuse the durable task core

- [ ] Use existing durable task acceptance and execution, or a bounded common semantic service extracted from it.
- [ ] Keep one coherent definition of accepted work across general assignment and intent-aware review.
- [ ] Reuse task, Run, dispatch, and result owners instead of adding a parallel registry.
- [ ] Keep immutable-target capture and settlement in their existing native owners.
- [ ] Internal semantic reuse does not require a shell or JSON round trip through a public command.
- [ ] Distinguish a project's roadmap Task label from a Dolgorae task identity or ownership credential.

Dolgorae is not responsible for inferring or executing an external AI's entire project graph. Do not add a scheduler, workflow language, replacement database, or new daemon for this correction.

### R05. Bind accepted work and recover it faithfully

Persist the accepted request before dispatch, including sufficient authorized content to recover what the caller actually assigned. A digest without retained content or a resolvable immutable source is not sufficient.

- [ ] Bind task text, context, expected result, target association, and configuration association through existing native owners.
- [ ] Preserve decoded strings exactly; use the adopted canonical JCS request identity rather than raw transport formatting as semantic identity.
- [ ] Changing an original request file, context source, Role definition, or Profile later must not rewrite the accepted task.
- [ ] An identical idempotent retry must reuse native state rather than create another accepted execution.
- [ ] Reusing an identity for different work, target, or accepted context must produce the native conflict.
- [ ] Preserve task-to-Run/dispatch/Turn/result association through disconnection and process recovery.
- [ ] Preserve native unknown-outcome handling when dispatch acceptance cannot be established. Do not promise exactly-once execution across an unresolved runtime boundary.

Keep necessary request material in protected native storage. Do not duplicate Codex conversation storage or publish sensitive briefs and context in broad diagnostics, tracked evidence, or public projections.

### R06. Deliver readable context without changing the candidate

Required context must reach the actual execution environment as authorized readable content. A caller-side path or artifact UUID alone is insufficient.

The adopted v3 design chooses bounded inline context with unique IDs and caller-declared provenance. Preserve that choice. This handoff does not require a new file resolver, attachment service, or artifact registry.

- [ ] Bind accepted inline content before the Turn and present it separately from candidate implementation.
- [ ] Reject unknown/duplicate context references, invalid provenance shapes, and unsupported bare paths or artifact identifiers.
- [ ] Do not silently read arbitrary host files, another repository, credentials, private conversations, or excluded data.
- [ ] Keep caller-declared provenance distinct from an independently verified authority claim.
- [ ] Preserve native immutable-target semantics for staged, workspace, dirty, HEAD, commit, and range.
- [ ] For staged review, only the index candidate can satisfy implementation criteria. Unstaged edits, including edits to staged paths, cannot make it pass.
- [ ] Permit relevant unchanged candidate files to be inspected at the correct basis.
- [ ] Support completion assessment of a committed candidate with no new diff, without staging or manufacturing changes.
- [ ] Separate an approved requirement at another revision from implementation evidence for the candidate.
- [ ] An unapproved candidate edit removing a criterion must not silently redefine the approved scope.
- [ ] Missing authority or required evidence must remain explicit and prevent an unqualified completion claim.

The reviewer must be able to compare the derived brief with the supplied applicable original authority. Do not present an excerpt as complete authority when necessary sections are absent.

Existing artifacts can remain supported through their own native contracts. Do not confuse retained legacy reference support with v3 permission to resolve arbitrary paths. Any future context carrier needs its own justified path, symlink, access, size, and cleanup design.

A later worktree change does not rewrite the historical captured candidate or its result. The old result must not be relabeled as assessment of a newer candidate.

### R07. Keep authority outside task prose

The effective action set comes from authenticated controller authority, bound Run capabilities, validated execution intent, and current native policy. Task instructions may request less activity; they cannot grant more.

- [ ] A read-only specialist does not gain write permission because a task asks it to fix a finding.
- [ ] Role, task, and context text cannot change controller identity, execution lane, writer ownership, network policy, approvals, recursion policy, or assurance.
- [ ] Preserve Turn-scoped access context and `policy_epoch`; do not put current access into stable Role identity.
- [ ] Honor static report-only review restrictions, including no source mutation and no test execution when that is the authorized contract.
- [ ] Changing from change review to completion review grants no additional actions.
- [ ] General specialist tasks retain their supported native execution intents. Do not make every task globally read-only because review motivated this work.
- [ ] Keep capability carriers and credentials out of model input and general result output.

Document enforcement limits accurately. Behavioral instructions are not equivalent to hardened process containment or perfect prompt-injection resistance. Repository files and supplied context are evidence, not authority to override controller instructions or native policy.

### R08. Support intent-aware review

Accept the substantive Review Brief instead of substituting a fixed generic objective.

A completion brief must express the work unit, intended outcome, criteria and sources, constraints, non-goals, authority basis, candidate boundary, completion checkpoint, available evidence, and requested result. Preserve existing invocation metadata where its owner requires it.

- [ ] `change` review assesses the selected change in context and must not claim whole-Task/Epic completion.
- [ ] `completion` review includes ordinary defect review and applicable criterion assessment in one authorized invocation.
- [ ] Inspect relevant unchanged code and production integration where required; missing work can be invisible in a diff.
- [ ] Compare the brief with applicable original requirements rather than accepting the author's completion claim.
- [ ] Respect approved exclusions and superseded requirements; do not turn personal design preferences into obligations.
- [ ] For member-Task review, assess obligations due now rather than unfinished future member work.
- [ ] For an Epic, include applicable member obligations and integration seams.
- [ ] Distinguish pre-closeout readiness from an already-claimed completed outcome. A later authorized commit/status update is not automatically an implementation defect.
- [ ] A demonstrable security, correctness, or data-integrity defect remains relevant even if absent from a literal acceptance list.

Dolgorae owns faithful delivery and its declared result contract. The caller owns project scope and the final workflow decision. Do not make Dolgorae a roadmap interpreter or launch another provider pass automatically.

### R09. Preserve complete results and evidence limits

Preserve the requested assessment through provider output, validation, durable artifact storage, collection, redelivery, and caller projection.

| Dimension | Required meaning |
| --- | --- |
| Backend lifecycle | Native execution, collection, closure, and settlement state. |
| Technical findings | Supported defects and regressions with valid evidence and identities. |
| Requirement assessment | Criterion source, implementation/absence evidence, verification basis, status, and remaining gap. |
| Evidence limits | Missing authority, unavailable required checks, static-only conclusions, and caller-reported claims. |
| Overall answer | A conclusion limited to the actual task, checkpoint, candidate, and evidence. |

- [ ] Zero findings must not imply completion.
- [ ] Completion output must cover the accepted criterion set under the adopted v3 schema, including its identity and ordering rules.
- [ ] Preserve distinctions equivalent to met, unmet, unverified, and justified not-applicable using the canonical status vocabulary.
- [ ] Missing required assessments or mandatory evidence prevent an unqualified completion approval.
- [ ] Do not hide an unread criterion behind not-applicable or infer a valid assessment from free-form summary text.
- [ ] Retain real source locations where available; use nullable locations and requirement/inspection evidence for genuine omissions.
- [ ] Do not fabricate a line number, missing file, provider finding ID, or claim that a limited search proves universal absence.
- [ ] Distinguish malformed required output from a valid report that identifies unmet work or insufficient evidence.
- [ ] Preserve provider assessment separately from coordinator interpretation.
- [ ] Do not silently discard detailed assessment because a legacy projection only supports summary/findings.
- [ ] Keep accepted results immutable and collectible without re-execution.

EPIC-014 adopts a structured v3 report. Implement and reconcile that contract rather than reopening an undocumented summary-field encoding. Preserve legacy v1/v2 meaning.

A validator can verify structure and coverage of accepted criterion IDs. It cannot prove the model's reasoning correct or prove that the supplied criteria exhaust the authoritative requirements.

### R10. Preserve deadlines, recovery, and settlement

- [ ] Follow the adopted v3 deadline start at durable task acceptance. Preserve legacy deadline meaning for legacy contracts.
- [ ] Document how input validation, context preparation, capture, acceptance, dispatch, execution, and cleanup relate to the relevant budgets.
- [ ] Do not reset the task budget on retry or recovery.
- [ ] Input, context, parser, and result failures must follow existing native state, cancellation, release, and settlement rules.
- [ ] A caller disconnect must not lose a committed result or cause redelivery to execute the task again.
- [ ] Uncertain dispatch stays uncertain until native recovery resolves it. Do not start a second review as an automatic substitute.
- [ ] Preserve one-active-task/member rules, writer coordination, controller authorization, and cleanup ownership.
- [ ] Keep cancellation and lifecycle transitions truthful even when report validation fails.

Do not claim stronger crash or cancellation guarantees than the verified native runtime contract. Any affected concurrency, locking, process, or protocol behavior needs the repository's required adversarial and empirical verification.

### R11. Keep version and interface boundaries explicit

The adopted approach is additive Specialist Review and External Specialist Facade v3 support with unchanged v1/v2 semantics. Public-v1 gRPC and the existing MCP availability/identity boundary remain unchanged.

- [ ] Inventory human CLI, Machine CLI, reusable facade, review adapters, paired skills, and affected internal callers.
- [ ] No supported intent-aware path may silently drop a brief or downgrade completion into generic change review.
- [ ] Do not pass unknown fields to legacy schemas and assume they are accepted.
- [ ] Preserve legacy request/result and persisted-state meaning; legacy generic review is not criterion-complete v3 review.
- [ ] Preserve historical records and configuration identities or document an explicit supported recovery/migration boundary.
- [ ] Do not rewrite immutable history to make old tasks appear created under the new contract.
- [ ] Preserve MCP per-request identity requirements and unadmitted/disabled states.
- [ ] Do not modify the frozen public gRPC surface for a private review carrier.
- [ ] Use existing diagnostics/capability surfaces where needed, not a new compatibility service.
- [ ] Document actual supported commands and installed-version requirements only when verified.

Command syntax need not match Mulgae or Orca. The required parity is separation of stable configuration from caller-assigned work, not identical lifecycle or CLI design.

### R12. Preserve Aquarium's independent rollout

- [ ] Do not modify Aquarium, Mulgae, Orca, or their installed integrations as part of this handoff.
- [ ] Aquarium Dolgorae-based Independent Review remains disabled until a separately authorized Aquarium change restores it.
- [ ] A producer release, successful probe, repaired installation, or passing test must not automatically re-enable the consumer.
- [ ] Do not bypass Aquarium's disabled route using low-level Run calls, a reusable engagement, an older binary, or a renamed wrapper.
- [ ] Producer readiness and consumer adoption must be reported separately.
- [ ] Unrelated explicitly authorized Dolgorae operations remain available.
- [ ] Do not cancel, settle, migrate, or clean up existing work merely because a consumer route is disabled.

Preserve historical completed integration milestones while describing current availability truthfully. Neither historical activation nor restored producer capability proves present Aquarium routing.

## 5. Illustrative usage

These examples describe semantics, not complete CLI payloads. Actual requests must satisfy the adopted schema.

### One Role, separately assigned work

```text
Configuration:
  Profile: the caller-selected Codex execution environment
  Role: architecture analyst
  Stable behavior: inspect contracts, compare trade-offs, state evidence limits

Task A:
  Compare the two approved request-routing designs using the supplied ADRs.
  Return a comparison and unresolved questions. Do not edit source.

Task B, assigned separately after Task A:
  Inspect cancellation in the declared candidate.
  Report contract violations and missing recovery cases.
  Do not run tests or modify source.
```

Task A does not become stable Role configuration. Deliberate session reuse for Task B is documented as reuse, not fresh independent judgment.

### Fresh one-shot completion review

```text
Configuration:
  Selected Profile and independent Reviewer Role
  Fresh Run under the existing static, read-only review contract

Task:
  Assess whether <work-unit-id> is ready for its approved closeout checkpoint
  in the staged index candidate. Do not count unstaged implementation.

Intended outcome:
  Make the request deadline configurable without changing the public API.

Applicable criteria:
  - Preserve the existing default when configuration is absent.
  - Apply one deadline to the initial request and all retries.
  - Cancel in-flight work on expiry.
  - Reject zero and negative configuration values.

Context:
  Supply applicable approved requirements and ADR content with provenance.
  Inspect relevant unchanged files from the same candidate.
  Label caller-reported checks as caller-reported.

Non-goals and action boundary:
  No replacement request engine, test execution, source edits, staging,
  commits, or publication.

Result:
  Return findings, criterion assessments, and remaining evidence gaps.
  Do not infer completion from zero findings.
```

One user-facing invocation is sufficient. Internally, configuration and work remain separate, and native collection/closure/settlement still apply.

## 6. Canonical documentation reconciliation

| Owner | Required responsibility |
| --- | --- |
| [Specifications](../specs/README.md) | Observable Role/task separation, hire-rationale meaning, accepted work, context, review purpose, output, authority, deadlines, and compatibility. |
| [Architecture](../architecture/README.md) | Instruction composition, shared semantic service, durable state ownership, dispatch/result flow, and failure boundaries. |
| [ADRs](../architecture-decision-records/README.md) | Accepted rationale for core reuse, additive v3, exact input preservation, inline context, structured assessment, and rejected alternatives. |
| [Protocols](../protocol/) | Checked requests/results, persisted shapes, examples, bounds, validation registries, and unchanged legacy/public contracts. |
| [Roadmap](../roadmap/README.md) | EPIC-014 identities, order, statuses, dependencies, verification, and release-boundary meaning. |
| [TODO/dossiers](README.md) | Any detailed adopted dossier under the established lifecycle, not a second permanent roadmap. |
| [Implementation tips](../implementation-tips/README.md) | Safe composition, test seams, candidate/context distinction, compatibility hazards, and recovery. |
| [Operations](../ops/README.md), [README](../../README.md), [paired skill](../../skills/use-dolgorae/SKILL.md) | Accurate usage, long-request examples, output interpretation, diagnostics, limits, and authorization. |

The earlier handoff asked the team to choose input/output, context, and compatibility designs before implementation. EPIC-014 records those choices. Do not treat restoration as permission to reopen them without a concrete contradiction or newly authorized change.

Check consistency across the canonical owners. Planned support, implemented code, verified behavior, released artifacts, installed runtime, and adopted consumer integration are separate states.

## 7. Existing roadmap mapping and delivery boundaries

Do not allocate another Epic for the same work. The earlier seven-unit proposal has already been adopted as follows.

| Existing owner | Responsibility | Recorded state at restoration |
| --- | --- | --- |
| EPIC-014 | Role/Task Separation and Structured Completion Review. | ACTIVE |
| TASK-039 | Freeze Role/Task and completion-review contracts, including additive v3 and compatibility. | COMPLETE |
| TASK-040 | Stable Reviewer Role and shared accepted-task composition/persistence. | COMPLETE |
| TASK-041 | Inline context binding, provenance, and candidate separation. | COMPLETE |
| TASK-042 | Additive v3 CLI and reusable facade execution. | COMPLETE |
| TASK-043 | Criterion-complete results, artifact preservation, and recovery. | COMPLETE |
| TASK-044 | Compatibility and adversarial regression. | COMPLETE |
| TASK-045 | Delivered documentation, complete deterministic gate, and independent review. | PLANNED |

Source: [canonical roadmap](../roadmap/README.md), inspected at `6ff1ef4`. These labels must not be used as independent proof that implementation or acceptance is correct.

The roadmap places EPIC-014 before EPIC-008 in delivery order without changing the EPIC-008 release boundary. It does not authorize Aquarium activation, MCP admission, public gRPC changes, a stable release, installation, or publication.

For remaining work and any discovered gap:

- [ ] Map R01-R12 and the applicable scenarios to existing owners before proposing new work.
- [ ] Preserve completed identities and established evidence; do not reset status merely because this file was restored.
- [ ] Resolve an actual implementation or acceptance gap in its appropriate owner rather than hiding it as optional future work.
- [ ] Record dependencies, included/excluded work, required evidence, and the next authorized step.
- [ ] Preserve the roadmap's single occupied Task slot and allowed states.
- [ ] Do not infer execution order from numeric identity.
- [ ] Any additional identity must follow the existing allocation rules and an explicit adoption decision.

Keep the canonical lifecycle vocabulary: `PLANNED`, `ACTIVE`, `IN_REVIEW`, `BLOCKED`, `COMPLETE`, and `SUPERSEDED`. At most one Epic may be `ACTIVE`; the existing `ACTIVE`, `IN_REVIEW`, and `BLOCKED` Task-slot rules still apply.

Task completion requires the actual repository gate, including synchronized owners, required verification, independent review, disposition of blocking findings, and task-scoped commits. This handoff creates none of that evidence by itself.

## 8. Verification and acceptance

Use Rust tests for product semantics and existing isolated integration/black-box facilities for execution. Verify the actual input presented to the runtime, not only an outer request field or help text.

Exact accepted-payload, request-identity, and stable Role-digest assertions are useful. Brittle matching of explanatory prompt prose is not a substitute for behavioral testing.

| Scenario | Required outcome |
| --- | --- |
| Specialist created without a task. | No implicit business work is dispatched. |
| Two tasks and different hiring rationales under one stable Role. | Separate work with unchanged Role/configuration identity. |
| Role/Profile sources change after creation. | Existing bound configuration is preserved. |
| Multiline Korean/English, Markdown, line endings, and metacharacters. | Exact accepted content reaches runtime input as data. |
| NUL, invalid structure, or oversized input. | Checked rejection before dispatch, without truncation. |
| Original request source changes after acceptance. | Recovery uses the original accepted basis. |
| Same identity and same accepted work retried. | Native idempotent behavior, not another execution. |
| Same identity reused with changed work/context/target. | Native conflict. |
| Unknown/duplicate context ID or unsupported host-path reference. | Checked rejection, not an implicit read or fake context-delivery claim. |
| Accepted inline context includes caller-declared provenance. | Content is readable; provenance is not falsely upgraded to verified authority. |
| Required code exists only unstaged. | Staged completion cannot count it. |
| Relevant unchanged candidate files exist. | Correct candidate versions remain inspectable. |
| Committed completion target has no new diff. | Assessment without staging or fabricated changes. |
| Requirements come from another approved basis. | Context remains separate from candidate implementation. |
| Candidate removes a criterion without an approved scope change. | The authority gap remains visible. |
| Brief omits a criterion present in supplied original authority. | Reviewer can identify it; no completeness claim based solely on the abbreviated brief. |
| Missing implementation has no real source line. | Requirement and inspected absence evidence, no fabricated location. |
| Read-only task asks for edits or context requests policy override. | No authority escalation. |
| Change review has zero findings. | No whole-Task/Epic completion inference. |
| Completion report contains unmet/unverified criteria. | Status and evidence survive validation, storage, and projection. |
| Missing, duplicate, unknown, or reordered required assessment. | Adopted v3 validation applies; no synthetic clean result. |
| Static support exists but mandatory runtime proof is absent. | Verification limitation remains explicit. |
| Oversized or malformed output. | Honest native result/error semantics, no silent loss or automatic provider retry. |
| Caller disconnects after result commitment. | Original result remains collectible without re-execution. |
| Crash leaves dispatch acceptance uncertain. | Existing unknown-outcome recovery, not speculative second execution. |
| Deadline expires during recovery. | No fresh budget; native recovery/cleanup rules apply. |
| Legacy client/state meets the new implementation. | Preserved v1/v2 meaning and explicit version boundaries. |
| New independent review follows earlier persistent work. | Fresh Reviewer Run, not silent conversation reuse. |
| Producer acceptance passes while Aquarium remains disabled. | No automatic consumer activation. |

Also cover supported native target kinds, configuration/credential binding, cancellation races, writer ownership, settlement, and applicable source-integrity invariants using existing suites.

Fake-runtime tests prove transport, structure, binding, and lifecycle behavior, not the quality of a real model's judgment. Cases involving interpretation of requirements need separately identified functional verification. Record unavailable evidence honestly.

Use the repository's required complete gate when implementing or closing the owning Task. Live Codex/provider calls require their existing explicit authorization and opt-in. Merely listing a scenario here does not authorize executing it or changing production runtime state.

## 9. Non-goals

This work does not introduce a universal task/workflow language, a new scheduler, a second conversation database, a new global context registry, a new target-capture engine, Role hot-reloading, additional task concurrency, hierarchy expansion, or a provider marketplace.

It does not require identical CLI syntax or isolation guarantees across Dolgorae, Mulgae, and Orca. It does not make every specialist read-only or grant a reviewer extra execution authority to fill an evidence gap.

Do not force a different Role for every task. Do not recreate existing general task assignment. Do not make this correction wait for all remaining Personal Alpha work without a demonstrated dependency.

Aquarium changes, consumer re-enablement, live runtime migration, installation, release, tags, push, and publication remain outside this restoration.

## 10. Handoff and completion checklist

For documentation/roadmap reconciliation:

- [ ] Identify canonical owners already updated and any remaining contradictions.
- [ ] Preserve accepted v3 decisions, bounds, exact-content semantics, context provenance, and compatibility.
- [ ] Map R01-R12 and acceptance scenarios to EPIC-014's actual Tasks.
- [ ] Do not duplicate the Epic or present recorded status as fresh verification.
- [ ] Document genuine remaining blockers and their owners without expanding scope speculatively.
- [ ] Keep current commands, planned capabilities, release state, installation, and consumer adoption distinguishable.

For eventual implementation closeout:

- [ ] Demonstrate caller request to actual runtime input, with stable Role identity.
- [ ] Demonstrate readable accepted context and exact candidate separation.
- [ ] Demonstrate criterion-complete result validation, artifact preservation, collection, and recovery.
- [ ] Verify legacy compatibility and applicable authority/deadline/unknown-outcome behavior.
- [ ] Complete required deterministic checks and independent review; resolve blocking findings.
- [ ] Update delivered guidance and report unperformed or unavailable checks.
- [ ] State producer readiness without claiming Aquarium activation or publication.

The end state remains simple: Dolgorae configures the expert and enforces native lifecycle and authority; the caller separately assigns the work; the result remains tied to that exact accepted request.
