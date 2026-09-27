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
[public v1 API](docs/protocol/dolgorae/public/v1/dolgorae.proto):

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

Specialist Roles keep reusable instructions separate from account and execution
configuration. Put an explicitly named Role source in either
`~/.dolgorae/roles/<name>.json` for machine-local use or
`.dolgorae/roles/<name>.json` for project scope. A policy input names each
source by both scope and name, then supplies the global Profile, model, lane,
access, approval, reuse, and lifecycle controls separately.

Start each selected Profile Server, then validate the complete resolved
snapshot before installing it:

```sh
dolgorae specialist policy validate \
  --workspace /absolute/project \
  --file /absolute/policy-input.json
dolgorae specialist policy add brokered-review \
  --workspace /absolute/project \
  --file /absolute/policy-input.json
dolgorae specialist policy list --workspace /absolute/project
```

Policy installation is create-exclusive. `show` reads one installed immutable
snapshot; `remove` deletes only the registry entry and does not rewrite an
existing session snapshot.

A trusted interactive client starts a Dolgorae-Orchestrated Session by creating
a protected Controller carrier with `--orchestration-policy <name>` and passing
that carrier to a parentless, direct-interactive public-v1 `StartRun`. Dolgorae
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
owned effect is settled. Compatible repeats and root recovery retain the same
operation identity across gateway restarts; unknown effects remain visible
for reconciliation instead of being replayed or hidden. Ordinary low-level Run
close and Primary-scoped pause/interrupt semantics are unchanged.

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

Specialist Review v3 keeps the immutable Reviewer Role separate from the work
being reviewed. Supply the checked request on non-TTY standard input; workspace
and Profile selection remain trusted CLI arguments:

```sh
dolgorae specialist review \
  --workspace /absolute/project \
  --profile reviewer \
  --request-stdin \
  --format json < review-request-v3.json
```

The request names the target kind that Dolgorae captures immutably and includes
`purpose` (`change` or `completion`), an exact multiline brief, inline contexts
with provenance, ordered criteria, fixed `structured_review_v3` output, and a
deadline. Completion review requires at least one criterion. The result reports
each criterion exactly once with
evidence and remaining gaps; its overall assessment is Reviewer evidence, not
automatic business approval. See the checked
[request example](docs/protocol/examples/specialist-review-v3-request.valid.json)
and [protocol](docs/protocol/dolgorae-specialist-review-tool-v3.schema.json).
Legacy v1/v2 review commands and results retain their existing meaning.

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

After changing canonical protocol files, maintainers regenerate the derived
resources with `python3 tools/validators/package_agent_skill.py sync` and run
`make validate-agent-skills`. Edit canonical files, never their packaged copies.

See [CONTRIBUTING.md](CONTRIBUTING.md) for development and validation guidance.
Maintainers should start with the [documentation index](docs/README.md) before
changing a contract. Release history is recorded in the
[changelog](CHANGELOG.md).
