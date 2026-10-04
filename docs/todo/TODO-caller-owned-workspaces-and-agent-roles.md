# Caller-Owned Workspaces and Explicit Agent Roles

Consumer Epic: [EPIC-017](../roadmap/README.md#epic-017-caller-owned-workspaces-and-explicit-agent-roles)

This dossier integrates the approved successor design. The roadmap owns IDs,
order, dependencies and lifecycle. The accepted
[caller-workspace contract](../specs/caller-workspaces-v1.md) now owns the
successor requirements, version map and implementation acceptance cases.
This dossier retains execution coordination until closeout; it is not evidence
that the new behavior is available.

## Outcome and Ownership

Dolgorae coordinates durable Runs in a workspace supplied by the caller. It
does not prepare copies or Git worktrees. A caller can explicitly prepare a
separate workspace and start one writer plus readers there. Ordinary starts
default to writable; review starts explicitly select read-only. Reusable Role
instructions and the current task remain separate.

| Authority | Delivery responsibility |
| --- | --- |
| [Specification](../specs/README.md) | Startup/access, instruction sources, review assurance and compatibility |
| [Architecture](../architecture/README.md) | Workspace ownership, process/thread cwd, instruction snapshots and recovery |
| [ADRs](../architecture-decision-records/README.md) | Successor decisions for source-copy retirement and explicit Role loading |
| [Protocol artifacts](../protocol/) | Versioned inputs, persisted identities, outputs, capabilities and errors |
| [Roadmap](../roadmap/README.md) | Six sequential member Tasks and handoffs to unfinished consumer work |

TASK-061 promotes the successor contracts among these owners before source changes.
TASK-062 through TASK-065 implement their accepted contracts; TASK-066 owns
migration, documentation, integrated acceptance and activation of the complete
successor capability. Never advertise an incomplete guarantee.

## Workspace and Access Contract

- Keep the existing canonical workspace identity, Git/non-Git initialization,
  Controller authorization and cross-Profile writer coordination. A separately
  prepared Git worktree is a distinct workspace; shared Git metadata and
  external editors remain outside the workspace writer guarantee.
- Modern starts accept `--access read-only|writable`. Omission means writable
  at this interface, not a hidden low-level service default. Persist the
  resolved selection. A successful writable start has acquired the writer
  guard and verified a writable dedicated runtime before returning success,
  including when no task Turn has yet been submitted.
- Writer contention rejects startup with the existing typed writer-conflict
  behavior before provider effects. Do not silently create a copy, downgrade
  access or allocate a second writer. Failed or ambiguous activation follows
  existing durable failure/unknown rules and retains any unproven writer guard.
- Modern read-only starts use the dedicated lane so the Controller can later
  request `run acquire-write`; `run release-write` returns authority only
  after idle/no-pending-interaction and process/read-policy proof. Retain fixed
  read-only legacy lanes as explicitly non-promotable. Unsupported same-Run
  transitions fail with a typed result before mutation; never emulate success
  through a new thread or an unrequested replacement Run.
- Runtime access is current effective policy, independently observable from
  startup selection. Acquire/release do not alter accepted Role or task bytes.
  A Run can remain read-only after release and reacquire when safe. Readers
  may coexist with a writer and see intermediate changes.
- No Role, prompt, peer mail or Profile choice grants writer or Controller
  authority. Existing review and broker-specific authority restrictions still
  apply; a review Run cannot promote itself.

## Explicit Common and Role Instructions

- The common instruction source is `~/.dolgorae/home/AGENTS.md`; one selected
  Role source is `~/.dolgorae/roles/<role-name>/AGENTS.md`. The directory name
  is the Role identity, using the existing Role-name constraint. Markdown
  supplies instruction text; Profile/model/access and policy display metadata
  remain execution configuration, not Role-file directives.
- Add `--role <name>` to modern startup and standalone review. Ordinary omission selects `general`;
  standalone review selects `reviewer` unless its checked request binds
  another Role. Brokered Specialists and Engagements bind their selected Role
  explicitly through the successor configuration/policy contract.
- Ship common, general and reviewer templates. Only an explicit initialization
  or migration operation may create missing sources; never create or overwrite
  them during start, discovery or recovery. Extend the existing `init`
  operation with no-overwrite template installation. Existing user files take
  precedence as sources and must pass admission; invalid files are not repaired
  silently. Missing selected sources fail source-resolving admission before
  new Run allocation; accepted policy/Run snapshots do not require live files.
- Load sources explicitly through bounded, descriptor-relative, no-follow reads
  with the existing owner/permission checks. Bound the combined common/Role payload to the existing 65,536-byte
  instruction-input ceiling; retain separate core, dynamic-context and task
  bounds. Capture admitted bytes and SHA-256 identities, including common and
  selected Role, before execution. Retries/resume use accepted captures, not
  newly edited source files; installed policies and session snapshots remain
  independent of subsequent source edits. Session bootstrap captures common
  instructions once for its child Runs; Role bodies come from the accepted
  policy. Standalone starts capture both selected sources on admission.
- Compose the core governance prefix, common instructions and selected Role
  as bounded developer instructions; retain dynamic access context separately.
  Submit the current task as the user request. This ordering communicates
  responsibilities; permission enforcement remains in the controller, worker
  and runtime rather than in prose. Conflicting Role/task wording cannot remove
  those enforced boundaries.
- The home directory is an instruction root, not the command cwd. Thread and
  tool execution use the selected canonical workspace. Keep Profile CODEX_HOME,
  credentials, model configuration and supported tools separate.
- Permit native Profile AGENTS as user guidance outside the captured common/Role
  snapshot. The managed developer prefix takes precedence over Profile guidance
  and task user input. Document that Profile edits can affect later runtime
  input without changing the captured Role or granting authority.
- Suppress automatic AGENTS loading from project and ancestor paths for the
  new managed instruction contract. Prove the supported Codex
  setting and actual thread behavior with conflicting-file probes; a different
  shell startup directory or prompt statement is not proof. If the pinned
  runtime cannot satisfy this, reject the successor start before allocation.
  Explicitly supplied project guidance may be task context, never an implicit
  Role replacement or authority grant.

## Copy-Free Review and Retirement

- New paths never materialize source trees, create worktrees, or provide
  `copy`/`isolated_write` access. This includes one-shot review, target
  capture, reusable Engagement and brokered launch paths. Keep durable
  instruction/configuration/result snapshots and bounded hashes/target metadata;
  the retirement concerns source copies, not durable control state.
- Workspace review reads the selected current tree, including eligible
  untracked content; staged review reads the index diff, and dirty review reads
  the existing dirty-scope contract. Preserve existing scope semantics,
  exclusions, bounded paths/bytes and secret-safe output rules. Non-Git
  workspaces support workspace scope only.
- HEAD, commit and range resolve once to full Git object identities before
  review dispatch. Bind both range endpoints and the existing range algorithm.
  Read required historical content and diffs through Git without checkout.
  Current dirty files must not replace pinned historical content; unavailable
  pinned objects fail rather than falling back to the current workspace.
- Candidate file and diff reads, including full-file context, must use a checked
  target-reading path bound to admitted index entries for staged scope and
  pinned Git objects for HEAD/commit/range. For these scopes, identify
  live-workspace reads as separate context; they cannot supply candidate
  evidence. Instructions alone
  do not establish this boundary. Verify the tool-reading boundary before
  dispatch and return a typed unsupported result if it cannot be maintained.
  Results with violated or unverifiable candidate provenance are not accepted.
- Workspace/index/dirty targets are mutable. Capture bounded fingerprints
  before dispatch and recheck before result acceptance. Known changes produce
  a typed drift/inconclusive outcome, never a clean accepted review. Report
  this as sampled drift detection, not immutable-source or snapshot isolation;
  undetected concurrent edits and external writers remain material limits.
- Keep fresh explicitly read-only Reviewer Runs, admitted Role/task separation,
  bounded findings, stable request identities and controller-bound recovery.
  Read-only protects source mutation; it does not claim hostile-process
  containment or unrestricted repository secrecy.
- At the integrated cutover, new legacy capture and isolated-write requests
  receive a typed retirement response before effects. Never reinterpret them
  as writes in the caller workspace. Already accepted requests retain their
  bytes, identities, cached receipts and result meaning.
- Retain historical capture inspection, settlement and cleanup only under the
  original proven ownership rules. Recovery must not recreate a missing source
  copy/worktree, redispatch unknown accepted work, delete unproven active state,
  or remove caller-owned roots. Accepted old Runs can finish using existing
  resources; a new launch from a legacy isolated policy requires migration.

## Migration and Downstream Handoffs

- Freeze TASK-053 public-v1 source/descriptor and TASK-056 consumer behavior.
  Legacy public-v1 startup retains its explicit lane and access semantics.
  New access/Role inputs use versioned Machine/private DTO and policy contracts;
  a new public Protobuf generation is outside this Epic.
- Explicit migration converts legacy common/project JSON Role sources into
  named global Markdown sources and successor policy inputs. Show destination
  and name collisions and require explicit selection; never choose a winner,
  merge Role bodies, overwrite sources, or rewrite accepted policy/Run snapshots.
  New source resolution has no JSON/project fallback or parent search.
- Keep persisted readers for existing Runs, policies, review manifests and
  recovery records. Version successor writes and safe projections; do not
  expose source paths, instruction bodies or controller credentials in observer
  output. Update help, examples, operator guidance and use-dolgorae together.
- Handoff to EPIC-009/TASK-027 covers captured Role selectors and activation in
  the caller workspace. EPIC-010/TASK-028 through TASK-031 consume safe access/
  review projections, deletion boundaries and governance composition without
  duplicating source loading. EPIC-011/TASK-032 through TASK-034 extends
  deterministic, crash and separately authorized live conformance.
- Those Epics receive canonical outcomes from EPIC-017 and keep their own
  roadmap gates; they do not consume or delay this dossier's closeout. Preserve
  their IDs and status and all completed history.

## Acceptance and Execution Boundaries

| Area | Required evidence |
| --- | --- |
| Sources | Correct captured bytes/digests; missing, unsafe, over-bound and raced inputs rejected; project/ancestor AGENTS suppressed; separate Profile user guidance documented; explicit init never overwrites |
| Startup/access | Default writable succeeds with actual authority; busy writer fails; read-only denial; authorized acquire/release, restart and idempotency across Profiles |
| Ownership | Every new facade creates no source copy/worktree; externally prepared roots survive cleanup, close, export and delete |
| Review | All six scopes; dirty/index drift; moved refs and historical content; Git-object loss; no clean result after detected drift; no checkout |
| Review sources | Index, commit and worktree differ before dispatch; deliberate native-tool worktree reads cannot become candidate evidence; checked target reads cover full-file context; unsupported or violated boundaries cause typed rejection even with unchanged refs/fingerprints |
| Compatibility | Old records/receipts recover without reinterpretation; JSON migration collisions handled explicitly; unchanged frozen public-v1 clients |
| Failure safety | Named fault barriers at instruction admission, startup, lease/policy transition, review dispatch/acceptance and cleanup; ambiguous effects retain guards |

Use Rust tests for product semantics and focused black-box executable tests for
adapter behavior. Run focused checks first, then the complete
`make PYTHON_BIN=.venv/bin/python test` gate during implementation. Each Task
requires independent read-only review under the roadmap completion gate.
Adversarial coverage includes competing controllers/Profiles, source replacement,
conflicting instructions, moved refs and crashes at each ownership/effect edge;
OS/runtime guarantees need empirical proof in addition to deterministic fakes.

Pinned live Codex discovery, effective-policy and review probes require separate
explicit authorization and documented opt-in. Design QA evaluates the dossier,
not runtime behavior. No initialization, Profile/server operation, live account,
installation, Git staging/commit/push or release is authorized by this design.

## Execution Readiness and Decision Boundary

The roadmap's completed TASK-062 records source admission, instruction captures,
policy/session storage, and explicit initialization. Its completion commit
keeps modern activation gated on runtime qualification. It does not establish
project/ancestor instruction suppression or working caller-workspace startup.
TASK-063 now connects that native mechanism to internal startup, access
transitions and recovery. Its bounded native pipeline passed with normal
cleanup. A later original-manifest retry correction has focused checkpoint
coverage and a passing complete repository gate; it does not replace the earlier
native execution evidence. Independent review and exact final candidate approval
remain required. A typed unsupported response alone does not complete TASK-063.

| Requirement | Current production boundary | Verification and remaining work |
| --- | --- | --- |
| CW-SOURCE, CW-CAPTURE, CW-INIT, CW-SNAPSHOT | Internal startup binds accepted captures to the Dedicated Run; retry reads original intent before live sources or registry | Native pipeline passed original receipt replay after source removal; offline checks cover source admission and immutable capture identity |
| CW-AMBIENT | Executable admission uses the qualified pin; owned Dedicated process flags and merged configuration confirm project-document suppression | Native mechanism probe passed conflicting project/ancestor input and positive controls; native pipeline passed qualified launch/configuration proof; permitted Profile guidance remains outside the managed snapshot |
| Startup defaults and selection | Strict generation-2 reservations retain original request, captures, Profile binding, observed defaults and capability snapshot; immutable receipt retains original successful startup | Native pipeline passed successful receipt replay; later real-store regressions cover interrupted manifest publication, bootstrap and Writer reservation without rebuilding the original manifest; selected review must assess this correction against the retained native evidence |
| CW-WRITER | Internal startup reserves canonical Writer authority before caller Dedicated effects and consumes current qualified Worker proof before commit | Native pipeline passed cross-Profile exclusion and reader startup; offline checks cover rejection and interrupted reservation paths |
| CW-TRANSITION | Controller-authorized acquisition/release verifies same-Thread native settings, process/socket identities, durable policy and workload absence under the Writer transaction | Native pipeline passed same-Thread transitions and guarded live recovery; offline and kernel checks cover ambiguous outcomes and recorded descendants |
| Manifest v3 and recovery | Provisional identities restore the original Thread as reader; explicit recovery/resume can retire a guard with current Reader proof; legacy adapters reject reinterpretation | Native pipeline passed reader recovery after owned Worker loss without automatic Writer acquisition; offline checks cover durable refusal and legacy boundaries |
| Native binding loss | Name storage and empty-history readback qualified retained turnless history on the pinned fixture; absent original identity fails without replacement or task replay | Offline checks cover unavailable-binding refusal and retained guards; no replacement or task replay capability is claimed |
| CW-NOCOPY and CW-SCOPE | TASK-064 and TASK-065 are planned | Remove new source materialization and verify checked index/object reads through every facade |
| Public activation | TASK-066 is planned | Integrate adapters, migration, help, installed skill, complete repository gate, and independent completion review |

Runtime diagnosis must produce a decision for each mechanism rather than a
sequence of account retries. Source/protocol inspection and offline tests come
first. The bounded native fixture then supplies exact tool calls through a
loopback Responses endpoint and inspects the actual request input and native
returns. It uses no account or actual model provider. Access observations and
instruction discovery observations are independent. A Profile marker records
permitted user guidance; a project or ancestor marker rejects the required
suppression mechanism even if the access sequence passes. Absence of project
and ancestor markers without a positive ambient control remains unqualified.

An explicitly authorized v3 fixture campaign has at most six native Turns, two
threads, four App Server processes, one loopback endpoint, and twelve Responses
POSTs. A separate positive-control thread first proves ambient instruction
loading with discovery enabled inside the disposable HOME's Git root. The
controlled Thread stores its name and must survive a restart with empty history
before its first Turn. Its pre-Turn resume supplies the frozen launch inputs
explicitly; native identity storage does not establish settings persistence or
authorize automatic writer reacquisition. Writable approval is `on-request`; read-only approval is
`never`, matching the coordinator. Complete loaded-Thread observations around
an idle-status read check the child guard before settings updates. The access
sequence is writable, read-only, writable, read-only, then
restart/resume of the original binding with the frozen launch inputs and an
explicit read-only policy, followed by a final read-only attempt. Recovery does
not rely on native persistence of the last settings update. Every
step requires readback and actual file-effect or denial evidence. The campaign
stops on its first access failure, performs cleanup, and never falls back to an
account, replacement thread, or automatic retry. Actual model/provider
verification, if later required, needs its own finite scope and authorization.

Classify the result before further runtime execution:

| Result | Required next action |
| --- | --- |
| Probe contract or transport mismatch | Correct the probe using pinned protocol evidence and offline regression tests; do not infer a product capability from the failure |
| Dolgorae path missing or incorrect | Implement the owning startup, transition, persistence, or recovery path and its focused Rust/executable tests |
| Mechanism supported and empirically observed | Bind that evidence to the exact runtime/configuration and implement production verification; fixture success alone does not activate the capability |
| Required mechanism unsupported by the selected runtime | Keep typed pre-allocation rejection and propose a supported runtime or an explicit requirement change; do not weaken the contract silently |
| Original native thread unavailable after restart | Implement the permitted unavailable-binding recovery with the original Run/receipt and conservative writer guard; this limit alone does not require replacement or replay |

This map defines the remaining work. Runtime observations remain local evidence
unless a reviewed retained package is separately required and approved. It does
not change Task order, commit authority, or completion status.

The corrected bounded v3 campaign observed all six requested Turns and twelve
loopback Responses POSTs. It observed two writable file effects, three read-only
denials, three same-Thread settings updates, original empty-history binding
across the first restart, and original completed history with explicit
read-only recovery across the final restart. The positive control loaded
Profile, project and ancestor instructions; the controlled input retained
Profile guidance and excluded project and ancestor guidance. This qualifies
the observed native mechanism on the pinned executable. Production activation
still requires accepted-input binding, process quiescence and matching durable
policy proof; the fixture did not mutate Profile capabilities.

### Native Caller Pipeline Acceptance

`make test-native-caller-pipeline` requires separate explicit authorization and
`DOLGORAE_RUN_NATIVE_CALLER_PIPELINE=1`. It uses the exact qualified Codex
executable, disposable Dolgorae and Codex homes, two Profiles and a loopback endpoint that rejects
all provider requests. The driver uses the internal caller service and actual
Worker transport. It checks writable and reader startup, cross-Profile Writer
exclusion, same-Thread access transitions, original receipt replay after source
removal, live blocked-transaction recovery, and reader recovery of a blocked
Writer guard after one verified owned Worker is killed. The guard injections
modify only the fixture's canonical Writer record. The
campaign permits up to seven App Server processes: five for the checks and two
for closing failed fixture Runs. It permits two Threads, zero Turns and zero
provider POSTs. It stops at the first failure without retrying.

Cleanup closes the fixture Runs and stops its Profile servers. A successful
campaign removes its disposable home; a failed campaign retains its owned
fixture and private diagnostic files for inspection and bounded recovery.
Machine failures retain their named check and details in bounded mode-0600
fixture files. If preparation failed before any Profile binding or lifecycle
root was created, cleanup records that no Server was launched without repeating
preparation. Missing or malformed binding history does not establish that state.
Native `HOME`, login identity and `TMPDIR` come from the actual account and OS,
as required by the Profile launch contract. Native authentication uses only the
empty fixture's file-backed `CODEX_HOME`, with account auth disabled. No model
Turn or tool command is submitted. If the driver times out, Python stops the
driver and loopback endpoint; separate managed processes may remain. The owned
fixture is retained and requires verified owner-bound recovery before removal.
`test_native_caller_pipeline.py` checks admission, credential isolation, rejected
provider activity, single execution and cleanup offline in the default gate.
The opt-in native campaign is excluded from that gate. The default unit gate
also checks private failure diagnostics and cleanup of rejected preparation.
The Worker kernel fixture additionally admits a verified startup descendant in
a separate group/session, tracks every nonleader ancestor through reparenting,
and checks strict refusal while a recorded child survives. It tests interrupted
startup before and after child observation, failed atomic runtime publication,
stale evidence and locator writes, and generation absence after completed
startup. These isolated checks do not establish native campaign acceptance.

The first full-pipeline attempt stopped before Run allocation with
`COMPATIBILITY_REJECTED`. Its error details were not retained, so the specific
first rejected check remains unknown. Offline inspection found that the fixture's
explicit file-only credential policy was absent from the closed configuration
manifest. The policy is now classified as process-static and covered by a launch
contract test. A later bounded native pipeline passed all requested checks and
normal cleanup with two Runs, two Threads, zero Turns and zero provider requests.
It observed cross-Profile Writer exclusion, same-Thread access transitions,
original receipt replay after source removal, guarded live recovery and reader
recovery after owned Worker loss. That execution preceded the original-manifest
retry correction. The correction preserves launch and qualification behavior
and has focused checkpoint regressions and a passing complete repository gate;
independent review must assess whether the combined evidence covers the final
candidate. Public adapter activation remains TASK-066.

### Profile Guidance Compatibility

Codex `rust-v0.159.3` resolves to source commit
`01fc69f4026735edfdf6789820549727a4867b11`. In that revision,
[App Server construction](https://github.com/openai/codex/blob/01fc69f4026735edfdf6789820549727a4867b11/codex-rs/app-server/src/message_processor.rs)
always supplies a `CodexHomeUserInstructionsProvider` rooted at `CODEX_HOME`.
The [provider](https://github.com/openai/codex/blob/01fc69f4026735edfdf6789820549727a4867b11/codex-rs/codex-home/src/instructions/mod.rs)
reads `AGENTS.override.md` or `AGENTS.md` independently of the project instruction
byte limit. The [instruction manager](https://github.com/openai/codex/blob/01fc69f4026735edfdf6789820549727a4867b11/codex-rs/core/src/agents_md_manager.rs)
combines its contribution with project and thread instructions. A thread
provider supplies another contribution; an empty thread contribution does not
remove the global one.

The accepted contract permits this native Profile contribution as user input
outside the captured common/Role snapshot. Disabling the global provider is
not an activation prerequisite. A project instruction byte limit controls a
different source and must still be qualified against project and ancestor
files. This distinction does not qualify startup or access transitions, nor
does it establish complete Dolgorae-pipeline acceptance.
Remaining implementation and native evidence belong to TASK-063 within the
Dolgorae repository; no external Codex source change is required by CW-AMBIENT.

Discovery acceptance cases must preserve default legacy loading and admit
explicit common/Role bodies when project discovery is suppressed. The controlled
thread must exclude conflicting project and ancestor bodies and separately
observe permitted Profile guidance. Skills, plugins, apps, MCP, caller cwd, and
the canonical Profile home must remain available. Restart and native-child
behavior must retain the selected instruction policy before Dolgorae qualifies
the mechanism for modern startup activation.

At closeout, promote durable behavior, architecture, protocol and operator
outcomes to their owners, update unfinished handoffs, replace EPIC-017's Detailed
SOT with Canonical Outcomes, and remove this dossier/index entry only after the
approved last-consumer closeout. Reassess the launch-preparation candidate after
removal of its obsolete copy/worktree responsibilities; retain independent
remaining work rather than creating speculative abstractions.
