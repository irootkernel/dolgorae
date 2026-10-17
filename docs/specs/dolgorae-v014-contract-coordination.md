# v0.1.4 external contract decisions

Status: TASK-067 contract-confirmation requests sent to Aquarium and Podway
through Sorage under Master's authorization on 2026-10-10. Both replies were
read on 2026-10-11. The technical reviews confirm source identities and
preservation boundaries; they do not adopt implementation plans or qualify
runtime support. Dolgorae returned a concrete provider response through both
existing Sorage exchanges on 2026-10-11, with selected carriers, actual isolated
host observations, closed digest/method decisions and bounded owner questions.
Both revision-2 replies were read on 2026-10-11. Aquarium identified no required
source change and confirmed that the original four-file pin remains valid.
Podway found the individual-task/synthesis design technically compatible and
the prepared source snapshot reasonable as a candidate. Both replies explicitly
withhold formal work-plan/contract-selection approval and runtime acceptance.
Master subsequently approved the concrete source selection and responsibility
split below and directed execution to continue. This adopts the H1 plan;
it does not relabel either technical review as runtime acceptance.
The [H1 contract lock](dolgorae-v014-h1-package.md) binds this decision;
the roadmap records TASK-068 acceptance and EPIC-018 handoff.
The [consumer acceptance specification](dolgorae-consumer-acceptance-v014.md)
owns the mandatory requirements; the [roadmap](../roadmap/README.md) owns status.
Master has authorized Dolgorae to transmit the two contract-confirmation
requests through Sorage. Dolgorae does not modify consumer repositories or
infer acceptance from inspected sources.

The current request asks each team to confirm its existing contract, supported
integration boundary and responsible owners. Dolgorae and its adapters must
respect those consumer contracts. Questions about missing implementation ask
for a gap assessment and proposed ownership. They do not require product
changes, roadmap adoption or implementation in response to this request.
Any necessary consumer-side changes require a separately agreed scope and
owner work plan. Existing compatible implementations need no replacement.
Mandatory H1 and later runtime acceptance requirements remain in force.

## Review dispositions and concrete provider work

Aquarium confirms that its current native Worker/Judge/Primary workflow does
not implement the successor MCP invocation identity. Native result collection
and SDK feedback acknowledgement evidence do not establish initial dispatch-loss
recovery. Aquarium retains semantic judgment, backend selection and final
acceptance; its dormant Dolgorae review route remains dormant.

Podway confirms its existing structural receipt, cursor and progression
boundaries. Its integration dossier is still Candidate, and its prepared v3
catalog does not establish released runtime support. Individual task exports
are compatible with the current receipt scope. A group-as-one-Action receipt
would require separate adoption and is not selected for v0.1.4.

Dolgorae selects the following provider-owned work under the existing roadmap:

| Work | Dolgorae owner and delivery | Consumer boundary |
| --- | --- | --- |
| Protected native hook, stdio transport and durable identity state | TASK-067/068 qualify the contract prototype; TASK-077 implements the production adapter, installation/configuration and state lifecycle | Aquarium selects and qualifies its supported consumer composition; no backend switch is requested |
| Accepted Role/configuration/task/result identity and recovery getters | TASK-076 implements accepted executor facts; TASK-077 exposes the external facade; TASK-086 exposes managed Broker/export mappings | Aquarium consumes supported facts and retains goal/attempt/candidate provenance and semantic acceptance |
| External and managed Podway export composition | TASK-077 and TASK-086 implement separate versioned adapters against supported Podway interfaces | Podway owns structural admission, final-item ordering, receipt lookup, fences and progression |
| Runtime acceptance and artifact binding | TASK-088 independently verifies all mandatory consumer rows; TASK-089 binds the intended release artifacts and final gate | Consumer acceptance owners independently accept their applicable paths; final consumer builds are H3/H4 records |

The provider adapter uses the native-hook binding selected in ADR-041 and the
[successor requirements](dolgorae-v014-successor-requirements.md). Dolgorae owns
its installation instructions and protected invocation state. Consumer-owned
collection, provenance and applicability checks are separate from this adapter.
These provider decisions do not assign new implementation work to Aquarium or
Podway, and do not turn their technical reviews into maintainer acceptance.

