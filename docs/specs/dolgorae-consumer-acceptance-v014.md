# Dolgorae v0.1.4 three-consumer acceptance

Status: Adopted planning and acceptance requirements. All runtime acceptance
remains unverified. This document publishes no checked successor contract and
completes no implementation Task. The [roadmap](../roadmap/README.md) owns
delivery; the [shared dossier](../todo/TODO-DOLGORAE-V014-RELEASE-ALIGNMENT.md)
owns the five-phase execution and handoff requirements.

## Authority and source selection

Aquarium, Podway and Gul are mandatory release consumers. Gul acceptance alone
cannot establish Dolgorae readiness. Keep the complete adopted CD/GI/EN scope
and EPIC-009, including internal lateral collaboration. Each matrix row below
needs applicable runtime evidence before the final release gate.

Aquarium's Primary/Judge owns semantic judgment and final acceptance. Aquarium
owns backend selection and consumer-side host qualification. Podway owns its
outer cursor, structural receipt, current fences and progression. Dolgorae owns
accepted executor identities, results, recovery, round facts and versioned
exports. Neither a completed worker nor an export advances Podway. Qualification
of a mandatory consumer route is required even while its production/default
activation remains a separate consumer-owned decision.

The historical source inspections below are planning inputs, not selected
installed consumer builds. TASK-067 must
refresh the applicable owners and select exact contract-source, schema and
descriptor revisions/digests before TASK-068 publishes H1. H1 also records agreed
semantics, limits, supported transports and compatibility requirements, the
host/adapter versions used for pre-contract feasibility evidence, named
acceptance owners, required scenarios and accepted external implementation plans.
Final implementation revisions and artifact/package checksums for work scheduled
in later phases remain explicitly pending until H3 or H4; the inspected source
baselines cannot stand in for qualified final implementations.
A release label or dated worksheet does not identify the current contract.

TASK-067's [current source inspection and owner requests](dolgorae-v014-contract-coordination.md)
refresh the historical baselines below. All listed source bytes match their
inspected commits; Podway also has unrelated working-tree changes. Master has
accepted the Phase 1 source/mapping selection and implementation/acceptance
responsibilities. Independent review accepted bounded native feasibility;
[H1 publication](dolgorae-v014-h1-package.md) binds this matrix; the roadmap
records its acceptance. Later actual consumer-runtime acceptance remains pending. The
[successor requirements reconciliation](dolgorae-v014-successor-requirements.md)
records the retained/new behavior map, failure rules and feedback coverage.

| Consumer | Inspected committed baseline | Current owning sources and contracts |
| --- | --- | --- |
| Aquarium | 2b7795443abb14b00b2f34bf7c50324285d10446 | `docs/specs/agent-orchestration.md`; `plugins/aquarium/references/agent-orchestration.md`; `native-agent-mappings.md`; `dolgorae-review-contract.md`. Document interfaces: `aquarium.worker-handoff/v1`, `aquarium.judge-assessment/v1`, `aquarium.orchestration-checkpoint/v1` |
| Podway | 0c316b95d460ef0ba4d6de6d30de798716ff12a4 | `docs/specs/interfaces/supervised-delegation.md`; `automation-client-contract.md`; `release/dolgorae-adapter-contract-v3.json`; `contracts/contract-manifest-v2.json`. Current direct envelope targets: `podway.output/v4`, `podway.error/v2`, `podway.contract-manifest/v2` |
| Gul | 2db9c4f12f88d5ee36810377248efcaf9bf4110c | `docs/roadmap.md`; `docs/todo/GUL-REBUILD.md`; required specifications, architecture and ADR owners. Target provider `dolgorae.public.v2` and consumer v2 remain unpublished; browser `gul.v2` remains a separate contract. Current ADR-0057 and roadmap map Phase 2 to E15 and Phase 4 to E16; historical E15-T5..T20 scope maps to E16-T1..T16 |

