# Dolgorae

Dolgorae is a local, durable control layer for persistent Codex runs. It adds
stable run identity, controller authorization, workspace writer coordination,
recovery, and auditability while leaving conversation storage with Codex.

The implementation is written in Rust. The product contract is defined by the
[specification](docs/specs/README.md),
[architecture](docs/architecture/README.md), accepted
[architecture decisions](docs/architecture-decision-records/README.md), and
checked [protocol](docs/protocol/) artifacts. The
[roadmap](docs/roadmap/README.md) is the sole delivery-status authority.

## Release maturity

The release train begins with `v0.1.0`, an Integration Preview covering the
cumulative product scope through `EPIC-004`. Later `v0.1.x` previews advance at
the `MILESTONE-ES1`, provider `MILESTONE-BH1-P`, actual-Gul `MILESTONE-BH1`,
and `MILESTONE-BC1` boundaries. They do
not claim the complete target specification, Personal Alpha readiness, or
customer support.

`v0.2.0` is the planned first customer-supported release. It remains a Personal
Alpha and requires every currently planned product Epic from `EPIC-005` through
`EPIC-011`, including the complete `MILESTONE-PA1` acceptance campaign. See the
[release train](docs/roadmap/README.md#release-train) for the exact version and
milestone boundaries.

## Build and use from source

Build Dolgorae with the supported Rust 1.97.1 toolchain. The complete test gate
also requires Go 1.26.6 for the frozen public-v1 consumers, Buf 1.69.0, and the
documented Python validation environment. Then inspect the currently available
command surface:

```sh
cargo build --locked
./target/debug/dolgorae --human --help
```

The source build includes a Controller-authorized Submit lookup diagnostic:

```sh
dolgorae run --controller-file /absolute/private/controller.json lookup-operation RUN_ID --workspace /absolute/workspace --idempotency-key ORIGINAL_KEY
```

It returns the retained receipt, definitive non-acceptance, unresolved effect
or authoritative absence without reopening attachments. See the
[durable operation contract](docs/specs/dolgorae-v014-durable-operations.md).
This native diagnostic does not establish public-v2 or consumer runtime support.

Session Close retains one operation across gateway and worker recovery. A
supported source build also provides explicit `run recover` handoff for a proved
quiescent worker from another executable build. Busy or uncertain work remains
fenced, and ordinary mutation keeps its exact-build checks. See the
[Close and build recovery contract](docs/specs/dolgorae-v014-close-recovery.md).

The source build provides operator-approved workspace ledger checkpoint rollover.
Create a dry-run plan for the current workspace generation, retain the complete
`data.plan` object in a private file, then apply that exact plan explicitly:

```sh
dolgorae maintenance plan --workspace /absolute/workspace --operator-file /absolute/private/operator.json --selected-generation GENERATION
dolgorae maintenance apply --workspace /absolute/workspace --operator-file /absolute/private/operator.json --plan-file /absolute/private/plan.json --approve
dolgorae maintenance get --workspace /absolute/workspace --operator-file /absolute/private/operator.json --selected-generation ORIGINAL_GENERATION --plan-id ORIGINAL_PLAN_ID
```

The initial workspace maintenance generation is 1. A completed operation reports
its new generation. Resolve interrupted Apply through the original plan ID;
retain that plan until observation confirms completion. Active writers and
uncertain work require their existing lifecycle owners to settle first.
Checkpoint rollover preserves old files and performs no content deletion.
See the [ledger capacity and maintenance contract](docs/specs/dolgorae-v014-ledger-maintenance.md).
Public-v2 activation and actual consumer qualification retain their roadmap owners.

For retained Profile journals, add `--kind membership_compaction` to Plan and
use its independent Profile maintenance generation. Apply/Get retain the same
operator approval and original-plan rules. Original journal and binding bytes
are kept. [Profile maintenance and cleanup](docs/specs/dolgorae-v014-profile-maintenance.md)
also describes cleanup recovery after an already successful Close.

For a removed development or test home, inspect registered detached processes
before cleanup. Use the exact digest from the inspection with the same selector:

```sh
dolgorae runtime orphan inspect --owner-root-under /absolute/removed-test-root
dolgorae runtime orphan cleanup --owner-root-under /absolute/removed-test-root --confirm-selection-sha256 INSPECTION_DIGEST
```

This does not treat PPID 1 as proof of orphaning and cannot reap processes
launched before the inventory was introduced. It leaves unverifiable processes
untouched. Ordinary shared Profile Servers remain alive across CLI invocations.

After initializing the Dolgorae home, a foreground local gateway can serve the
[public v1 API](docs/protocol/dolgorae/public/v1/dolgorae.proto) and the
negotiated modern [public v2 API](docs/protocol/dolgorae/public/v2/dolgorae.proto):

```sh
dolgorae serve --socket /absolute/private-directory/dolgorae.sock
```

The socket's parent must be owned by the current user with mode 0700. Startup
prints one checked readiness result; `--ready-fd <fd>` sends it to an inherited
descriptor instead. The readiness result always remains machine JSON, including
when the global `--human` flag is present. Clients negotiate with
`GetCapabilities` and use its advertised methods and Controller carrier policy.
Stopping or replacing the gateway preserves durable Runs and their workers.

## Specialist policies and orchestrated sessions

Explicit `init` also installs private common and default Markdown instruction
templates at `~/.dolgorae/home/AGENTS.md` and
`~/.dolgorae/roles/{general,reviewer}/AGENTS.md`. Existing files keep their bytes;
invalid files require correction before initialization succeeds. New Runs use
the [caller workspace contract](docs/specs/caller-workspaces-v1.md): omitted
access and Role select `writable` and `general`. Read-only Runs and Reviews use
the same selected canonical workspace without a copied tree or new worktree.
A selected Profile must pass the pinned startup, instruction-discovery, and
reading-boundary requirements for the requested operation.

**Caution: Instruction precedence and Profile guidance**

Codex may also load `AGENTS.override.md` or `AGENTS.md` from the selected
Profile's `CODEX_HOME`. Dolgorae does not disable this Profile guidance.
Dolgorae's core, common and selected Role
instructions form the developer input; Profile guidance and the current task
are user input. The developer instructions take precedence over both. Writer
and Controller permissions remain enforced by Dolgorae and the runtime.

Keep Profile guidance limited to general preferences. Its contents are outside
Dolgorae's captured common/Role snapshot, so editing it can affect later runtime
input even when the captured Role remains unchanged. Automatic project and
ancestor instruction discovery is controlled separately by the successor
contract; it does not load project or ancestor AGENTS files as Role sources.

New Specialist Roles use `~/.dolgorae/roles/<name>/AGENTS.md`. The common
instructions come from `~/.dolgorae/home/AGENTS.md`. A v3 policy explicitly
names a Markdown Role and separately supplies its Profile, model, access,
approval, reuse, and lifecycle controls. Accepted legacy JSON Roles, policies,
Runs, review results and recovery records keep their original interpretation.
New JSON policy admission returns `ROLE_MIGRATION_REQUIRED`.

Plan migration before choosing destination names and access:

```sh
dolgorae instructions migrate --workspace /absolute/project \
  --file /absolute/legacy-policy-input.json
dolgorae instructions migrate --workspace /absolute/project \
  --request-stdin < migration-request.json
```

The plan enumerates common and project JSON sources, their digests, and name
collisions. The checked [migration request](docs/protocol/examples/instruction-migration-request.valid.json)
requires explicit source selection, destination names, publication choices,
and per-Role access. Applying it creates new Markdown and policy authoring
files. It never overwrites sources or rewrites accepted state. A late failure
may leave some new files; inspect them and prepare a new explicit plan before
retrying.

Validate the policy against the selected Profile definitions before installing
it. Validation does not start a Profile Server:

```sh
dolgorae specialist policy validate \
  --workspace /absolute/project \
  --file /absolute/policy-input.json
dolgorae specialist policy add brokered-review \
  --workspace /absolute/project \
  --file /absolute/policy-input.json
dolgorae specialist policy list --workspace /absolute/project
```

Policy installation is create-exclusive. `show` returns safe metadata and Role
digests for v3 policies. `remove` deletes only the registry entry; accepted
sessions retain their original snapshots.

A trusted interactive client starts a Dolgorae-Orchestrated Session by creating
a protected Controller carrier with `--orchestration-policy <name>` and passing
that carrier to a parentless, direct-interactive Machine/private start. The
frozen public-v1 `StartRun` retains its accepted legacy policy contract. Dolgorae
atomically prepares the session around the preallocated Primary Run identity.
The Run-scoped `dolgorae_orchestration` dynamic tool connects that Primary to
the existing orchestration service without editing the shared Profile. The
public gateway exposes `GetOrchestratedSession` and
`ListOrchestratedSessionResults` only after authenticating the root Controller.
The first returns one coherent aggregate snapshot and independent revision; the
second returns captured-head pages of retained Primary-owned result artifacts.
These reads do not repair state, start Workers, acknowledge private delivery,
or expose child Controller authority. For a registered root, `CloseRun` now
records one durable whole-session intent, stops new Broker admission, retires
owned Specialists, and reports success only after the root and every known
owned effect is settled. Modern roots use checked Machine `run close`,
`run recover` and `run reconcile` through that same coordinator; the public-v1
Run adapter retains its legacy contract. Compatible repeats and root recovery retain the same
operation identity across gateway restarts; unknown effects remain visible
for reconciliation instead of being replayed or hidden. Ordinary low-level Run
close and Primary-scoped pause/interrupt semantics are unchanged.

Public v2 starts from explicit captured provisioning: workspace, Controller,
Profile, Markdown Role/configuration, requested access, lane, assurance and key.
It projects modern Runs directly and retains their original identities during
replay and lifecycle operations. Negotiate its exact supported method subset;
activity and interaction delivery retain their roadmap owners. Its group service
accepts immutable research assignments, waits for any/all/k terminal tasks and
collects complete coverage under the aggregate Controller. Sources are captured
before acknowledgement; recovery uses the original operation key and retained
bytes. The checked managed and external facades expose the same group owner.
`MergeGroupResults` publishes deterministic findings with the exact collected
source set, explicit conflicts and coverage; it never invokes a model.
`AssignSynthesis` explicitly accepts a separate task over that immutable merged
input. Retain its own key and receipt, and read merged bytes through their
recorded Artifact owner. Source progress after an accepted merge does not change
its frozen identity. See the [group operations guide](docs/ops/README.md#findings-merge-and-synthesis-recovery)
for recovery and capacity limits.
Public-v1 listing returns accepted legacy Runs in mixed workspaces, and its
workspace Writer status remains readable when a modern Run holds the slot.

Fixed read-only successor members share a compatible Profile Server, with native
subagents disabled in its immutable launch. They cannot acquire Write. Dedicated
Runs retain authorized same-Thread access transitions. The
[pool operations guide](docs/ops/README.md#successor-read-only-pool-and-capacity)
describes configuration, capacity refusals and isolated native qualification.

The verified v0.1.3 provider profile contains exactly 27 advertised public-v1
methods. A generated client must negotiate protocol zero, compare the returned
descriptor digest and method inventory with its checked contract, and then use
the protected root Controller carrier for session, interaction, result, and
Controller-only artifact calls. See the
[provider operations runbook](docs/ops/README.md#v013-provider-operation) for
the supported call order, retry rules, event reconnect, artifact verification,
and version limits. Provider conformance does not claim actual Gul integration,
release publication, or Personal Alpha readiness.

## Task-aware completion review

Specialist Review v4 keeps the captured Markdown Reviewer Role separate from the work
being reviewed. Supply the checked request on non-TTY standard input; workspace
and Profile selection remain trusted CLI arguments:

```sh
dolgorae specialist review \
  --workspace /absolute/project \
  --profile reviewer \
  --request-stdin \
  --format json < review-request-v4.json
```

The request selects the caller source directly and includes
`purpose` (`change` or `completion`), an exact multiline brief, inline contexts
with provenance, ordered criteria, fixed `structured_review_v4` output, and a
deadline. Completion review requires at least one criterion. The result reports
each criterion exactly once with
evidence and remaining gaps; its overall assessment is Reviewer evidence, not
automatic business approval. See the checked
[request example](docs/protocol/examples/specialist-review-v4-request.valid.json)
and [protocol](docs/protocol/dolgorae-specialist-review-tool-v4.schema.json).
Candidate evidence carries host-issued read receipts. Native source reads are
separate context and invalidate the checked review. Accepted v1/v2/v3 results
and their original recovery operations remain readable; new source captures
return `SOURCE_ISOLATION_RETIRED`.

To retain access after CLI or response loss, create a private Controller credential
and retain a UUIDv7 reference before starting the review. Pass both
`--request-ref <UUIDv7>` and `--recovery-controller-file <absolute-path>` to
`specialist review`. Add `--temporary-server` to retire a server created exclusively
for this invocation once its members are terminal. A pre-existing server is preserved.

From a new process, inspect the original operation with:

```sh
dolgorae specialist review-inspect --workspace /absolute/project \
  --request-ref "<retained-UUIDv7>" --format json
```

Authorized cleanup uses `specialist review-recover` with the same selectors,
`--recovery-controller-file <original-absolute-path>` and `--action cleanup`.
Inspection returns saved results and diagnostics without dispatching another Turn.
Cleanup requires the original credential and does not interrupt an active Turn;
unknown or unsafe states remain blocked. See the
[one-shot recovery contract](docs/specs/README.md#one-shot-specialist-review-adapter).

## Agent skill

The source tree distributes the optional complete
[`use-dolgorae` skill](skills/use-dolgorae/SKILL.md) for AI coding agents that
operate Dolgorae's currently advertised workspace, profile, immutable-target,
Specialist Policy, one-shot Specialist Review, public-provider, and externally
planned reusable Specialist Engagement surfaces. It is guidance, not a deployed runtime artifact:
installing the Dolgorae binary does not install or activate the skill.

For Codex, run the following command from the repository root of a checkout of
the exact stable tag that contains the skill. It installs the directory under
`$HOME/.agents/skills`. Other agents may use a different discovery root.

```sh
python3 tools/validators/package_agent_skill.py install \
  --destination "$HOME/.agents/skills/use-dolgorae"
```

The installer requires Python 3 without additional packages. It checks the
resource inventory and canonical byte equality before copying the guidance,
schemas, examples, and local schema dependencies. The installed skill resolves
contracts under its own `resources/protocol/` directory; schema URLs identify
local resources and do not require network access or a checkout.
The installer refuses an existing target, including an empty directory or symlink.
For an upgrade, first verify and explicitly remove or relocate the exact
installed `use-dolgorae` directory, then rerun the
installer from the intended stable tag.

After changing canonical protocol files, run `make validate-agent-skills`.
Installation derives the resources from those canonical files; the source skill
contains only Markdown. Edit canonical files, never installed copies.

See [CONTRIBUTING.md](CONTRIBUTING.md) for development and validation guidance.
Maintainers should start with the [documentation index](docs/README.md) before
changing a contract. Release history is recorded in the
[changelog](CHANGELOG.md).