The accepted scope selects the exact source pins, separate external/managed
field/digest mappings and individual receipt boundary below. Dolgorae supplies
the isolated candidate and both production adapters; authorized consumer-side
agents check consumption under existing Primary/Judge and Master acceptance
roles. No additional consumer source change or new consumer work item has been
identified for this scope. A later concrete integration gap remains blocking
until its scope, owner and required verification are accepted.

## Inspected sources

### Revision-2 disposition and proposed owner decision

Master accepted this decision after reviewing the two technical replies and
the distinction between H1 contract verification and later runtime integration.
The heading is retained so existing contract-review references still resolve.

The executing Aquarium agent performs authorized consumer checks; its existing
Primary/Judge retain semantic responsibilities and Master retains applicable
final acceptance. Aquarium identified no need to appoint another person or
team. Dolgorae owns both adapters, protected state, authoritative getters and
installation/configuration instructions. No additional Aquarium source work
has been identified. Later actual qualification may establish a concrete gap;
that gap needs its own scope and authorization.

Podway confirmed the proposed digest separation, original-key recovery,
caller-scoped rounds, current idle/released proof, final-items-before-acceptance
ordering and separate explicit progression. An individual accepted task or
synthesis task fits the existing Action boundary. A standalone 50-task group
cannot be one Action under a 32-round ceiling. Full group provenance remains
executor-owned; no Podway fan-out or child-Action work is selected.

The accepted H1 decision retains the exact source pins below, selects Podway's
prepared contract snapshot without claiming released 0.2.12 runtime support,
and adopts the separate external/managed mappings under the existing
individual-Action scope. Dolgorae's TASK-076/077/086 produce accepted executor
facts and adapters. Authorized consumer-side workflow agents perform independent
consumption checks, with existing Primary/Judge and Master acceptance authority;
TASK-088 independently verifies the final combination. No new consumer source
work item is assigned by this decision. Isolated unpublished candidates are
permitted for the required qualification; installation, execution and account
use retain their explicit authorization requirements. The dormant Aquarium
Dolgorae review backend is not reactivated.

The native prototype observations remain subject to independent TASK-067/068
acceptance. Actual facade/Broker, Podway receipt, Controller interaction and
final-artifact qualification remain mandatory in Phase 3/H3 and final gates.
V2DFC-017/018 corrections must be assessed for impact before freezing selected
Podway bytes; any changed agreed contract requires reinspection and affected
requalification. The original source snapshot is not a future compatibility
promise. The selected committed manifest validates against its closed schema
and binds 120 exact asset digests, including the adapter-v3 catalog and Store-v15
asset. DOLV3-001 through DOLV3-009 remain actual-artifact qualification gates;
this byte-coherence check establishes neither their runtime results nor release
readiness. If V2DFC-017/018 changes a selected semantic contract, reassess the
affected mappings and revise the lock before admitting the changed candidate.

The following committed bytes were inspected on 2026-10-10. These are review
inputs selected for the H1 plan. Release and runtime qualification pins remain
pending.
SHA-256 below hashes each complete file's raw bytes. Source changes require
reinspection and owner confirmation before a changed H1 selection.

| Consumer | Commit | Repository state at inspection |
| --- | --- | --- |
| Aquarium | `1e1e9293ade2f884757ee51a675d130e1acdaf04` | Clean |
| Podway | `4101a014a187d175e6c7cd53df9b671d613ba625` | Other working-tree changes; all files listed below match this commit |
| Gul | `04ef1e577013439b9c691cd50410d665dd0c97b0` | Clean |