For reproducible review, the inspected Aquarium orchestration reference has
SHA-256 `30069e6534e204c0a55e7db48ca34b3e0bbadc3ffbb5762e0175ef29d9648099`,
its native mappings reference
`d48d076c2e4fc88f32527f59b3008cc986e80286338fabe89a7537ca96364d8b`,
and its dormant Dolgorae review reference
`5c061f0e8d9b026f358adfa9efccc7f17abdb1cea5f3e10f50ebb6f8f2a8df9f`.
Podway's supervised-delegation specification has
`7d5bb0a770739c50d9d623aed437f2870cfb5656432548fe7615dd478cef093b`.
These content identities distinguish inspected working-tree authority from its
committed baseline; they are not immutable release pins or passing evidence.

Current Aquarium orchestration uses native Codex/Claude mappings. TASK-111 and
TASK-112 are recorded Completed; remaining whole-workflow qualification and
physical dispatch-response-loss coverage remain consumer-owned. The mappings
inspect Codex CLI 0.162.0, Claude Code 2.1.295 and Claude Agent SDK 0.3.295;
these reference versions do not prove successor MCP support. The Dolgorae review
route is dormant and must not be automatically reactivated. Explicit reusable
Specialist requests still have their own supported facade boundary.

Podway's current planning tree uses Store v15 and the regenerated adapter v3
catalog now points to `spec/sqlite-v15.sql`. The catalog remains
`prepared-not-released`, while its manifest currently declares product 0.2.12
and V2DFC's conditional release target is 0.2.13. TASK-067 must select a coherent
remediated contract-source/schema/catalog combination with the Podway owner for
H1; actual Podway consumer/adapter implementation and artifact bindings follow
at H3. The inspected catalog and manifest do not prove a shipped daemon-free
adapter or a working Dolgorae path. Its old September mapping worksheet records
gaps; it does not establish current runtime support.

Gul's current ADR-0057/roadmap amendment retains E15-T1..T4 completed history
and E15-T21 in Phase 2. Original E15-T5..T20 remain Historical/Retired;
E16-T1..T16 inherit their complete scope in Phase 4. The historical IDs remain unchanged and do not imply completion. Native harness, early
packaging and final campaign now belong to E16-T3, E16-T5 and E16-T16,
respectively. Completed H2/H4 outputs and runtime evidence remain pending.

## Requirements-to-evidence matrix

Contract references in the second column name the exact current schema or the
source-selection row above. TASK-067 binds each row to the agreed contract-source,
schema/descriptor/manifest revisions and digests, semantics/limits/compatibility,
supported transport, pre-contract feasibility host/adapter versions and named
acceptance owner. TASK-068 includes that immutable requirements matrix in H1.
For each row, H1 defines the contents, producer/acceptance owners and delivery
handoff of its later runtime qualification record. Final implementation revisions,
artifact/package checksums and qualification results remain explicitly pending
where their work is scheduled later; H1 completion does not require those future
artifacts. No inspected, provisional or example identity may substitute for a
qualified final runtime pin. Every row feeds TASK-088 independent acceptance and
TASK-089/Gate D; the last column also identifies earlier required checks.

