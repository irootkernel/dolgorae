# Caller-Owned Workspaces and Explicit Agent Roles

Consumer Epic: [EPIC-017](../roadmap/README.md#epic-017-caller-owned-workspaces-and-explicit-agent-roles)

This dossier integrates the approved successor design. The roadmap owns IDs,
order, dependencies and lifecycle. Current product owners remain authoritative
until TASK-061 promotes the successor contracts before implementation. Do not
interpret this planning dossier as evidence that the new behavior is available.

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

TASK-061 resolves contradictions among these owners before source changes.
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
- Suppress automatic AGENTS loading from ambient Profile/project/ancestor
  paths for the new managed instruction contract. Prove the supported Codex
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
| Sources | Correct captured bytes/digests; missing, unsafe, over-bound and raced inputs rejected; no implicit ambient AGENTS; explicit init never overwrites |
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

At closeout, promote durable behavior, architecture, protocol and operator
outcomes to their owners, update unfinished handoffs, replace EPIC-017's Detailed
SOT with Canonical Outcomes, and remove this dossier/index entry only after the
approved last-consumer closeout. Reassess the launch-preparation candidate after
removal of its obsolete copy/worktree responsibilities; retain independent
remaining work rather than creating speculative abstractions.