| Consumer / repository-relative path | SHA-256 |
| --- | --- |
| Aquarium: `docs/specs/agent-orchestration.md` | `f1665f913ee7b8f833ee84cdc93668c1b86f60a9bd093a0551d5270c3c5087b9` |
| Aquarium: `plugins/aquarium/references/agent-orchestration.md` | `61c40f36c17942cf750990ae300785263e891409099b3b6d44d66ef8851337cd` |
| Aquarium: `plugins/aquarium/references/native-agent-mappings.md` | `c5c3244b6db4e855c1ef149ae362a725c7ef5cc750e828c7aa78bea8a8354cf7` |
| Aquarium: `plugins/aquarium/references/dolgorae-review-contract.md` | `5c061f0e8d9b026f358adfa9efccc7f17abdb1cea5f3e10f50ebb6f8f2a8df9f` |
| Podway: `docs/specs/interfaces/supervised-delegation.md` | `7d5bb0a770739c50d9d623aed437f2870cfb5656432548fe7615dd478cef093b` |
| Podway: `docs/specs/interfaces/automation-client-contract.md` | `9642d9205e90ddd51fb641cf64ff6108652b8968256a93c985d2229206545f29` |
| Podway: `release/dolgorae-adapter-contract-v3.json` | `0d2af732e6f61f3e5c86d7b5a2223977f312fba77b75112fa709e5b91e7d633c` |
| Podway: `contracts/contract-manifest-v2.json` | `35d5938a97163e1f4dbcab1c53c0a3711552e4313ea4c94165e94eb6ad06462e` |
| Podway: `docs/todo/TODO-podway-supervised-delegation-integration.md` | `a062e198171eee75c1c134f065a7b3b8a59218e5e6701047dca382c446eba76a` |
| Gul: `docs/roadmap.md` | `b1a1ccf31b2d142fb5dcd3dabe5499d6fdddd23d3a70df992635bdc4fb0b6041` |
| Gul: `docs/todo/GUL-REBUILD.md` | `15f448e39ffa77402fb70fadcde54abf23826b57231fc2eb1de5e7160f10ede0` |

## Request to Aquarium: EXT-AQ-01

Please review the existing Aquarium contracts and confirm the supported
integration boundaries and responsible owners for Dolgorae v0.1.4. Identify
any gaps and propose who should address them. This informs the Phase 1
prerequisite decisions before qualified H1 publication for Gul. It does not
request production activation, a backend switch or reactivation of the dormant
Dolgorae review route.

The required rows are AQ-01 through AQ-06 in the consumer acceptance
specification. Aquarium Primary/Judge retains semantic judgment and final
acceptance. Dolgorae supplies operational facts. Codex app-server remains the
executor, including when Claude hosts the external Primary. Existing native
paths and reusable-facade boundaries must remain supported.

Please confirm the existing facts below, identify gaps and state objections:

1. Name the accountable host/integration maintainer, installation/configuration
   owner, protected-state owner and qualification/acceptance owner. Identify
   existing permanent adopted work IDs, or an exact committed implementation
   with evidence covering the requested behavior. If integration is missing,
   identify its necessary scope and accepted owner work plan, or an explicitly
   unaccepted proposal for the accountable owner. No implementation execution
   or consumer repository mutation is requested here.
2. Select supported external-Primary stdio MCP routes for both Codex and Claude.
   Initial isolated host candidates are Codex CLI 0.160.0 and Claude Code
   2.1.295. Confirm each exact host product/version and trusted adapter source
   revision/version, or propose an explicit replacement for coordinated approval.
   Aquarium's native mappings alone do not establish successor MCP support.
3. Identify the trusted component that creates a logical invocation UUIDv7
   before first delivery, outside model arguments, and durably retains it with
   the accepted input digest and receipt/lookup linkage. Review the ADR-041
   successor: Codex native call/thread metadata selects a precommitted binding;
   Claude hook-rewritten input carries a protected opaque binding. The historical
   ADR-031 carrier and SR1 disposition remain unchanged. Confirm schema, write
   ordering, retention, ownership, replay and authoritative lookup scope.
   Connection/process/JSON-RPC IDs and equal text cannot replace the UUIDv7.
4. Define restart/reconnect recovery and failure behavior. Missing/corrupted
   identity state or failed lookup must refuse before effects or preserve
   uncertainty. An ambiguous delivery must not mint a new key or redispatch.
   State the protected credential carrier without including credentials.
5. Accept the pre-contract matrix on both actual hosts: initial invocation,
   exact replay, changed-input conflict, concurrent replay and distinct calls,
   reconnect, MCP process restart, and loss before acceptance, after acceptance
   before acknowledgement, and after receipt commit. Include corrupted-state
   and lookup-failure negatives. Specify the executable adapter/receipt path,
   bounded isolated account-free harness, evidence producer and independent
   acceptance owner. Static fixtures or typed unsupported do not pass support.
