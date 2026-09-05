# EPIC-013: Global Codex Profile Cutover

Roadmap Epic: [`EPIC-013`](../roadmap/README.md#epic-013-global-codex-profile-cutover)

This temporary dossier owns the integrated execution contract needed while
EPIC-013 is active. The canonical roadmap remains the sole authority for
identifiers, ordering, dependencies, lifecycle vocabulary, and status.

## Goal and user outcome

Replace workspace-local Runtime Profiles with global Codex Profiles. A Profile
represents one Codex account and its fixed native execution environment, and can
be selected explicitly for a Run in any initialized workspace. Dolgorae must
produce the same selection and launch behavior whether its caller is `codex`,
`codex-hsy`, or another frontend.

Workspace initialization continues to establish workspace identity and
workspace-scoped operational authority. It never fixes an account, executable,
`CODEX_HOME`, default Profile, or Specialist character for the project.

## Authority and terminology

- The [specification](../specs/README.md) owns the global Profile behavior,
  selection, validation, home-generation cutover, failure semantics, and
  Specialist consumer contract.
- The [architecture](../architecture/README.md) owns global versus workspace
  state, Profile Server ownership, immutable Run binding, and process topology.
- The [ADR index](../architecture-decision-records/README.md) records the
  replacement of workspace-local Runtime Profiles and the hard-cut rationale.
- Checked artifacts under [`docs/protocol/`](../protocol/) own exact registry,
  persisted-state, machine-output, and public gRPC shapes.
- The roadmap owns EPIC-013 and TASK-036 through TASK-038 identity, ordering,
  dependencies, status, completion gates, and release placement.

Use these terms after the cutover:

- **Codex Profile** or **Profile**: a global named account launch contract. It
  owns the native Codex executable, canonical `CODEX_HOME`, validated global
  arguments, explicit non-secret environment, and process-static capability
  declarations.
- **Specialist Role** or **Role**: Specialist character and instructions. Role
  sources, resolution, immutable snapshotting, and Specialist Policy belong to
  TASK-024, not this Epic.
- **Agent Configuration**: the immutable Run-facing composition that references
  a selected Profile and, when supported by TASK-024, a resolved Role and policy.

`profile_name` may remain the CLI and public-gRPC identifier spelling. Completed
v1 fields named `runtime_profile` remain historical contract vocabulary and are
not silently reinterpreted; affected live contracts receive successor versions.

## Current problem and durable boundary

The current implementation stores `local.yaml` below each workspace state root
and requires workspace discovery for profile commands. That makes an account
appear to be project configuration and duplicates the same Codex account across
workspaces. It also lets the invoking frontend's environment appear relevant to
runtime selection even though Dolgorae is intended to be the durable control
layer.

The durable correction is one global registry at:

```text
~/.dolgorae/profiles.yaml
```

The registry is user-local, mode 0600, strict, credential-free, and protected by
a user-global configuration lock and atomic write protocol. Profile names are
unique across the Dolgorae home. The existing executable, argument,
`CODEX_HOME`, required environment, native-subagent, ownership, permission,
symlink, and secret-exclusion requirements remain unless an owning Task updates
their canonical rationale.

`codex-hsy` or another shell wrapper is not a supported Profile executable. A
separate account is represented by its native Codex executable and that
account's canonical `CODEX_HOME`. The caller's executable, shell, environment,
and account home are never fallback configuration.

## State layout and hard cut

TASK-036 introduces a mode-0600 `~/.dolgorae/state.json` marker before any new
stateful layout is accepted. It is strict JSON containing exactly
`schema_version: 1` and `state_generation: "global-profile-v1"`, with its checked
schema owned under `docs/protocol/`. An absent home or an empty newly created
home may atomically establish the marker. A nonempty unmarked home, malformed or
unsupported marker, legacy layout, or mixed-generation content returns
`LEGACY_STATE_UNSUPPORTED` before any stateful read or mutation.

The cutover has no automatic migration or recovery compatibility:

- Do not import or infer Profiles from workspace-local `local.yaml` files.
- Do not recover old Run, Profile Server, engagement, writer, or aggregate
  state under the new generation.
- Do not delete, overwrite, rename, or partially initialize a legacy home.
- Keep stateless help, version, and runtime capability discovery available.
- Require the operator to stop Dolgorae processes, back up or move the existing
  `~/.dolgorae`, initialize a fresh home, recreate Profiles explicitly, and then
  initialize or rediscover each compatible project workspace.

The tracked `<workspace>/.dolgorae/config.yaml` remains portable workspace
policy and contains no account selection or stored workspace ID. Against a
fresh new-generation home, `init` accepts an existing marker only when its
strict schema, mode, canonical placement, and Git/non-Git discovery rules are
compatible. It preserves the policy files byte-for-byte, recomputes the same
workspace ID from the unchanged canonical path, and atomically creates a fresh
`workspace.json` plus empty workspace state below that ID. This successful
rediscovery reports `created:true` because the machine-local workspace
registration was created; only a later call with the same compatible portable
policy and exact new-generation `workspace.json` reports `created:false`.
Incompatible policy, mode, placement, canonical identity, or an existing
conflicting workspace record returns `WORKSPACE_INITIALIZATION_CONFLICT` before
mutation. No accepted project marker authorizes reading or reconstructing state
from the moved legacy home.

The production hard cut has one activation boundary. TASK-036 and TASK-037
prepare checked target contracts and implementation paths that are unreachable
from production commands; they do not create a new-generation marker, change
the existing CLI default path, or make any completed EPIC-006 path unavailable.
TASK-038 performs the switch only after the global registry, Run runtime, and
both Specialist consumers are ready. Its final task-owned change atomically
activates the new home gate, global Profile commands, account-neutral `init`,
Run admission, and both Specialist entry paths. There is no dual read, dual
write, feature flag, or supported compatibility window before or after that
activation. The complete pre-cut behavior remains usable at the completed
TASK-036 and TASK-037 revisions; the complete post-cut behavior is required at
the completed TASK-038 revision.

## Task ownership and order

### TASK-036: Global Codex Profile Contract and Hard-Cut Home Layout

Synchronize specification, architecture, and ADR authority first, then implement
the new generation boundary and global registry.

Owned behavior and interfaces:

- Add a distinct checked global Profile registry contract; retain the completed
  local registry v1 artifact unchanged as historical input. Implement the
  global registry and home-generation validator behind the inactive cutover
  boundary; no production command may select them in this Task.
- Define the post-cut profile add, list, get, remove, migrate, start, stop, and
  diagnostics behavior against only the fixed Dolgorae home and never a
  workspace. TASK-038 owns activation of that command surface.
- Define removal of `--workspace` from post-cut profile command syntax and its
  immediate rejection, with no warning release or compatibility alias;
  TASK-038 activates the new syntax.
- Keep Profile selection mandatory on Run and Specialist creation. Do not add a
  default Profile at user, workspace, or command level.
- Define post-cut `dolgorae init` to create only portable workspace policy and the
  workspace-scoped state needed for Run, writer, audit, and recovery authority.
  It creates no registry entry and no workspace-local `local.yaml`; TASK-038
  activates that behavior together with every consumer.
- Add the home-generation error and operator recovery contract with no
  automatic deletion or conversion, but do not gate production stateful
  commands until TASK-038.
- Correct public gRPC v1 before TASK-023 implements it: remove the workspace
  fields from ListProfilesRequest, GetProfileRequest, and
  ListProfileDiagnosticsRequest, reserve their old field numbers and the old
  `workspace` field name, and update the checked descriptor and conformance
  artifacts. StartRunRequest keeps both workspace and `profile_name`.

The public Profile DTO continues to describe the selected launch contract and
capability status. Profile management authorization remains local-user and
operator controlled; the gRPC correction does not broaden the minimum BH1
method set or expose credentials.

### TASK-037: Global Profile Runtime, State, and Immutable Run Binding

Move runtime consumers from workspace-local lookup to the global Profile
authority while preserving workspace isolation for every non-Profile owner.
The complete successor path remains behind the inactive cutover boundary in
this Task; current Run and Specialist behavior stays available until TASK-038.

Owned behavior and interfaces:

- Resolve and validate the named Profile once at Run admission, derive the
  Profile Server key from the resolved global launch contract, and persist a
  complete immutable snapshot and digest in the Run generation.
- Keep Profile Server state and physical generation global to the Profile while
  tracking membership across every workspace that uses it.
- Keep Run directories, writer leases, engagement and orchestration databases,
  aggregates, audit ledgers, evidence, and recovery state below their canonical
  workspace roots.
- Allow one global Profile to serve multiple workspaces and allow one workspace
  to select different Profiles for different Runs.
- Treat active, unreadable, or outcome-unknown membership in any workspace as a
  blocker for Profile replacement, removal, migration, server stop, or physical
  generation change. Never check only the caller's workspace.
- Ignore caller `PATH`, `CODEX_HOME`, executable name, shell aliases, and frontend
  identity after command parsing. Only the resolved global Profile supplies the
  child process launch environment.
- Version every completed persisted or machine-readable artifact whose field
  set or meaning changes. Do not mutate v1 schemas in place and do not add a
  decoder or recovery path for the rejected legacy home.

This Task must perform an affected-contract census covering at least the global
registry, Run manifest, Profile Server state, runtime discovery, membership,
machine success/error envelopes, and public Profile DTO. A distinct successor
artifact is required whenever preserving the old bytes would silently change
their meaning.

### TASK-038: Specialist Consumers and v0.1.2 Acceptance

Move both currently usable Specialist entry paths onto the global Profile
contract, activate the complete cutover, and close the release boundary.

Owned behavior and interfaces:

- As the first production activation step, revalidate that the TASK-036 global
  registry and TASK-037 Run path plus both Specialist consumers are complete.
  Then switch the home gate, Profile CLI, account-neutral `init`, Run admission,
  one-shot review, and External Specialist Engagement together; do not land or
  commit a partially active state.
- Preserve explicit `--profile` selection for one-shot Specialist Review and
  explicit Profile selection in External Specialist hire requests.
- Resolve the global Profile before Run allocation and persist its immutable
  snapshot through Agent Configuration, engagement membership, and Run state.
- Update the External Specialist Facade's input normalization and idempotency
  digest so identical requests replay and a changed Profile or configuration
  conflicts deterministically.
- Update member restoration, task dispatch, restart reconciliation,
  completed-not-delivered result retention, and redelivery to use only the
  stored immutable selection; never re-resolve a mutable Profile to explain an
  existing member or replay accepted work.
- Version Agent Configuration, External Specialist Facade, Run manifest,
  Specialist Review machine contracts, and checked examples when their field
  names, required values, or semantics change. Preserve completed v1 artifacts.
- Retain external aggregate-owner authorization, per-Run Controller authority,
  one active Turn per Specialist, no implicit preemption, no unknown-input
  replay, writer coordination, target immutability, recursive-review denial,
  nested-hire denial, and the external host's semantic-control boundary.
- Update TASK-024's input contract to call the behavior-bearing persona a Role
  and to own common and project Role sources plus explicit resolution. Do not
  implement that storage or precedence in this Epic.

## Failure behavior and compatibility

- A missing requested Profile is `PROFILE_NOT_FOUND`; malformed new registry
  content remains `PROFILE_CONFIG_INVALID` unless the home generation itself is
  absent, legacy, partial, or mixed.
- Legacy or mixed Dolgorae-home state is always
  `LEGACY_STATE_UNSUPPORTED` and is inspected only enough to classify and refuse
  the state safely.
- No command falls back to a workspace-local registry, caller environment,
  invoking frontend, first Profile, or inferred default.
- Profile mutation is serialized globally and remains atomic and durable. A
  concurrent winner is observed or produces a deterministic conflict; it never
  loses an accepted update.
- Cross-workspace membership uncertainty fails closed. Administrative
  convenience does not override active or unknown runtime state.
- This is a pre-release hard cut. No compatibility window, state converter,
  dual read, dual write, old Run recovery, or automatic cleanup is accepted.

## Verification and Epic acceptance

TASK-036 verification must cover:

- strict global registry parsing, duplicate rejection, executable and
  `CODEX_HOME` validation, permissions, ownership, symlinks, atomic replacement,
  locking, and concurrent CRUD;
- Profile commands from initialized, uninitialized, and unrelated directories,
  including immediate `--workspace` rejection;
- `init` producing no Profile, default, or workspace-local registry;
- fresh, absent, legacy, partial, malformed, and mixed-generation homes, with
  byte-for-byte proof that rejected legacy input was not mutated; and
- exact public-v1 source, descriptor, and conformance changes with removed field
  numbers and names reserved; plus regression proof that the production CLI,
  Run paths, and both completed EPIC-006 Specialist paths still use the complete
  pre-cut behavior at the TASK-036 revision.

TASK-037 verification must cover:

- one Profile used concurrently by Runs in two workspaces and two Profiles used
  by Runs in one workspace;
- immutable snapshot and server-key stability after registry changes;
- cross-workspace active and unknown membership blocking every unsafe lifecycle
  operation;
- caller `codex` versus `codex-hsy`, conflicting `PATH` and `CODEX_HOME`, and
  shell alias differences producing the selected Profile's same launch result;
- concurrent start, stop, removal, migration, restart, and server-generation
  races; and
- deterministic new-generation restart and diagnostics without legacy reads;
  plus regression proof that no production selector reaches the prepared path
  and all pre-cut behavior remains usable at the TASK-037 revision.

TASK-038 verification must cover:

- one activation-boundary test proving that no production command can observe
  a mixed old/new state, followed by the full post-cut CLI, init, Run, and
  Specialist matrix;
- rediscovery of an existing compatible project policy into a fresh home with
  the same deterministic workspace ID, `created:true`, byte-identical tracked
  files, fresh empty machine state, idempotent `created:false`, and fail-closed
  non-mutation for every incompatible marker or workspace-record case;
- one-shot Specialist Review and reusable External Specialist Engagement across
  multiple Profiles and workspaces;
- hire same-key replay, changed-input conflict, member recovery, repeated
  sequential tasks, restart reconciliation, and completed-result redelivery;
- no target Turn replay after ambiguous acceptance or delivery loss;
- authorization, writer, canonical versus isolated-write, recursive-review,
  nested-hire, collaboration, and external task-graph denial regressions;
- updated schemas, fixtures, validation indexes, CLI help, user guidance, and
  operations guidance; and
- `make PYTHON_BIN=.venv/bin/python test` plus independent read-only review under
  the repository Task completion gate. Any live Codex smoke test remains outside
  the default gate and requires separate explicit authorization.

Epic completion requires all three Tasks to be committed and complete, every
affected canonical owner to agree, no valid blocking finding to remain, and the
complete gate to pass. Only then may `v0.1.2` become release-eligible. Completion
does not itself authorize a release, tag, push, installation, or publication.

## Prohibited shortcuts and non-goals

- Do not implement Specialist Role storage, common/project precedence,
  Specialist Policy resolution, or general persona management before TASK-024.
- Do not add project or user defaults, implicit Profile selection, provider
  abstraction, arbitrary wrapper execution, or frontend-specific behavior.
- Do not migrate, import, convert, recover, delete, or overwrite legacy state.
- Do not weaken EPIC-005 safety, EPIC-006 external engagement semantics, writer
  authority, Controller authorization, immutable targets, or audit durability.
- Do not absorb deferred feedback DF-001 through DF-003 or imply that the Role
  terminology resolves DF-003.
- Do not implement the TASK-023 gateway, Brokered Hierarchy, live Primary
  transport, lateral collaboration, operator UI, release, or publication work.

## Canonical handoffs and closeout

Each Task updates specifications first for behavior changes, architecture for
ownership or topology changes, ADRs for accepted and rejected alternatives,
protocol artifacts for exact shapes, implementation and tests for behavior,
and roadmap lifecycle only after its completion gate passes.

TASK-038 hands the following fixed inputs to TASK-023 and TASK-024:

- TASK-023 receives the corrected workspace-independent public-v1 Profile
  requests and the global Profile semantic service.
- TASK-024 receives the term Specialist Role, ownership of common and project
  Role sources and explicit resolution, and Agent Configuration composition over
  an already selected global Profile.

Before marking EPIC-013 complete, promote every durable dossier statement to its
canonical owner or remove it as temporary delivery context. Then delete this
file and its TODO index entry, replace the roadmap's `Detailed SOT` link with
`Canonical Outcomes`, and perform the Epic lifecycle transition in one approved
closeout change. Keep no archive or tombstone copy of this dossier.