| ID / consumer | Contract/version | Responsible owner | Required behavior and transport | Implementing Tasks | Verification and evidence | Consumer prerequisite | Final gate |
| --- | --- | --- | --- | --- | --- | --- | --- |
| AQ-01 / Aquarium | Aquarium sources above; successor MCP identity contract selected by TASK-067/068 | Dolgorae host/facade maintainer; Aquarium host-integration owner | Trusted Codex and Claude external-Primary stdio MCP routes; protected durable invocation identity, receipt/lookup, replay and conflict; Codex remains executor | TASK-067, TASK-068, TASK-077 | Pre-contract two-host matrix, then actual host-to-facade dispatch/replay/reconnect/restart/response-loss qualification; final artifact binding | EXT-AQ-01: named adapter/install/state owner and supported consumer route; no model-supplied authority | Phase 1 feasibility; Phase 3 route acceptance; TASK-088 and Gate D |
| AQ-02 / Aquarium | `aquarium.worker-handoff/v1`; exact native mapping revision | Dolgorae identity maintainer; Aquarium delivery/acceptance owner | Exact accepted Role body/dependencies/guidance, admitted configuration, accepted task and complete result identities through permitted facade/Broker reads | TASK-076, TASK-077, TASK-086 | Known answers plus actual admitted bytes; same-name Role drift, configuration drift, task versus envelope, full result versus preview; no private-store scraping | Consumer binding/collector must consume supported successor facts and retain provenance; local prepared digests alone are insufficient | Phase 3 qualification; TASK-088 and Gate D |
| AQ-03 / Aquarium | `aquarium.judge-assessment/v1`, `aquarium.orchestration-checkpoint/v1` | Aquarium Primary/Judge and host qualification owner; Dolgorae operational owner | Preserve semantic acceptance, exact candidate/goal/attempt applicability, Worker/Judge boundaries and current native paths; Dolgorae completion supplies facts only | TASK-076, TASK-077, TASK-086 | Actual bounded consumer-path acceptance/rejection of stale candidate, wrong goal/attempt, incomplete or superseded delivery; distinct semantic and structural results | EXT-AQ-01: qualified opt-in Dolgorae composition if current consumer code lacks it; default backend choice remains Aquarium-owned | Phase 3 compatibility acceptance; TASK-088 and Gate D |
| AQ-04 / Aquarium | Native mappings and checkpoint v1; frozen successor recovery rules | Aquarium collector/host owner; Dolgorae lifecycle maintainer | Recover original accepted identity/results after dispatch or result loss; conservative rounds and current idle/released proof; no redispatch of unknown work | TASK-070, TASK-071, TASK-074, TASK-076, TASK-077, TASK-086 | Physical loss around acceptance, supported lookup, host/MCP/executor restart, complete result recovery and quiescence; negative metadata cases are not positive loss evidence | Consumer-owned physical dispatch-loss and recovery integration; prior native result-read evidence is reused only within its tested scope | Phase 3 actual-path qualification; TASK-088 and Gate D |
| AQ-05 / Aquarium | Successor interaction contract and current approval boundaries | Dolgorae interaction owner; Aquarium Controller/Primary owner | Supported command/permission/MCP approvals reach the declared Controller or an explicit bounded policy decline; credentials remain outside model traffic | TASK-077, TASK-079, TASK-086 | Actual approval routing, ownerless decline, protected-content canaries and no stalled task; mandatory host support cannot pass as unsupported | Agree host/controller answer ownership and allowed decisions | Phase 3 acceptance; TASK-088 and Gate D |
| AQ-06 / Aquarium | Current reusable-facade contract and versioned Machine/review shapes; selected successor lock | Dolgorae compatibility/package owner; Aquarium integration owner | Preserve supported facade/review/recovery and same-release skill behavior; exact supported-source/bounds admission without activating the dormant review backend | TASK-068, TASK-071, TASK-078, TASK-087 | Historical read/collect/close/recovery, package/skill/capability checks and actual supported consumer regressions after cleanup; exact candidate provenance | Aquarium qualifies any new candidate-admission code; no automatic backend or production installation switch | Phase 3 post-cleanup candidate; TASK-088 and Gate D |
| PW-01 / Podway | `podway.dolgorae-adapter-contract/v3`, output v4/error v2/manifest v2; exact remediated pins | Dolgorae adapter owner; Podway contract/release owner | Supported in-process direct CLI contract; manifest/schema/catalog verification, success-only outcome lookup/exact replay, reactivation refresh and retired-command handling; no daemon/job settlement or DB access | TASK-067, TASK-068, TASK-077, TASK-086, TASK-087 | DOLV3-001 through DOLV3-009 against actual selected CLI/artifact; closed schemas, packaged bytes, unsupported-peer refusal and current Store compatibility | EXT-PW-01: coherent remediated manifest/catalog/runtime selection; V2DFC-018 contract corrections and applicable qualification | Phase 1 coherent selection; Phase 3 adapter acceptance; TASK-088 and Gate D |
| PW-02 / Podway | `podway.delegation-accept-input/v1`, `podway.delegation-receipt/v1`; versioned external/managed mappings | Dolgorae export owner; Podway mapping owner; supervising Primary | All binding/adapter/host/scope/supervisor/worker/task fields and exact Role/configuration/task/complete-result digest domains; external facade and managed run-bound Broker remain distinct | TASK-076, TASK-077, TASK-086 | Field-by-field known answers and actual external/managed paths through public/authorized bytes; complete result including accepted change components; no substitute digest | EXT-PW-02: accepted mapping and trusted binding configuration for both Primary-host cases | Phase 3 runtime receipt-path acceptance; TASK-088 and Gate D |
| PW-03 / Podway | Delegation input v1: `max_rounds` 1..32, `after_accept` retain/release | Dolgorae accounting/membership owner; Podway structural owner | Count accepted assignments once, including unknown dispatch conservatively; retain requires proven idle, release requires settled release; polls/replays do not count | TASK-076, TASK-080, TASK-083, TASK-086 | Actual feedback/reuse, exhaustion, unknown outcomes, restart and current disposition checks; correct idle/released receipt and conflict rejection | Primary/integration retains caller scope and refuses unsupported evidence; no receipt based on allocation or interrupt acknowledgement | Phase 3 acceptance; TASK-088 and Gate D |
| PW-04 / Podway | Work-package v1, package-result v1, accept-result v2, receipt-result v1 | Podway package/receipt owner; trusted adapter; Primary/Judge | Materialize full bounded evidence and exact accepted task; Primary records items/semantic acceptance, then Podway structural acceptance; cursor advances only through Podway's separate completion | TASK-076, TASK-077, TASK-086 | Actual package-to-Dolgorae-to-receipt consumer flow with current fences, complete evidence, receipt read and explicit Primary completion; bounds and absent-source failures | EXT-PW-02: executable trusted consumer composition if not supplied by existing public CLI/host path; Podway owns acceptance, never Dolgorae auto-progression | Phase 3 supported-path qualification; TASK-088 and Gate D |
| PW-05 / Podway | Current automation contract; delegation receipt v1 and outcome-lookup-result v1 | Dolgorae recovery owner; Podway transaction/adapter owner | Original-key lost-stdout lookup; item edits make receipt stale; fresh attempt/rework never relabels an old acceptance or automatically redispatches external work | TASK-070, TASK-071, TASK-077, TASK-086 | Cold-process reconstruction, acceptance-response loss, exact replay/conflict, stale/cosmetic-item changes, new goal/attempt, cancellation/late result and simultaneous unaffected Direct work | Consumer recovery keeps both executor and Podway identities/fences; named maintenance/reset authority remains separate | Phase 3 acceptance; TASK-088 and Gate D |
| PW-06 / Podway | Current single-Action declaration plus explicit owner mapping decision | Dolgorae group/export owner; Podway contract owner; Primary/Judge | Full group/merge/synthesis provenance and round accounting; never represent one group as one Podway Action without supported meaning and explicit owner approval | TASK-067, TASK-068, TASK-084, TASK-085, TASK-086 | Group source identity/conflict/gap checks, explicit synthesis result, bounded round export; negative group-as-Action claim; actual allowed receipt path | EXT-PW-02 resolves required mapping. Current contract excludes fan-out/child Actions; a demanded group receipt needs approved consumer contract work, not an optionality relabel | Phase 1 scope decision; Phase 3 qualified mapping; TASK-088 and Gate D |
| GU-01 / Gul | Public v2 and consumer v2 package; browser gul.v2 remains distinct | Dolgorae public owner; Gul contract/foundation Epic owner | Modern creation/provisioning, stable requested mode, writer-holder reads, accepted legacy compatibility and current authority | TASK-068, TASK-070, TASK-071, TASK-076, TASK-078 | Gul-owned Phase 4 actual create/write/read-only/mixed-state/restart/close suite; independent provider reproduction in TASK-088 | EXT-GU-01: completed E15 containing E15-T21 corrections and validated pin before Gate A | Gate A; Phase 4 campaign; combined Gate C and Gate D |
| GU-02 / Gul | Checked submission/error/effect/lookup successor package | Dolgorae intent owner; Gul attempt/draft owner | Definitive refusal/retry, accepted replay, immutable unresolved material and original-key lookup; unknown prior effects cannot become new prompts | TASK-070, TASK-071, TASK-075 | Actual lost responses, changed input/attachment paths, restart, conditional draft cleanup/shared attachment preservation and no duplicate effects | Gul product Epics preserve current CAS/draft-version and attachment protections | Phase 4 campaign; TASK-088 independent acceptance; Gate D |
| GU-03 / Gul | Checked activity/interaction/timeline/result successor shapes | Dolgorae observation/interaction owner; Gul presentation owner | Typed approvals, protected input, safe activity, forward/latest byte-bound pages, chunks/digests and complete group coverage | TASK-075, TASK-078, TASK-079, TASK-084, TASK-085 | Actual approval/history/artifact/privacy cases including oversized content, historical unsupported records, summary versus complete result and partial history degradation | Gul owns decoder/storage/UX and does not infer authority from cache | Phase 4 campaign; combined Gate C and Gate D |
| GU-04 / Gul | Agreed timing/stream policy and Gul current fairness/query requirements | TASK-075/079 owners; Gul allocator/query/UX owner | Six-stream cap, prompt interaction visibility, cancel/resume and fair foreground/background observation; current warm budgets and bounded local reads | TASK-067, TASK-068, TASK-075, TASK-079, TASK-088 | Phase 4 saturated-stream/load/reconnect/native measurements; scoped independent final-artifact latency/recovery reproduction in Phase 5 | Gul owns foreground preemption and bounded actor queries; contract freezes provider timing/cancellation assumptions | Phase 4 evidence; TASK-088 independent measurements; Gate D |
| GU-05 / Gul | Exact candidate/harness/runtime package and explicit account authorization | Gul final qualification owner; Dolgorae provider acceptance owner | Actual Gul/Dolgorae/pinned Codex deterministic native fault suite plus required real-account create/task/approval/result/follow-up/normal-close | TASK-087, TASK-088, TASK-089 | Gul produces reproducible Phase 4 campaign evidence; TASK-088 independently validates and reproduces necessary provider/compatibility cases using the completed Gul artifact | EXT-GU-02: completed product Epic(s), intended Gul v0.1.0 artifact and bounded account evidence; no concurrent active Task prerequisite | Combined Gate C; TASK-089/Gate D |
| GU-06 / Gul | Gul-owned device/24-hour soak, packaging/recovery and product acceptance | Gul device/package/release owner; Dolgorae exact-pair verifier | Preserve own complete QA, final recovery rehearsal, device usability and deterministic soak; normal close and compatibility on fixed Dolgorae candidate | TASK-087, TASK-088, TASK-089 | Distinct device, deterministic fault/soak and real-account results, intended artifacts/checksums and actual client acceptance; independent provenance/applicability review | Gul product Epic may close on complete own acceptance without TASK-088/089; publication waits for Gate D | Phase 4 Epic closeout; combined Gate C and Gate D |