6. Review the Phase 3 obligations for actual facade/Broker identity collection, physical
   dispatch-loss recovery, full results, conservative rounds, current
   idle/released proof and Controller-owned interactions. Preserve exact
   goal/attempt/candidate applicability and Worker/Judge boundaries. Qualify
   the isolated unpublished candidate without requiring production activation.
7. Select the committed contract sources and raw-file digests from the table
   above, or return their exact replacements. Define the later H3 runtime
   record: consumer/adapter source revisions, artifact checksums, host/runtime
   versions, configuration, scenario results, evidence applicability and owners,
   bound to the eventual H1 package and AQ row IDs.

Phase 1 needs accepted route/ownership/work plans and successful pre-contract
host feasibility. Phase 3 needs completed implementation and actual-path
qualification. Future H3 implementation/artifact pins may remain pending at H1;
accepted ownership and feasible mandatory routes may not. A missing route or
owner acceptance blocks freeze; it does not reduce mandatory release scope.

Please return this contract review through Sorage, or through Master:

```text
Prerequisite: EXT-AQ-01
Contract review: confirmed | gaps | objections (enumerate unresolved items)
Accepting owner and authority:
Reviewed scope: AQ-01..AQ-06; respond to questions 1..7 explicitly
Selected contract source commits, paths and SHA-256 values:
Codex host / trusted adapter version and source:
Claude host / trusted adapter version and source:
Invocation carrier / protected state schema / receipt and lookup surface:
Maintainer / installation owner / state owner / acceptance owner:
Existing work IDs or verified implementation/evidence; proposed gap ownership:
Phase 1 feasibility harness, evidence owner and completion boundary:
Phase 3 implementation, qualification work and H3 delivery boundary:
Later runtime-record contents and owners:
Open objections and their blocking dependencies:
```

## Request to Podway: EXT-PW-01 and EXT-PW-02

Please confirm Podway's current contract selection and review the real-executor
integration boundary for Dolgorae v0.1.4. Identify gaps and propose their owners.
This informs Phase 1 prerequisites for H1; it does not request deployment, production
installation, Podway-owned model execution or a change to Direct operation.

The required rows are PW-01 through PW-06. Podway retains its single outer
cursor, structural receipt, fences and progression. Dolgorae exports executor
facts; completion or receipt export cannot advance Podway. Aquarium
Primary/Judge retains semantic acceptance. Work IDs must come from the
consumer's adopted roadmap. The EXT labels identify prerequisites only.

For EXT-PW-01:

1. Select a coherent committed contract-source/schema/manifest/catalog pin.
   At the inspected commit, the adapter v3 catalog is `prepared-not-released`,
   references Store v15, and the manifest declares product 0.2.12; V2DFC-018
   remains Planned. Confirm the applicable remediated corrections and their
   accepted completion boundary. Do not infer readiness from a version label.
2. Return exact commits, raw-byte SHA-256 values, schema IDs and paths for
   output v4, error v2, manifest v2, adapter v3, supervised-delegation schemas
   and store contract. Include the coherent catalog/manifest relationship and
   DOLV3-001 through DOLV3-009 acceptance requirements. These must be reproducible
   before H1 selection; current installed daemon tooling is a separate runtime.
3. Name contract/release and adapter maintainers and their acceptance owners.
   Adopt qualification against an isolated unpublished Dolgorae candidate if
   the existing consumer admission rule otherwise allows only released
   providers. Later H3 supplies actual implementation revisions/artifact pins.

For EXT-PW-02, jointly with the Aquarium/trusted host integration owner:

1. Identify existing permanent work IDs or a verified committed implementation.
   If integration is missing, identify the necessary scope and accepted owner
   work plan, or an explicitly unaccepted proposal for its accountable owner.
   No implementation execution or consumer repository mutation is requested.
   The inspected real-executor integration document
   is Candidate with no owning Epic or assigned release; it is not an accepted
   implementation plan. Keep external-Primary facade and managed-Primary Broker
   mappings separate, with explicit adapter IDs/versions and authorized reads.
2. Accept exact byte-domain mappings for Procedure Role content, admitted
   configuration, accepted task, complete Primary-accepted result, worker,
   logical binding, adapter, supervisor and caller scope. State encoding,
   canonicalization, fields/exclusions, digest domain/version, producer,
   permitted getter, retained acceptance linkage and response-loss lookup.
   A locally prepared request is not executor acceptance; private-store reads
   and field-name similarity cannot establish the mapping.
3. Preserve `max_rounds` 1..32, count accepted/uncertain assignments
   conservatively and exclude replay/collection. Map exact idle/released
   disposition to current workload/membership proof. Define feedback, retained
   reuse, required release, cancellation, exhaustion, late results and
   restart/loss recovery. Unknown work cannot be redispatched.
4. Identify executable receipt composition and final-item-before-acceptance
   ordering, exact replay, stale-current-state refusal and fresh-attempt
   requirements after correction. Preserve independent Direct progress.
5. State group/synthesis receipt scope explicitly. Current Podway contracts do
   not promise fan-out, child Actions or one group receipt. Preserve individual
   exports and full group provenance; do not fabricate group support. Any
   proposed group-as-one-Action mapping needs its owner's separate adoption.
6. Review Phase 3 real-path implementation and qualification needs, including both
   external and managed mappings, loss around acceptance, restart, current
   disposition and post-acceptance correction cases. Specify producer and
   independent acceptance owners, evidence route and H3 delivery boundary.

Phase 1 requires coherent contract selection and accepted semantics, owners
and implementation plans. Phase 3 requires actual supported paths and final
runtime records. H3 records bind consumer/adapter source and artifact checksums,
host/runtime/configuration and scenario evidence to H1 and PW row IDs. Future
H3 artifact values may remain pending at H1; these Phase 1 decisions may not.

Please return this contract review through Sorage, or through Master:

```text
Prerequisites: EXT-PW-01; EXT-PW-02
Contract review per prerequisite: confirmed | gaps | objections
Accepting owner and authority per prerequisite:
Selected source commit / paths / schema versions / raw-file SHA-256:
Coherent catalog-manifest-store relationship and correction work IDs:
Accepted external mapping adapter ID/version and digest domains:
Accepted managed mapping adapter ID/version and digest domains:
Permitted receipt composition, getters and authoritative lookup:
Round / disposition / post-acceptance correction rules:
Group and synthesis receipt scope decision:
Maintainers / acceptance owners / existing work IDs / proposed gap ownership:
Isolated unpublished-candidate admission decision:
Phase 1 completion and Phase 3 qualification/H3 delivery boundaries:
Later runtime-record contents, evidence producer and acceptance owner:
Open objections and their blocking dependencies:
```

## Resumption and Gul boundary

After the owner replies return, TASK-067 revalidates the dependency DAG
and source pins, reconciles the trusted host boundary with ADR-031, completes
its requirements and ownership decisions, and verifies its exact candidate.
TASK-068 then publishes checked successors only after both host routes pass
the required native feasibility matrix. Objections remain blockers until their
owners accept a correction; request text is never evidence of acceptance.
Contract confirmation establishes only the facts explicitly confirmed. A gap
assessment or proposed owner alone does not satisfy the separate H1 requirement
for accepted ownership and necessary implementation work plans.

Gul receives qualified H1 after EPIC-018 validation and closeout. Gul E15/H2
is not a prerequisite for EPIC-018. EPIC-019 remains unopened until Gul
completes E15, including E15-T21 and foundation closeout, and returns accepted
H2 bound to H1. Preserve E15-T1..T4 history and the retired E15-T5..T20 to
E16-T1..T16 mapping. These prerequisite requests do not ask Gul to start E15
against an unqualified package.

## Corrected release-proof package coordination

EPIC-023/TASK-091 selects the
[release-proof erratum](dolgorae-v014-release-proof-correction.md) adopted at
`664c6648ec5c69f78733a58d5c46e88b335459ba`. Preserve the eleven historical
external source pins above and native feasibility inputs without repinning to
current consumer checkouts. The original H1
`55ba623fce231192226d29ba718153e18b6644a3` and its H2/Gate A acceptance retain
their historical scope.

The corrected lock identifies its own producer and original ancestry, and
separates that accepted historical handoff from the pending Gul delta H2.
Gul's separately authorized correction unit owns package mirror/pin adoption
and affected foundation checks. TASK-092 independently accepts the completed
delta H2 before EPIC-020 resumes. Unchanged outer protocols do not imply
acceptance of changed nested proof bytes. No current Gul package acceptance,
consumer product qualification or broker transmission follows from publication
of the corrected contract archive.