### Contract pins and runtime qualification bindings

| Completion point | Required pins and records | Owner |
| --- | --- | --- |
| H1 / TASK-067 and TASK-068 | Exact contract-source/schema/descriptor revisions and digests; agreed semantics, limits, transports and compatibility; versions used for required pre-contract host/adapter feasibility evidence; named owners, scenarios and accepted external work plans; later runtime-record contents, owners and delivery points. Future final consumer implementation revisions/checksums remain explicitly pending | TASK-067 selects requirements and owners; TASK-068 qualifies and publishes the immutable package |
| H3 / TASK-087 | Final intended-version Dolgorae source and binary/package checksums; actual Aquarium/Podway consumer and adapter implementation revisions and artifact identities used for qualification; applicable host/runtime, configuration, scenarios/results and evidence after cleanup/packaging, all referencing H1's package identity and matrix rows | TASK-087 binds the candidate and accepted TASK-077/086 results with the Aquarium/Podway qualification owners |
| H4 / Gul E16 completion | Final Gul implementation revision and artifact/package checksums; binding to unchanged H3 provider and immutable H1 contract; completed actual-client campaign and Gul-owned acceptance evidence with runtime/configuration/scenario bindings | Gul E16 produces the completed artifact and evidence package |
| TASK-088 and TASK-089 | Independently verify the H3/H4 runtime combination against immutable H1 and accept all mandatory Aquarium/Podway/Gul evidence on final artifacts; consume exact runtime pins, applicability, required gates and final QA | TASK-088 performs substantive independent verification; TASK-089 consumes complete acceptance and provenance for Gate D |

H3/H4 runtime qualification records reference H1's immutable package identity
and matrix row IDs. Completing a planned implementation and binding it to the
unchanged agreed contract does not require rewriting or republishing H1. H1's
pending delivery fields describe obligations; H3/H4 supply the final values in
separate qualification records. Changes to agreed semantics, limits, compatibility
requirements or required host/transport assumptions still require the existing
correction, republication and acknowledgement process. Candidate changes still
require all-consumer impact assessment and affected requalification. Recording
a later binding alone proves neither compatible implementation nor runtime acceptance.

## External implementation and decisions

These labels are prerequisites, not invented consumer roadmap Task IDs. TASK-067
must obtain each owner's accepted scope, permanent work IDs or a verified
existing implementation, evidence route and completion boundary before package
publication. If additional consumer implementation is required, expose it here
and in the dossier; a five-phase diagram cannot hide it.

The [Aquarium request](dolgorae-v014-contract-coordination.md#request-to-aquarium-ext-aq-01)
and [Podway request](dolgorae-v014-contract-coordination.md#request-to-podway-ext-pw-01-and-ext-pw-02)
were sent through Sorage under Master's authorization for contract and
responsibility-boundary review. Both revision-2 technical replies identify no
required consumer source change for the selected individual-task/synthesis scope.
Aquarium confirms its existing agent/Primary/Judge/Master acceptance roles;
Podway confirms the mapping principles and prepared snapshot as a reasonable
candidate. Master accepted the concrete source/mapping selection, Dolgorae-owned
implementation and existing consumer acceptance roles, then directed execution
to continue. The [revision-2 disposition](dolgorae-v014-contract-coordination.md#revision-2-disposition-and-proposed-owner-decision)
records that decision. Phase 1 ownership/selection is settled; required native
independent acceptance and later runtime gates remain separate.

| Prerequisite | Owner and work | Required before | Current state |
| --- | --- | --- | --- |
| EXT-AQ-01 | Dolgorae TASK-077 owns both trusted adapters, installation and protected state; authorized Aquarium-side agents check consumption, with existing Primary/Judge semantic roles and Master final acceptance | Phase 1: selected routes, feasibility and accepted work plan; Phase 3: implementation and actual-path evidence | Phase 1 source/ownership plan and bounded native feasibility accepted. No required Aquarium source change identified. Actual facade/Broker, recovery and interaction qualification is mandatory before H3/final gates. Dormant review backend stays dormant |
| EXT-PW-01 | Podway retains its structural contract and release ownership; Dolgorae adapter owner consumes the selected coherent prepared manifest/catalog/schema snapshot and assesses applicable corrections | Phase 1 contract publication; Phase 3 actual adapter qualification/H3 | Prepared source snapshot selected, with 120 manifest-bound asset digests verified. This is not released 0.2.12 runtime support. V2DFC-017/018 changes require impact assessment; DOLV3-001..009 and final implementation/artifact pins remain due at H3 |
| EXT-PW-02 | Dolgorae TASK-077/086 implement distinct external/managed adapters and receipt composition; authorized consumer workflow agents, Primary/Judge and Master retain consumption/semantic acceptance; Podway retains structural acceptance/progression | Phase 1: semantics/owner/work plan; Phase 3: required supported paths qualified | Individual accepted task and separate synthesis mappings adopted; complete group provenance remains executor-owned, with no group-as-one-Action or child-Action claim. No new Podway implementation identified. Actual supported getters, both mappings and receipt/recovery qualification remain mandatory before H3/final gates |
| EXT-GU-01 | Gul E15 owner: complete E15-T21 and foundation closeout under ADR-0057, with no TASK-069 entry/completion dependency | Phase 2 and TASK-069 entry | Whole-Epic split is adopted in the current planning tree; E15-T21 remains Planned and the completed H2 pin/evidence is pending |
| EXT-GU-02 | Gul E16 owner: complete E16-T1..T16 and product closeout, preserving retired E15-T5..T20 history/scope mapping; build intended final artifact and finish own campaign | Phase 4 and TASK-088 entry | ADR-0057 and E16 membership are adopted; E16-T1..T16 remain Planned, final artifact and H4 evidence are pending |

Do not wait for a Dolgorae release to test an isolated permitted consumer
candidate, or require consumer production activation to establish compatibility.
If a current consumer contract only admits released providers, its owner must
explicitly adopt the isolated qualification route in Phase 1. Installation,
backend selection and publication remain separate grants. An owner refusal or
missing mandatory implementation blocks the dependent Epic; it does not waive
the requirement or make the preferred five-phase path pass.

## Final-artifact acceptance and evidence

TASK-077 and TASK-086 establish implementation-stage consumer facts. TASK-087
owns the post-cleanup intended-version candidate and rechecks/requalifies their
applicability to its actual packaged bytes before the Phase 3 handoff. Phase 3
cannot close with a missing mandatory Aquarium/Podway row. Qualification uses
separately authorized runtime/host/account operations and isolated state.
H3 records the exact Dolgorae candidate and actual Aquarium/Podway consumer and
adapter implementation revisions used for this qualification; it binds these
runtime facts to H1 without replacing H1's contract-source pins.

Gul produces its final implementation revision, exact v0.1.0 artifact/package
checksums and complete own campaign in Phase 4/H4, bound to unchanged H3 and H1.
TASK-088 then independently verifies that the H3/H4 runtime combination satisfies
the immutable H1 contract and checks reproducibility, provenance, coverage and
fresh provider facts against that completed artifact and the unchanged Dolgorae
candidate. It reproduces the required supported integration cases and validates
or revalidates Aquarium/Podway runtime evidence. Acceptance requires more than a report acknowledgement, fixture or unchanged
version string. Gul development Tasks stay closed.

Each row records scenario identity, result and limits, source/contract/consumer
revision, exact binary/package checksums, host/runtime/adapter versions, relevant
configuration, test/harness revision, producer and independent acceptance owner,
and evidence applicability to the final artifact. Keep native deterministic,
real-account, device/soak and Aquarium/Podway host/receipt evidence distinguishable.
For cross-repository retained evidence, use a reviewed bounded non-sensitive
`aquarium.promoted-evidence/v1` package under `evidence/aquarium/` when required.
Raw logs, provider prose, credentials and runtime identities remain local.
Published acceptance facts and reproducible scenario specifications accompany
the handoff; the handoff text itself never counts as runtime proof.

TASK-089 consumes every mandatory accepted row, both products' required gates
and final QA, and exact artifact provenance. Neither release may publish before
Gate D. No rebuild or package/signature/configuration change is introduced in
TASK-089. Only the release workflow's explicitly permitted QA-neutral metadata
exceptions apply. A substantive change triggers an impact assessment across all
three consumers and affected runtime requalification before acceptance/release.
Corrective implementation requires a recorded blocker and explicit scheduled
work; never mark an incomplete Epic complete to retain the preferred sequence.

## Corrected H1 consumer selection

The [release-proof correction](dolgorae-v014-release-proof-correction.md)
requires an independently selected EPIC-023/TASK-091 package. Completed
TASK-068/069 and Gul E15/H2 qualify only original H1
`55ba623fce231192226d29ba718153e18b6644a3`. Preserve that history and all eleven
external source pins. Gul's separately authorized correction unit adopts the
new committed package and returns completed delta H2; TASK-092 independently
accepts that exact selection and the affected foundation checks before
EPIC-020 resumes. TASK-087 consumes this acceptance when producing H3.

The nested JSON release proof changes; public-v2 methods and generated wire,
Machine/facade-v5 outer envelopes and Podway fields remain unchanged. Dolgorae
validates the release proof before projection. Contract fixture checks do not
qualify runtime settlement, host/account paths or consumer product behavior.
