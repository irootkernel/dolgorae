# Dolgorae Product Specification

Status: Normative target specification for the first supported release.

This document owns Dolgorae's externally observable behavior. Technical structure
is owned by [architecture.md](architecture.md), decision rationale by
[architecture-decisions.md](architecture-decisions.md), and delivery state by
[roadmap.md](roadmap.md). A contradiction between SOT documents is an invalid
state and must be reconciled before an implementation task becomes active.
Document roles and the required synchronization procedure are defined by the
[documentation authority map](README.md).

Only the uppercase key words **MUST**, **MUST NOT**, **SHOULD**, and **MAY** are
normative; lowercase prose is descriptive and grants no additional authority.
Constraints in checked JSON Schemas and rejection by an executable semantic
validator explicitly named by this specification are also normative.

## Definitions

- **Master**: the existing compatibility term for a human, interactive client,
  workflow orchestrator, or local automation process that invokes a public
  Dolgorae adapter. New product prose SHOULD name the more precise orchestration
  owner defined by the selected use case.
- **Controller**: the one capability-bound authority allowed to mutate a
  particular Run. A Controller is not an agent role or a session-level control
  plane.
- **User-Facing Use Case**: one of exactly two product entry models:
  `Dolgorae-Orchestrated Session` or `External Specialist Engagement`. The
  selected use case determines who owns semantic orchestration.
- **Dolgorae-Orchestrated Session**: a durable user-facing session in which
  Dolgorae owns the Primary Agent, internal Specialist brokerage, operational
  orchestration state, and recovery. Gul is the canonical client for this use
  case.
- **External Specialist Engagement**: a durable operational boundary in which
  an external AI remains the Primary Agent and semantic control plane and hires
  one or more Dolgorae-managed Specialists.
- **Orchestration Session Record**: the authoritative Dolgorae record for one
  Dolgorae-Orchestrated Session. In v1 its session ID equals its Primary Run ID.
- **Specialist Engagement Record**: the authoritative Dolgorae operational
  record for Specialists hired by one external control plane. It does not store
  or own the external plan or task graph.
- **Aggregate Bootstrap Operation**: the durable write-ahead operation that
  creates exactly one Orchestration Session Record or Specialist Engagement
  Record. It owns aggregate identity, bootstrap idempotency, request digest,
  provenance, and crash recovery before any member Run is published.
- **Primary Orchestration Tool Bridge**: the private run-bound tool surface used
  by a Primary Agent to request, assign, await, inspect, and release Specialists.
  Source session, Run, Turn, tool-call identity, Controller authority, and
  idempotency are bound outside model-controlled arguments.
- **External Specialist Facade**: the private CLI or trusted MCP surface used
  by an external AI to explicitly open an empty External Specialist Engagement
  and then inspect, hire, assign, await, collect, cancel, release, or close its
  Specialists without adding another semantic planner.
- **Orchestration Launch Intent**: optional non-secret metadata in a protected
  Controller carrier. Its presence explicitly selects a Dolgorae-Orchestrated
  Session and names the Specialist Policy to snapshot. It is creation input, not
  Controller authority, and is never inherited by a Specialist credential.
- **Specialist Policy Registry**: the machine-local, workspace-scoped registry
  of checked Specialist Policy documents selectable by Orchestration Launch
  Intent. Active sessions retain complete immutable snapshots and do not depend
  on later registry contents.
- **Specialist Policy**: the immutable, schema-validated session snapshot that
  defines approval mode, allowed roles, Agent Configuration references,
  cardinality, reuse, access, activation, and collaboration permissions.
- **Agent Topology**: internal composition terminology over independent Runs. It
  is not a user-facing mode, `control_mode`, or public enum.
- **Standalone Primary composition**: the internal composition state of a
  Dolgorae-Orchestrated Session with no active Independent Specialist Run.
- **Brokered Hierarchy**: the internal composition state of a
  Dolgorae-Orchestrated Session with one Primary Run and one or more durable
  Independent Specialist Runs owned by the Dolgorae Orchestration Broker.
- **Orchestration Broker**: the Dolgorae control-plane component that owns
  Brokered Hierarchy membership, internal Specialist Controller credentials,
  idempotent spawn operations, task delivery, result redelivery, collaboration
  mailboxes, activation, and recovery. It is not an LLM-visible authority.
- **Brokered Specialist Collaboration**: bounded logical direct communication
  between two Independent Specialist Runs in the same active Brokered Hierarchy.
  The Primary Agent is not a message relay; the Dolgorae Collaboration Plane
  validates, persists, schedules, delivers, and audits every exchange.
- **Collaboration Plane**: the internal Orchestration Broker subsystem composed
  of the Collaboration Service, durable mailbox store, Mailbox Scheduler,
  Activation Manager, and run-bound private tool bridge.
- **Collaboration Exchange**: one durable, correlated Specialist-to-Specialist
  request and result boundary with independent execution and delivery states.
- **Durable Mailbox**: the SQLite-backed ordered inbox for work addressed to one
  Specialist Run. A mailbox is durable state and is not a polling loop inside
  the Specialist.
- **Virtual Actor**: an Independent Specialist Run whose logical identity,
  thread, role, mailbox, and recovery state persist even when its Worker and any
  Run-owned physical lane generation are absent. A shared Profile Server may
  remain resident independently.
- **Passivation**: verified release of a Virtual Actor's Worker and Run-owned
  physical lane while preserving its resumable logical Run, thread, aggregate
  membership, and mailbox.
- **Activation**: restoration of a passivated Virtual Actor into a resident,
  dispatchable Run generation. Mail-triggered activation is performed by the
  Activation Manager, never by the model itself.
- **Primary Run**: the independent Run that hosts the user-facing Primary Agent
  of a Dolgorae-Orchestrated Session.
- **Primary Agent**: the Codex agent hosted by a Primary Run.
- **Independent Specialist Run**: an independent durable Dolgorae Run selected
  for a bounded specialist role. It has its own thread, Worker, Controller,
  execution lane, audit ledger, and recovery state.
- **Independent Specialist Agent**: the Codex agent hosted by an Independent
  Specialist Run.
- **Native Delegation**: a Codex agent's use of Codex-native subagents inside
  its own Run.
- **Native Subagent Policy**: the orthogonal Runtime Profile capability and
  immutable instruction policy governing Native Delegation. It is not a use
  case or Agent Topology.
- **Observer**: a same-OS-user caller allowed to read client-safe projections
  without acquiring mutation authority.
- **Run**: one durable Dolgorae session primitive, identified by a UUIDv7 and
  bound to one Codex thread. Aggregate membership never merges Run identity or
  lifecycle.
- **Turn**: one identified Codex execution within a Run, beginning when
  `turn/start` is accepted and ending only when Codex confirms completed,
  interrupted, or failed status.
- **Worker**: the hidden per-Run Dolgorae process that owns one direct App Server
  WebSocket connection, worker control socket, Run lifecycle, and audit writer.
- **Server epoch**: one globally unique lifetime of any physical Codex App
  Server generation, whether the shared Profile Server or a Dedicated Lane
  Server.
- **Run generation**: one Worker lifetime and its direct App Server connection
  lifetime within a Run. Access-policy changes do not increment it.
- **Policy epoch**: the monotonic version of a Run's effective read/write policy.
  A verified in-place access transition increments this value without changing
  Run generation, thread identity, or logical lane.
- **Thread generation**: the monotonic Dolgorae binding generation for a Codex
  thread start, resume, or fork operation that installs immutable developer
  instructions.
- **Runtime Profile**: a user-local named Codex execution configuration
  consisting of a direct absolute executable, normalized global argv, canonical
  `CODEX_HOME`, deterministic non-secret environment, process-static
  configuration, and verified runtime capabilities. It does not define agent
  character.
- **Agent Configuration**: the immutable role-facing configuration resolved for
  a Run: Runtime Profile snapshot, model, default effort, purpose, required
  capabilities, and Controller instructions. Different Agent Configurations may
  share one Runtime Profile.
- **Profile Server**: the shared-read-only Codex App Server singleton selected
  by one Runtime Profile launch-authority contract.
- **Dedicated Lane Server**: one physical Codex App Server generation owned by a
  Run's immutable dedicated logical lane.
- **Event Projection**: the `minimal` or `operational` delivery view over one
  durable event cursor domain.
- **Public RPC Gateway**: the public gRPC adapter hosted by the foreground
  `dolgorae serve` process over a user-private Unix domain socket. The adapter is
  not durable authority. The process is optional for finite low-level clients,
  but an active Dolgorae-Orchestrated Session that enables Brokered Specialist
  Collaboration requires its supervised Control-Plane Runtime.
- **Control-Plane Runtime**: the reconstructable in-process Orchestration Broker,
  Collaboration Plane, Mailbox Scheduler, Activation Manager, private tool
  bridge, and sole workspace orchestration-database mutation owner hosted by
  `dolgorae serve`. SQLite, not this runtime, remains durable authority.
- **Codex Config Profile**: a Codex `--profile` selection inside normalized
  global argv; it is not a Dolgorae Runtime Profile.
- **Reader**: a Run whose Turns use Codex read-only sandbox policy.
- **Writer**: the single Run named by durable Dolgorae writer authority for a
  canonical workspace and whose Turns may use workspace-write sandbox policy.
- **Terminal Turn**: a Turn confirmed as completed, interrupted, or failed.
- **Forkable Turn**: a terminal Turn whose exact status is listed in the checked
  Codex required-subset manifest as accepted for `lastTurnId` by the pinned
  profile. Terminal and forkable are intentionally not synonyms.

### Canonical User-Case Mapping

The user or trusted integration explicitly selects one of two product use
cases. User-facing clients MAY resolve low-level settings from that selection,
but the semantic service MUST receive complete `control_mode`, execution lane,
required assurance, Runtime Profile, purpose, and native-subagent policy values.
An `UNSPECIFIED` value or hidden interactive default is invalid.

| User-facing use case | Primary placement | Run composition | Semantic orchestration owner | Dolgorae operational ownership |
| --- | --- | --- | --- | --- |
| **Dolgorae-Orchestrated Session** | Primary Agent inside Dolgorae | one `direct_interactive` Primary Run, plus optional internally brokered `managed_agent` Specialist Runs | Dolgorae | Primary Run, Brokered Hierarchy, Specialist membership, spawn, task delivery, collaboration mailboxes, activation, audit, and recovery |
| **External Specialist Engagement** | Primary Agent outside Dolgorae | one or more externally controlled `managed_agent` Specialist Runs | external AI or host | Specialist Run, accepted task boundary, result delivery, audit, and recovery |

#### Compilation Over the Unchanged Public Run Contract

The two product facades compile to the existing public v1 Run operations. The
checked public Protobuf remains unchanged, but aggregate creation is explicit
inside the semantic service and durable orchestration store. Client name,
process name, and undocumented defaults MUST NOT select a use case.

A public `StartRun` bootstraps a Dolgorae-Orchestrated Session only when all
of the following are true:

- the protected Controller carrier has `kind: human_cli` or
  `kind: interactive_client`;
- the carrier contains `orchestration_launch.use_case` equal to
  `dolgorae_orchestrated_session` and a valid `specialist_policy_name`;
- `control_mode` is explicitly `direct_interactive`; and
- the request has no parent reference.

Controller kind, client name, process name, and `direct_interactive` alone do
not infer the product use case. A direct-interactive root without launch intent
remains a low-level Run primitive and has no Orchestration Session, Specialist
Policy, or Brokered Hierarchy. A launch intent on a managed Run, a parented Run,
or an unsupported Controller kind is `INVALID_ARGUMENT` before allocation.

Before publishing an accepted Orchestrated Session root, Dolgorae MUST:

1. validate the complete explicit Run configuration, Controller credential, and
   Orchestration Launch Intent;
2. resolve `specialist_policy_name` from the canonical workspace's
   Specialist Policy Registry, validate it, and include its name, revision, and
   JCS SHA-256 in the normalized idempotency request;
3. preallocate one UUIDv7 used as both `run_id` and `session_id`, plus one
   Aggregate Bootstrap Operation ID that is also the cross-store correlation
   identifier;
4. commit the `create_orchestrated_session` operation, the `creating` session
   record, the complete immutable Specialist Policy snapshot, and the
   orchestration event;
5. append and fsync the matching Run creation intent carrying the same bootstrap
   operation ID before publishing the Run manifest; and
6. mark the bootstrap operation `ready` and the session `active` only after the
   empty Primary Run is authoritatively published.

Exact replay of the same idempotency key and normalized request returns the
original Run and session. A changed policy snapshot under the same idempotency
key is a conflict. A crash after either durable boundary is reconciled using the
preallocated identity and bootstrap operation ID. It MUST NOT create another
Primary Run or another session. An authoritative bootstrap failure is retained
as terminal evidence, while an ambiguous boundary requires recovery and is
never silently replayed. A `direct_interactive` Run with any parent reference
is invalid.

An External Specialist Engagement is opened explicitly through
`open_external_engagement` on the External Specialist Facade defined by
[`dolgorae-external-specialist-facade-v1.schema.json`](protocol/dolgorae-external-specialist-facade-v1.schema.json).
The open request supplies immutable external provenance and one aggregate-scoped
idempotency key. The adapter binds the canonical workspace and exactly one
protected aggregate-owner Controller credential outside model-visible payloads.
Only `workflow_orchestrator` and `automation` credentials are accepted. Dolgorae
persists an immutable Aggregate Controller Binding containing the public
Controller ID, generation 1, kind, normalized-principal digest, and
capability digest; raw capability bytes are never persisted. The opaque
`external_controller_ref` remains semantic provenance and grants no authority.
Dolgorae allocates the `engagement_id` and bootstrap-operation ID, then
atomically commits the `open_external_engagement` operation and an empty active
engagement before returning the server-generated identifier. Opening an
engagement does not create a Run, lane, thread, or Turn.

Exact open replay with the same canonical workspace, Aggregate Controller
Binding, external provenance, idempotency key, and request digest returns the
same engagement. Same-key drift is `IDEMPOTENCY_CONFLICT`. Every later facade
call MUST name that engagement ID and present the same aggregate-owner
Controller credential. Dolgorae reopens and validates the protected carrier at
the serialization point and compares Controller ID, kind, normalized principal,
and capability digest against the stored binding. A
`hire_external_specialist` call additionally supplies a fresh per-Run
Controller credential carrier outside model-visible payloads. Dolgorae preallocates the hire-operation ID and child Run
ID and commits the write-ahead hire operation, member, and child Run reservation
before any process, lane, thread, or Turn side effect. The trusted facade then
compiles the accepted hire into a complete `managed_agent` Run start with the
reserved parent projection:

```text
namespace = dolgorae.external-specialist-engagement.v1
kind      = specialist
id        = <Dolgorae-generated engagement UUIDv7>
```

Only the trusted External Specialist Facade may create that reserved projection.
A raw low-level `StartRun` MUST NOT implicitly open, join, or infer an External
Specialist Engagement. A generic `managed_agent` Run outside an aggregate
remains a valid low-level primitive, but it receives no engagement membership,
hire-operation, accepted-task, or result-redelivery contract. Existing generic
Runs cannot be attached in place; hiring creates a new Run.

The internal Orchestration Broker uses the reserved public parent projection
`dolgorae.orchestrated-session.v1 / specialist / <session_id>` for owned
Specialists. That projection supports safe presentation and filtering only. The
internal aggregate registry, bootstrap and spawn operations, and member records
remain authoritative. Reserved namespaces never grant Controller authority.

A Dolgorae-Orchestrated Session starts in Standalone Primary composition and
enters Brokered Hierarchy composition when the Orchestration Broker adds at
least one active Specialist. This is a dynamic internal composition state, not a
second user mode and not a change of session ownership.

An External Specialist Engagement never becomes a Brokered Hierarchy. V1
prohibits a Specialist in that use case from hiring another first-class
Dolgorae Specialist. The external control plane hires additional roles directly.

Runtime Profile selection never selects agent character. The complete immutable
Agent Configuration snapshot defines the role used by a Primary or Specialist
Run. Native Subagent Policy is orthogonal to both use cases. The derived
[use-case and topology guide](agent-topology-terminology.md) provides examples
without owning this contract.

## SPEC-001: Product Boundary and Supported Environment

Dolgorae MUST expose exactly two user-facing use cases through one distributable
`dolgorae` executable:

1. **Dolgorae-Orchestrated Session**, in which Dolgorae owns the Primary Agent,
   brokered Specialist operation, durable operational orchestration state, and
   recovery. Gul is the canonical client for this use case.
2. **External Specialist Engagement**, in which another AI remains the Primary
   Agent and semantic control plane and selectively hires Dolgorae-managed
   Specialists.

These use cases are product entry models, not additional `control_mode` values.
Every Primary or Specialist remains an independent Run with its own thread,
Worker, Controller binding, lane, audit ledger, and recovery state. A
Dolgorae-Orchestrated Session begins in Standalone Primary composition and
enters Brokered Hierarchy composition when its Orchestration Broker owns at
least one active Independent Specialist Run. An External Specialist Engagement
MUST NOT create a second semantic control plane and MUST NOT become a Brokered
Hierarchy.

A Codex-native subagent is an internal descendant of one Codex Run. An
Independent Specialist Run is a durable peer Run. These concepts MUST NOT be
conflated. Native Delegation remains an orthogonal Runtime Profile capability
and never creates a Dolgorae aggregate member, Controller, Worker, or writer
authority.

Dolgorae MUST NOT install a Dolgorae global daemon, project daemon, launchd
unit, Codex binary, authentication material, or `CODEX_HOME`. It MAY manage one
Codex App Server singleton per canonical Runtime Profile launch-authority
contract and one or more Run-owned Dedicated Lane Server generations. Those
Codex processes are not Dolgorae daemons.

The first supported release is a personal alpha for Apple Silicon macOS 26.0
or later (`aarch64-apple-darwin`) on local APFS. The canonical workspace and
Dolgorae's configured mutable state and lock root MUST report `MNT_LOCAL` and
`f_fstypename == "apfs"`; there is no v1 override. Intel macOS, Linux,
Windows, network filesystems, non-APFS local filesystems, public installers,
and automatic updates are not supported release targets. Empirical release
evidence is valid only for the recorded OS build and MUST be refreshed on a new
macOS major version.

Dolgorae depends on user-prepared Runtime Profiles. Codex App Server 0.147.0 is
the current compatibility baseline. Background-process safety is owned by each
Sticky Dedicated logical lane across its successive physical generations and
by the macOS process census; it MUST NOT depend on a future Codex terminal-
management API. A newer native API MAY supply additional evidence but never
replaces lane-generation identity, census, or cleanup.

V1 is an offline shell-execution environment. Reader and writer Turns use
`networkAccess:false`. Dependency installation, remote Git operations, and
arbitrary external API calls are outside the supported shell contract unless a
future SOT revision defines a separate network policy. MCP servers, plugins,
and apps may perform side effects outside Codex's shell sandbox; such effects
are trusted profile behavior and are outside Dolgorae's hard one-writer
guarantee.

Dolgorae MUST be the only Codex App Server supervisor used by supported Gul and
external-AI integrations. A client MUST use the stable Machine CLI or public
local gRPC adapter and MUST NOT start, connect to, or control a Profile Server,
Dedicated Lane Server, or private Worker socket. This is a supported-
integration boundary, not a claim that Dolgorae can prevent a hostile same-user
process or an unrelated editor from mutating the workspace.

Dolgorae remains local-only. V1 MUST NOT bind a public TCP port, provide remote
authentication, require a remote client to remain connected, expose direct
Tailscale access, or make a private Worker or App Server transport public. A Gul
deployment owns remote HTTP authentication, authorization, and presentation
outside Dolgorae.

## SPEC-002: Workspace Initialization and Discovery

`dolgorae init [PATH]` MUST initialize a Git workspace. `dolgorae init
--non-git [PATH]` MUST explicitly opt a general directory into Dolgorae. A Run
or aggregate MUST NOT start in an uninitialized workspace.

In Git mode, Dolgorae runs
`git -c core.quotePath=true -C <supplied-existing-directory> rev-parse --show-toplevel`
without a shell and requires exit 0 and exactly one LF-terminated stdout
result. Bounded stderr is diagnostic only when exit is zero. A double-quoted
result is decoded with Git's documented C-style path quoting, including octal
byte escapes; an unquoted result is the bytes before the sole final LF. Invalid
quoting, trailing output, or NUL is a Git discovery failure. The canonical
workspace is libc `realpath(3)` applied to that decoded existing directory,
even when the supplied path is a subdirectory. In non-Git mode it is
`realpath(3)` applied to the existing initialized directory.

The canonical path is the returned absolute POSIX byte sequence with no
trailing slash except for root, followed by the macOS Data-volume alias rule.
Dolgorae performs no Unicode normalization or case folding. Symlink and case-
insensitive lookup belong to `realpath(3)`, but APFS firmlinks do not. When the
result is exactly `/System/Volumes/Data` or begins with
`/System/Volumes/Data/`, Dolgorae derives the candidate `/` or the path with
that prefix removed and substitutes it only when no-follow `stat` of both paths
yields the same `(st_dev, st_ino)`. The same rule is applied in Git and non-Git
mode before any digest is computed.

The workspace digest is lowercase hexadecimal SHA-256 over
`"dolgorae-workspace-v1\0"` followed by the canonical path bytes. The full
64-character digest is the workspace ID. The short Worker socket name remains
RFC 4648 uppercase unpadded base32 of the first 160 bits of SHA-256 over
`"dolgorae-socket-v1\0" || workspace_digest_bytes || run_uuid_bytes`, where
the digest and UUID inputs are their raw bytes. Every component MUST use the
same preimages.

Each linked Git worktree has its own canonical top-level path and is therefore
a separate Dolgorae workspace, aggregate registry, Run store, and writer
authority. Dolgorae serializes at most one writer per canonical worktree and
does not serialize worktrees that share a Git common directory.

Dirty Git workspaces are allowed. Run creation MUST record a read-only baseline
containing HEAD, branch, tracked changes, and untracked paths. Dolgorae MUST NOT
discard, reset, stash, or otherwise rewrite pre-existing changes.

Later commands discover the nearest ancestor containing `.dolgorae`; an
explicit `--workspace PATH` overrides discovery. Discovery selects a workspace
only. It MUST NOT implicitly select a Run, Dolgorae-Orchestrated Session, or
External Specialist Engagement.

Git-mode `.dolgorae` MUST be at the canonical Git top level. `--non-git` is
rejected for a path inside any Git worktree, and a nested `.dolgorae` below an
already initialized workspace is rejected. Non-Git mode records an empty Git
baseline. Absence of `git`, a Git version older than 2.39, or a Git discovery
failure returns `WORKSPACE_INITIALIZATION_CONFLICT` rather than silently
falling back to non-Git mode.

The agent-writable workspace contains only portable project policy:

```text
<canonical-workspace>/.dolgorae/
  .gitignore
  config.yaml
```

`config.yaml` is strict YAML and contains exactly `schema_version: 1` and
`mode: git|non_git`. Unknown or duplicate keys, wrong types, unsupported schema
versions, and malformed YAML return `CONFIG_INVALID`. Dolgorae never rewrites
it after first initialization. `.dolgorae/.gitignore` contains exactly
`/exports/`; it MUST NOT ignore `.dolgorae` as a whole. Non-Git initialization
creates the same two files without claiming that they are tracked.

All machine-local configuration and mutable authority live outside the
canonical workspace at:

```text
~/Library/Application Support/Dolgorae/workspaces/<workspace-id>/
  workspace.json
  local.yaml
  specialist-policies/
  runs/
  runtime/
  orchestration/
  evidence/
  cache/
```

`workspace.json` binds the workspace ID to the lossless canonical path and
initialization mode. `local.yaml` is the mode-0600 Runtime Profile registry.
`specialist-policies/` is a current-uid-owned mode-0700 directory containing
mode-0600 checked JSON policy documents named `<policy-name>.json`. `runs/`,
`runtime/`, `orchestration/`, `evidence/`, and `cache/` contain only
Dolgorae-owned mutable state. The per-workspace state root is current-uid-owned
mode 0700, is verified local APFS, and is never placed in a Codex
`writableRoots` list or model-visible path. An agent may read the tracked project
policy files when the task requires it, but it cannot reach machine-local
profiles, Specialist Policies, or mutable Dolgorae authority through the
workspace sandbox.

Initialization uses create-exclusive temporary files, file `fsync`, rename,
and parent-directory `fsync` for both project policy and the Application
Support workspace record. Repeating `init` succeeds with `created:false` only
when mode, canonical workspace, workspace ID, schema, and existing policy files
are compatible. It never overwrites an existing tracked policy file. A partial
layout, nested workspace, changed mode, state-root identity conflict, or
incompatible policy returns `WORKSPACE_INITIALIZATION_CONFLICT`.

## SPEC-003: Profile, Account, and Singleton Binding

The earlier Writer Capsule candidate is preserved only in historical ADR and
review artifacts. `SPEC-014` is the sole current authority for execution-lane
cardinality, residency, server generations, profile lifecycle, and process
census.

The machine-local Runtime Profile registry lives at:

```text
~/Library/Application Support/Dolgorae/workspaces/<workspace-id>/local.yaml
```

The canonical workspace contains no mutable profile registry. Runtime Profile
configuration is execution, account, tooling, and process-static capability
configuration. Agent character is owned by the immutable Agent Configuration
resolved for each Run.

A Runtime Profile contains:

- a unique name;
- an absolute Codex executable and shell-free validated global argv;
- an absolute expected `CODEX_HOME`;
- the symbolic `profile_state_directory_v1` launch-cwd policy;
- an explicit non-secret environment map; and
- a required `native_subagents: enabled` acknowledgement.

A public profile MUST state `native_subagents: enabled` explicitly. Absence is
`PROFILE_CONFIG_INVALID`. `disabled` remains diagnostic-only because the pinned
runtime does not enforce it.

`local.yaml` is strict YAML with top-level `schema_version: 1` and a `profiles`
mapping keyed by name. Each entry contains nonempty `argv: [string, ...]`,
absolute `codex_home: string`, `environment: {string: string}`, and required
`native_subagents: enabled`. An absent policy is `PROFILE_CONFIG_INVALID`. An
explicit `disabled` value is rejected with
`NATIVE_SUBAGENT_DISABLE_UNAVAILABLE`; it is reserved to diagnostic probes and
is not a supported production profile contract. `argv[0]`
MUST be an absolute regular Codex executable; v1 rejects shell interpreters,
arbitrary wrappers, and argv that already contains an app-server subcommand.
Only the required-subset manifest's `profile_launch.global_arguments` are
allowed after `argv[0]`. V1 accepts canonical `--profile <name>`, repeatable
`--enable <feature>`, repeatable `--disable <feature>`, and flag-only
`--strict-config`; it rejects aliases, `--flag=value`, missing values, and every
other option. Normalization preserves argument and repetition order exactly.
The `multi_agent` Codex flag is reserved to Dolgorae and MUST NOT appear in raw
profile argv. Dolgorae injects exactly one canonical `--enable multi_agent`
pair. The `--disable multi_agent` form is diagnostic-only because the pinned
campaign observed that it did not prevent child creation. The corrected exact-version
campaign for an enabled 0.147.0 profile proved child identity, parent
relationship, active/terminal lifecycle, persisted history, restart continuity,
and cleanup, so it advertises lifecycle observation and quiescence tracking as
`supported`, while disable enforcement remains `unavailable`. Active or unknown
native state still blocks pause, physical-generation replacement, profile stop,
and shutdown. A disabled diagnostic result is recorded as `unverified`; it can
never be published as a usable profile capability.
For the 0.147.0 production profile, initialize MUST send
`optOutNotificationMethods:[]`. It MUST NOT suppress `item/started`,
`item/completed`, `thread/started`, turn lifecycle, or correlation methods.
Observed lifecycle suppression downgrades `native_subagents` to `unverified`
and rejects every quiescence-dependent operation. Reasoning content is
discarded after receipt; this pin does not use initialization suppression
because it cannot safely isolate reasoning-only methods from required native
lifecycle evidence.
Changing the policy changes the immutable launch contract and requires an
operator-authorized migration.
Environment names are explicit, require `PATH`, `LANG`, and `LC_ALL`, reserve `CODEX_HOME`, `HOME`,
`USER`, `LOGNAME`, `SHELL`, `TMPDIR`, and every `DOLGORAE_*` name to Dolgorae, and treat
all stored values as non-secret local configuration. Unknown or duplicate keys,
empty argv, relative homes,
wrong types, a missing required environment value, malformed YAML, and unsupported schema versions return
`PROFILE_CONFIG_INVALID`. Profile add/remove holds a per-workspace Application Support config lock and uses
write-temp, file `fsync`, rename, and directory `fsync`; the registry is
hand-editable and MUST NOT contain credentials, tokens, or other secrets.
The per-workspace Application Support root is mode 0700 and `local.yaml` is
mode 0600; creation and replacement reject a wrong-owner or more-permissive
file. That root is outside the agent-writable workspace.

Profile names are unique within one project. Every profile command MUST resolve
an initialized workspace through `--workspace` or normal upward discovery.
`profile add` MUST reject an existing name with
`PROFILE_ALREADY_EXISTS`; it MUST NOT overwrite a profile implicitly. Replacement
requires an explicit remove followed by add.

### Specialist Policy Registry

The machine-local Specialist Policy Registry lives at:

```text
~/Library/Application Support/Dolgorae/workspaces/<workspace-id>/specialist-policies/
```

Each entry is a create-exclusive `<policy-name>.json` file that validates
against
[`dolgorae-specialist-policy-v1.schema.json`](protocol/dolgorae-specialist-policy-v1.schema.json)
and its executable semantic validator. The content `policy_name` MUST match the
filename exactly. Files are current-uid-owned mode 0600, no-symlink regular
files, at most 1 MiB, and installed through a descriptor-relative temporary
file, file `fsync`, rename, and directory `fsync`. Unknown schema versions,
duplicate role references, unresolved Runtime Profiles, unavailable required
capabilities, write-capable roles without Dedicated Lane configuration, or
invalid collaboration activation are rejected before installation.

Policy add is create-exclusive. Replacement requires an explicit remove and
add; removing or replacing a registry entry never mutates an existing session
because every session stores the complete policy snapshot and digest. A new
Orchestration Launch Intent MUST name an installed policy. Policy resolution,
Agent Configuration validation against the current Runtime Profile capability
snapshot, and JCS hashing occur before root Run allocation. The resolved policy
name, revision, and digest participate in StartRun idempotency normalization.

V1 does not select a hidden default policy. Gul or another trusted interactive
client chooses a named policy when creating the protected Controller carrier.
A client MAY present simple presets such as approval-required and fully-
delegated, but each preset resolves to an explicit installed policy name.

Dolgorae MUST construct the singleton environment from the Runtime Profile definition,
not from the invocation that wins startup. It obtains `HOME`, `USER`, `LOGNAME`,
and `SHELL` from the current account record, obtains `TMPDIR` from the platform
user-temporary-directory API, sets canonical `CODEX_HOME`, copies only the
profile's explicit non-secret environment entries, and removes every other
inherited value. A diagnostic context marker MAY be present, but is advisory
and MUST NOT authorize or reject an operation. Controller and operator
capabilities are the only mutation authorities.

`PATH` is a colon-separated sequence of existing absolute directory paths;
empty, relative, `.` and duplicate components are invalid. `LANG` and `LC_ALL`
are explicit nonempty locale identifiers, and offline `profile doctor` verifies
that the selected locale is reported by the platform locale database. The three
exact values enter the immutable launch authority and profile state. Caller
`PATH`, virtual-environment variables and locale categories are never inherited.

`run start` MUST require an explicit Runtime Profile. Before allocating a Run,
Dolgorae MUST validate the executable, version, App Server schema,
initialization handshake, login readiness, model listing, actual `codexHome`,
and required capabilities. A `codexHome` mismatch is a hard failure. This
readiness check MAY start or reuse the shared Profile Server even for a
Dedicated Run; it MUST NOT start that Run's physical Dedicated Lane Server or
allocate its Codex thread.

Run creation stores a complete immutable Runtime Profile snapshot and a
separate immutable Agent Configuration snapshot, not only their digests. The
Runtime Profile snapshot contains exactly the profile name, canonical
`CODEX_HOME`,
normalized argv, `launch_cwd_policy`, derived concrete launch cwd, sanitized environment, enabled
and disabled features, normalized process-static configuration, initial configuration
observation, executable identity, Codex version, generated App Server schema
digest, compatibility-manifest digest, launch-contract digest, and initial
server key. It contains sufficient non-secret bytes and explicit-absence markers
to reconstruct the accepted launch contract after registry edit or deletion.
Existing Runs MUST NOT be rebound to another account or `CODEX_HOME`. The
Agent Configuration snapshot additionally records the accepted model, default
effort, purpose, required capabilities, role reference, normalized Controller
instructions, instruction digests, and Runtime Profile snapshot digest.
Different Agent Configurations MAY share one Runtime Profile and one compatible
Profile Server launch contract.

The launch-authority contract records
`launch_cwd_policy:"profile_state_directory_v1"`; it MUST NOT contain the
server-key-derived concrete path. The server key is computed first, after which
the current-uid-owned mode-0700 concrete cwd is derived as
`Dolgorae/profiles/<server-key>/` in Application Support. `PWD` is constructed
from that path and caller cwd is never inherited. Every launch and doctor check
verifies the derived path against the policy and recorded full server key. The launch
contract is the JCS object with exactly these keys: `schema_version`,
`canonical_codex_home`, `normalized_argv`, `launch_cwd_policy`,
`executable_identity` (resolved path/device/inode/SHA-256), `launch_mode`,
`sanitized_environment`, `process_static_configuration`, `codex_version`,
`app_server_schema_sha256`, `compatibility_manifest_sha256`,
`enabled_features`, and `disabled_features`. Environment and configuration object keys are JCS-sorted;
argv order is significant.

Configuration fields are classified by a checked closed manifest as
`process_static`, `operator_migratable`, `runtime_mutable`, or `ignored`.
Unknown fields and unclassified include mechanisms fail compatibility. Only
normalized process-static and explicitly accepted migratable fields enter the
launch contract. Runtime-mutated trust and operational state are recorded as an
initial observation but their enclosing file's raw digest MUST NOT enter
`server_key`. The key remains fixed for the lifetime; a later process-static
change requires operator migration rather than silently invalidating the live
server. Pinned probes own the classification and hot-reload verdict for every
field used by v1.

The singleton `server_key` is the domain-separated SHA-256 of that object, never
the profile display name or incidental caller state. A compatible profile
MUST reuse its live singleton across workspaces and runs. A different live
launch contract for the same canonical home MUST fail with
`PROFILE_LAUNCH_CONFLICT`; it MUST NOT start a second shared singleton or silently fall
back to a reader-per-run server. Run-owned Dedicated Lane Server generations
are the only additional App Server lifetimes. Stopped profile definitions with different contracts
MAY coexist, but only the contract selected for the next singleton lifetime
becomes active; another contract cannot start until the prior lifetime is
verified stopped and its membership reconciled.
Every singleton lifetime has a monotonically increasing `server_epoch`.

Dolgorae, not the official Codex daemon lifecycle, starts the singleton as the
validated profile argv followed by `app-server --listen
unix://<dedicated-socket>`. Authority files live below the platform Application
Support directory. A home-keyed root
`Dolgorae/homes/<home-key>/{home.lock,active.json}` uses domain-separated
SHA-256 of canonical `CODEX_HOME`; `active.json` records the only starting/ready
server key and epoch for that home. Contract state lives at
`Dolgorae/profiles/<server-key>/`. All components are current-uid-owned mode
0700/0600 and opened without symlink traversal.

The socket node instead uses the macOS-safe short path
`/tmp/dolgorae-<uid>/p/<server-token>.sock`, where `server-token` is uppercase
unpadded base32 of the first 160 server-key bits. The private root is validated
like worker sockets; full path and device/inode are recorded in server state.
The token is only a locator. An existing node is attachable only when profile
state proves the full 32-byte server key, Runtime Profile identity, canonical
home, launch-contract digest, epoch, socket device/inode, PID/PGID/UID/start
time, and executable identity all match. Any mismatch or unverifiable field is
`RUNTIME_PATH_COLLISION`; Dolgorae MUST NOT attach, unlink, signal, or infer
staleness.
The dedicated socket is never the default Codex control socket and MUST NOT be
shared with an unrelated client. The profile manager owns `server.lock`,
`state.json`, append-only `membership.jsonl`, derived `members.json`, and
`epoch`; it is lock-serialized logic in the Dolgorae binary, not an installed
daemon. Startup PREPARE holds `home.lock` before `server.lock`, validates and
persists a start operation token plus reserved epoch, fsyncs, then releases both
locks. APPLY spawns and probes without coordination locks. COMMIT reacquires the
same prefix, revalidates token/revisions/identity, and alone publishes ready.
Stop clears
the active contract only after verified process/socket termination and durable
membership reconciliation. Therefore different server keys for one home cannot
race separate locks into concurrent lifetimes.

The shared and dedicated App Servers are placed in their own process groups
with `/dev/null` for stdin and separate nonblocking stdout and stderr pipes to
their scoped Dolgorae log drainer. They MUST NOT inherit
the invoking CLI's descriptors or command-substitution pipes. The drainer is a
TASK-005 profile component, never a run worker or run descendant. It applies the
same secret/redaction exclusion as other diagnostics and owns mode-0600
`server.log` and `server.log.1`, each capped at 1 MiB with one rotation; an
individual diagnostic line is capped at 1 MiB and invalid UTF-8 is replaced
before redaction. A redaction failure drops the source line and writes only a
fixed marker. A file-sink failure keeps draining and discarding, records a
degraded profile state, and never backpressures the App Server. Its exact
process identity is profile state; missing or unverifiable drainer identity
fences new attachments until controlled profile recovery or restart.

A `shared_readonly` Run uses the shared Profile Server. A `dedicated` Run owns
one immutable logical lane whose physical Dedicated Lane Server may be absent
until first input and may later be replaced by a new generation in the same
lane. Every physical generation receives a unique `server_epoch`, short socket,
exact process/log-drainer identity, and append-only journal record. First write
starts or resumes the thread under verified writer policy only after durable
authority reservation; release changes policy in the same lane and never moves
the thread to the shared server. Dedicated descendants are sampled every 100
milliseconds and cleanup requires exact identity revalidation, five complete
empty samples after leader exit, and unrelated-process non-signalling as
specified by SPEC-014. The global
`--dangerously-bypass-approvals-and-sandbox` option remains forbidden because it
would bypass normalized approvals and effective-policy verification.

`state.json` records server key, launch-contract digest, canonical home,
process/executable/socket identity, lifecycle, compatibility verdict,
timestamp, server epoch, and membership revision. The directory-fsynced,
hash-chained `membership.jsonl` is the authoritative catalog of registration,
state-transition, and removal records across project roots. Registration is
appended before a run connection is published. `members.json` is an atomic
snapshot derived only through the journal revision/checksum and records each
workspace/run/controller, worker generation, thread, connection/lifecycle and
writer states, observed epoch, and runtime locator. Startup replays the journal,
validates every referenced manifest/runtime record, and rewrites the snapshot;
it never claims completeness by scanning incidental directories. A missing,
corrupt, or revision-mismatched journal is incomplete and blocks profile-global
operations. It is repairable only through the operator procedure below; there
is no automatic or force rebuild.
Each new server process
reserves and fsyncs a never-reused higher epoch before spawn, then publishes
`ready` only after identity, WebSocket initialize, compatibility, membership,
and directory fsyncs. Corrupt or unverifiable membership is incomplete and
profile-wide stop/restart MUST fail with `PROFILE_MEMBERSHIP_INCOMPLETE`.

Bare `profile doctor` performs static registry, executable, environment,
configuration and schema checks and MUST NOT start a singleton. `profile doctor
--launch-probe` may run the staged server probe; it stops only a server that it
started unless `--leave-running` is explicit. `profile server start` or `run
start` MAY start the singleton. `profile server stop|restart|migrate` MUST reject a
profile with live members unless the requested handling is explicit. Stop and
restart, migration, and membership repair always require the separate operator
capability; `--interrupt`
additionally requires exact `--confirm-server-key`. The manager serializes with
connection and recovery, proves membership complete, requests interruption of
every active turn, records each observed terminal or uncertain outcome, appends
an operator-override record to every affected run, and only then stops the
verified singleton. Restart advances `server_epoch`, invalidates every old
connection, and forces every affected run through reconciliation; it MUST NOT
infer turn failure from connection or server termination and MUST NOT
auto-resume runs. Controller capabilities and the server key do not authorize
this profile-wide operation.

Stop and restart use three phases. Fence acquires and revalidates operator,
home, and server locks, persists `stopping` or `restarting` plus a fresh quiesce
revision, rejects new attach/recovery/turn work, fsyncs, and releases all global
locks. Quiesce interrupts and classifies member turns without a global lock.
Commit-preparation reacquires and revalidates operator, home, and server locks
plus the same revision and exact server identity, persists a shutdown token,
then releases all locks. Shutdown APPLY sends `SIGTERM`, waits with an
identity-bound kqueue observation, enumerates exact PGID members, sends
`SIGKILL` only to revalidated survivors when necessary, proves group and log
drainer absence, and verifies the recorded socket inode without a coordination
lock. Final COMMIT reacquires the prefix and clears identity, unlinks only the
revalidated socket inode, and advances state after absence proof. Unknown turn,
member, process, group, or socket identity remains fenced and blocks a new
epoch.

`profile membership verify` replays the valid journal prefix and reports
orphans without mutation. `profile membership tombstone-orphan` requires the
operator credential and exact server-key, workspace-ID, and run-ID
confirmations; `profile state reset` additionally requires recorded singleton
absence. Repairs append and fsync a tombstone/audit record and new membership
revision, never delete history or fabricate a turn result. Run start failure,
close/delete, workspace relocation/deletion, and profile/server-key migration
each have an explicit append-only membership transition.

Version or executable acceptance is a profile-server migration. The operator-
only migration transaction stores an accepted generation contract containing
old/new server keys and epochs, versions, executable/schema/manifest digests,
migration ID, operator ID/generation, invocation ID, and timestamp. Allowed
drift is limited to Codex version, identity within the already approved direct
executable chain, generated schema, compatibility manifest, verified feature
availability, and fields classified `operator_migratable`. Home, account,
wrapper chain, global argv semantics, environment, launch cwd, and unapproved
features require a new run/profile definition.

Migration PREPARE acquires operator and home locks, then old/new server locks in
binary server-key order, fences old members and persists one migration token,
then releases all locks. APPLY proves the old lifetime absent, starts and
validates the new lifetime, and reconciles persisted threads without filesystem
coordination locks. COMMIT reacquires the same ordered prefix, revalidates the
token and both lifetimes, moves membership with the migration ID, and commits
`active.json`. A run is never an active member of both keys;
prepared duplicate records are projections of one home-authoritative migration.
Before new ready, failure stops the new process and retains old membership. If
the old contract cannot be restored, state remains `migration_blocked`; it does
not select either server implicitly.

## SPEC-004: Runtime and Session Identity

A run owns no Codex thread before its first turn and exactly one thereafter.
One live run generation owns exactly one worker and one private direct
WebSocket client connection to its selected physical generation: the shared
Profile Server for `shared_readonly`, or the same logical lane's current
Dedicated Lane Server generation for `dedicated`.
Dolgorae imposes no artificial run-count limit.

The required control path is:

```text
master
  -> dolgorae CLI (JSON on stdin/stdout)
  -> per-run worker (Unix domain socket)
  -> HTTP Upgrade + WebSocket frames over a dedicated Unix domain socket
  -> profile-scoped codex app-server singleton
  -> zero Codex threads before first turn; exactly one thereafter
```

The master MUST NOT connect directly to app-server. The worker is the sole
client of its WebSocket connection and audit interposer. Each connection performs
its own initialize handshake and subscription. It MUST NOT assume that the
server provides perfect connection isolation: every thread- or turn-scoped
object is routed only after exact root/accepted-descendant, turn, item, request,
server-key, and epoch checks. A foreign-thread object cannot mutate run state;
it records bounded profile diagnostic metadata. Profile-global notifications go
to profile diagnostics and are never attributed to the connection's run.
Unknown events are ignored only when the required-subset policy proves them
non-semantic; an event that could affect an active turn fails closed. The
CLI-worker socket MUST use a short
user-private runtime path derived from the canonical workspace identity and run
ID; durable state remains under `<application-support-workspace>/runs/`.

The actual worker socket node is the sole per-Run exception to Application
Support runtime storage and lives below `/tmp/dolgorae-<uid>/s/`; its identity authority
lives in `<application-support-workspace>/runtime/runs/<run-id>.json`. A live worker MUST detect a
missing socket pathname or private directory, safely recreate the private root,
bind a replacement listener, increment `control_socket_epoch`, and atomically
replace the runtime record without restarting its active App Server connection
or turn. Existing
accepted connections remain valid. A foreign occupant, unsafe root, or failed
rebind MUST interrupt an active turn and enter `RECOVERY_REQUIRED`; it MUST NOT
unlink an unverified socket.

Run IDs are UUIDv7 values. V1 has no run aliases and no current-run pointer.
Every run-scoped command MUST receive the run ID explicitly.

Every run manifest also records the controller binding, controller generation,
purpose, optional external label and parent reference, required capabilities,
and the profile capability snapshot accepted at creation. It additionally
stores an optional immutable `aggregate_binding` containing aggregate kind,
aggregate ID, operation ID, and, for a Specialist, role reference plus role and
Agent Configuration digests. The Orchestrated Session Primary binding names the
Aggregate Bootstrap Operation and policy digest; a brokered or external
Specialist binding names its spawn or hire operation. Generic low-level Runs
have no aggregate binding. This binding is cross-store reconciliation evidence,
not a replacement for SQLite aggregate authority, and must agree with the
reserved public parent projection when one exists. These fields are durable Run
identity; worker, connection, singleton, and CLI restart MUST NOT discard or
reinterpret them.

The CLI-worker handshake includes schema version, Dolgorae semantic version,
binary SHA-256, workspace/run identity, and expected run generation. A
mismatch returns `DOLGORAE_PROTOCOL_MISMATCH`; upgrade does not silently mix CLI
and worker versions within one run generation.

During `starting`, the worker or WebSocket connection may not yet exist. Once
started, the worker remains alive and normally retains its connection while the
run is idle, running, or waiting. Worker or connection loss does not prove turn,
command, native-subagent, or singleton termination. Logout, reboot, pause,
close, outcome-unknown quarantine, or failure may stop the worker. No launchd
recovery is installed. Every command that attaches or recovers a worker,
inspects writer authority, or verifies a prior generation performs the same
on-demand discovery/recovery procedure before its ordinary operation.

The WebSocket adapter MUST implement HTTP Upgrade, client masking, text and
continuation frames, ping/pong, close handshake, invalid-frame rejection, a
16 MiB frame limit, and a 32 MiB reassembled-message limit. It emits the same
normalized JSON-RPC objects consumed by Dolgorae's correlation layer. A lost
connection before a solicited request is written is retryable; loss after
possible acceptance or during a turn is non-retryable and enters ordinary
history reconciliation or `outcome_unknown` handling.

## SPEC-005: CLI Surface

The initial public command surface is:

```text
dolgorae [--human] --help
dolgorae [--human] --version
dolgorae [--human] init [PATH] [--non-git]
dolgorae [--human] serve --socket <absolute-private-socket-path> [--ready-fd <fd>]

dolgorae [--human] runtime capabilities
dolgorae [--human] engagement call --workspace <path> --request-fd <fd> (--controller-file <path> | --controller-fd <fd>) [--new-controller-file <path> | --new-controller-fd <fd>]
dolgorae [--human] controller credential create --kind <kind> --instance-id <id> [--subject-id <id>] [--orchestration-policy <name>] --output <new-path>
dolgorae [--human] operator credential initialize --output <new-path>
dolgorae [--human] operator credential rotate [--operator-file <path> | --operator-fd <fd>] --output <new-path>

dolgorae [--human] workspace inspect [--workspace <path>]
dolgorae [--human] workspace writer status [--workspace <path>]
dolgorae [--human] workspace writer reset [--workspace <path>] [--operator-file <path> | --operator-fd <fd>] --confirm-workspace-id <id> --require-worker-absence
dolgorae [--human] workspace writer handoff-prepare --workspace <path> --from <run-id> --to <run-id> --expected-generation <n> [--controller-file <path> | --controller-fd <fd>]
dolgorae [--human] workspace writer handoff-commit --workspace <path> --handoff-id <id> --expected-generation <n> [--controller-file <path> | --controller-fd <fd>]
dolgorae [--human] workspace writer handoff-cancel --workspace <path> --handoff-id <id> [--controller-file <path> | --controller-fd <fd>]

dolgorae [--human] profile add <name> [--workspace <path>] --codex-home <absolute-path> --native-subagents enabled [--env <name=value>]... -- <argv...>
dolgorae [--human] profile list [--workspace <path>]
dolgorae [--human] profile show <name> [--workspace <path>]
dolgorae [--human] profile remove <name> [--workspace <path>]
dolgorae [--human] profile doctor <name> [--workspace <path>] [--launch-probe [--leave-running]]
dolgorae [--human] profile server status <name> [--workspace <path>]
dolgorae [--human] profile server start <name> [--workspace <path>]
dolgorae [--human] profile server stop <name> [--workspace <path>] [--operator-file <path> | --operator-fd <fd>] [--interrupt --confirm-server-key <key>]
dolgorae [--human] profile server restart <name> [--workspace <path>] [--operator-file <path> | --operator-fd <fd>] [--interrupt --confirm-server-key <key>]
dolgorae [--human] profile server migrate <name> [--workspace <path>] [--operator-file <path> | --operator-fd <fd>] --confirm-old-server-key <key> --confirm-new-server-key <key> [--interrupt]
dolgorae [--human] profile membership verify <name> [--workspace <path>]
dolgorae [--human] profile membership tombstone-orphan <name> [--workspace <path>] [--operator-file <path> | --operator-fd <fd>] --confirm-server-key <key> --confirm-workspace-id <id> --confirm-run-id <id>
dolgorae [--human] profile state reset <name> [--workspace <path>] [--operator-file <path> | --operator-fd <fd>] --confirm-server-key <key> --require-server-absence
dolgorae [--human] profile diagnostics list <name> [--workspace <path>] [--after <cursor>] [--limit <n>] [--projection <minimal|operational>] [--operator-file <path> | --operator-fd <fd>]
dolgorae [--human] profile events <name> [--workspace <path>] [--after <cursor>] [--follow] [--projection <minimal|operational>] [--operator-file <path> | --operator-fd <fd>]

dolgorae [--human] specialist policy add <name> [--workspace <path>] --file <path>
dolgorae [--human] specialist policy list [--workspace <path>]
dolgorae [--human] specialist policy show <name> [--workspace <path>]
dolgorae [--human] specialist policy validate --file <path> [--workspace <path>]
dolgorae [--human] specialist policy remove <name> [--workspace <path>]

dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] start --workspace <path> --profile <name> --control-mode <direct-interactive|managed-agent> --execution-lane <shared-readonly|dedicated> --required-assurance <best-effort-personal-alpha|verified-thread-scoped-control|strong-process-containment> [--model <model>] [--effort <effort>] --purpose <purpose> [--purpose-label <label>] [--parent-namespace <value> --parent-kind <value> --parent-id <value>] [--require-capability <name>]... [--instructions <text> | --instructions-file <path> | --instructions-stdin] --idempotency-key <key>
dolgorae [--human] run list [--workspace <path>]
dolgorae [--human] run status <run-id> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] send <run-id> [--workspace <path>] [--write] [--message <text>] [--image <auto|low|high>=<path>]... [--effort <effort>] --idempotency-key <key> [--timeout <duration>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] submit <run-id> [--workspace <path>] [--write] [--message <text>] [--image <auto|low|high>=<path>]... [--effort <effort>] --idempotency-key <key>
dolgorae [--human] run wait <run-id> <turn-id> [--workspace <path>] [--timeout <duration>]
dolgorae [--human] run events <run-id> [--workspace <path>] [--after <cursor>] [--follow] [--projection <minimal|operational>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] timeline <run-id> [--workspace <path>] [--after <cursor>] [--limit <n>]
dolgorae [--human] run pending <run-id> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] interaction get <run-id> <request-id> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] respond <run-id> --request-id <id> --idempotency-key <key> [--workspace <path>] [--response-fd <fd>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] artifact show <run-id> <artifact-id> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] artifact read <run-id> <artifact-id> --offset <n> --length <n> [--workspace <path>]
dolgorae [--human] run (--controller-file <path> | --controller-fd <fd>) artifact export <run-id> <artifact-id> --output <path> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] interrupt <run-id> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] set-effort <run-id> <effort> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] acquire-write <run-id> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] release-write <run-id> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] pause <run-id> [--workspace <path>] [--interrupt]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] resume <run-id> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] recover <run-id> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] reconcile <run-id> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] fork --from <run-id> [--workspace <path>] [--fresh] [--model <model>] --idempotency-key <key>
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] create-write-continuation --from <run-id> --from-turn <turn-id> --reason <shared-readonly-source|access-transition-unavailable|access-transition-unverified> --purpose <purpose> [--purpose-label <label>] [--model <model>] [--effort <effort>] [--required-assurance <level>] [--require-capability <name>]... [--instructions-fd <fd>] [--handoff-summary-fd <fd>] [--artifact-ref <artifact-id>]... --idempotency-key <key> [--workspace <path>] (--new-controller-file <path> | --new-controller-fd <fd>)
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] close <run-id> [--workspace <path>] [--interrupt]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] delete <run-id> --confirm [--workspace <path>]
dolgorae [--human] run verify <run-id> [--workspace <path>]
dolgorae [--human] run (--controller-file <path> | --controller-fd <fd>) export <run-id> [--output <directory>] [--workspace <path>]
dolgorae [--human] run [--operator-file <path> | --operator-fd <fd>] controller reset <run-id> [--workspace <path>] --confirm <run-id> [--new-controller-file <path> | --new-controller-fd <fd>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] controller verify <run-id> [--workspace <path>]
```

`engagement call` is the Machine CLI carrier for the checked External
Specialist Facade schema. It reads exactly one JSON request from the protected,
non-TTY `--request-fd`, emits one ordinary machine envelope whose data validates
as the corresponding result variant, and never accepts engagement requests in
argv or environment variables. Every operation requires exactly one
`--controller-file` or `--controller-fd` carrying the aggregate-owner
Controller credential. `open_external_engagement` accepts only
`workflow_orchestrator` or `automation` and snapshots generation 1 into the
Aggregate Controller Binding. Every later operation revalidates the same
Controller ID, kind, normalized principal, and capability digest before any
observation or mutation. `--new-controller-file` or `--new-controller-fd` is
required only for `hire_external_specialist`, is forbidden for every other
operation, and carries the fresh Controller credential for the new Specialist
Run. The owner carrier and new-Run carrier are distinct protected inputs.
Trusted MCP adapters call the same semantic facade and checked payload contract.
V1 does not support in-place engagement-owner Controller transfer or rotation.

`run start` creates an empty idle Run and MUST NOT allocate a Codex
thread, acquire writer authority, start a physical Dedicated Lane Server, or
start the first turn. Every newly allocated Run begins with no writer authority.
A dedicated Run publishes its logical lane with `server_lane.state=absent`,
`thread_id=null`, and null physical generation/epoch/socket identity. First
`send`/`submit` starts the selected physical generation when needed, allocates
the thread, and starts the turn under one fsynced intent/idempotency transaction. Its
options include model, reasoning effort, and immutable run-specific
instructions. Instructions accept exactly one source: `--instructions`,
`--instructions-file`, or `--instructions-stdin`. They MUST NOT weaken Dolgorae's
hard agent invariants.

`run start` MUST bind a Controller credential and MUST receive explicit
`control_mode`, purpose, execution lane, and required assurance for every
Controller kind. `UNSPECIFIED`, omission, and hidden interactive defaults are
invalid. A user-facing Gul or external-AI facade MAY resolve these low-level
values from the selected use case, but it sends the complete normalized request
to the semantic service. A parentless `direct_interactive` root with checked Orchestration Launch Intent
atomically bootstraps its Orchestration Session as specified above. A root
without launch intent remains a low-level Run. A raw `managed_agent` start never
opens or joins an External Specialist Engagement; reserved aggregate parent
namespaces are accepted only from the authenticated internal Broker or External
Specialist Facade. Purpose, including its creation-time external label, is
immutable. Parent-reference arguments are all-or-none. Required
capabilities are checked before run allocation; an unavailable capability
returns `CAPABILITY_UNSUPPORTED` and leaves no run directory. Every subsequent
state-changing run command MUST present the bound credential through exactly
one of `--controller-file` or `--controller-fd`; credential bytes MUST NOT be
accepted in argv or the environment.

For an existing worker the CLI validates only carrier ownership, type, mode,
size, and syntax, then transfers the opened descriptor with `SCM_RIGHTS` over
the private worker socket. The request names run, command, invocation ID,
controller ID/generation, expected state revision, and idempotency key when
applicable. At the mutation serialization point the worker reloads authoritative
state, reads and hashes the bounded capability, compares controller ID,
generation, revision, and digest in constant time, and zeroizes the bytes before
applying the state transition. A CLI-side check or an unqualified
`already_validated` claim is never authoritative. Run creation performs the same
check in the byte-0 bootstrap owner before publishing the manifest.

A shared Run is published only after the shared Profile Server has a ready,
non-null epoch. A dedicated Run may use that same server only as pre-allocation
profile-readiness evidence. The dedicated Run is published after durable
logical-lane allocation while its own physical state remains absent; only a
Turn requires a ready non-null Dedicated Lane Server epoch. Failure to start a physical dedicated generation
leaves the logical Run idle and returns `DEDICATED_SERVER_START_FAILED`; it does
not fabricate a Turn or change lanes.

`run create-write-continuation` requires an exact current terminal Turn, no
active Turn, no unresolved interaction, and one closed creation reason:
`shared_readonly_source`, `access_transition_unavailable`, or
`access_transition_unverified`. The first reason requires a `shared_readonly`
source; the latter two require a dedicated source whose recorded transition
support or result has the named verdict. It creates a new Run ID and dedicated
lane. Workspace, profile, and control mode are inherited
and cannot be overridden. Model and effort may be overridden only when the
selected profile supports them. Required assurance defaults to the source and
may be retained or raised, never lowered; required capabilities are the union
of the source set and additions and are revalidated before allocation. The
source Controller authorizes creation, but the destination is bound to a new
same-principal per-Run Controller credential supplied through exactly one new
controller descriptor. Possession of the source secret does not authorize the
continuation after publication. It never mutates the source lane, source writer
authority, effective policy, or recovery evidence, and it never copies hidden
reasoning, source Controller instructions, native-subagent hidden history, or
writer authority. A blocked source writer therefore continues to block the
destination's later first-write attempt until independently reconciled. The
immutable lineage records source Run/thread/terminal-Turn IDs,
creation reason, source/destination Controller kinds, timestamp, workspace
baseline, at most 64 artifact references, and the SHA-256 of an optional UTF-8
handoff summary of at most 65,536 bytes. The continuation remains threadless and
physically absent until first input; its first instruction composition may
include the bounded summary, selected artifact references, and bounded
destination instructions read from `--instructions-fd`. Common, mode, and
purpose prefixes are recomposed; source Controller instructions and hidden
history are never copied. A different principal is rejected in v1.

`serve` is a supervised foreground command, not a finite semantic invocation.
It MUST bind only the supplied absolute Unix-socket path and MUST NOT accept a
TCP, remote-bind, or daemonization option. When `--ready-fd` is supplied it
writes exactly one bounded `rpc_server.ready` success or failure envelope and
closes that descriptor; otherwise it writes the readiness envelope to stdout
and performs no later stdout writes. Graceful SIGTERM exits zero after the
five-second drain contract. Startup collision, unsafe socket state, or a fatal
runtime failure exits with the mapped typed error and MUST NOT affect Runs,
workers, writer authority, Profile Servers, or Dedicated Lane Servers.

In JSON mode `--help` and `--version` emit ordinary success envelopes with
commands `help` and `version`; `--human` selects presentation-only text. A
syntax failure before command identification emits the ordinary failure
envelope with command `unknown`.

The two app-server requests are not claimed to be atomic. After `thread/start`,
Dolgorae appends and fsyncs the provisional thread ID before sending `turn/start`.
If the `turn/start` response is lost, recovery proves generation absence and
queries persisted history for that provisional thread. `thread/read` is
`Absent` only when both the pinned error code and the manifest-pinned
independent absence discriminator match. If the pinned profile exposes no stable
independent discriminator, this automatic-replay exception is disabled. A successful read
with no accepted turn proves `NotAccepted`. Every other error, malformed
response, timeout, unknown code, or unusable status is `Unreadable`. Dolgorae may
replace the thread and retry the reserved first-turn intent only for `Absent` or
`NotAccepted`. Any accepted/in-progress or `Unreadable` result follows ordinary reconciliation and may become
`outcome_unknown`; it is never retried. The permanent one-thread binding begins
when accepted history or the response supplies a turn ID. This is the sole
automatic retry exception because Codex history proves app-server did not
accept the turn.

`send` and `submit` accept exactly one text source: `--message` or stdin. Empty
text is rejected. If `--message` is absent, stdin is required and MUST NOT be a
TTY. `respond` accepts a JSON body only from exactly one protected inherited
`--response-fd` or non-TTY stdin; an interaction response body is never accepted
in argv. The bounded body is read into mutable protected buffers, never echoed,
logged or persisted as plaintext, and zeroized after upstream transmission where
the implementation language permits. Secret answers are excluded from all
externally visible idempotency normalization. Each
repeatable `--image` value starts with the required detail token `auto=`,
`low=`, or `high=`; everything after the first `=` is the literal readable
local path, so colons and detail-like filename suffixes are unambiguous. Dolgorae
stores the canonical path, detail, byte length, and streaming SHA-256 and MUST
NOT copy image bytes. The digest is part of idempotency normalization. Later
replay is permitted only while the file still matches those recorded facts;
change or disappearance makes acceptance uncertain and forbids replay.
`--idempotency-key` is an opaque, nonempty UTF-8 string scoped to
the run; the same key with byte-identical normalized input returns the original
accepted turn, while reuse with different input returns
`IDEMPOTENCY_CONFLICT`.

`<duration>` is a positive base-10 integer followed immediately by `ms`, `s`,
`m`, or `h`. Fractions, compound durations, zero, negative values, and values
greater than 24 hours are rejected with `INVALID_ARGUMENT`.

`run recover` explicitly performs generation discovery, identity validation,
verified process-group cleanup, and reconciliation that other commands may
perform on demand. It is allowed for a nonfinal run whose worker is unreachable
or whose prior generation is proved absent. A live worker that fails control
`hello` is not automatically taken over. V1 has no force override: a live
`Match` returns `RUN_BUSY`, while an unverifiable prior worker or recorded
background-execution uncertainty returns non-retryable `RECOVERY_REQUIRED`
without signalling or starting a replacement. A run worker never claims OS
ownership of Codex commands, native subagents, or other singleton descendants.
`fork --fresh` is the only explicit immediate escape; it creates a threadless
new run and retains source provenance without reading the source Codex thread.
It is always read-only and acquires no writer authority.

When a generation-starting command finds that the accepted launch contract
would produce another server key, it returns `PROFILE_MIGRATION_REQUIRED` with
old/new keys and the closed drift classification. A run controller cannot
accept that profile-wide change. Only the operator-authorized `profile server
migrate` command may append accepted generation contracts to every affected run
after the full compatibility gate and membership transaction succeed. A change
outside the migration allowlist returns `COMPATIBILITY_REJECTED` and requires a
new run or profile definition.

## SPEC-006: Machine Output and Turn Control

JSON mode is the default. A finite command writes exactly one newline-terminated
JSON object to stdout. Both success and structured failure use stdout; stderr is
reserved for unstructured diagnostics. `--human` is an optional presentation
layer and MUST NOT change semantics.

Success envelope:

```json
{
  "schema_version": 1,
  "ok": true,
  "command": "run.status",
  "invocation_id": "019...",
  "data": {}
}
```

Failure envelope:

```json
{
  "schema_version": 1,
  "ok": false,
  "command": "run.send",
  "invocation_id": "019...",
  "error": {
    "code": "RUN_STATE_CONFLICT",
    "message": "human-readable diagnostic",
    "retryable": false,
    "details": {}
  }
}
```

JSON field names use `snake_case`. Machine-output schema version 1 is closed:
producers MUST NOT add fields and consumers MUST reject unknown fields. Any
additive, removed, or meaning-changing producer field requires a new schema
version. This is distinct from the Codex-input compatibility rule, where Dolgorae
records and tolerates unknown additive app-server data.

Before TASK-001 begins, the unreleased v1 artifacts may be corrected together
as part of a stabilization Task. After the first production implementation,
each successor uses a new `$id` and integer `schema_version`; producers emit
one version, consumers inspect `schema_version` before any other field and
reject unsupported versions, and there is no implicit dual emission. Export
verification retains readers for every released bundle version. A newer binary
opening an unsupported on-disk run, audit, or hash version fails closed and
requires the matching binary; v1 defines no in-place migration.

The checked [machine-output schema](protocol/dolgorae-machine-v1.schema.json),
[error contract](protocol/dolgorae-error-contract-v1.json),
[client-event-record schema](protocol/dolgorae-event-record-v1.schema.json),
[event-delivery schema](protocol/dolgorae-event-delivery-v1.schema.json),
[interaction schema](protocol/dolgorae-interaction-v1.schema.json),
[capability schema](protocol/dolgorae-capabilities-v1.schema.json), and
[controller-credential schema](protocol/dolgorae-controller-credential-v1.schema.json),
[operator-credential schema](protocol/dolgorae-operator-credential-v1.schema.json),
[artifact schema](protocol/dolgorae-artifact-v1.schema.json),
[Controller timeline schema](protocol/dolgorae-timeline-v1.schema.json),
[RPC mutation policy](protocol/dolgorae-rpc-mutation-policy-v1.json),
[gRPC client policy](protocol/dolgorae-grpc-client-policy-v1.json),
[gRPC error map](protocol/dolgorae-grpc-error-mapping-v1.json),
[Protobuf source](protocol/dolgorae/public/v1/dolgorae.proto), and
[descriptor manifest](protocol/dolgorae-public-v1.descriptor.json)
are normative.
`command` is a closed dotted
subcommand enum and `invocation_id` is a UUIDv7. `data` is a command-tagged
union built from these reusable objects:

- `workspace`: workspace ID, lossless canonical path, mode, and `created`;
- `capabilities`: Dolgorae protocol versions, transports, projection profiles,
  stable feature flags, and profile-specific interaction support;
- `profile`: name, argv, expected/actual `codex_home`, launch cwd,
  executable/version/schema digests, compatibility verdict, models,
  diagnostics, `server_key`, `server_epoch`, membership state, accepted
  generation contracts, and its validated capability snapshot;
- `run`: workspace/run IDs, lifecycle/access, `server_epoch`, `run_generation`,
  `control_socket_epoch`, profile, thread/active
  turn when present, controller, purpose, parent reference, model/effort,
  ledger cursor, pending count, writer state, writer-authority generation, identity verdict,
  recovery projection, and last terminal result;
- `turn`: thread/turn IDs, status, model/effort, usage, cursor, response, and
  bounded `workspace_changes`;
- `interaction`: generation-qualified ID, closed normalized kind, bounded
  client-safe payload, available decisions, status, and namespaced response
  schema;
- `writer`: current writer and handoff eligibility or a prepared/committed
  generation-bound handoff;
- `verification`: checked head/hash, canonicality, repaired-tail evidence,
  projection status, and `verification_failed`;
- `export`: output path, bundle schema version, included filenames and
  verification result.

Commands returning collections use `items` of the relevant object. Lifecycle
commands return the resulting `run`; `send` returns terminal `turn`, `submit`
returns accepted `turn`, and `wait` returns the same `turn` shape whether it is
terminal or still waiting. Every error code has a required `details` schema;
irrelevant details are forbidden. `--human` is explicitly not machine-parseable
and carries no compatibility guarantee.

`workspace_changes` is present on every turn and includes `measured`. It is
`false` with empty paths on accepted, running, or waiting turns and `true` only
after terminal post-turn observation; an empty unmeasured value is not proof of
no workspace changes.

Bare `profile doctor` returns `ok:true` whenever its offline checks ran, with the verdict in
`data.compatibility` and every failure/warning in diagnostics. It emits a
failure envelope only when the check itself could not execute. Profile
add/list/show do not execute a profile and therefore report validation-derived
fields as `unknown`, null, or empty as their schema permits.
`--launch-probe` uses the profile start PREPARE/APPLY/COMMIT protocol. If no
server was running, successful or failed probing performs a staged verified stop
before return unless `--leave-running` was given. It never stops a server that
predated the invocation.

`retryable` means the identical invocation may be safely issued again unchanged;
it does not promise progress or absence of prior side effects. A request rejected
before intent reservation or external write with `RUN_BUSY` or `WRITER_BUSY` is
retryable. Validation, policy, compatibility, integrity, state, stale-request,
and outcome-unknown errors are not. `TRANSPORT_FAILURE` is retryable only when
the operation made no external write, or reconciliation using its idempotency
key proves non-acceptance. Any uncertain acceptance emits `false`.

CLI-worker frames are limited to 8 MiB, App Server WebSocket frames to 16 MiB,
complete reassembled WebSocket messages to 32 MiB, stderr diagnostic lines to
1 MiB, and raw app-server payloads
selected for ledger representation to 2 MiB before marker escaping/redaction.
The post-transform representation allowance is 3 MiB. The terminating newline
is excluded. SHA-256 always covers the exact raw wire payload bytes before any
transform. Detection is streaming: count and hash while discarding beyond the
applicable bound. An oversized CLI
frame returns `PROTOCOL_FRAME_TOO_LARGE` only to that caller and never changes
run state. Oversized stderr records bounded metadata and continues. Oversized,
invalid, duplicate-member, or otherwise unrepresentable unsolicited messages record bounded
metadata, closes the affected connection, and moves an accepted active turn to
`outcome_unknown`; it never asserts `TURN_FAILED`. Observer delivery MUST read
fsynced ledger records by cursor; a slow or disconnected observer MUST NOT
delay draining app-server output or block the active turn.

A JSON-RPC object containing both `method` and `id` is classified first as a
server request; it is never a solicited response. App-server request IDs and
Dolgorae-originated request IDs occupy independent correlator maps, so a numeric
collision cannot change that precedence. A solicited
`thread/read(includeTurns: true)` response is parsed by a
constant-memory streaming visitor. Before byte 16 MiB the visitor MUST yield a
unique top-level `id` matching an outstanding `thread/read`; a top-level
`method` without `id` proves an unsolicited notification. An ambiguous prefix that reaches
16 MiB is never classified from the number of outstanding requests: it fails
the compatibility/transport check and closes that connection. An
accepted active turn is quarantined; a transient read fails only its caller.
After the matching ID is observed, the visitor retains the addressed
`Thread.id`, `parentThreadId`, ordered `Turn.id/status/items/itemsView`, and for
each semantically relevant item its ID, type, status and root/descendant
ownership evidence. Completed root `agentMessage` phase and text are selected in
array order; text beyond 1 MiB streams directly to the artifact writer. Command
items retain status/process/background evidence, file-change items retain the
latest approval snapshot fields, and interaction items retain request
correlators. It also retains raw byte length and streaming SHA-256; it has
the 120-second operation deadline but no arbitrary total-response cap. This is
the sole normative exception to the ordinary 32 MiB reassembled-message cap;
it applies only to a solicited, early-ID-confirmed `thread/read(includeTurns:true)`
response parsed by that streaming visitor. `itemsView` must be the pinned
complete value before absence of an item class is evidence; missing or partial
views never prove no command, response, file change, or interaction. Timeout, malformed
structure, or an unusable status fails only the requesting recovery/reconcile
command and leaves lifecycle state unchanged. Compatibility for every profile
generation therefore includes the early-ID behavioral probe; JSON Schema alone
cannot establish object-member order. For a complete invalid JSON line,
an envelope-only scanner may extract a unique top-level response ID solely to
scope the failure; extracted content is never used semantically. A truncated or
active-correlated line remains fail-closed generation quarantine.

An asynchronous redaction or representation failure appends
`payload_unrepresentable` even when no command is waiting. A later command that
requires the missing semantic payload returns `REDACTION_FAILURE`; pure
inspection still reports the metadata. Duplicate JSON members are protocol
violations rather than last-wins input. Non-UTF-8 stderr is never decoded into
the ledger: only its byte length and streaming digest are retained.

Exit status classes are stable:

| Exit | Meaning |
| ---: | --- |
| 0 | Successful operation or expected nonterminal control state |
| 2 | CLI syntax or input validation failure |
| 3 | Workspace, profile, run, turn, or request not found |
| 4 | State/serialization conflict, policy rejection, recovery precondition, or writer conflict |
| 5 | Codex compatibility, profile validation, or Dolgorae protocol-version failure |
| 6 | Worker, app-server, transport, or internal runtime failure |
| 7 | A `send` or `wait` request observed its turn end failed or interrupted |
| 8 | Audit, artifact, or durable run-state integrity verification failure |

Exit statuses outside `{0,2,3,4,5,6,7,8,130}` carry no machine envelope and
MUST NOT be interpreted as semantic Dolgorae error codes. Exit 130 is reserved
for caller SIGINT and likewise carries no final envelope.

Stable semantic error codes, exit classes, emitters, and conditions are
normative:

| Code | Exit | Emitting commands | Condition |
| --- | ---: | --- | --- |
| `INVALID_ARGUMENT` | 2 | any command | CLI syntax, input source, duration, effort, response JSON, incompatible option combination, or pre-existing export destination is invalid |
| `WORKSPACE_NOT_INITIALIZED` | 3 | all commands except `init` and profile-only commands | the addressed path has no valid Dolgorae workspace |
| `CONFIG_INVALID` | 3 | workspace commands | `config.yaml` is malformed, unsupported, duplicated, or has an unknown/wrongly typed key |
| `PROFILE_CONFIG_INVALID` | 3 | profile commands and `run start` | `local.yaml` is malformed, unsupported, duplicated, or has an unknown/wrongly typed key |
| `PROFILE_NOT_FOUND` | 3 | profile commands and `run start` | the profile name is absent |
| `RUN_NOT_FOUND` | 3 | every `run` command except `start/list` | the run ID is absent in the selected workspace |
| `THREAD_NOT_FOUND` | 3 | `run resume/send/submit/wait/recover/reconcile` and history-copying `run fork` | the pinned Codex history required by the operation is absent; never emitted by `fork --fresh` |
| `TURN_NOT_FOUND` | 3 | `run wait` | the turn ID is absent from both ledger and Codex history |
| `INTERACTION_NOT_FOUND` | 3 | `run respond` | the interaction ID is absent |
| `INTERACTION_FULL_PAYLOAD_REQUIRES_CONTROLLER` | 4 | `run interaction get` | an observer attempted to fetch the full normalized interaction |
| `INTERACTION_ARTIFACT_REQUIRES_CONTROLLER` | 4 | artifact show/read | an observer attempted to fetch a controller-only interaction artifact |
| `OBSERVER_MUTATION_FORBIDDEN` | 4 | state-changing run commands | an observer attempted a Controller mutation without presenting the bound credential |
| `PROFILE_ALREADY_EXISTS` | 4 | `profile add` | the name already exists; replacement is never implicit |
| `PROFILE_SERVER_BUSY` | 4 | profile lifecycle and run connect/recovery | another serialized profile operation or active-member condition temporarily blocks the command |
| `PROFILE_LAUNCH_CONFLICT` | 4 | `profile doctor/server start`, `run start/resume/recover/fork` | the canonical `CODEX_HOME` already has a different active launch contract |
| `PROFILE_MIGRATION_REQUIRED` | 4 | generation-starting run commands | compatible accepted drift changes the shared server key and requires operator-authorized profile migration |
| `PROFILE_MEMBERSHIP_INCOMPLETE` | 4 | profile lifecycle, migration, and membership repair | the manager cannot prove its durable membership journal complete; details name the operator repair action when available |
| `PROFILE_SERVER_EPOCH_MISMATCH` | 4 | run attach/recovery and writer operations | a connection or writer record names a different selected shared/dedicated lane-server epoch |
| `WORKSPACE_INITIALIZATION_CONFLICT` | 4 | `init` | re-init, nesting, Git mode, partial-layout, or policy-file facts conflict |
| `RUN_STATE_CONFLICT` | 4 | all state-changing `run` commands | the lifecycle state forbids the requested transition |
| `RUN_STATE_INVARIANT_VIOLATION` | 8 | persistence and projection of run state | the executable run-state semantic validator named by SPEC-014 rejects a cross-field invariant; schema-only acceptance is never sufficient |
| `POLICY_REJECTED` | 4 | policy-sensitive commands | workspace or hard-agent policy rejects the operation |
| `RUN_BUSY` | 4 | state-changing run commands | another turn owns turn-start serialization, or another contender owns per-run worker startup/attachment serialization |
| `WRITER_BUSY` | 4 | `run send/submit --write`, `run acquire-write` | durable workspace authority names another run or is blocked unknown; details identify the holder and whether a same-controller idle handoff may be prepared |
| `WRITER_HANDOFF_NOT_ALLOWED` | 4 | writer handoff commands | source, destination, or workspace state blocks safe handoff |
| `CROSS_CONTROLLER_RELEASE_REQUIRED` | 4 | writer handoff commands | source and destination have different controllers |
| `STALE_WRITER_GENERATION` | 4 | writer handoff commands | a prepared handoff no longer matches writer or run generations |
| `CONTROLLER_MISMATCH` | 4 | controller-authorized mutation | the supplied capability does not own the run |
| `CONTROLLER_RESET_NOT_ALLOWED` | 4 | `run controller reset` | active work, a pending interaction, handoff, or unverifiable writer state blocks reset |
| `OPERATOR_MISMATCH` | 4 | operator credential rotation, profile stop/restart, controller reset, `workspace writer reset` | the supplied separate local operator capability is absent, stale, or invalid |
| `CONTROL_MODE_REQUIRED` | 2 | `run start` | `control_mode` is omitted or `UNSPECIFIED`; no Controller kind inherits a hidden mode |
| `CONTROL_MODE_CONTROLLER_MISMATCH` | 4 | `run start`, `run create-write-continuation` | the controller kind is not permitted by the requested control mode, or is `other` |
| `PURPOSE_REQUIRED` | 2 | `run start` | purpose is omitted or `UNSPECIFIED` for any Run |
| `EXECUTION_LANE_REQUIRED` | 2 | `run start` | execution lane is omitted or `UNSPECIFIED`; no interactive default exists |
| `CAPABILITY_UNSUPPORTED` | 4 | `run start`, projection and interaction commands | a required Dolgorae or profile feature is unavailable |
| `NATIVE_SUBAGENT_DISABLE_UNAVAILABLE` | 4 | profile add/update/doctor | the public profile requests disable enforcement that pinned Codex 0.147.0 did not provide |
| `ACCESS_TRANSITION_UNSUPPORTED` | 4 | write acquire/release and handoff | the tested profile cannot safely apply the requested policy to the existing thread; create a lineage-linked write continuation |
| `BACKGROUND_EXECUTION_UNVERIFIED` | 4 | writer release/handoff/close, `workspace writer reset`, and recovery | the Dedicated lane-generation census or exact cleanup cannot prove the supported process scope empty |
| `IDEMPOTENCY_CONFLICT` | 4 | `run start/fork/send/submit/respond/create-write-continuation` | an operation-scoped key was reused with different normalized input |
| `INTERACTION_ALREADY_RESOLVED` | 4 | `run respond` | another valid response already won |
| `INTERACTION_STALE` | 4 | `run respond` | the interaction belongs to an older or cleared generation |
| `INTERACTION_RESPONSE_INVALID` | 4 | `run respond` | the response does not satisfy the recorded normalized schema |
| `INTERACTION_RESPONSE_TOO_LARGE` | 4 | `run respond`, `ResolveInteraction` | the raw response body exceeds the advertised 1-MiB pre-parse bound |
| `INTERACTION_PAYLOAD_TOO_LARGE` | 4 | `run interaction get`, `GetControllerInteraction` | the typed safe payload exceeds the advertised 8-MiB encoded bound and is not returned |
| `INTERACTION_OUTCOME_UNKNOWN` | 4 | Interaction lost-response reconciliation | authorized snapshots cannot determine whether the resolution committed; protected input must not be replayed automatically |
| `WRITE_CONTINUATION_PROFILE_OVERRIDE_FORBIDDEN` | 4 | `run create-write-continuation` | the request attempts to change inherited workspace, profile, or control mode |
| `WRITE_CONTINUATION_MODEL_UNSUPPORTED` | 4 | `run create-write-continuation` | the requested destination model or effort is not supported by the inherited profile snapshot |
| `WRITE_CONTINUATION_SOURCE_NOT_TERMINAL` | 4 | `run create-write-continuation` | the named source Turn is absent, nonterminal, stale, or no longer current |
| `WRITE_CONTINUATION_LINEAGE_INVALID` | 4 | `run create-write-continuation`, projection validation | source existence, workspace identity, fresh Run/thread identity, creation reason, or immutable lineage validation failed |
| `WRITE_CONTINUATION_CONTROLLER_INVALID` | 4 | `run create-write-continuation` | the destination credential is reused, belongs to a different principal, or has an incompatible Controller kind |
| `EXECUTION_LANE_UNSUPPORTED` | 4 | `run start`, `run create-write-continuation` | the selected profile cannot host the requested execution lane |
| `EXECUTION_LANE_IMMUTABLE` | 4 | state-changing run commands | an operation attempted to change a run's recorded execution lane |
| `SHARED_RUN_WRITE_FORBIDDEN` | 4 | `run send/submit --write`, `run acquire-write` | a `shared_readonly` run requested write; a lineage-linked write continuation is required |
| `THREAD_RESIDENCY_CONFLICT` | 4 | run attach/recovery/reconcile and writer operations | a thread was observed outside its immutable logical lane |
| `SAME_HOME_MULTI_SERVER_UNSAFE` | 5 | `profile doctor`, `run start`, and lane-starting run commands | the pinned same-home shared/dedicated coexistence campaign did not pass for this profile |
| `ASSURANCE_LEVEL_UNAVAILABLE` | 4 | `run start`, `run create-write-continuation` | `required_assurance` is `UNSPECIFIED` or exceeds the profile snapshot's achievable level; a complete supported value is required before allocation |
| `DEDICATED_HISTORY_BARRIER_FAILED` | 4 | `run resume/recover/reconcile` | the persisted history revision/digest barrier did not admit a successor generation in the same logical lane |
| `PROFILE_LANE_MIGRATION_REQUIRED` | 4 | `profile server migrate` | a live lane is incompatible with the requested generation contract and the complete lane set requires operator migration |
| `DEDICATED_SERVER_START_FAILED` | 6 | `run send/submit`, `run resume` | a physical Dedicated Lane Server generation could not start; the logical run stays idle and the same lane may be retried |
| `FILE_CHANGE_ARTIFACT_UNAVAILABLE` | 4 | `run pending/respond` | the exact correlated proposed change cannot be represented or its artifact is missing, oversized, or digest-stale |
| `ARTIFACT_NOT_FOUND` | 3 | artifact show/read/export | the artifact ID is absent or does not belong to the addressed Run |
| `ARTIFACT_RANGE_INVALID` | 2 | artifact read | offset/length is outside the artifact or the 1-MiB call bound |
| `ARTIFACT_INTEGRITY_FAILURE` | 8 | artifact show/read/export and run verify | stored bytes do not match authoritative artifact metadata |
| `PROJECTION_PROFILE_UNSUPPORTED` | 4 | `run events` | the requested client-safe projection is unavailable |
| `OUTCOME_UNKNOWN` | 4 | state-changing run commands | the run is quarantined; this code takes precedence over `RUN_STATE_CONFLICT` |
| `RECOVERY_REQUIRED` | 4 | writer acquire/release, `workspace writer reset`, and lifecycle/recovery commands | prior same-run identity or a `blocked_unknown` workspace writer generation cannot be proved safe; new reader runs and projection-only commands are excluded and the code is never generically retryable |
| `PROFILE_MISMATCH` | 5 | all commands that start or reconnect a worker/app-server | executable, `CODEX_HOME`, account, or immutable profile identity differs from the manifest |
| `COMPATIBILITY_REJECTED` | 5 | `profile doctor`, `run start/send/submit/set-effort/resume/recover/reconcile/fork/create-write-continuation` | version, model, effort, schema, login, sandbox, or app-server capability validation fails |
| `DOLGORAE_PROTOCOL_MISMATCH` | 5 | every ordinary command connecting to an existing worker | workspace/run/generation identity or mutation protocol differs, or Dolgorae version/binary digest differs; retry `hello`, bounded `status`, or `shutdown` through control protocol v1 |
| `PROTOCOL_VERSION_UNSUPPORTED` | 5 | machine commands | the caller requests an unsupported machine or event schema version |
| `UNSUPPORTED_SCHEMA_VERSION` | 5 | gRPC requests and projections | the caller supplies an unknown input enum or unsupported projection/detail schema version |
| `TRANSPORT_FAILURE` | 6 | every command that contacts a worker or singleton | Unix socket, WebSocket, or protocol transport fails |
| `OPERATION_TIMEOUT` | 6 | `profile doctor` and run commands performing local replay/schema work | a bounded local operation expired without uncertain external acceptance |
| `PROTOCOL_FRAME_TOO_LARGE` | 6 | every command receiving a CLI-worker frame | the CLI-worker request or response frame exceeds its byte limit |
| `RUNTIME_PATH_INVALID` | 6 | `init` and every run command that starts or attaches a worker | private lock/runtime root or socket path fails validation |
| `RUNTIME_PATH_COLLISION` | 6 | `init` and every run command that starts or attaches a worker | a recorded root, existing short path, lock inode, or runtime socket record belongs to different identities |
| `REDACTION_FAILURE` | 6 | any command that would append an unclassifiable wire payload | safe redaction or serialization cannot be proved |
| `INTERNAL_ERROR` | 6 | any command | an otherwise unmapped invariant or runtime failure occurs |
| `TURN_FAILED` | 7 | `run send/wait` | the addressed turn reaches failed terminal state |
| `TURN_INTERRUPTED` | 7 | `run send/wait` | the addressed turn reaches interrupted terminal state |
| `AUDIT_INTEGRITY_FAILURE` | 8 | `run verify/export/events/status` and any state-changing run command after ledger validation | newline-terminated record structure, sequence, or hash validation fails |
| `THREADLESS_REQUIRES_WRITE_TURN` | 4 | `run acquire-write`, `AcquireWriter` | a threadless dedicated Run must perform its first write through `SubmitTurn(write_intent=WRITE)` |
| `SLOW_CONSUMER` | 4 | `WatchRunEvents` | one Run stream exceeded its bounded delivery queue or stall deadline; resume from the client's last committed cursor |
| `EVENT_CURSOR_INVALID` | 2 | `run events/timeline`, `WatchRunEvents`, `ListRunTimelineItems` | the cursor is noncanonical or beyond the authoritative Run ledger head; projection gaps are never this error |
| `RPC_SERVER_ALREADY_RUNNING` | 4 | `serve` | the installation-scoped gateway lock is held by a matching live server |
| `RPC_SOCKET_UNSAFE` | 6 | `serve` | the requested socket path, parent, existing node, ownership, mode, or stale identity cannot be proved safe |
| `SERVER_SHUTDOWN` | 6 | public gRPC calls | graceful gateway shutdown ended an admitted call or stream without changing Run state |

`RUN_BUSY` means another turn is already active or won the serialized turn-start
race, or another process owns the run's worker startup/attachment serialization.
`RUN_STATE_CONFLICT` means the run's lifecycle state forbids the requested
operation. Every command specification and conformance fixture MUST use this
table rather than assign an exit class locally.
“State-changing run command” means start, send, submit, respond, interrupt,
set-effort, acquire-write, release-write, pause, resume, recover, reconcile,
fork, create-write-continuation, close, delete, controller reset, or writer
handoff. Status, list, wait, events, pending, writer status, controller verify, and
verify are observer operations. Timeline, artifact export, and whole-Run export
require immediate Controller authorization. Whole-Run export embeds a failing
verification result rather than refusing. Confirmed delete is
the explicit integrity-failure escape described in SPEC-010.
For a write recovery path, same-run identity safety is evaluated before
writer-authority activation: `RECOVERY_REQUIRED` therefore takes precedence
over `WRITER_BUSY` when both could apply.
For the same run, an answering control channel wins over startup-lock
observation. Otherwise prior-generation identity is evaluated before startup
serialization: `RECOVERY_REQUIRED` precedes `RUN_BUSY`; when identity is safe
but another byte owner wins, `RUN_BUSY` is emitted.

`run events` and `profile events` are the only command families that emit
multiple JSON objects. Run and profile cursor domains are independent.
`--after` defaults to the string `"0"` and accepts the canonical unsigned
decimal ledger sequence string without leading zeroes. The string representation
avoids JSON number precision loss and remains stable
through torn-tail repair; `projection_rewound` never renumbers committed
records. `--projection` defaults to `minimal`. Projection membership is closed:

| Event | Minimal | Operational |
| --- | --- | --- |
| `run.state_changed`, `turn.state_changed`, `response.final` | Yes | Yes |
| `interaction.opened`, `interaction.resolved`, `runtime.error` | Yes | Yes |
| `writer.state_changed`, `recovery.required` | Yes | Yes |
| `usage.reported`, `workspace.changes` | No | Yes |
| `command.started`, `command.completed` | No | Yes |
| `diagnostic.reported`, `generation.changed`, `reasoning.suppressed` | No | Yes |

The event-delivery schema enforces this matrix. Minimal never contains command
argv/output, diagnostics, raw changes/diffs, generation internals, or reasoning
metadata. A durable client-event record contains cursor,
identity, server key/epoch, type, and its discriminator-bound payload; it never
contains reader projection or replay state. Delivery wraps that record with
`projection` and `replay`. Heartbeat/end envelopes carry their own kind and
cursor. Public events MUST NOT contain an audit record, raw ledger line, raw
app-server payload, reasoning text or summary, raw diff, or unbounded command
output. Different projection profiles filter the same cursor domain, so cursor gaps are
valid and reconnect remains exclusive by the last observed cursor.

Every `turn.state_changed`, `response.final`, `interaction.opened`,
`interaction.resolved`, `command.started`, `command.completed`, and
`workspace.changes` record requires non-null exact root `thread_id` and
`turn_id`. `run.state_changed` and run-scoped runtime/recovery records may omit a
turn only where their discriminator branch explicitly permits it.
`writer.state_changed` requires the affected workspace and writer run when a
writer exists. A profile-global warning or foreign-thread diagnostic is never a
Run event and uses the separate profile diagnostic schema.
Without `--follow`, Dolgorae emits records through the head captured at command
start, then one `end` frame and exits 0, including when there are no records.
With `--follow`, while caught up, a nonfinal run emits a heartbeat every 30 seconds containing
the current cursor and state. After `closed` or `start_failed` is caught up it
emits `end` and exits 0. A midstream failure emits one ordinary error envelope
and exits with its mapped status. Caller disconnect or SIGINT exits 130 without
altering the run. A successfully accepted `run interrupt` command itself
returns exit 0; exit 7 describes a caller that requested the interrupted turn's
result.
Because an exit-7 failure envelope is intentionally minimal, a Master needing
response, usage, cursor, or measured workspace changes reads
`run status.data.last_terminal` after the failed or interrupted turn.

`run timeline` and `ListRunTimelineItems` expose a Controller-authorized
`controller_timeline` projection version 1. They use the Run ledger cursor
domain and exclusive `after_cursor`, default to 100 items, reject limits above
500, and return the captured head plus a nullable next cursor. Filtered cursor
gaps are valid. The only item types are `user_input.accepted`,
`assistant_response.final`, `interaction.opened`, `interaction.resolved`, and
`turn.terminal`. Each item carries Run/Turn identity, ledger cursor, UTC time,
and provider item ID/order when supplied. Ordering follows authoritative
provider item order normalized into the ledger, never notification arrival
order.

The accepted user input record is fsynced before `submit` acknowledges the
Turn. UTF-8 text at most 1 MiB is inline; larger accepted text up to the 8-MiB
public-request bound is streamed to a `user_input` Controller-only artifact.
Image inputs retain only caller order, detail, media type, raw byte length, and
SHA-256. Their source path and bytes are not retained for timeline display.
Interaction timeline items contain typed `interaction_kind`, typed
`interaction_status`, bounded `interaction_safe_title`, and identity only;
the full payload remains available solely through the separately authorized
interaction operation. Timeline records MUST NOT contain hidden reasoning,
internal planning, private worker payloads, raw tool payloads, private transport
frames, hidden native-subagent history, credential material, or an internal
artifact path.

Profile diagnostics have a separate append-only journal and canonical decimal
cursor. `profile events` and `profile diagnostics list` never reuse or advance a
Run cursor. Minimal profile events are same-uid readable, redacted, bounded, and
contain only lifecycle severity/category, timestamp, Runtime Profile name, full
server key/epoch when available, and an opaque diagnostic ID. Operational
profile events additionally contain verified process/socket identities,
foreign-thread routing metadata, configuration drift fields and bounded
transport detail and require the operator capability. Neither projection
contains credentials, secret answers, reasoning, raw command output or raw App
Server payloads. Retention is 16 MiB or 30 days per server key, whichever
boundary is reached first, with hash-chained segment rollover; profile logs are
non-authoritative and do not replace this journal.

Normative internal timeouts and expiry results are: socket connect 5 seconds
(`TRANSPORT_FAILURE`); startup `bound` 10 seconds (`RUN_BUSY`); initialize and
`model/list` 30 seconds each (`TRANSPORT_FAILURE` when non-acceptance is
proved); full ledger replay on every worker start, with a five-minute budget
(`OPERATION_TIMEOUT`); schema generation and `profile doctor` 120 seconds
(`OPERATION_TIMEOUT`); solicited history and transient reconciliation 120
seconds (`TRANSPORT_FAILURE`, lifecycle unchanged); worker control `hello` 10
seconds (`RUN_BUSY` for a revalidated `Match`, `RECOVERY_REQUIRED` for
`Unverifiable`); TERM grace 5 seconds and handoff/group absence 10 seconds
(`RECOVERY_REQUIRED` unless interruption uncertainty requires
`OUTCOME_UNKNOWN`); and interrupt terminal wait 5 seconds
(`OUTCOME_UNKNOWN`). Projection publication within 100 milliseconds is a
diagnostic target, never an error deadline. A user-provided shorter command
timeout limits only its caller and never weakens cleanup proof.

`send` waits without a default timeout until the turn is terminal or requires
master interaction. A caller-supplied timeout returns the current nonterminal
state without interrupting the worker. `submit` returns only after app-server
accepts `turn/start` and Dolgorae fsyncs the permanent thread binding and turn ID.
`wait` requires both run and turn
IDs. Waiting and caller-timeout returns use exit 0.

An idempotency key is REQUIRED for `run start`, `run fork`, `send`, `submit`,
`respond`, and `create-write-continuation` and is unique
for the run's lifetime within its operation class. Reusing a key with
the same normalized message, image paths/details/byte digests, and turn options resolves to
the original turn. Reusing it with different input returns
`IDEMPOTENCY_CONFLICT`. `send` and `submit` share the same key space.
Normalization is UTF-8 message bytes, image tuples in caller-supplied order as
`(detail,lossless-canonical-path,byte-length,sha256)`, fixed model,
requested/default effort, and access-derived turn
options serialized with JCS. Before any App Server request, policy transition,
or writer-authority mutation, PREPARE correlates one turn transaction ID,
opaque idempotency key and normalized digest, controller/run revisions, server
key/epoch, provisional-thread intent and writer generation; it fsyncs the run
intent before the writer reservation and fsyncs every authority record before
releasing locks. APPLY performs thread/policy/turn requests without locks.
COMMIT revalidates the same operation token and revisions. A reservation with
no accepted turn is released only after stable history proves non-acceptance;
otherwise it remains bound to the original intent across restart.

Run allocation reserves its key before publishing a Run. `run start`
normalization contains canonical workspace identity, resolved profile snapshot,
Controller ID/generation, control mode, execution lane, purpose/parent,
model/effort, assurance, required capabilities, and instruction byte length and
SHA-256. `run fork` additionally contains source Run, fork mode, selected model,
and the exact history/provenance boundary. Their carrier paths and secret bytes
are excluded. Response loss is reconciled by retrying the identical allocation
key, which returns the original Run; changed normalized input is
`IDEMPOTENCY_CONFLICT` and can never allocate another Run under that key.

`create-write-continuation` has its own operation-class key space. Its
JCS-normalized
digest contains source Run ID and exact terminal Turn ID, purpose/label, selected
or inherited model and effort, requested assurance, required capabilities in
canonical sorted unique order, handoff and destination-instruction byte lengths
and SHA-256 values, artifact references in caller order, creation reason, and
the public identity and generation of the new destination Controller.
Credential carrier paths,
descriptor numbers, and secret bytes are excluded. A same-key retry returns the
original destination Run and credential-delivery receipt; any changed normalized
field returns `IDEMPOTENCY_CONFLICT` without allocating another Run.

A terminal result includes the final agent response, stable outcome code and
state, workspace/run/thread/turn IDs, fixed model, turn reasoning effort, usage
when supplied by Codex, event cursor, and:

```json
{
  "workspace_changes": {
    "measured": true,
    "observed_paths": [],
    "truncated": false,
    "attribution": "unverified"
  }
}
```

Final-response extraction uses authoritative turn-item order, never notification
arrival order. It considers only completed `agentMessage` items belonging to the
addressed root thread and turn and excludes native descendant threads. The last
item with `phase:"final_answer"` wins. When none exists, the last root item with
`phase:null` is the compatibility fallback; `commentary` never becomes a final
answer. Selection occurs only after terminal turn confirmation and is identical
for live delivery and `thread/read` reconciliation. If no valid item exists,
`final_response` is null and no `response.final` event is fabricated. A selected
response of at most 1 MiB UTF-8 is the closed `inline` variant. Larger text is
streamed without whole-value materialization into a run artifact with media type
`text/markdown`, raw byte length and SHA-256 and becomes the `artifact` variant.
One final-response artifact is limited to 32 MiB. A larger value, quota failure
or durable write failure preserves terminal Turn status and produces the
`unavailable` variant with digest, observed byte length and closed reason; it is
never truncated or converted into a failed Turn. The event and machine Turn use
the same union, and only inline/artifact variants emit `response.final`.

Run artifacts are create-exclusive mode-0600 files under the run-private store
with an append-only metadata index. Supported kinds are `file_change_diff`,
`user_input`, and `final_response`; reasoning content is forbidden. A
file-change or user-input artifact is at most 8 MiB, a final response at most 32
MiB, and total retained artifact bytes
per Run are at most 256 MiB. Retention equals Run lifetime; an unresolved
interaction's artifact cannot be removed. `artifact show` returns metadata only.
`artifact read` uses raw-byte offsets, requires `1 <= length <= 1048576`, returns
base64 content plus actual range and EOF, and verifies the full artifact digest
before first access in an invocation. Each artifact is durably classified as
`observer` or `controller_only`; interaction-derived artifacts are always
`controller_only` and carry their request ID. `artifact export` is controller-authorized,
uses safe create-exclusive destination handling and streaming verification.
Same-uid observers may show/read only `observer` artifacts referenced by their
client-safe Run projection. A controller-only artifact returns
`INTERACTION_ARTIFACT_REQUIRES_CONTROLLER` before metadata or bytes are exposed;
the optional Controller carrier unlocks show/read only after serialization-point
revalidation. No command exposes the internal artifact path.

Observed paths are populated only when `measured` is true and describe workspace
changes during the terminal turn interval. In Git mode they are the sorted
unique workspace-relative paths from
`git status --porcelain=v2 -z --untracked-files=all`; ignored paths and
`.dolgorae/exports/` are excluded, while tracked `.dolgorae/config.yaml` and
`.dolgorae/.gitignore` policy-file changes remain visible. Application Support
state is outside the workspace and therefore cannot appear in Git status. In
non-Git mode observed paths are the changed regular files from no-follow pre/post
`(device,inode,size,mtime_ns)` snapshots, also excluding `.dolgorae/exports/` and
never traversing the Application Support state root. Valid UTF-8 paths are strings; other POSIX bytes use
`{"$dolgorae_path_bytes":"<base64>"}` using padded RFC 4648 base64 grammar.
The machine schemas enforce the alphabet, four-character grouping, and exact
terminal padding in addition to declaring `contentEncoding`. At most 4,096 paths are retained and
`truncated` reports omission. Dolgorae MUST NOT claim that Codex caused every
reported change. Full diff inspection is delegated to Git; Dolgorae has no
`run diff` command.

## SPEC-007: Access and Concurrency

This section owns workspace writer serialization and durable authority. Physical
lane topology and thread residency are owned by SPEC-014; where older field
names conflict, SPEC-014 is authoritative.

Every newly allocated Run begins with no writer authority. A shared Run is
always read-effective. A dedicated Run is read-effective unless its first
operation completes the durable first-write activation protocol. A resumed
dedicated generation derives its effective policy from durable writer authority
and reconciliation; it never becomes write-effective without active matching
authority. Readers use thread `sandbox:"read-only"`, turn
`sandboxPolicy:{"type":"readOnly","networkAccess":false}`, and
`approvalPolicy:"never"`. Writers use turn
`sandboxPolicy:{"type":"workspaceWrite","writableRoots":[...],
"networkAccess":false,"excludeSlashTmp":false,
"excludeTmpdirEnvVar":false}`, and `approvalPolicy:"on-request"`. A thread's
`sandbox` value is selected once by the `thread/start` or `thread/resume` that
creates its generation and MUST NOT be treated as the writer discriminator. The
pinned client method set cannot change it on a bound thread, so a dedicated
reader that later activates writer authority keeps thread `sandbox:"read-only"`
while its writer turns carry the workspace-write turn policy above. Only the
turn carrier and verified effective policy determine write capability.
`writableRoots` is the sorted unique set of the canonical workspace plus, in
Git mode, libc `realpath(3)` of `git -C <canonical-workspace> rev-parse
--path-format=absolute --git-common-dir` and `git -C <canonical-workspace>
rev-parse --path-format=absolute --git-path .`. Every member MUST be absolute
and is deduplicated after resolution. These exact thread and turn carriers are
part of the required-subset manifest. Readers MAY run concurrently without a
Dolgorae limit and MAY observe a writer's intermediate files; there is no
snapshot isolation or rollback. A canonical workspace has at most one Dolgorae
writer across every run and profile.

An Independent Specialist Run in an External Specialist Engagement or Brokered Hierarchy
participates in that same workspace writer authority without regard to which
client or coordinator caused its creation. If a Gul-origin Primary Run, an
external-primary-origin specialist, or any other Dolgorae Run already owns the
canonical workspace, a competing specialist write MUST return `WRITER_BUSY`; it
MUST NOT queue, take over, or infer permission from its parent relationship.
Editors and Codex processes operating outside Dolgorae remain outside this
serialization guarantee.

Writer acquisition is lazy and explicit. `run send|submit --write` MUST activate
durable writer authority before submitting any prompt. `run acquire-write` is
valid only after a thread is durably bound; a threadless run returns
`THREADLESS_REQUIRES_WRITE_TURN`. It never
creates a turnless writer thread or treats a reservation as an idle writer.
Dolgorae MUST NOT infer write intent from natural language or use mid-turn
permission escalation. A failed activation MUST NOT start the turn.
Authority remains active across idle, running, waiting, and worker loss until an
explicit release or safe terminal/absence reconciliation completes. Acquisition
never queues automatically.

The writer serializer is BSD `flock(2)` with exclusive semantics on
`<application-support-workspace>/runtime/locks/writer.lock`; a free lock is never evidence that no
writer exists. The authoritative `<application-support-workspace>/runtime/writer.json` is a
versioned, atomically replaced and directory-fsynced state machine with
`none`, `reserved`, `active`, `releasing`, `handoff_prepared`, and
`blocked_unknown` states. It
records workspace/run IDs, controller ID/generation, writer and worker
generations, profile server key/epoch, thread/active-turn IDs, lifecycle,
pending interaction count, last durable event cursor, and recovery state.
Per-run startup locks are
`<application-support-workspace>/runtime/locks/startup/<run-id>.lock`; the handoff serializer is
`<application-support-workspace>/runtime/locks/handoff.lock`. Creation and validation use
workspace-fd-relative no-symlink operations, validate `EEXIST`, ownership and
mode 0700/0600 with `fstat`, and require the canonical workspace to report
`MNT_LOCAL` plus `f_fstypename == "apfs"`. Path or device/inode drift fails
with `RUNTIME_PATH_COLLISION`; nonlocal or non-APFS workspaces are unsupported.
The lock descriptor is close-on-exec and MUST NOT be inherited by the
singleton or descendants. A conflict returns `WRITER_BUSY` with nullable
holder run/profile/state, controller kind, writer generation, and handoff eligibility;
it MUST NOT return a handoff token or prompt content.

Writer authority and effective Codex policy are independent authorities. Run
state records `effective_policy.access` as `read`, `write`, `transitioning`,
`unsupported`, or `unknown`, plus `verification`, server epoch and thread
generation. It separately records writer authority state/generation and
workspace/run identity. A run is externally writable only when effective access
is `write`, verification is `verified`, and writer authority is `active` for the
same run and generation. Neither dimension is inferred from the other.

Activation first rejects a known `unavailable` transition without changing
authority. It acquires home, server, writer, then run serialization, validates controller,
run/writer revisions and normalized input/idempotency, persists/fsyncs the turn
transaction intent, then persists `reserved` and provisional-thread intent
referencing that same transaction, reserves a never-reused dedicated process
generation/server epoch, and releases all file locks before any App Server wait.
For a threadless first write, `SubmitTurn(write_intent=WRITE)` reserves writer
authority before any prompt submission. The worker starts the Run's Dedicated
Lane Server, then calls `thread/start` there with writer policy, verifies the
effective policy, and reacquires home, server, writer, then run locks to fsync the lane/thread
binding and `active` state; only afterward may it release locks and call
`turn/start`. An existing dedicated reader uses the same
prepare/apply/verify/commit shape in its current physical generation. If that
generation is absent, it may use `thread/resume` only on a proved successor
generation of the same logical lane. A shared reader cannot use this protocol
and must create a dedicated write continuation. An existing
writer turn revalidates `active` under writer then run locks, fsyncs its turn
intent, releases the writer lock, and starts the turn under the worker mutation
mutex. No global file lock is held across a network or turn-completion wait.

A crash before dedicated-generation continuation reconciles `reserved` to
`none` only after proving that no physical generation or provisional thread
exists. A crash after continuation or `thread/start` but before binding searches the exact lane generation and
server epochs for the provisional thread and never creates another without
absence proof. A crash after binding but before activation verifies policy and
lands in `active`, `none`, or `blocked_unknown`. A crash after activation but
before `turn/start` retains an idle active writer and never replays an
unaccepted turn. Failed policy application returns to `none` only after reader
policy is positively reverified; otherwise it persists `blocked_unknown`.

Crash reconciliation is keyed by the turn transaction ID. Intent without a
writer reservation is safe to retry after revalidation; reservation without
`thread/start` may clear only after proving no provisional thread; accepted
`thread/start` without a durable binding requires exact-epoch provisional-thread
discovery; binding without active writer requires policy verification; active
writer without `turn/start` remains an idle writer; a sent request with lost
response or a lost terminal event uses `thread/read` and returns the existing
accepted turn or `outcome_unknown`. No stage creates a second thread, writer
generation, or accepted turn for the same transaction.

Release PREPARE acquires writer then run serialization, validates no active turn
or interaction, fences new work, and fsyncs `releasing` with the selected lane
and census revisions. APPLY releases file locks, cleans exact workload
descendants without stopping or changing the Dedicated Lane Server, proves five
complete empty censuses, then applies and verifies reader policy in that same
physical generation. COMMIT reacquires writer then run locks and revalidates the
operation token and revisions. If reader verification fails, authority remains
`active` only when writer policy is positively reverified; otherwise it becomes
`blocked_unknown`. The thread never resumes on the shared Profile Server.
Worker death and kernel unlock alone never perform release.

Foreground command observation and terminal completion are recorded separately
and never imply background-process absence. Writer release/handoff/close first
fences new work and completes the Dedicated Lane workload cleanup protocol. A complete
five-sample empty workload census proves
`background_execution:verified_absent`; any live member is `active`. Timeout,
identity drift, PID reuse, truncated/unreadable census, unregistered survivor,
or detected process-group/session escape is `unverified`, persists
`blocked_unknown`, and returns `BACKGROUND_EXECUTION_UNVERIFIED`. Prompt
instructions, connection close, and leader or kernel-lock loss are never
absence proof. A native Codex terminal API, when present and live-tested, is
additional `hybrid` evidence only; Dolgorae process census remains authoritative.
`BACKGROUND_EXECUTION_UNVERIFIED.details.required_action` is `retry_census` for
a transient incomplete sample, `reconcile_dedicated_lane` for a durable lane record
that needs exact-incumbent recovery, or `operator_repair` when identity or
journal integrity prevents automated reconciliation. Its details include the
run/thread/server identities, nullable lane identity/epoch, census revision,
and a bounded reason.

Writer transfer uses explicit prepare, apply, commit, and cancel operations. PREPARE
requires one controller capability that owns both runs and persists a single
workspace handoff record bound to workspace, source and destination run IDs,
both run generations, controller ID and digest, writer generation, and authority
revision. Under home, server, handoff, writer, then canonically ordered run
locks it fsyncs the source `releasing` fence, destination reservation intent,
and never-reused destination dedicated-lane generation/server epochs; it expires after five
minutes and releases all locks. APPLY fences
and cleans the idle source lane's workload descendants, proves its census
empty, applies verified reader policy without moving its thread, then starts or
reuses the destination lane generation and verifies writer policy without a filesystem
coordination lock. COMMIT reacquires home, server, handoff, writer and
ordered run locks, revalidates the operation token and revisions, then fsyncs
the destination lane/thread binding and activates its generation. Requester
failure may leave `none` or `blocked_unknown` and MUST NOT roll the source back
to write. Cancel is idempotent before commit. A stale
binding returns `STALE_WRITER_GENERATION`; a different controller returns
`CROSS_CONTROLLER_RELEASE_REQUIRED`; active, waiting, interrupting, recovering,
reconciliation-required, outcome-unknown, closing, or unverifiable source or
destination/server epoch or known/unverified background execution returns
`WRITER_HANDOFF_NOT_ALLOWED`. V1 provides no force unlock, signal, automatic
queue, or kill-based takeover. Dedicated workload cleanup may signal only its exact
persisted identities and is not takeover. Promotion, demotion, and handoff never
restart the shared Profile Server. If live policy probes cannot prove an existing thread
changes sandbox/approval/writable-root/network policy, the operation returns
`ACCESS_TRANSITION_UNSUPPORTED` and requires a new lineage-linked run/thread.

While the workspace authority record is `handoff_prepared`, the source Run
projection remains `dedicated_releasing`; its writer-authority projection uses
`handoff_prepared`, retains the handoff transaction ID and writer generation,
and never presents effective write access as newly granted. This is the sole
Run-state projection pairing for `handoff_prepared` in v1.

Writer and startup lock files are permanent after create-exclusive creation;
normal operation never unlinks or recreates either pathname. Every writer
transaction compares the held fd's `(st_dev, st_ino)` with a root-fd-relative
`fstatat`; mismatch fails with `RUNTIME_PATH_COLLISION`. The durable authority
record is never reconstructed merely because a kernel lock is free or its prior
worker is absent.
If any lock pathname or the locks directory is missing while a run history
exists, Dolgorae MUST fail closed rather than create a new inode that could split
an existing serializer. Recovery requires explicit operator repair only after
all recorded workers are absent and the incumbent thread/turn and singleton
epoch have been reconciled.

`workspace writer reset` is that operator repair and is the only v1 escape from
a `blocked_unknown` authority record. It is an exceptional same-user correctness
override, not a force takeover: it MUST require the operator capability, an
exact `--confirm-workspace-id` match, and explicit `--require-worker-absence`.
PREPARE acquires writer then run serialization, revalidates the operator
capability and the recorded authority revision, and fsyncs a reset token. APPLY
drops every file lock and MUST prove every run recorded by the authority record
`Absent` under SPEC-007 process identity, with a complete five-sample empty
workload census for each recorded dedicated lane generation. COMMIT reacquires
the same ordered prefix, revalidates the token and revision, appends the repair
evidence to each affected run ledger, and only then publishes authority `none`.
If any recorded worker is `Match` or `Unverifiable`, or any census is
incomplete, the command MUST fail with `RECOVERY_REQUIRED` or
`BACKGROUND_EXECUTION_UNVERIFIED` and leave the record unchanged. It MUST NOT
signal any process, recreate a missing lock inode without the same absence
proof, or fabricate a turn outcome. This is the action named by
`BACKGROUND_EXECUTION_UNVERIFIED.details.required_action` value
`operator_repair`. A writer crash with an active or uncertain turn
persists `blocked_unknown`; kernel unlock, worker exit, connection close, and
socket absence are never sufficient to clear it.

Runtime identity is `(boot_session_uuid, pid, pgid, uid, start_tvsec,
start_tvusec, executable_path, executable_dev, executable_ino,
executable_sha256)`. Dolgorae samples BSD info, `proc_pidpath` and executable
identity, then BSD info again and rejects a changed start time. A live process
whose path is unavailable is not mismatched when the remaining tuple and group
proof match; replacement at the same path is not identity continuity. Recovery
classifies a Dolgorae worker as `Absent`, `Mismatch`, `Match`, or
`Unverifiable`; only a revalidated `Match` may receive an individual signal.
Worker or process-group absence is not writer-release or turn-terminal proof.
No recorded generation means there is no prior process to classify and yields
`Absent`. A present-but-unreadable or incomplete record is `Unverifiable`.
`Unverifiable` is otherwise exhaustive for failure to read the current boot UUID, a
short/permission-failed BSD sample, changed start time between samples, failure
to bind or revalidate a live kqueue process instance other than `ESRCH`, invalid/truncated group
enumeration, or a survivor whose required uid/pgid/start operands cannot all be
read. It also includes missing/unparseable required recorded generation or
identity fields and failure to obtain required executable device/inode/hash
when no documented live-path fallback applies. A missing live `proc_pidpath`
alone is the explicit exception above.

A boot UUID mismatch proves the entire recorded generation absent. `ESRCH`
while binding `EVFILT_PROC/NOTE_EXIT` also proves that exact PID absent. On the same
boot, `proc_listpgrppids` starts with positive entry capacity, passes
`capacity * sizeof(pid_t)` as the byte buffer size, and repeats with doubled
entry capacity whenever the returned PID count equals capacity. `-1`, possible truncation,
or `pgid <= 1` is `Unverifiable`; zero with positive capacity proves empty.
Each survivor matches only with recorded uid/pgid and a start time no earlier
than the leader's recorded start. Earlier occupants are dismissed; `ESRCH` and
zombies are absent. This is a possible-survivor predicate, not by itself signal
authority. Individual member signalling is permitted only after the recorded
leader or at least one already recorded cleanup-snapshot member was kqueue-bound
and revalidated `Match`, which establishes continuity of the still-live process
group. While continuity exists, Dolgorae snapshots every member's
`(pid, uid, pgid, start_tvsec, start_tvusec)` and
signals only members from that snapshot whose full tuple revalidates. A newly
appearing/recycled member, or any nonempty group after leader loss that was not
in the snapshot, is `Unverifiable` and is never signalled. If the leader is
initially absent/mismatched and possible
members remain, the group is `Unverifiable`. Once continuity is established,
cleanup fsyncs `cleanup_in_progress` plus the snapshot tuples, revalidates and
signals each member with TERM, waits five seconds, re-enumerates/revalidates the
snapshot, and individually uses KILL where needed. A later recoverer inherits
only the tuples: it must bind/revalidate at least one still-live snapshot member
before signalling or extending the snapshot. Ten seconds without proven group
absence returns `RECOVERY_REQUIRED`. `killpg` is forbidden.

Profile singleton launch uses spawn attributes
`POSIX_SPAWN_START_SUSPENDED | POSIX_SPAWN_SETPGROUP |
POSIX_SPAWN_SETSIGDEF | POSIX_SPAWN_SETSIGMASK`; the default set covers every
catchable signal and the child mask is empty. Before `SIGCONT`, the profile
manager opens
the suspended spawn-image path reported by `proc_pidpath` without following
symlinks and derives device, inode, and SHA-256 from that same fd between two
unchanged BSD-info samples. Because v1 forbids wrappers, this image MUST be the
configured Codex executable. It writes
the complete ten-field provisional identity with `spawn_image_*` executable
fields using temp-file, `fsync`, rename, and directory `fsync`.
Replacement/unavailability is `Unverifiable`, not a partial record. Before a
`ready` profile state exists, an exec transition with identical PID, PGID, UID,
and BSD start time is continuity and is `Match`, not `Mismatch`. The
post-WebSocket-handshake sample is the sole final-executable identity authority
and is stored with the server epoch. No run worker launches or owns this
process.

The recorded lock root already denotes the directory ending in `locks/`. The
fixed pathnames above are relative to that root and MUST never share an inode.
The per-run startup file contains two POSIX byte-range locks: byte 0 is held by
the CLI before fork until worker `bound`; byte 1 is acquired as the worker's
first post-`setsid`/re-exec action and held for its entire serving lifetime.
The 8192-byte lock body contains separate version-1, zero-padded owner records
at `[0,4096)` for byte 0 and `[4096,8192)` for byte 1. A short file is
`Unverifiable`. Each contains range, workspace/run/generation,
boot UUID, full Dolgorae process identity, executable-path SHA-256, and a SHA-256
checksum over the preceding bytes. All-zero, stale, checksum-invalid, or
unknown-layout slots never establish identity and do not block a kernel-lock
winner; a locked range with no valid matching record is `Unverifiable`. Each
valid record is updated with `pwrite` on the already-open lock fd and fsynced
after its range is acquired and before work proceeds, then cleared and fsynced
immediately before release. The worker emits
`bound` only after byte 1, socket bind, and fsynced runtime
identity; `ready` follows full replay/validation. Contenders query both ranges
with `F_GETLK`; `l_pid <= 0` is `Unverifiable`, while a positive `l_pid` is only
a hint checked against the matching owner record.
Normal attachment to an answering socket does not acquire the startup lock. A
contender uses Darwin `F_SETLKWTIMEOUT` with a ten-second relative budget and
`struct flocktimeout { struct flock fl; struct timespec timeout; }` field order.
A timed-out byte-0
transient starter may be terminated outside the lock only after the run-keyed
file, exact Dolgorae executable identity, the defined
BSD/path/executable/BSD process-identity sample, and kqueue binding all match.
A byte-1 owner is a serving worker and never follows the ten-second-only path;
reader and writer workers both require the control `hello` plus 30-second
progress/abort procedure below. `Mismatch` or `Unverifiable` is never signalled
and returns `RUN_BUSY`. Surviving contenders compete for byte 0 and only the
winner proceeds. Failure to acquire byte 1 writes a structured fd-3 `RUN_BUSY`
failure and exits without binding, ledger writes, or other observable effects.
Every process opens the lock fd
once and does not reopen or close it during ownership; owner revalidation uses
`fstat` on the held fd and never a second open, including through a hardlink.
The CLI parent alone retains byte 0 across the fork; the child's inherited
startup-lock fd is `FD_CLOEXEC` before `__worker` re-exec. Startup fd 3 has
`FD_CLOEXEC` cleared for that re-exec, and the worker sets it before app-server
launch. The worker opens exactly one startup-lock fd for byte 1 and never closes
another descriptor for the same file while serving. It is close-on-exec for
app-server.
Startup lock files are permanent once created. Every claimant verifies the held
fd and root-relative pathname device/inode before writing an owner slot; unlink,
replacement, or mismatch is `RUNTIME_PATH_COLLISION`, never recreate-and-proceed.

The worker always services a dedicated control channel from `bound`, including
while replay prevents ordinary mutations. Version-frozen control
protocol v1 contains only `hello`, bounded `status`, and `shutdown`, accepts a
binary-digest mismatch, and validates workspace, run, generation, boot UUID,
and process identity. Ordinary mutations still require matching current
protocol, version, and digest. Identity-verified `shutdown` first interrupts an
active turn and waits up to five seconds for terminal history.

When ordinary attachment detects binary or mutation-protocol skew, `run pause`,
`run close`, and `run recover` perform control-v1 `status` followed by
identity-bound `shutdown`. After exact worker exit, the current binary starts a
control/replay worker and finishes the requested lifecycle operation. Failure
to obtain an identity-confirmed response never authorizes a signal.

When any same-run byte-1 worker's control `hello` exceeds ten seconds, recovery
applies the full identity and kqueue rules to the worker itself without
requiring the possibly-held startup lock. A live revalidated `Match` returns
retryable `RUN_BUSY` and is never signalled merely because control, ledger,
runtime generation, logging, or CPU appears inactive. `Unverifiable` returns
non-retryable `RECOVERY_REQUIRED`. Automatic byte-1 takeover and
activity-derived kill authority do not exist in v1.

If worker `SIGTERM` arrives during an active turn, it sends `turn/interrupt`,
waits up to five seconds for a terminal event, fsyncs terminal evidence when
observed, and records `outcome_unknown` on expiry before generation cleanup.
The worker holding byte 1 normally unlinks its own socket. There is no volatile
sibling sidecar: `<application-support-workspace>/runtime/runs/<run-id>.json` is the sole socket
identity authority. On recovery, only the byte-0 election winner may authorize
unlink after an exact matching runtime record and prior-generation absence are
proved; the replacement worker performs it after acquiring byte 1 and before
bind. An occupied path with no matching record fails closed. Shutdown attempts
this cleanup for ten seconds and otherwise leaves the path for that verified
next owner.

Acquire or release never changes a Run's logical lane and never substitutes a
Worker or connection generation for authorization. For a Dedicated Run whose
pinned profile reports a verified transition, read-to-write and write-to-read
apply, verify, and commit policy in place on the same physical Dedicated Lane
Server generation and the same Codex thread. The Run's `policy_epoch`
increments on each committed access transition; `run_generation`, thread ID,
thread generation, lane ID, process generation, and server epoch do not change.

A known unavailable or unverified Dedicated transition returns
`ACCESS_TRANSITION_UNSUPPORTED` before reservation and leaves authority
unchanged. That Run may use a lineage-linked Dedicated write continuation.
`shared_readonly` Runs always require a Dedicated write continuation and never
transition in place. Failure during a started transition follows the durable
`reserved` or `releasing` landing rules and never falsely restores the prior
state.

A writer transfer between the Primary Run and a brokered Specialist normally
crosses Controller identities. V1 therefore performs explicit source release,
authoritative verification that workspace writer state is `none`, and
destination acquisition. The same-Controller atomic handoff operation MUST NOT
be used across those identities, and V1 does not claim an atomic cross-
Controller handoff. A competitor may win the intervening acquisition; the
Orchestration Broker handles `WRITER_BUSY` by rescheduling rather than taking
over or queueing.

Start, resume, fork, and recovery otherwise create read-effective Runs and do
not acquire writer authority.

Readers MAY run during writer turns and may observe intermediate workspace
state. Dolgorae provides no read snapshot isolation. A consistent review SHOULD
begin only after the writer turn reaches a terminal state.

Codex native subagents belong to their parent run and inherit its access
boundary. They do not acquire separate Dolgorae writer authorities. Dolgorae therefore
serializes independent writer runs, not every execution lane inside one
writer session; the injected instructions require Codex to avoid overlapping
write-heavy delegation and to prefer parallel subagents for independent or
read-heavy work.

Reader requests for write or additional filesystem permission are
automatically declined. Writer approvals are never automatically accepted.
Profile MCP servers, apps, and plugins remain available; side effects performed
outside Codex's sandbox are outside the one-Dolgorae-writer-per-worktree
guarantee.

## SPEC-008: Run Lifecycle, Recovery, and Forking

The lifecycle states are:

- `starting`
- `idle`
- `running`
- `waiting_interaction`
- `reconciliation_required`
- `paused`
- `closed`
- `start_failed`
- `outcome_unknown`

The checked Codex manifest names the terminal notification method, status JSON
Pointer, and closed terminal and nonterminal status sets. A listed terminal
status drives the corresponding Dolgorae terminal result; a listed nonterminal
status preserves running/waiting state. A missing, malformed, or unlisted
status is `Unreadable` and can never authorize idle or replay.

Normal transitions are:

```text
starting -> idle -> running <-> waiting_interaction
running -> idle
waiting_interaction -> idle
idle <-> paused
idle -> closed
paused -> closed
starting -> start_failed
running|waiting_interaction -> outcome_unknown
running|waiting_interaction -> paused (only after confirmed interrupt terminal evidence)
running|waiting_interaction -> closed (only after confirmed interrupt terminal evidence)
running|waiting_interaction|outcome_unknown -> reconciliation_required
reconciliation_required -> paused|outcome_unknown
outcome_unknown -> paused
outcome_unknown -> closed
```

`closed` and `start_failed` are final. A failed start that allocated a run ID is
retained with its audit and any known Codex thread ID. It supports only status,
events, verify, export, and confirmed deletion.

A byte-0 election owner that proves no worker reached `bound` may use the sole
bootstrap-writer exception to append `starting -> start_failed` and its final
seal. It must hold startup serialization and fsync both records. A present but
`Unverifiable` generation is never rewritten as start failure; the Master may
wait for group emptiness or a boot-session change.

Pause and close reject running or waiting runs unless `--interrupt` is present.
With `--interrupt`, Dolgorae first answers an outstanding supported approval with
`cancel`, requests turn interruption, and waits five seconds. Confirmed terminal
evidence permits the direct paused/closed transition; expiry records and emits
`OUTCOME_UNKNOWN` and uses the corresponding `outcome_unknown` edge before
stopping or closing. Pause is resumable; close is not. Neither action reverts
workspace changes. The direct `running|waiting_interaction -> paused|closed`
ledger transition carries `interrupt_terminal_confirmed:true`; conformance
rejects that field on any other edge and rejects either direct edge without it.

After worker loss, an idle run may be recovered by starting a new process
generation and using `thread/resume`. Recovery of `paused` performs only
coordination cleanup and record repair; `resume` starts its next generation. If
failure occurred during an active turn, Dolgorae reconciles only terminal status
confirmed by persisted Codex history. It MUST NOT replay the input automatically.

Cross-epoch reconciliation never connects to an absent recorded epoch. It reads
the recorded server key/epoch and applies the immutable lane-specific barrier.
A dedicated Run proves its exact prior Dedicated Lane Server generation and
descendants absent, then starts the same logical lane's successor generation; a
shared-readonly Run validates or profile-recovers the shared Profile Server and
never treats it as run-owned. It records the new or validated epoch and uses a
transient read-only connection on that epoch for
`thread/read(includeTurns:true)`. It inspects the exact root thread and turn,
classifies terminal, active, absent, unreadable, or unknown, and fsyncs one
reconciliation record containing recorded/observed keys and epochs, the old-
epoch absence verdict, history verdict, and writer resolution. It does not
resume a turn or clear writer authority merely because the epoch changed.

If no terminal evidence exists, the run becomes `outcome_unknown`. Its worker
connection stops and any active writer authority becomes `blocked_unknown`;
process loss does not release it. `reconcile` uses a read-only connection and only
`thread/read(includeTurns: true)`; it never resumes the thread or starts a turn.
Terminal evidence moves the run to `paused`, where bare `resume` uses read
access. Otherwise the run remains blocked from new turns; only status, events,
verify, export, recover, reconcile, ordinary fork after proven absence,
`fork --fresh`, and close after proven
generation cleanup are allowed.

History-copying fork is allowed from idle, paused, closed, and outcome-unknown
runs after required absence proof, but not from running or waiting runs. The
read-only `fork --fresh` escape is additionally allowed when a `running` or
`waiting_interaction` source socket is unreachable and its process identity is
`Unverifiable`. The source is immutable. A fork uses the same
profile snapshot, defaults to read access, and inherits the source model unless
another model is explicitly selected. It inherits the source's immutable
run-specific instructions. A cross-profile fork is forbidden.

Every history-copying fork scans confirmed history newest first and
selects the latest status listed as forkable in the checked profile manifest.
Rejected interrupted/failed statuses are skipped rather than treated as generic
terminal boundaries. If confirmed history exists but no forkable boundary is
accepted, the command returns `COMPATIBILITY_REJECTED`; only the separately
defined outcome-unknown/no-confirmed-turn fallback creates a fresh thread.

Any fork that must copy confirmed history requires the source Codex thread to
exist in the pinned `CODEX_HOME`; the Dolgorae transcript is not a substitute.
`fork --fresh` reads the immutable source manifest plus read-only fsynced
state/runtime projections needed to establish current lifecycle, socket reachability,
identity verdict, and unresolved-turn provenance. It never reads the source
Codex thread and never appends, repairs, or otherwise mutates the source ledger
or projections. It always creates an empty read-only run whose first turn
allocates its thread. Its manifest
records the source run, observed source state, and unresolved turn as provenance
without asserting an outcome. Explicit write access is rejected. The
ordinary automatic fresh-thread fallback remains limited to an outcome-unknown
source with no confirmed turns and proven prior-generation absence.
This escape does not stop a possibly live source writer and provides no
snapshot isolation; its read-only turn may observe that writer's concurrent or
partial workspace mutations. Provenance records the unresolved source rather
than implying the hazard is resolved.

Projection-only `status`, `events`, and `verify` include the current process
identity verdict in their data but MUST NOT fail solely because that verdict is
`Unverifiable`. They read the fsynced ledger/runtime projection directly and do
not start, attach, recover, or contend on the per-run startup lock.

Changing effort during an active turn updates only the default for the next
turn. An effort value is syntactically valid when it is a nonempty app-server
string. Model resolution exhausts every `model/list.nextCursor`; `model` is the
identity field, omitted `--model` selects exactly one `isDefault`, and effort is
read from each `supportedReasoningEfforts[].reasoningEffort`. Zero or multiple
model defaults is `COMPATIBILITY_REJECTED` only when a command needs the
omitted-model default. Omitted `--effort` at run creation selects the first
advertised effort for the resolved model and records it as the run default.
Omitted effort on later turns uses that recorded default; `send`/`submit
--effort` is one-turn-only, while `set-effort` changes future defaults. The
manifest stores the initial model capability snapshot; later successful generations append a new snapshot, and
`set-effort` while no generation is live validates against the latest fsynced
snapshot. Every generation start revalidates the stored default against its new
snapshot. The effort is accepted only when that snapshot advertises the exact value. An empty
value returns `INVALID_ARGUMENT`; an unadvertised value returns
`COMPATIBILITY_REJECTED`.


### Aggregate and Broker Recovery

Dolgorae-Orchestrated Session and External Specialist Engagement recovery is
SQLite-state and event-log driven and MUST preserve independent Run recovery
semantics.

Every aggregate has exactly one referenced Aggregate Bootstrap Operation. A
non-`creating` Orchestration Session and every External Specialist Engagement
require a `ready` bootstrap operation. Startup reconciliation joins the
bootstrap operation, aggregate row, bootstrap operation ID carried by the Primary Run
creation intent, Primary Run manifest, and Primary audit ledger. A missing or mismatched side cannot be
fabricated from the other side. It is completed with the original preallocated
identity only when the persisted evidence proves the external side effect was
not duplicated; otherwise the aggregate enters recovery and blocks mutation.

Before creating a brokered or externally hired Specialist process, Worker,
thread, or physical lane generation, Dolgorae MUST transactionally allocate the
child Run ID and append a write-ahead spawn or hire operation with its aggregate
ID, idempotency key, parent or external provenance, role snapshot digest, and
Run-configuration digest. Repeating the same key and normalized identity returns
the original operation and child Run. Identity drift is
`IDEMPOTENCY_CONFLICT` and MUST NOT allocate another Specialist.

Startup reconciliation verifies the orchestration SQLite database and its
hash-chained event table, scans queued mail and nonterminal operations, and
compares them with authoritative Run, Worker, lane, thread, and ledger state. A `requested` or `provisioning` operation with no accepted external
side effect may continue using the preallocated child Run ID. A Specialist task
whose result is durably `completed_not_delivered` redelivers the existing result
reference without rerunning the task. A task that was accepted or running but
whose completion is not authoritative becomes `interrupted_unknown`; Dolgorae
MUST NOT replay it automatically or infer semantic completion.

Loss of a Dolgorae-Orchestrated Session's Primary Run changes the aggregate to
`degraded` or `recovering`; it MUST NOT automatically destroy owned Specialist
Runs. Explicit session completion gracefully retires owned idle Specialists.
Explicit abort cancels or interrupts owned Specialists under ordinary Run rules
and preserves unresolved evidence. External Specialist Engagement recovery
restores Specialist operational state and delivery receipts but never invents
or reconstructs the external AI's plan or task graph.

One Run may belong to at most one active aggregate. Active reparenting,
in-place role conversion, and in-place transfer between the two use cases are
forbidden in v1. A change of orchestration owner or role requires a new Run and
explicit context or artifact handoff.

## SPEC-009: Pending Requests and Approvals

The checked [Codex required-subset manifest](protocol/codex-0.147.0-required-subset.json)
maps stable server requests as follows:

- `item/commandExecution/requestApproval` and
  `item/fileChange/requestApproval` are supported and become
  `waiting_interaction`;
- `item/tool/requestUserInput` is supported only when the profile capability
  snapshot records a successful pinned live round trip; it becomes
  `waiting_interaction`;
- `item/permissions/requestApproval` and `mcpServer/elicitation/request` are
  recognized but unsupported in v1. Dolgorae MUST persist an immediately
  resolved `unsupported_request` interaction, reply JSON-RPC method-not-found,
  and keep draining the turn. They MUST NOT appear in `run pending`. Connector
  approval remains unavailable.

Supported requests are represented by the discriminator-bound interaction
kinds `command_execution_approval`, `file_change_approval`, and `user_input`.
`run pending` is an observer-safe operation and returns only strict
`dolgorae-interaction-summary/v1` records: request and Run IDs, kind, status,
generic title, Controller kind, whether user escalation is required, whether
the request contains protected input, and timestamps. It MUST NOT return command
text, cwd, questions/options, response schema, decision tokens, artifact IDs,
thread/turn/item IDs, or server epoch. The title MUST be selected only from the
checked fixed enum by kind/status and the protected-input boolean, and MUST NOT
be derived from command, cwd, diff, question, option, reason, response schema, or
other payload text. `run interaction get` MUST be
Controller-authorized and returns the full discriminator-bound interaction
needed to resolve that request. Approval responses
contain only `decision`. User-input responses contain an `answers` map keyed by
question ID, each with a nonempty string array; Dolgorae validates IDs, option
membership, and `isOther` semantics before translating to the pinned Codex
response. If any answered question is secret, all answers MUST exist only in the
controller request and upstream write buffer and MUST be zeroized. The durable
resolution stores `contained_secret:true`, answer count, the winning
idempotency key, and an opaque UUIDv7 resolution receipt; it MUST NOT store
plaintext, an unkeyed digest, an HMAC, or any other content-binding value. Unknown decisions,
question IDs, response schemas,
or raw Codex tokens return `INTERACTION_RESPONSE_INVALID`. Malformed frames
stop the generation. Other known-but-unsupported requests receive JSON-RPC
method-not-found and are recorded.
After method-not-found, Dolgorae keeps draining the generation until Codex emits
a terminal turn event. If Codex instead leaves the turn nonterminal, the Master
sees the run as `running` with no Dolgorae pending request and uses `run interrupt`
as the bounded escape.

The interaction MUST be fsynced before any observer delivery. Controller
disconnect does not affect it, the first valid idempotent response MUST win, an
identical non-secret retry returns the recorded result, and another response
returns `INTERACTION_ALREADY_RESOLVED`. A response from a non-controller returns
`CONTROLLER_MISMATCH`; schema mismatch returns
`INTERACTION_RESPONSE_INVALID`. Interactions from an older run generation,
cleared upstream request, terminal turn, or different server epoch return
`INTERACTION_STALE`; their audit records survive but they are not
resolvable or silently replayed. Their accepted
turn follows ordinary persisted-history reconciliation. Waiting has
no automatic timeout. A waiting writer retains durable writer authority while a
response or interruption returns it through running/idle; only explicit release, pause,
close, or terminal/safe-absence reconciliation releases it.

For a secret-bearing response, an authenticated retry using the winning
idempotency key returns the original receipt and result without comparing,
hashing, or persisting its supplied answer body. A different key returns
`INTERACTION_ALREADY_RESOLVED`. If no question is secret, ordinary JCS digest
comparison continues to provide byte-normalized idempotency.

The pinned `item/fileChange/requestApproval` request does not itself contain a
diff. The initial `fileChange` item is the durable revision-0 baseline for exact
`(threadId,turnId,itemId)` correlation. Each supported
`item/fileChange/patchUpdated` replaces the snapshot and advances its revision;
the approval binds the latest durably stored revision. A patch update after the
request makes that request stale. An approval request with no complete bound
snapshot MUST be rejected rather than displayed. Each upstream change preserves the
closed `add`, `delete`, or `update` kind. `move_path` is null for add/delete and
may be null or a destination only for update; Dolgorae does not invent a
separate rename kind. Every change has a lossless workspace-contained path and
bounded unified diff. A named runtime semantic validator enforces at most 4,096
files, 4,096 UTF-8 bytes per path, 64 KiB per diff, and at most 64 KiB across
all inline `changes[].diff`. Custom `x-*` keywords are annotations unless that
validator explicitly implements them. A larger aggregate up to 8 MiB is streamed to a create-exclusive
run-private mode-0600 artifact and represented by artifact ID, media type,
byte length, SHA-256, and `truncated:false`; the ledger stores only the reference.
Larger, missing, path-escaping, uncorrelated, changed, or digest-mismatched
content returns `FILE_CHANGE_ARTIFACT_UNAVAILABLE` and MUST NOT be approved. The
approval binds the snapshot SHA-256 and revision; a later patch update or item
disappearance makes it `INTERACTION_STALE`.

For user input, Dolgorae preserves the pinned fields `isBlocking`, question ID,
header, prompt, optional choices, `isOther`, and `isSecret`. It does not invent
required/optional or single/multiple semantics absent from the pinned schema.
An interactive run requiring `interaction.user_input` fails before allocation
unless the profile has a successful live capability observation. The narrowly
enabled experimental client capability does not make permission or MCP
elicitation public features.

Command- and file-change approval decisions exposed by Dolgorae are:

- `accept_once`
- `decline`
- `cancel`

For command and file-change approvals they map respectively to the pinned wire
values `accept`, `decline`, and `cancel`.

Reader auto-decline MUST be implemented solely by `approvalPolicy:"never"`;
Dolgorae MUST NOT install a second approval interception mechanism. A server request
that nevertheless arrives is handled by the recognized-unsupported rule above.

The pinned server's session-scoped approval value remains an observed wire
capability but is not exposed as v1 public input.

## SPEC-010: Audit, Retention, and Deletion

Every allocated Run has a private directory at `<application-support-workspace>/runs/<run-id>/` with:

- `manifest.json`: fixed run configuration and provenance;
- `audit.jsonl`: the sole append-only audit authority;
- `state.json`: a disposable materialized view rebuilt from the ledger.
- `worker.log` and `worker.log.1`: bounded diagnostics, never audit authority;
- `recovery/`: preserved torn-tail and repair evidence.

The workspace also has
`<application-support-workspace>/orchestration/orchestration.sqlite3` as the sole
transactional authority for Dolgorae-Orchestrated Session, External Specialist
Engagement, aggregate bootstrap, membership, spawn or hire operation, Specialist task,
Collaboration Exchange, mailbox, activation, and result-delivery transitions.
It MUST use SQLite WAL, foreign keys, `synchronous=FULL`, a bounded busy timeout,
and one workspace mutation owner. A hash-chained append-only
`orchestration_event` table is committed with each state transition.
`orchestration/state.json` and any orchestration JSONL are disposable exports
that MUST validate against
`protocol/dolgorae-orchestration-state-v1.schema.json`. A multi-object change that affects an aggregate and a Run uses the durable
Aggregate Bootstrap, spawn, or hire Operation ID as its cross-store correlation
identifier. The SQLite portion commits in one crash-consistent transaction, and
the matching Run creation intent is fsynced with that same Operation ID before
runtime publication. Exports are never coequal authorities.

The audit contains Dolgorae lifecycle records and redacted app-server wire
records in one total order. Each record contains schema version, sequence/event
cursor, UTC timestamp, run ID, run generation, kind, payload,
`previous_hash`, and `hash`. Lines use RFC 8785 JCS. `sha256-jcs-v1` hashes the
JCS record with `hash` omitted and `previous_hash` retained; the genesis
`previous_hash` is 64 zeroes. SHA-256 chaining detects accidental corruption or
ordinary tampering; it is not a signature and does not defend against a hostile
same-user attacker.

The v1 audit-kind enum is closed:
`workspace_initialized`, `run_created`, `write_continuation_created`, `turn_intent`, `thread_bound`,
`turn_started`, `turn_terminal`, `lifecycle_transition`,
`run_generation_started`, `run_generation_stopped`, `app_server_request`,
`app_server_response`, `app_server_notification`, `approval_requested`,
`approval_decided`, `interaction_opened`, `interaction_resolved`,
`client_event`, `controller_reset`, `reasoning_content_suppressed`,
`writer_acquired`, `writer_released`, `writer_handoff_requested`,
`writer_handoff_cancelled`, `writer_handoff_completed`, `profile_observed`,
`idempotency_reserved`, `reconciliation`, `cleanup_intent`, `cleanup_result`,
`ledger_tail_repaired`, `projection_rewound`, `payload_unrepresentable`,
`start_failed`, and `outcome_unknown`. Adding a kind is a machine-contract
schema change; an unknown kind fails ledger verification rather than flowing
through `events` as an open object.

An allocated Run's v1 ledger bootstrap is closed and reconstructable without a
worker or Codex process. Its first three records MUST be, in order,
`workspace_initialized`, `idempotency_reserved`, and exactly one of
`run_created` or `write_continuation_created`. The first record binds the
workspace ID; the second validates against
[`dolgorae-idempotency-intent-v1.schema.json`](protocol/dolgorae-idempotency-intent-v1.schema.json)
and binds the normalized operation identity and allocated Run ID; the third
publishes the initial Run state. No later record may repeat
`workspace_initialized` or either allocation kind. A later operation may append
another `idempotency_reserved` record with its own checked intent.
A durable `(operation, idempotency_key)` may occur more than once only with the
same normalized identity digest and Run ID; a conflicting replay is an
audit-integrity failure.
A process-local reservation that has not appended `idempotency_reserved` and
has not published the Run MUST be released when its guard is abandoned; an
accepted reservation is permanent and exact replay returns its original Run.

Terminal history is also closed. `start_failed` evidence is legal only while
the Run is `starting` and only for the byte-0 bootstrap writer authorized by
SPEC-008. A normal close requires a preceding `cleanup_result`. In either case
the final record MUST be a `lifecycle_transition` whose `current` value is
`start_failed` or `closed`, whose `previous` value equals the reconstructed
state, and whose `terminal_seal` value is `true`. A terminal transition without
its required evidence, a nonterminal transition carrying `terminal_seal`, or
any record after a terminal seal is an audit-integrity failure. Verification
also rejects either terminal-evidence record when it is not immediately followed
by its matching seal, including a crash after fsyncing only `cleanup_result`.
Implicit lifecycle effects of turn, interaction, reconciliation, and
`outcome_unknown` records are checked against the same closed transition table;
they cannot move a Run from `starting` into an active state. Verification
reconstructs the lifecycle from the closed transition table and requires every
stored record to survive parse, hash verification, and canonical serialization
as an exact byte-for-byte fixed point.

Inbound JSON rejects duplicate object members and preserves number lexemes until
numeric adaptation. Direct deserialization into `serde_json::Value` or a typed
struct is forbidden. The approved ingest path is a duplicate-detecting custom
visitor over `serde_json::value::RawValue`; it observes every map entry,
rejects a repeated decoded key, and retains each numeric leaf's source lexeme
until adaptation. The in-repo `sha256-jcs-v1` canonicalizer orders object keys
by UTF-16 code units and formats binary64 values with ECMAScript's shortest
number representation. Verification uses that formatter rather than echoing a
preserved lexeme. Any canonicalizer byte change requires a new
`sha256-jcs-vN`; RFC 8785 published vectors and Dolgorae numeric vectors are
normative fixtures. Before inserting any Dolgorae marker, each inbound object key
matching `^\$+dolgorae_` is escaped by prefixing one additional `$`. Processing
order is escape, recursive redaction, numeric adaptation, then JCS; the
redaction tokenizer never examines Dolgorae-owned marker keys. Before JCS, a
decimal lexeme is parsed to finite binary64, rendered with the ECMAScript
shortest form, and compared by exact decimal numeric value with the original.
A nonfinite/overflowing value or unequal value becomes
`{"$dolgorae_number":"<original-lexeme>"}`. Verification also requires every
stored line to be byte-identical to its own JCS serialization. Timestamps are
UTC RFC 3339 with exactly six fractional digits and `Z`.

Any nonempty bytes after the last newline are a recoverable torn tail, including
bytes that happen to parse as a complete JSON object but lack their terminating
newline. Recovery writes it without overwrite as
`recovery/tail-<next-sequence>-<raw-sha256>.bin`, file- and directory-fsyncs the
evidence, truncates and fsyncs the ledger, then appends and fsyncs the repair
record. Restart detects each completed prefix of that sequence and finishes it
idempotently. A broken newline-terminated
record or any corruption before the tail returns `AUDIT_INTEGRITY_FAILURE`.
Ledger durability MAY use a 100-millisecond group-commit window for streaming
records, but MUST `fsync` turn intent and idempotency reservation before
`turn/start`, and an approval decision before forwarding its response. It also
MUST `fsync` before acknowledging an accepted turn ID, pending interaction,
terminal result, or access/lifecycle transition. `state.json.ledger_head` is the
observer watermark and is published within 100 milliseconds of a newly fsynced
streaming group. It never projects a
head beyond the last fsynced record; an ahead projection is rebuilt and audited
as `projection_rewound`, not treated as integrity failure. V1 does not claim
power-loss durability.

The 100-millisecond publication interval is diagnostic and never fails a run.
`events --follow` waits with `EVFILT_VNODE` on the ledger parent and active
file, reopens after rename, and rechecks the fsynced watermark on every wake;
its 30-second stream heartbeat is the bounded lost-wakeup fallback.

Redaction recursively visits objects within objects and arrays. For ASCII keys,
it splits on `-` and `_`, before an uppercase letter following a lowercase
letter or digit, and before the final uppercase letter of an uppercase run when
followed by lowercase. Tokens are ASCII-case-folded and empty tokens are
dropped. For secret matching only, a trailing ASCII digit run is stripped from
each non-digit token and a digit-only token is discarded; therefore
`password2`, `password_2`, `oauth2_token`, and `api_key_2` match their
digit-free secret sequences. Compact form is the concatenation of this matching
view. Non-ASCII keys never
match but their subtrees are still traversed.

The canonical secret token sequences are `authorization`, `proxy
authorization`, `cookie`, `set cookie`, `password`, `secret`, `client secret`,
`api key`, `access token`, `refresh token`, `id token`, `session token`, `session key`,
`bearer token`, `auth token`, `api token`, `oauth token`, `security token`,
`private key`, `secret key`, `signing key`, `signing secret`, `encryption key`,
`api secret`, `credential`, `passphrase`, and `passwd`. A single trailing ASCII
`s` is removed only from the final token of the candidate matching window.
Matching is whole-token
sequence containment or exact compact equality, never raw substring matching.
Bare `token`, `key`, `auth`, `id`, `session id`, `client id`, `thread id`,
`turn id`, `signature`, `nonce`, and `pwd` are explicit non-secret exclusions.

A match preserves the key and replaces its entire value with
`{"$dolgorae_redacted":{"reason":"secret_key","original_type":"<type>"}}`,
where type is `string`, `number`, `boolean`, `null`, `object`, or `array`.
Over-redaction such as `password_hash` and `api_key_id` is intentional. JSON
encoded inside a string is not reparsed in v1 and remains within the documented
value-secret limitation. If safe classification or serialization fails, Dolgorae
records only `payload_unrepresentable` metadata and never raw bytes.
Run directories use mode 0700 and sensitive files mode 0600. Prompts and command
or tool output may still contain secrets; same-OS-user confidentiality is not
guaranteed.
`<application-support-workspace>/runtime/` is mode 0700 and its records are mode 0600. `worker.log` is
limited to 1 MiB with one rotation and remains diagnostics-only.

Audit completeness is limited to Dolgorae lifecycle, app-server-exposed main-turn
wire traffic, approvals, writer-authority transitions, and profile/account provenance.
Encrypted or otherwise unexposed native-subagent communication is represented
as opaque activity when observable and is not claimed as reconstructable audit.

Reasoning text, reasoning summaries, reasoning deltas, and internal planning
streams MUST NOT be persisted in the ledger, projections, logs, diagnostics, or
exports. The worker MUST independently filter every reasoning method before
representation. Initialization-time suppression is not available on the pinned
0.147.0 production profile, whose SPEC-003 launch contract requires
`optOutNotificationMethods:[]` because reasoning-only methods cannot be
isolated from required native lifecycle evidence. Receipt-side filtering is
therefore the sole normative mechanism for that profile; a future pin that
proves safe isolation MAY additionally request suppression. It appends only the method,
raw byte length, SHA-256, and `reasoning_content_not_retained` classification.
Client-safe events are normalized and schema-validated at append time; later
projection never needs to reinterpret a version-specific raw payload.

The Codex thread in the pinned `CODEX_HOME` is conversation-continuation
authority. Dolgorae's ledger is audit authority. Missing Codex history cannot be
reconstructed as the same session from the Dolgorae transcript, and missing
Dolgorae audit cannot be reconstructed as equivalent audit from Codex history.

Every represented ledger payload obeys the 2 MiB raw and 3 MiB post-transform
limits in SPEC-006. Larger payloads retain only
source kind, byte length, streaming SHA-256, JSON Pointer when known, and reason
in `payload_unrepresentable`; v1 creates no raw or redacted sidecar. `events`
projects only client-event-v1 records. The explicit local `export` bundle may
contain bounded redacted wire evidence, but no public event profile does.
`verify` validates structure, sequence, and hashes but not the truth
of model or command claims. `export` first captures a fsynced
`state.json.ledger_head`, copies `audit.jsonl` only through that complete
record, and regenerates bundled state and transcript projections from that
prefix. It creates a mode-0700 directory containing 0600 `bundle.json`,
`manifest.json`, bounded `audit.jsonl`, `state.json`, redacted
`transcript.jsonl`, and `verification.json`. `bundle.json` records the ledger
boundary as well as
schema version, workspace/run identity, filenames, hashes, and source-derived
timestamps; lexicographic filenames and source bytes make repeated exports
content-deterministic. Runtime records, locks, logs, machine-local Runtime Profile configuration,
`CODEX_HOME`, images, and raw torn-tail evidence are excluded. A failing audit
does not suppress export: both bundle and verification set
`verification_failed:true`, while other state-changing commands fail closed.
The bundle may contain plaintext prompts/output that key-name redaction cannot
detect. Its output
path MUST NOT already exist; Dolgorae never merges or overwrites an export and
returns `INVALID_ARGUMENT` on collision.

Automatically retained probe, recovery, and diagnostic evidence MUST live under
`<application-support-workspace>/evidence/`. An export without an explicit output path defaults to a
create-exclusive child of that directory. A user MAY explicitly request an
external export destination; that copy is user output, not runtime authority.

There is no retention limit or automatic deletion. `run delete` is allowed
only for closed or start-failed runs and requires `--confirm`; it is the sole
state-changing command allowed after audit integrity failure and appends no
record to a ledger it cannot trust. It permanently
deletes the Dolgorae Application Support Run directory only. It MUST NOT delete the Codex thread from
`CODEX_HOME`, and Dolgorae MUST NOT later auto-import that orphaned thread.
When verification fails, this escape additionally requires the final complete
ledger line to remain an independently canonical, self-hashed terminal seal and
the existing mode-0600 `state.json` to identify that exact sequence/hash, Run,
and terminal lifecycle. The broken earlier chain remains reported and no other
mutation is permitted. The confirmed delete path revalidates the exact
mode-0700 Run directory beneath the secure workspace `runs/` directory
immediately before removal.

## SPEC-011: Agent Instruction and Side-Effect Policy

Dolgorae composes two instruction layers.

The **generation-immutable instruction contract** is installed whenever a
thread generation is started, resumed, or forked. It contains the Run ID,
canonical workspace, use-case role presentation, instruction-contract versions,
Native Subagent Policy, and behavior invariants that do not change during that
thread generation. It MUST NOT contain mutable current access, writer ownership,
writer generation, or `policy_epoch`.

Every Turn additionally receives a **dynamic access context** derived at the
mutation serialization point from authoritative writer state and the verified
sandbox carrier. It identifies `read` or `write` access, `policy_epoch`, writer
generation when present, and the closed network policy. A committed Dedicated
access transition updates this Turn-scoped context and increments
`policy_epoch` without changing `run_generation`, thread ID, thread generation,
or immutable Controller instructions.

Role presentation follows authoritative aggregate membership, not model text.
A `direct_interactive` root of a Dolgorae-Orchestrated Session is described as a
Primary Agent. A `managed_agent` member in a Brokered Hierarchy or External
Specialist Engagement is described as an Independent Specialist Agent. A
generic managed Run outside those aggregates is described as an externally
managed workflow agent. `parent_ref` may support non-authoritative provenance
wording but grants no authority.

Dolgorae distinguishes enforced invariants from agent behavior policy.

**Enforced invariants** include:

- Controller and Operator credential validation;
- Run, aggregate, writer, and idempotency state transitions;
- one Dolgorae writer per canonical workspace;
- read-only or workspace-write sandbox selection;
- `networkAccess:false` for v1 shell execution;
- mutable Dolgorae authority stored outside the agent-writable workspace;
- append-only audit and orchestration-journal ownership; and
- fail-closed recovery on ambiguous work, identity, policy, or delivery state.

**Agent behavior policy** is injected as defense in depth and MUST state:

- answering, explaining, reviewing, and diagnosing do not authorize mutation;
- implementation may mutate only when the current Turn context is write;
- safe local in-scope edits and checks may proceed when the task authorizes them;
- destructive actions, external side effects, and meaningful scope expansion
  require Controller direction;
- Git add, commit, and push each require explicit Controller authorization;
- background process creation requires explicit Controller authorization;
- Native Delegation must avoid overlapping write-heavy work and prefer native
  children for independent or read-heavy work; and
- the response reports outcome, material changes or findings, verification, and
  blockers without imposing a rigid model-visible JSON format.

The tracked `.dolgorae/config.yaml` and `.dolgorae/.gitignore` files are ordinary
workspace policy files. Mutable Run, writer, audit, orchestration, profile,
evidence, and cache authority is under Application Support and MUST NOT be in a
Codex writable root. Prompt policy is not a hostile same-user security boundary.

The profile's Codex configuration, AGENTS instructions, skills, plugins, apps,
and checked MCP servers remain available unless they conflict with enforced
invariants. Their mutable external contents are not part of Dolgorae's byte-
immutable role claim. The explicit normalized Controller instructions and Agent
Configuration digest are the durable role snapshot. Side effects performed by
MCP servers, plugins, or apps outside the shell sandbox remain outside the hard
one-writer guarantee.

The selected Runtime Profile MUST explicitly declare `native_subagents:
enabled`. Exact Codex 0.147.0 enabled evidence reports lifecycle observation and
quiescence tracking as `supported`; its disabled diagnostic still created a
child and therefore proves that disable enforcement is unavailable. V1 MUST NOT
claim a per-Run native-subagent opt-out.

## SPEC-012: Orchestration Boundary and Compatibility

Dolgorae uses one shared Run core behind two product-level facades. Run
authority remains hub-and-spoke: only each Run's bound Controller may mutate it,
and a model never receives another Run's Controller credential, Operator
credential, private Worker socket, or database path. Brokered Specialist
Collaboration adds a bounded broker-mediated message path, not peer Run control.

### Aggregate Bootstrap and Facade Entry

Aggregate identity is never inferred from a list of Runs. A parentless
direct-interactive root with a checked Orchestration Launch Intent creates its
Orchestration Session through a durable `create_orchestrated_session` bootstrap
operation. A root without that intent remains a low-level Run. An external
integration creates its engagement only through an explicit
`open_external_engagement` call. That call atomically creates one
`open_external_engagement` bootstrap operation and an empty active engagement,
then returns the Dolgorae-generated engagement ID. Each later
`hire_external_specialist` call targets that ID and durably reserves its hire
operation, member, and child Run before runtime side effects. A raw Run, parent
projection, or later listing MUST NOT lazily infer, attach, or regroup an
engagement.

Each aggregate record stores `bootstrap_operation_id`, and each bootstrap
operation points back to exactly one aggregate. Same-key replay with the same
normalized request returns the original aggregate. Same-key drift is
`IDEMPOTENCY_CONFLICT`. Aggregate bootstrap state, the hash-chained event, and
any prepared Run creation identity are committed before process, lane, thread,
or Turn side effects.

### Dolgorae-Orchestrated Session

A Dolgorae-Orchestrated Session is a first-class durable aggregate whose
`session_id` equals its Primary Run ID in v1. Dolgorae owns the Primary Agent's
semantic orchestration loop, while Gul owns presentation, user input, approval
UX, and any remote authentication boundary.

The internal Orchestration Broker may accept a Primary Agent's advisory request
for configured Specialist work only under the session's explicit approval
policy and immutable Specialist Policy snapshot. The snapshot validates against
`protocol/dolgorae-specialist-policy-v1.schema.json`; its complete JCS SHA-256 is
recorded on the session, and role references resolve only against that snapshot.
Runtime Profile identity never selects role character.

The broker owns a distinct internal Controller credential for every Specialist
Run and keeps all credentials outside prompts, model input, events, audit
payloads, and Gul projections. The Primary Agent may request work and receive
bounded results but MUST NOT mutate, interrupt, resolve interactions for, or
acquire writer authority on a Specialist Run directly.

#### Primary Orchestration Tool

The Primary Run uses the private run-bound payload contract
`protocol/dolgorae-orchestration-tool-v1.schema.json`. The bridge binds session,
source Run, source Turn, tool-call identity, root priority, Controller authority,
and idempotency outside model-controlled arguments. The model cannot provide or
override those fields and cannot select a Runtime Profile, model, Controller,
priority elevation, or access beyond the immutable Specialist Policy. The
checked operations are:

- request and await a Specialist instance;
- list safe Specialist status;
- assign, await, collect, and cancel Specialist tasks; and
- request graceful Specialist release.

Under `user_approval_required`, every new Specialist instance creates a durable
request and a typed `user_input` Controller Interaction on the Primary Run before
any child Run is allocated. Approval resumes the same idempotent request;
rejection records a terminal request result. Under `fully_delegated`, automatic
provisioning is allowed only when the requested role exists in the session
policy, has `auto_approve_when_fully_delegated: true`, remains below its instance
limit, and requires no capability beyond the accepted policy. Missing or
disallowed roles are rejected rather than silently created or silently escalated.

Role reuse is deterministic and does not create a second membership operation.
`never` always requests a new instance subject to limits.
`reuse_idle_compatible` selects an idle active member with the exact same role,
role snapshot, Agent Configuration digest, and admitted access.
`reuse_any_compatible` may also select a busy compatible member, whose later
task is queued normally. Selection orders idle before busy, then lower pending
mail count, then lower Run ID. A reused `request_specialist_result` returns the
existing member's original `spawn_operation_id` with `reused: true`; no new
spawn operation is appended. The Primary Run's durable tool-call/result ledger
binds that reuse decision to the source Turn and tool-call idempotency identity,
so exact replay returns the same member even if queue state later changes.
Reparenting, role conversion, configuration drift, and reuse across sessions are
forbidden.

Assigning a task to an existing ready Specialist does not repeat hire approval.
The task inherits the Primary root priority; the model cannot raise it. A role
selector resolves an existing eligible member deterministically and never
auto-hires a missing role. Cancellation is safe for queued work and uses the
ordinary interrupt and `interrupted_unknown` rules after possible target Turn
acceptance. A compatible existing Specialist may be reused only when its
immutable role policy permits reuse; the operation result reports whether reuse
occurred. Canonical-workspace write intent remains subject to the ordinary
writer protocol and may return a typed writer conflict without changing the
member or task. Release is graceful: it prevents new work, waits for
authoritative terminal work and delivery state, and never discards an unknown
outcome.

Dolgorae is authoritative for the Orchestration Session Record, Primary Run,
Brokered Hierarchy membership, parent-child lineage, role and Agent
Configuration snapshots, owned Specialist lifecycle, write-ahead spawn
operations, accepted Specialist tasks, collaboration exchanges, durable
mailboxes, result-delivery receipts, activation, passivation, writer
coordination, event sequence, audit, and recovery. Standalone Primary and
Brokered Hierarchy are dynamic composition states of this one use case, not
separate user modes.

An active Dolgorae-Orchestrated Session that permits Brokered Specialist
Collaboration MUST have a Gul-supervised foreground `dolgorae serve` control-
plane runtime. The public gRPC adapter remains a transport adapter; the same
process hosts reconstructable broker, scheduler, activation, and private tool-
bridge services. SQLite is the durable authority. No installed daemon is added.

### Brokered Specialist Collaboration

A Specialist in one active Brokered Hierarchy MAY submit a bounded consultation
to an already provisioned Specialist in the same Orchestration Session. The
request is logically direct because the Primary Agent does not relay its body or
result. The request is physically mediated by the Collaboration Plane, which
MUST validate authority, persist the exchange, schedule the target, deliver the
result, and record causal audit state.

The Collaboration Plane MUST NOT provide peer lifecycle control. A Specialist
cannot through collaboration hire another Specialist, obtain a peer credential,
address another session, change a peer role or Runtime Profile, interrupt or
close a peer Run, resolve a peer interaction, or transfer writer authority.
External Specialist Engagements do not support Specialist-to-Specialist
collaboration in v1.

A collaboration-capable Specialist uses a private run-bound tool bridge. Source
Run, source Turn, and tool-call identity MUST be bound outside model-controlled
arguments. The model MUST NOT receive or choose an idempotency key. The bridge
provides separate submit and await operations so multiple requests can proceed
concurrently. A bounded convenience operation MAY compose submit and await.

The pinned transport MUST prove source identity and bounded wait behavior before
implementation. If a shared Profile Server cannot bind a private tool invocation
to one Run and Turn, every collaboration-capable member MUST use a Dedicated
Lane with a private bridge. A model invoking the public CLI or connecting to a
peer Worker remains forbidden.

### Durable Mailbox and Virtual Actor Activation

Each brokered Specialist has a durable SQLite mailbox. The Specialist itself
MUST NOT poll SQLite. The control-plane runtime owns one central Mailbox
Scheduler for all workspaces it serves. After committing a mailbox change it
adds the target Run to an in-memory dirty set and wakes the scheduler. Startup
and one low-frequency global reconciliation scan repair lost wake signals,
expired pre-dispatch claims, and process crashes; this is not per-Run polling.

A Specialist is a Virtual Actor. When resident and idle, the scheduler may
immediately dispatch its next mailbox item. When running or waiting, new items
remain queued and MUST NOT preempt the active Turn. When safely passivated under
`on_mail` policy, the Activation Manager MUST use a compare-and-swap transition
and one bounded activation lease to start a new Worker generation, start or
reuse the physical lane, resume the recorded thread, verify effective
configuration, and wake the scheduler. Concurrent mail MUST produce one
activation and multiple queued items, not duplicate Worker generations.

`passivated` is distinct from core Run `paused` and `closed`. A passivated Run is
logically active and resumable. A paused Run requires explicit authorized
resume. A closed, retired, or released Run is terminal and MUST reject new mail.
Activation failure retains queued mail, records a safe blocker, and marks the
member unavailable; it MUST NOT discard the request or silently replace the
Specialist.

Passivation is allowed only while the core Run is idle, owns no writer, has no
pending interaction, has no blocking outbound collaboration, has no required
unacknowledged result, has no dispatchable mailbox item, satisfies background-
process cleanup, and has authoritative resumable thread state. Passivation
preserves Run ID, logical lane, thread ID, Agent Configuration, aggregate
membership, mailbox, and audit history.

### Scheduling, Priority, and Backpressure

A target Run executes at most one Turn at a time. Collaboration queueing is
owned by the Orchestration and Collaboration layer; low-level public `run send`
and `run submit` keep their existing `RUN_BUSY` behavior.

The scheduler MUST choose the next dispatchable item using this deterministic
order:

1. internal recovery or lifecycle work;
2. starvation override after the session policy limit;
3. inherited root priority: `interactive`, then `normal`, then `background`;
4. dependency-unblock boost when the source Turn is actively waiting;
5. earliest deadline; and
6. oldest mailbox sequence.

A Specialist cannot raise its own priority. A collaboration exchange inherits
its root Specialist task priority. The scheduler MUST enforce source fairness,
no-preemption, queue and fan-out limits, bounded blocking depth, and an acyclic
blocking-wait graph. Exact queue position is not a contract because priority,
aging, cancellation, and recovery can reorder items.

The default v1 collaboration-policy snapshot is: starvation limit 300 seconds;
maximum two consecutive items from one source; maximum 16 pending items per
Specialist, four per source-target pair, and 128 per session; maximum two
blocking waits per Run, depth two, and eight exchanges per root task; maximum
65,536 inline request bytes; passivation after 600 idle seconds; activation
timeout 30 seconds with three attempts; and one global reconciliation scan every
30 seconds. Large context MUST be passed by immutable artifact reference.

A blocking collaboration MUST be rejected if the source owns writer authority
or if adding its wait edge creates a cycle. Collaboration never implies writer
handoff. The source may submit a nonblocking read-only request, release writer
work, and await later.

### Request, Response, and Recovery Semantics

A Collaboration Exchange has independent execution and delivery state. Request
submission, mailbox insertion, event append, and any activation-request marker
MUST commit in one SQLite transaction before the in-memory wake. Target result
commit, immutable result artifact reference, source result-mailbox insertion,
and event append MUST also commit in one transaction.

A busy target queues the request. The active Turn is never interrupted for a
higher-priority mailbox item. A short claim lease covers only the boundary before
authoritative target Turn acceptance. It MUST NOT permit another executor to
steal or replay a running LLM Turn. Unknown Turn acceptance or outcome becomes
`interrupted_unknown` and is not automatically replayed.

Result notification is at least once and consumption is idempotent by exchange
ID and immutable result artifact. A transport await ending does not cancel the
exchange. A completed result may remain `pending` in the source mailbox and be
awaited or collected after source restart. Dolgorae MUST NOT start an unsolicited
continuation Turn solely because a result arrived.

### External Specialist Engagement

An External Specialist Engagement exists when another AI is already the Primary
Agent and semantic control plane. It is opened and operated through the checked
private CLI or MCP payload contract
[`dolgorae-external-specialist-facade-v1.schema.json`](protocol/dolgorae-external-specialist-facade-v1.schema.json).
Opening is an explicit, empty aggregate operation. The adapter binds the
canonical workspace and an aggregate-owner Controller credential outside the
payload. Dolgorae stores an immutable Aggregate Controller Binding containing
Controller ID, generation 1, kind, normalized-principal digest, and capability
digest. The opaque external reference is immutable semantic provenance, not an
authorization token. Every later facade call must present the same owner
credential and is rejected before observation or mutation when the workspace or
binding differs. Hiring additionally creates a member Run through a write-ahead
hire operation and a separately supplied per-Run Controller carrier. Raw managed
Run creation never infers or joins an engagement.

The checked facade operations are `open_external_engagement`,
`get_external_engagement`, `hire_external_specialist`,
`assign_external_specialist_task`, `await_external_specialist_tasks`,
`collect_external_specialist_results`, `cancel_external_specialist_task`,
`release_external_specialist`, and `close_external_engagement`. `Open` creates
only the empty durable aggregate; `get` exposes only safe operational membership
and task counts. Cancellation follows the ordinary fail-closed Turn-acceptance
boundary, and close applies explicit complete or abort semantics.

The external AI decides the objective, task
graph, role selection, dependencies, retries, replacement, and semantic
completion. Dolgorae MUST NOT create a Primary Run or another orchestration loop
for that engagement.

Dolgorae owns only the hired Specialist boundary: engagement identity, external
provenance, Specialist membership, role and Agent Configuration snapshots,
idempotent hire operations, accepted tasks, results, delivery receipts, Run and
Turn state, interactions, writer state, audit, and recovery. It does not persist
or infer the external AI's full plan or workflow graph. A Specialist in this use
case MUST NOT hire another first-class Dolgorae Specialist or address another
Specialist through the Collaboration Plane in v1; the external control plane
hires and coordinates additional roles directly.

External Specialists default to read-only analysis or isolated change
production. Direct writes to the canonical workspace are safe only when the
external integration quiesces its own writer and participates in Dolgorae's
writer protocol. Writes by external tools remain outside Dolgorae serialization.

### One-Shot Specialist Review Adapter

Dolgorae MUST provide a narrow convenience adapter for the first and most common
External Specialist Engagement flow: one independent read-only review of the
current working tree. The canonical Machine CLI command is:

```text
dolgorae specialist review \
  --workspace <workspace> \
  --profile <reviewer-runtime-profile> \
  --scope working-tree \
  --format json
```

The optional external stdio MCP adapter exposes exactly one corresponding
model-facing tool named `dolgorae_review`. Both entry points use the checked
review payloads in
[`dolgorae-specialist-review-tool-v1.schema.json`](protocol/dolgorae-specialist-review-tool-v1.schema.json).
The CLI wraps a successful review result in the ordinary checked machine
envelope with command tag `specialist.review`; an enabled MCP adapter returns
the checked review result or checked review error directly. Both compile to the
ordinary External Specialist Facade sequence: explicit engagement open,
read-only Reviewer hire, one task assignment, bounded await, result collection,
release, and close. This adapter is not a third user-facing use case and does
not create a Primary Run, task graph, Brokered Hierarchy, or Collaboration
Exchange.

The Machine CLI path is mandatory for `MILESTONE-SR1`. The MCP path is
capability-gated by a pinned-host probe. MCP is stateless across requests: the
adapter MUST NOT infer logical-request continuity from a connection, JSON-RPC
request ID, or stdio process. Replay-safe mode requires the trusted host to
generate one UUIDv7 per logical call and repeat it unchanged in
`tools/call params._meta` under the checked vendor key
`xyz.rootkernel.dolgorae/externalRequestRef`. The metadata fragment is validated
against
[`dolgorae-specialist-review-mcp-meta-v1.schema.json`](protocol/dolgorae-specialist-review-mcp-meta-v1.schema.json).
The model cannot supply or override this value through tool arguments. Same
reference and same normalized request return the original review identity;
same reference with different input returns `IDEMPOTENCY_CONFLICT`.

If the pinned host cannot prove preservation of the per-request reference
across every supported retry and reconnect boundary, the MCP tool MUST NOT be
advertised for SR1 and Codex CLI uses the Machine CLI carrier through its shell
tool. There is no connection-derived, process-derived, or best-effort MCP
fallback. Same reference with different normalized input returns
`IDEMPOTENCY_CONFLICT`, `retryable:false`, and required action
`fix_host_request_carrier` without allocating another Reviewer Run.

The model-visible request contains only the review objective, fixed
`working_tree` scope, allowlisted focus dimensions, fixed structured result
contract, and bounded deadline. Canonical workspace, Runtime Profile,
aggregate-owner and per-Run Controller credentials, external provenance,
request identity, and idempotency MUST be bound by the trusted adapter and MUST
NOT be accepted from model arguments. The external MCP adapter does not require
Dolgorae source Run or Turn identity because the external AI is already the
semantic control plane.

The Reviewer MUST be an independent `managed_agent` Run with a separate Codex
thread, immutable Reviewer Agent Configuration, canonical-workspace read-only
access, and shell network disabled. Its Runtime Profile MUST NOT register the
`dolgorae_review` adapter. The semantic service MUST also reject nested
first-class Specialist creation from the externally hired Reviewer.

A successful review MUST validate against the checked result shape, store one
immutable result artifact, contain no hidden reasoning or raw protocol frame,
report `workspace_write_observed: false`, and order findings by `P0`, `P1`,
`P2`, then `P3` with stable input order within one severity. Reviewer failure,
timeout, cancellation, invalid structured output, observed workspace mutation,
or unknown Turn acceptance or outcome MUST produce a checked non-success
result. Unknown work MUST NOT be replayed automatically.

The first adapter profile supports one Reviewer, one active task, read-only
access, and working-tree scope only. It does not queue a second task, retain a
reusable member after the adapter closes the engagement, or enable lateral
Specialist collaboration. Later durable External Specialist Engagement features
extend the same aggregate and Run contracts rather than replacing this adapter.

### Durable Broker State and Operations

The authoritative orchestration store is
`<application-support-workspace>/orchestration/orchestration.sqlite3` in WAL
mode with foreign keys enabled and full synchronous durability. A hash-chained
append-only event table is committed in the same transactions as aggregate,
mailbox, activation, execution, and delivery state. JSONL and JSON snapshots are
exports or replaceable diagnostics, never coequal authorities.

The checked `dolgorae-orchestration-state-v1.schema.json` owns the exported
materialized shape for aggregate bootstrap operations, both aggregates,
membership, spawn or hire operations, Specialist tasks, collaboration exchanges,
mailbox items, and activation
operations. Cross-object validity is owned by the executable
`protocol/validators/validate_orchestration_state_v1.py`; schema-only acceptance
is insufficient. At minimum the SQLite authority records:

- aggregate bootstrap operation ID, kind, idempotency key, request digest,
  provenance, state, and timestamps;
- aggregate ID, kind, status, revision, semantic owner, and collaboration-policy
  snapshot;
- Primary Run or external Controller provenance and, for an External
  Specialist Engagement, the immutable Aggregate Controller Binding containing
  public Controller identity, generation, kind, normalized-principal digest,
  and capability digest;
- member Run ID, parent relationship where applicable, ownership, role reference,
  role snapshot digest, Agent Configuration digest, membership, and actor
  residency;
- idempotency key and write-ahead state for each spawn, hire, and collaboration
  operation;
- source, target, root task, parent exchange, priority, deadline, and causal depth;
- mailbox sequence, item kind, claim lease, and delivery receipt;
- activation trigger, lease, attempts, outcome, and safe blocker;
- independent execution and delivery states, including durable pending results
  and `interrupted_unknown`; and
- aggregate event sequence and hash-chain head.

Child Run identity and the operation record MUST be committed before starting a
Worker, physical lane generation, or Codex thread. Same-key and same-normalized-
identity replay returns the original operation. After restart, a durably
completed but undelivered result is redelivered without re-execution. A task or
exchange whose outcome is not authoritative is never automatically replayed.

A Primary Run failure marks its Dolgorae-Orchestrated Session `degraded` or
`recovering`; owned Specialists remain available for reconciliation and are not
automatically destroyed. New collaboration requests are not accepted while the
Primary orchestration authority is unavailable. Explicit completion or abort
applies ordinary Run close and interruption rules to owned Specialists and
queued work. Attached external state is never destroyed merely because an
engagement or client disconnects.

One Run may belong to at most one active aggregate. V1 forbids active
reparenting, in-place role conversion, nested first-class Specialist hiring from
an External Specialist Engagement, and in-place conversion between the two use
cases. Parent-held Run-control capabilities, direct Worker-to-Worker transport,
unbounded peer chat, and cross-Run model authority remain future work. Bounded
broker-mediated Specialist Collaboration is part of Brokered Hierarchy v1.

The existing public v1 gRPC Run operations remain the low-level Gul wire
contract. A direct-interactive root `StartRun` carrying checked launch metadata
in its protected Controller carrier is aggregate-aware inside the semantic
service and creates the Orchestration Session without a new RPC. The
Machine CLI additionally carries the private External Specialist Facade through
`engagement call --request-fd`; trusted MCP adapters use the same checked
payloads. Primary orchestration and Specialist collaboration use separate
run-bound private tools. None of these private facades changes the checked public
Protobuf source or descriptor. A future additive read-only aggregate query may
improve presentation, but it is not required for state ownership or recovery.

Gul v1 obtains a safe operational view from the existing Primary Run stream,
Controller Interactions, `ListRuns`, and public parent projections. It MAY show
that a Specialist was requested, provisioned, busy, passivated, degraded, or
retired, but MUST treat those projections as presentation data. It MUST NOT use
`DiagnosticReported` strings, local caches, or reconstructed parent links as
aggregate recovery authority.

`parent_ref` remains authority-neutral provenance on the public Run contract.
For brokered members, the internal Orchestration Session Record is the canonical
membership authority. For externally hired Specialists, the Specialist
Engagement Record is the canonical operational membership authority. A parent
reference or collaboration exchange never authorizes a Run mutation or proves
Controller ownership.

The shared Profile Server has one profile-generation environment, so Dolgorae
MUST NOT claim that a per-Run `DOLGORAE_*` marker reaches commands, MCP servers,
or native subagents. A diagnostic marker, if observed, is advisory only. A
command launched by Codex that invokes Dolgorae without the Run's Controller
capability may use ordinary same-uid client-safe observation but cannot mutate
that or another Run. Controller and Operator credentials are never placed in
the workspace, prompts, argv, App Server environment, or client-safe events.

Native Subagent Policy is orthogonal to both user-facing use cases. Native
children belong to their parent Run and App Server-managed session tree. They do
not create Independent Specialist Runs, aggregate members, Controllers, Workers,
or writer authorities. Active or unknown native state blocks every operation
that requires quiescence.

Dolgorae uses the stable app-server API surface plus the narrowly pinned
`item/tool/requestUserInput` capability. A connection that requires tested
user-input may advertise `experimentalApi`; all other experimental requests
remain unsupported and are not implied by that carrier. Dolgorae validates
0.147.0 as tested. For an
unlisted newer version, Dolgorae may run the version as `unverified` only when:

1. `codex app-server generate-json-schema` is available;
2. the manifest comparison engine resolves `$ref` before comparison, addresses
   named definitions rather than `$ref` keys or positional `oneOf` indices,
   preserves type/const/requiredness, and verifies required enum values by set
   containment; the engine reads and obeys the manifest's own additive-change
   flags;
3. live initialize, `initialized`, paginated `model/list`, actual `codexHome`,
   absent-thread `thread/read`, persisted resume/read, early response-ID,
   sandbox, terminal notification/status, fork-boundary, pending-restart,
   effort, and required server-request probes pass. Every
   `behavioral_observations` entry MUST be re-measured or the version is
   rejected, and each probe reads its expected value from the checked manifest.

Missing generation support, required schema, lifecycle behavior, or identity
causes fail-closed rejection. Unknown additive fields and notifications are
recorded and tolerated. Unknown server requests are recorded, receive JSON-RPC
method-not-found, and do not stop the generation; unparseable frames fail
closed. All app-server messages are correlated with request ID, thread
ID, turn ID, and run generation before affecting state.

The required-subset manifest has two independent eligibility fields.
`architecture_contract_eligible` is owned by TASK-000-D and becomes true only
after checked artifacts, reproducible pinned evidence, the self-contained
package, and a no-P0/P1 independent architecture review agree.
`production_runtime_eligible` is owned by TASK-015 and remains false until the
implemented two-profile runtime passes every production smoke, migration,
cleanup, interaction, artifact, and review gate. Architecture closure never
promotes production eligibility.

## SPEC-013: External Runtime and Controller Contract

The Machine CLI and supervised local gRPC gateway are the two public v1
adapters. The gateway is optional for finite low-level clients and required for
any live Gul Orchestrated Session, including Standalone Primary, Brokered
Hierarchy, and later Brokered Specialist Collaboration. The Machine CLI is the
mandatory SR1 carrier. Both adapters MUST call one semantic application service
with identical state
transitions, authorization, idempotency, errors, audit effects, and safe
projections for every overlapping operation. The CLI additionally owns
the built-in credential-creation command, Operator-authorized profile/repair
operations, and path-writing export. No credential-creation RPC exists; a
trusted same-user client may instead create a checked-schema Controller carrier
under its capability-advertised descendant. The private per-Run worker socket, shared/dedicated App
Server sockets, direct WebSocket transport, and Codex protocol remain private.
V1 provides no public MCP adapter, TCP listener, remote bind, direct Tailscale
exposure, remote authentication, client-streaming, bidirectional streaming, or
workspace-wide event stream. The private run-bound collaboration bridge in
SPEC-012 is an internal control-plane transport and is not a public adapter.

`runtime capabilities` and `GetCapabilities` MUST return finite machine/event/
RPC protocol versions, accepted client range, checked descriptor SHA-256,
supported transports and methods, projection/timeline versions, stable
Dolgorae feature flags, and known interaction kinds. `supported_transports` is
exactly `machine_cli` and `local_grpc`; `public_local_socket` is true while
`workspace_event_stream` remains false. It also exposes
`access_policy_transition` as
`unverified`, `supported`, or `unavailable`; only a successful pinned live
transition probe may produce `supported`. `profile doctor` and `profile show`
MUST additionally return the profile-specific Codex capability snapshot.
It exposes `background_execution_control` as a closed object containing
`support` (`supported`, `unavailable`, or `unverified`) and `mechanism`
(`dedicated_lane_process_census`, `hybrid`, or null). A profile becomes
`supported` only after same-home shared/dedicated concurrency, fixed residency,
closed-generation history, identity census, cleanup, and unrelated-process
non-signalling pass live probes. Codex 0.147.0 uses
`dedicated_lane_process_census`; a future pinned, complete native terminal API
may upgrade the mechanism to `hybrid` but is not a release prerequisite.
It also exposes profile-specific `native_subagents` as a closed object containing
`lifecycle_observation`, `disable_enforcement`, `quiescence_tracking`, and a
bounded reason. A feature flag or successful root turn alone MUST NOT produce
supported lifecycle or quiescence; the pinned probe must observe the two exact
native item families, child identity, parent relationship, ordered
active-to-terminal lifecycle, persisted history, restart behavior, and cleanup.
A binary-level query without a profile reports lifecycle and quiescence as
`unverified`. The exact 0.147.0 enabled probe passed that complete gate. Disable
enforcement is `unavailable` because the diagnostic disabled case still created
a child. A later pin must rerun the same gate; a policy change still
requires operator-authorized profile migration. Binary-level support
does not override a rejected or incapable profile. A run declaring a required
capability MUST fail before allocation when that profile does not provide it.
The stable binary feature `brokered_independent_subagent_runs` retains its
existing wire name for compatibility. It is true when SPEC-012's public-adapter
composition, Controller separation, and durable operational records for
Independent Specialist Runs are implemented. The internal Orchestration Broker
and every external Specialist client MUST require that feature before presenting
Brokered Hierarchy composition or External Specialist Engagement as supported.
The flag does not advertise an MCP adapter, Operator authority, parent-held
delegation capability, or ownership of an external AI's plan. Durable Brokered
Hierarchy and External Specialist operational state are Dolgorae authorities
defined by SPEC-012.

A controller credential is a strict object conforming to the checked v1 schema.
It contains a UUIDv7 `controller_id`, one of `human_cli`,
`interactive_client`, `workflow_orchestrator`, `automation`, or `other`, a
nonempty instance ID, optional subject ID, exactly 32 random capability bytes
encoded as unpadded base64url, and optional non-secret `orchestration_launch`
metadata. That metadata is valid only for a `human_cli` or
`interactive_client` credential used to create a parentless
`direct_interactive` root. `--orchestration-policy <name>` emits
`use_case: dolgorae_orchestrated_session` plus the explicit policy name; it is
invalid for all other credential kinds. The metadata participates in root
StartRun idempotency but not in Controller principal normalization or capability
digest comparison. It is ignored for later authorization after its accepted
snapshot is stored, is never copied into a child credential, and never grants
membership or mutation authority. Instance and subject IDs are at most 128 and
256 UTF-8 bytes and reject NUL and control characters. The credential file
is caller-owned, same-uid, regular, no-symlink, mode 0600, at most 4 KiB, and
opened before worker discovery. The inherited-fd form has the same 4 KiB bound.
The two carriers are mutually exclusive. Dolgorae MUST strip the secret before
serialization, logging, audit, worker argv, environment construction, or error
reporting and persist only the domain-separated digest defined above.
Capability validation decodes the URL-safe alphabet without padding, requires
exactly 32 bytes, re-encodes canonically, and compares the result to the supplied
43 characters. Every `x-maxUtf8Bytes` schema annotation has a mandatory runtime
validator; JSON Schema character-count validation alone never satisfies a byte
limit.

The non-secret Controller principal key is `(kind, subject_id)` when
`subject_id` is present and `(kind, instance_id)` otherwise. “Same principal”
means exact byte equality of that normalized pair; it is a local correctness
identity, not remote authentication. A write-continuation destination MUST use
a different Controller UUID, generation 1, and the same principal key as the
source. The source and destination Controller UUID/generation are both part of
the operation's idempotency identity.

The gRPC adapter MUST NOT accept Controller or Operator secret bytes in a
Protobuf field or gRPC metadata. Its `ControllerCarrierRef` contains only an
absolute protected file path plus the expected public Controller ID and
generation. The path MUST remain below the canonical current-uid-owned
mode-0700 Application Support root `Dolgorae/controller-carriers/`. Immediately
before authorization, the semantic service MUST reopen it descriptor-relative,
reject every symlink, require a same-uid mode-0600 regular file no larger than 4
KiB, parse the expected Controller identity, compare the current Run generation
and capability digest in constant time, and zeroize capability bytes. Carrier
path, secret bytes, secret digest, raw content, and mismatch subreason MUST NOT
appear in a response, status detail, log, event, audit payload, or metadata.

`run controller verify` and `VerifyController` are side-effect-free. They MUST
perform the complete serialization-point carrier and target-Run authorization
check but MUST NOT start or attach a worker, append to the ledger, change a
projection, reserve idempotency, or update last-access metadata. Success returns
only the verified public Controller ID, generation, kind, Run ID, and
verification time; a mismatch uses the same non-oracular `CONTROLLER_MISMATCH`
shape as a mutation.

Controller authorization applies before worker attachment or any external
effect to `send`, `submit`, `respond`, `interrupt`, `set-effort`,
`create-write-continuation`, write acquire
or release, pause, resume, recover, reconcile, fork, close, delete, and writer
handoff. Controller equality requires both the controller ID and a constant-time
comparison of the persisted capability digest. A normal fork inherits the
controller. A mismatch is non-retryable and MUST NOT reveal whether controller
ID or capability comparison failed. All same-uid local callers may list Runs and read status, wait results,
pending interaction summaries, client-safe events, writer status, and
verification without a capability. Whole-Run export requires immediate
Controller authorization because it may contain complete prompts, responses,
and operational history. Controller metadata is visible; capability
bytes and their digest are never visible. External applications own any
authentication and authorization applied before exposing those observer results
remotely.
Full interactions, controller-only artifacts, and the safe Run timeline require
the same immediate Controller revalidation even though they are reads.

The operator credential is a separate UUIDv7 and canonical 256-bit capability
registered by create-exclusive `operator credential initialize` in the
user-private Application Support root. Only its domain-separated digest and
monotonic generation persist. Rotation requires the current credential and
atomically publishes a new digest; loss fails closed. Operator credentials use
the same file/fd carrier checks and secret exclusions as controllers and never
authorize ordinary turn mutations.

Rotation and every operator-authorized operation serialize through the global
`operator.lock`. The initiating manager retains its already-open descriptor;
when another worker is the authoritative consumer, it transfers that descriptor
with `SCM_RIGHTS`. While holding the lock, that consumer reloads operator ID,
generation, and digest, rereads the bounded capability, compares it in constant
time, and zeroizes the bytes before acquiring the next lock or applying effects.
A pre-lock CLI check or prior generation can never authorize an operation after
a concurrent rotation.

The one global acquisition hierarchy is:

```text
operator.lock
-> canonical-home home.lock
-> server.lock (multiple server keys in ascending decoded 32-byte order)
-> workspace handoff.lock
-> workspace writer.lock
-> run startup locks (multiple UUIDs in ascending RFC 4122 16-byte order)
-> in-process run mutation mutexes (same UUID order)
```

No operation acquires upward or waits for an external process, WebSocket
response, turn completion, approval/user input, compatibility probe, process
spawn/exit or signal/absence proof while holding a filesystem lock. Every
multi-stage operation uses `prepared`, `applying`, and terminal
`committed|failed|blocked_unknown|reconciliation_required` states. PREPARE
persists a UUIDv7 operation token, expected revisions/generations/epochs and
conflict fence, fsyncs, and releases locks. APPLY performs all external work
without coordination locks. COMMIT reacquires the identical ordered prefix,
revalidates the token and every operand, and fsyncs one terminal result. A stale
token never restores a prior assumption or creates two authorities.

| Operation | PREPARE/COMMIT lock prefix | APPLY lock prefix |
| --- | --- | --- |
| Ordinary read turn | Run mutation mutex | None during App Server wait |
| Threadless first write / reader promotion | Writer, run startup/mutation | None |
| Existing writer turn | Writer validation, run mutation | None |
| Writer release | Writer, run mutation | None |
| Writer authority operator reset | Operator, writer, run startup/mutation | None during absence proof and census |
| Handoff prepare/commit | Handoff, writer, source/destination run locks in UUID order | None |
| Handoff cancel | Handoff, then affected run locks in UUID order | None |
| Write-continuation creation | Source/destination run locks in UUID order | None; allocation and capability delivery occur after PREPARE |
| Controller reset, non-writer | Operator, run startup/mutation | None when external work is needed |
| Controller reset, writer | Operator, writer, run startup/mutation | None |
| Run deletion | Writer when named by authority, then run startup/mutation | None |
| Membership mutation | Server, membership transaction | None |
| Profile start | Home, server | None during spawn/initialize/probes |
| Profile stop/restart | Operator, home, server | None during quiesce/termination/absence proof |
| Profile migration | Operator, home, old/new server locks in server-key order | None during old absence/new start/reconciliation |

Lock-order inversion is an implementation invariant failure and deterministic
test failure, not a public `LOCK_ORDER_VIOLATION` error.

`run controller reset` is an exceptional same-user correctness override, not a
security boundary. It requires the operator capability, an exact run-ID
confirmation, and a distinct new controller credential. A non-writer PREPARE
uses operator then run locks. A writer PREPARE uses operator, writer, then run,
fences new turns and fsyncs a reset token before releasing all locks. APPLY
interrupts/inspects work, applies reader policy and verifies effective policy
without coordination locks. COMMIT reacquires the same prefix, revalidates the
token, releases writer authority, increments controller generation, installs
the new controller and terminally resolves the token in that order. Any failure
leaves the old controller authoritative and writer state active or
`blocked_unknown`; it never installs the new controller first. Active turn,
pending interaction, handoff, or unverifiable generation remains a rejection.
Paused and `outcome_unknown` resets retain all recovery evidence.

Purpose and public parent metadata are authority-neutral.
`purpose.kind` is the closed enum `interactive`, `planning`, `implementation`,
`review`, `research`, `discussion`, `workflow_stage`, or `other`; an optional
external label is at most 128 UTF-8 bytes. `parent_ref.namespace`, `kind`, and
`id` are all-or-none, limited to 128, 64, and 256 UTF-8 bytes, and reject
NUL/control characters.

`parent_ref` supports filtering, presentation, role wording, and audit
provenance but never grants lifecycle or mutation authority. A
`direct_interactive` Primary Run MUST NOT carry a parent reference. A
`managed_agent` Run may carry external provenance, but Brokered Hierarchy and
External Specialist Engagement membership are established only by the durable
aggregate registry. Dolgorae does not implement the external AI's mission,
task graph, findings, or review-disposition semantics.

Client-safe event cursors are canonical decimal-string run-ledger sequences and
survive observer disconnect, worker replacement, and server epoch changes.
Every record identifies its originating server key/epoch. Multiple observers
never apply backpressure to the App Server connection, hold writer authority, or affect pending
interaction persistence. Retention equals the run ledger lifetime in v1.
Duplicate transport delivery is permitted, but an event ID plus cursor MUST
make deduplication deterministic and replay MUST NOT execute a side effect.

## SPEC-014: Control Modes, Execution Lanes, and Assurance

Every Run MUST durably record immutable `control_mode`, `execution_lane`,
required assurance, purpose, Runtime Profile snapshot, and Agent Configuration
snapshot at creation. `control_mode` is `direct_interactive` or `managed_agent`;
`execution_lane` is `shared_readonly` or `dedicated`. The semantic service MUST
reject omission and every `UNSPECIFIED` value. User-facing clients may resolve
the low-level mapping from the selected use case, but hidden service defaults do
not exist.

A Dolgorae-Orchestrated Session Primary Run uses `direct_interactive` with a
`human_cli` or `interactive_client` Controller and no public parent reference.
Its accepted Run ID is the session ID. An internally brokered Specialist uses
`managed_agent` with an internal `automation` Controller, authoritative
Orchestration Session membership, and the presentation-only parent reference
`dolgorae.orchestrated-session.v1 / specialist / <session_id>`. An External
Specialist Engagement uses `managed_agent` with a `workflow_orchestrator` or
`automation` Controller and the grouping parent reference
`dolgorae.external-specialist-engagement.v1 / specialist / <engagement_id>`.
Generic managed workflow Runs remain valid outside either aggregate only with a
non-reserved provenance namespace. Controller kind `other` MUST NOT bind a v1
Run.

`purpose` is one of `interactive`, `planning`, `implementation`, `review`,
`research`, `discussion`, `workflow_stage`, or `other`. The canonical purpose
is the immutable object `{kind,external_label}`. Purpose is descriptive and
MUST NOT select or change execution lane, writer authority, Controller,
aggregate ownership, or assurance policy.

Instructions use `dolgorae.instructions/v1`: a generation-immutable common,
mode, purpose, and role prefix plus bounded Controller instructions. Current
access is not part of that immutable prefix. Every Turn carries a separate
access context derived from authoritative policy and writer state, including
`policy_epoch`. The Controller capability remains outside prompts, developer
instructions, model or tool input, environment, machine output, audit, events,
diagnostics, and persisted Run projections.

For a shared Run, every Turn additionally carries Codex
`collaborationMode:{mode:"plan",settings:{model:<selected>,developer_instructions:<composed>}}`,
read-only sandbox, `networkAccess:false`, and `approvalPolicy:"never"`. Its
behavior policy prohibits workspace modification and directs write-requiring
validation to a Dedicated write continuation. For a Dedicated Run, a verified
read/write transition changes only effective access and increments
`policy_epoch`; it does not change Run generation, thread generation, thread
identity, lane, process generation, or server epoch. This remains a personal-
alpha behavior and coordination boundary, not hardened per-Run process
containment.

Observers receive only authorized redacted projections and MUST NOT resolve an
interaction. V1 has no single-use observer delegation.

Every persisted Run state and every machine-readable Run projection MUST pass
both `dolgorae-run-state-v1.schema.json` and the executable normative validator
`tools/validators/run_state_semantic_validator_v1.py` after extracting the
shared state fields. Machine projections additionally pass
`tools/validators/validate_run_projection_v1.py` with authoritative policy,
selected-server, shared-server, and lineage context.
Schema-only acceptance is insufficient; persistence and projection fail closed
when the validator rejects any cross-field invariant.
The module's `validate_run_state_v1` helper checks structural cross-fields for
fixtures only. The normative persistence CLI calls
`validate_authoritative_run_state_v1`, which requires committed-policy and
selected-server context and, for lineage, authoritative source/workspace and
destination identity. Missing context is rejection, never an omitted check.

One profile owns one shared read-only logical lane and zero or more dedicated
logical lanes. A shared Run's persistent thread is loaded only in the shared
server and its effective policy MUST remain verified read-only. It MUST NOT
acquire workspace writer authority or be promoted in place. When it needs
write, Dolgorae creates a lineage-linked dedicated write continuation or returns
`SHARED_RUN_WRITE_FORBIDDEN`. Read-only foreground commands are allowed in Plan
Mode. Shared background control is `profile_aggregate_only`: Run close cannot
claim per-Run descendant cleanup, and profile stop owns complete aggregate
census and cleanup.
The shared lane is for lightweight read-only analysis. Select a dedicated lane
for compiler or test execution, formatters, file watchers, long-running
validation, background processes, reliable per-Run command lifecycle, or
higher cleanup assurance. Scheduling follows expected command behavior,
expected write behavior, and required assurance—not purpose alone. A planning
or review Run that executes substantial local tooling therefore uses a dedicated
lane.

A write-capable Run owns one UUIDv7 dedicated logical lane for its entire
lifetime. Its thread MUST be started, resumed, and read only through that lane.
The lane may be read-effective without writer authority or write-effective
while it holds authority. Policy changes occur on the same App Server process
generation; lane selection and thread residency never change. Server existence
MUST NOT imply writer authority or workload activity. Run creation publishes
the logical lane with absent physical state and null thread/server epoch. First
input starts the physical generation. V1 performs no automatic idle shutdown;
explicit pause alone stops a quiescent generation. A profile diagnostic warns
before starting a sixth concurrent dedicated physical generation but imposes no
hard cap.

A dedicated lane may stop its physical App Server while paused. It may start a
new process generation only after the previous generation and every recorded
descendant have exact `Absent` identity, the five-sample complete empty census
has passed, no active or unknown interaction, turn, or native descendant
remains, and persisted `thread/read` history satisfies the revision/digest
barrier. The new generation keeps the same lane ID, receives a new process
generation and globally unique server epoch, and resumes the thread only in
that lane. Failure returns `DEDICATED_HISTORY_BARRIER_FAILED`,
`BACKGROUND_EXECUTION_UNVERIFIED`, or `RECOVERY_REQUIRED`; it never tries a
different logical lane.

Workspace writer authority remains per canonical workspace: zero or one active
writer and zero or one write-effective Run. One profile MAY have concurrent
writers in different workspaces. Every active writer MUST use a dedicated lane
whose effective policy is verified `write` for the same Run and generation. A
second writer for the same workspace is `WRITER_BUSY` regardless of Controller,
mode, or App Server. Handoff retires authority without moving either thread;
source and destination retain their own lanes.

Dedicated server infrastructure and workload background state are independent
authorities. `server_lane` records logical lane ID, process generation, server
epoch, lifecycle, and socket identity. `workload_background_state` records
process-census mechanism, revision, observed count, quiescence, and verdict.
The server leader and log drainer are infrastructure, not workload descendants.
The Dolgorae-owned 100-ms exact-identity census remains authoritative for the
personal-alpha boundary. The experimental 0.147.0 background-terminal API is
advisory because the pinned live probe returned no entry after the background
workload request.

Profile stop, restart, and migration MUST enumerate the shared lane and every
dedicated-lane journal record. Stop fences new work, drains or interrupts each
Run under controller/operator rules, proves every physical generation absent,
and then stops the shared server. Restart brings back only the shared server;
dedicated generations start lazily when their Runs resume. Migration persists
one operation over the shared lane and every dedicated lane; partial
enumeration is `PROFILE_MEMBERSHIP_INCOMPLETE` and an incompatible live lane is
`PROFILE_LANE_MIGRATION_REQUIRED`.

Assurance levels are ordered `best_effort_personal_alpha`,
`verified_thread_scoped_control`, and `strong_process_containment`. Run creation
MUST compare `required_assurance` with the profile snapshot before allocating a
Run ID, lane, thread, or server. Failure is `ASSURANCE_LEVEL_UNAVAILABLE`.
Requested and achieved levels are durable Run state. Codex 0.147.0 is capped at
`best_effort_personal_alpha`: same-home, policy transition, multi-workspace,
closed-generation history, and Dolgorae process-census cleanup tests passed.
Background-terminal completeness failed. The prior native-subagent semantic
result contradicted its retained wire shapes and is withdrawn. The corrected
campaign recognized `subAgentActivity` and `collabAgentToolCall` per case and
proved enabled parent/child identity, active-to-terminal lifecycle, persisted
history, restart continuity, and cleanup; enabled 0.147.0 therefore reports
`supported`. Active or unknown native state still blocks every operation that
requires quiescence. Polling alone MUST NOT claim either higher assurance level.

The checked decision rejects transient shared↔dedicated thread migration. On
exact 0.147.0 `thread/unsubscribe` returned `unsubscribed` while the source
still reported the thread loaded after two seconds, and a second server rejected
resume as an active writer. `THREAD_RESIDENCY_CONFLICT` therefore fails closed;
a shared Run needing write uses a fresh dedicated write continuation. A dedicated reader
whose access transition is unavailable or unverified uses the same generalized
public operation. `run create-write-continuation` requires the source's current terminal Turn,
no active Turn or pending interaction, source-Controller authorization, a new
same-principal destination Controller credential, and an idempotency key. It
records immutable lineage and creates a threadless, physically absent dedicated
Run. Profile/workspace/control-mode overrides and assurance downgrades are
forbidden; validated model, effort, capability additions, assurance raises,
purpose, and bounded destination instructions are allowed. Ordinary `run fork`
remains a separate history-copy operation.

Lane-specific errors are used only for distinct recovery semantics.
`EXECUTION_LANE_UNSUPPORTED` rejects a profile that cannot host the selected
lane; `EXECUTION_LANE_IMMUTABLE` rejects an attempted in-place lane change;
`SHARED_RUN_WRITE_FORBIDDEN` requires a dedicated write continuation;
`THREAD_RESIDENCY_CONFLICT` requires residency reconciliation;
`SAME_HOME_MULTI_SERVER_UNSAFE` rejects a profile whose pinned campaign failed;
`ASSURANCE_LEVEL_UNAVAILABLE` permits only lowering the pre-allocation request;
`DEDICATED_HISTORY_BARRIER_FAILED` blocks generation replacement;
`PROFILE_LANE_MIGRATION_REQUIRED` requires operator migration of the complete
lane set; and `DEDICATED_SERVER_START_FAILED` permits a bounded retry of the
same logical lane. Identity uncertainty still uses `RECOVERY_REQUIRED`, active
native work uses `RUN_BUSY`, workload uncertainty uses
`BACKGROUND_EXECUTION_UNVERIFIED`, and writer/handoff conflicts keep their
existing errors.

## SPEC-015: Supervised Local gRPC and Gul Integration

Dolgorae MUST expose versioned package `dolgorae.public.v1` as standard gRPC
over HTTP/2 on the user-private Unix socket owned by `dolgorae serve`. The
protocol version is integer 1 and the initial accepted client range is `[1,1]`.
The pre-negotiation `GetCapabilities` request MUST set
`context.protocol_version=0` and supply the client's minimum and maximum
versions. The server selects a common version or returns typed
`PROTOCOL_VERSION_UNSUPPORTED`. Every later request MUST carry the negotiated
version and a client request UUID. Initial `InspectWorkspace` is the sole
workspace bootstrap exception: it requires only an absolute path and may carry
an expected workspace ID for attachment revalidation. Its response contains
the canonical root, calculated workspace ID, typed inspection status, and
typed compatibility/safety blockers. Every other workspace request requires
the absolute path and expected workspace ID; every Run request also carries
the Run UUID. Workspace canonicalization and identity MUST use SPEC-002 and
MUST NOT trust an in-memory gateway registration.

The public gRPC v1 surface is:

- `RuntimeService`: unary `GetCapabilities`, `InspectWorkspace`,
  `ListProfiles`, `GetProfile`, and `ListProfileDiagnostics`;
- `RunService`: unary `StartRun`, `ListRuns`, `GetRun`, `SubmitTurn`,
  `InterruptTurn`, `SetDefaultEffort`, `PauseRun`, `ResumeRun`, `CloseRun`,
  `DeleteRun`, `RecoverRun`, `ReconcileRun`, `ForkRun`, `VerifyRun`, and
  `CreateWriteContinuation`;
- `ObservationService`: server-streaming `WatchRunEvents` and unary
  `ListRunTimelineItems`;
- `InteractionService`: unary `ListPendingInteractions`,
  `GetControllerInteraction`, and `ResolveInteraction`;
- `WriterService`: unary `GetWorkspaceWriterStatus`, `AcquireWriter`,
  `ReleaseWriter`, `PrepareWriterHandoff`, `CommitWriterHandoff`, and
  `CancelWriterHandoff`;
- `ControllerService`: unary `VerifyController`; and
- `ArtifactService`: unary metadata-only `GetArtifact` and bounded
  `ReadArtifactChunk`.

The descriptor is frozen, but runtime method availability is capability-
advertised. `GetCapabilities.supported_methods` MUST contain only methods whose
full semantic and safety contract is implemented. `MILESTONE-BH1` requires the
checked minimum Run gateway set recorded in
`dolgorae-capabilities-v1.schema.json` and
`dolgorae-grpc-conformance-v1.json`: capability/workspace/profile bootstrap,
Primary Run start/get/list/submit and basic lifecycle recovery, Run event
streaming, Controller interaction handling, basic writer status/acquire/release,
Controller verification, and artifact metadata/bounded chunk retrieval.
Timeline, profile diagnostics, advanced Run operations, writer handoff, delete,
verification, and write continuation remain unavailable and unadvertised until
`TASK-010-A`. A client
MUST fail closed rather than call an unadvertised method. `MILESTONE-PA1`
requires the complete descriptor method set.

The checked public v1 Protobuf and descriptor remain unchanged by the two-use-
case and internal-broker revision. Gul uses the existing Run surface and a protected Controller carrier containing
explicit Orchestration Launch Intent to create and control the
`direct_interactive` Primary Run. The semantic service resolves the named
Specialist Policy and creates the matching Orchestration Session and Aggregate
Bootstrap Operation before the Run is published. Dolgorae's internal Orchestration Broker uses protected
internal credentials and the same semantic Run operations to provision and
supervise brokered Specialists. Gul MUST NOT become the authority for Brokered
Hierarchy membership, spawn state, or result-delivery recovery. It may
reconstruct presentation through `ListRuns`, reserved parent projections, and
root/member Run observations, while Dolgorae's orchestration SQLite state and
event log remain authoritative.

An external AI opens and operates an External Specialist Engagement through the
private External Specialist Facade, which compiles each hire and task into the
same semantic Run core. A raw public `managed_agent` StartRun does not infer
engagement membership. Dolgorae persists the accepted Specialist boundary but
MUST NOT persist or execute an additional external task graph. Future aggregate-
query RPCs, if needed, are additive v1 extensions and do not alter this
release's checked wire shape.

Workspace initialization, profile mutation/lifecycle, Operator-authorized
reset/repair/migration, and server-side filesystem export MUST remain Machine
CLI-only. Controller credential creation has no gRPC method, but a trusted
same-user caller MAY create a strict checked-schema credential create-
exclusively under the capability-advertised carrier root. A Gul installation
uses `controller-carriers/gul/<installation-id>/`; every parent is same-uid
mode 0700 and every carrier is a same-uid, no-symlink, mode-0600 regular file.
`GetCapabilities` publishes the credential schema ID/version/SHA-256, accepted
Controller kinds, 32-byte base64url-no-padding capability encoding, 4-KiB file
bound, root/layout policy, generation-1 rule, and normalized-principal rule. The
gRPC projection carries the canonical
`application_support/Dolgorae/controller-carriers` root locator separately from
the typed Dolgorae-owned Application Support root policy. Capability encoding
and normalized-principal selection are closed enums; Gul MUST NOT parse
diagnostic text to derive either rule.
Method availability is capability-advertised;
absence from gRPC MUST NOT create an alternate semantic rule. There is no
client-streaming or bidirectional RPC in v1.

`StartRun` MUST reject `CONTROL_MODE_UNSPECIFIED`,
`EXECUTION_LANE_UNSPECIFIED`, `ASSURANCE_LEVEL_UNSPECIFIED`, and absent purpose
before allocating a Run. Gul and external-AI adapters may choose explicit values
from their selected use case, but the semantic request is complete and
deterministic.

`GetCapabilities` MUST also return typed Dolgorae/protocol versions, descriptor
digest, supported methods, transports, control modes, lanes, assurance and lane
capabilities, stable feature flags, Interaction kinds, access-transition and
background-control support, native-subagent behavior, Interaction request and
safe-payload byte limits, inline/fetched artifact bounds, and event/timeline
projections. The limits are exactly 1,048,576 raw bytes in
`ResolveInteractionRequest.response_json` before JSON parsing and 8,388,608
bytes for the encoded selected typed `ControllerInteraction` payload
submessage before transmission. `GetProfile` MUST return typed compatibility,
runtime version, structured model/default/effort capabilities, lanes, maximum
assurance, transition/background/Interaction/native-subagent support, feature
flags, and typed blockers. Closed output enums unknown to a v1 client are not
actionable: Gul MUST fail closed, refresh capabilities, and MUST NOT infer a
state from diagnostic text.

`CapabilityBlocker.code` is the closed `CapabilityBlockerCode` enum. It covers
configuration, workspace safety, profile compatibility/migration/membership,
lane and assurance support, access/background support, Interaction support,
and native-subagent support. `safe_message` is presentation-only; clients MUST
derive actions from the enum and the surrounding typed capability state.

Interaction support has its own closed three-state enum: `supported`,
`recognized_unsupported`, and `unavailable`. It MUST preserve the identically
named Machine capability state and MUST NOT collapse recognized unsupported
requests into runtime unavailability. For a compatible profile with a nonempty
model catalog, `ModelCapability.is_default` is the sole default-model authority:
exactly one model is default, model IDs are nonempty and unique, and every model
has a nonempty unique effort list. The removed draft `default_model_id` field
has no compatibility meaning and its Protobuf name and number remain reserved.

`RunProjection.configuration` MUST durably expose the accepted profile, closed
purpose and optional label, effective accepted model, current default effort,
sorted unique required capabilities, optional parent provenance, instruction-
contract versions, and the byte length and SHA-256 of the normalized Controller
instructions. Profile, purpose, model, capabilities, parent, and instruction
identity are immutable; only current default effort changes through
`SetDefaultEffort`. The projection survives gateway/Dolgorae restart and is the
authority after reconnect rather than the client's original request.
`RunProjection` MUST otherwise preserve the checked run-state distinctions through typed
lifecycle/state variant, Controller, thread and active Turn, effective policy
and verification epochs, writer authority/generation/transaction and
reconciliation action, server lane, background census, requested/achieved
assurance, recovery state/action, state revision, and immutable lineage.
`WriterState` MUST expose owner, authority, generation/handoff, effective
policy, lane, assurances, background/recovery blockers, reconciliation action,
revision, and a `ProjectionStamp`. Run, Writer, Interaction, and durable-event
projections carry the same stamp shape: captured durable head plus Run, Writer,
and Interaction revisions. A client MUST derive actions only from compatible
typed projections and MUST keep mutations disabled while any required
aggregate is older than an invalidating event or the stamps do not converge.

`SubmitTurn` is accepted-operation unary RPC. It MUST return only after the
App Server accepts `turn/start` and Dolgorae fsyncs the permanent thread and
Turn binding. Its result contains the accepted Turn, correlation and
idempotency identities, and the authoritative typed Run and Writer projections
at acceptance. For a threadless write these projections prove reservation,
lane activation, thread creation, effective-policy verification, and active
writer authority; `AcquireWriter` remains forbidden before the first write
Turn. It MUST NOT wait for terminal completion. Progress, final response,
interactions, terminal
status, and recovery requirements arrive through `WatchRunEvents` or fresh
authoritative snapshots. Successful RPC delivery MUST NOT be interpreted as
successful Turn completion.

`WatchRunEvents` requires Run identity, exclusive `after_cursor`, projection
profile, and projection version. Events are ordered within that Run by the
durable cursor defined in SPEC-006. A stream is only a delivery mechanism;
disconnect, cancellation, HTTP/2 failure, or gateway restart MUST NOT change
the Run or imply its failure. Projection gaps are valid. Duplicate delivery is
permitted and deduplicated by event ID plus cursor. A heartbeat MAY report the
current durable head and Run state every 30 seconds but MUST NOT append a record
or advance the cursor.

The stream envelope is a typed `oneof`: a durable event, heartbeat, or stream
end. A durable event carries the complete client-safe metadata and exactly one
typed variant for every minimal/operational event in the checked public event
schema. There is no untyped `event_type` plus JSON payload escape hatch.
Heartbeat and stream-end envelopes carry an advisory durable head but never
create or advance a semantic cursor. Unknown event variants fail closed and
require capability/snapshot refresh.

Event variants have the following aggregate semantics. “Invalidate” means the
event is a notification, not a replacement snapshot; the named projection MUST
be fetched at or beyond the event's `ProjectionStamp` before it enables a
mutation.

| Durable variant | Complete update | Invalidates / required refresh | Mutation effect |
| --- | --- | --- | --- |
| `run_state_changed` | None | Run; Writer when lifecycle can affect authority | Disable affected Run actions until Run/Writer stamps converge |
| `turn_state_changed` | Turn status only | Run and timeline | Never enables a mutation by itself |
| `final_response_available` | Final-response value | Run and timeline | No writer action |
| `interaction_opened` | Interaction identity/kind only | Interaction and Run pending count | Fetch summary and authorized full Interaction |
| `interaction_resolved` | Interaction outcome only | Interaction, Run pending count, and timeline | Disable response action until refreshed |
| `writer_state_changed` | Writer generation hint only | Writer and Run | Disable all writer actions until both refresh |
| `recovery_required` | Recovery notification only | Run and Writer | Disable conflicting mutations |
| `runtime_error_occurred` | Diagnostic only | Run when its stamp advances | Never marks the Run failed by itself |
| `generation_changed` | Generation/epoch only | Run, Writer, and Interaction | Disable mutations until all required aggregates refresh |
| `workspace_changes` | Workspace-change observation | Timeline only | No authority change |
| `command_started`, `command_completed` | Command observation | Timeline only | No authority change |
| `usage_reported` | Usage observation | Timeline only | No authority change |
| `diagnostic_reported` | Diagnostic observation | Timeline only | No authority change |
| `reasoning_suppressed` | Suppression metadata | Timeline only | No authority change |

Timeline compatibility is established by `captured_head_cursor`; Run, Writer,
and Interaction compatibility is established by the complete projection stamp.
A snapshot with a later stamp supersedes an earlier invalidation. Equal stamps
are mutually compatible. A client observing continually advancing state keeps
the relevant action disabled rather than combining revisions.

Each Run stream has a queue bounded by 32 envelopes, 4 MiB of encoded payload,
and five seconds of stalled delivery. Exceeding any bound MUST close only that
stream with gRPC `RESOURCE_EXHAUSTED` and typed `SLOW_CONSUMER`. Any server-last-
sent cursor is advisory; the client MUST resume from its last committed cursor.
Streams for other Runs multiplexed on the same HTTP/2 channel
remain independent. A client that cannot reconcile its view MUST fetch
`GetRun`, capture its head cursor, page the Controller timeline when authorized,
and resume event replay from the chosen durable boundary.

`RUN_TERMINAL` is a normal end of Run observation: the client obtains the final
Run and required timeline/artifact snapshots and MUST NOT reconnect
indefinitely. `SERVER_SHUTDOWN` is a gateway interruption; durable Run state is
unchanged and the client MAY reconnect and resume after its last committed
cursor. `SLOW_CONSUMER` is not a stream-end envelope in v1: it is
`RESOURCE_EXHAUSTED` with typed `SLOW_CONSUMER` detail and closes only the
affected Run stream. A transport failure is never evidence that a Run failed.

Artifact access retains the offset/length contract. `GetArtifact` returns only
opaque ID, kind, visibility, media type, exact byte length, SHA-256, and maximum
chunk size. `ReadArtifactChunk` requires `1 <= length <= 1048576`, uses raw-byte
offsets, returns Protobuf `bytes`, actual offset/length and EOF, verifies the
full digest before first access in the invocation, and honors gRPC cancellation.
Observer versus Controller-only authorization is identical to the CLI; no RPC
returns an internal path.

Provider bounds are 1 MiB for inline final responses, 32 MiB for one artifact,
and 1 MiB for one requested chunk. A client MAY use a smaller presentation
threshold and request smaller chunks. The effective total client-download
limit is the minimum of the provider and client limits. An inline provider
response above a client's presentation threshold is not a provider contract
violation; Gul MAY convert it to its own browser-safe presentation. Every
artifact download MUST verify both exact byte length and SHA-256 before use.

The checked mutation-policy registry is normative. `StartRun`, `ForkRun`,
`SubmitTurn`, `ResolveInteraction`, and `CreateWriteContinuation` require an
idempotency key and permit only same-key/same-normalized-identity replay. All
other mutation RPCs are explicitly tokenless and state-convergent or
reconciliation-driven as recorded in that registry. The checked gRPC service
config MUST define no transparent retry policy for a mutation. A client MUST
recover a lost `StartRun` response first by repeating the same request with the
same idempotency key, Controller ID/generation, and normalized semantic
identity. That identity is canonical workspace, accepted profile snapshot,
Controller, control mode, purpose/label, effective model/current default
effort, lane, assurance, sorted unique required capabilities, parent reference,
and Controller-instruction byte length/SHA-256. Exact replay returns the
original Run with `exact_replay=true`; Controller or identity drift is
`IDEMPOTENCY_CONFLICT` and never allocates another Run. `ListRuns` filtered by
the unique Controller ID is secondary reconciliation when the Run ID is not
known; `GetRun` and `ReconcileRun` cannot be primary in that case.

StartRun allocation records have no TTL and live for the workspace lifetime.
Explicit Run deletion retains a non-secret allocation tombstone. Replay after
deletion returns `RUN_NOT_FOUND` with the original safe Run ID and MUST NOT
allocate a replacement under the old key. Other lost responses use the
interaction/writer snapshot, returned operation ID when known, and
`ReconcileRun` where required.
`OUTCOME_UNKNOWN` and `RECOVERY_REQUIRED` always forbid blind automatic retry.

`CreateWriteContinuation` returns a dedicated threadless destination Run,
typed immutable lineage, new destination Controller, idempotency key, and a
source receipt proving the source revision and writer authority were unchanged.
The destination Controller ID and capability MUST be new, generation MUST be
1, and its normalized principal `(kind, subject_id)` or, absent subject,
`(kind, instance_id)` MUST equal the source principal. Same key, destination
Controller, source terminal Turn, and normalized request replay MUST return the
original destination Run; any identity drift returns typed
`WRITE_CONTINUATION_CONTROLLER_INVALID` or `IDEMPOTENCY_CONFLICT`.

Interaction summaries MUST use typed kind/status/Controller kind and include
creation, nullable expiry, and nullable resolution timestamps; absent expiry
means the provider supplied no deadline. `user_escalation_required` is
`true` exactly while a supported Interaction is `pending` and `false` for
`resolved` or `stale`; it is a deterministic delivery signal, not a prediction
about whether a human will answer. `ControllerInteraction` MUST select
exactly one typed payload matching `summary.kind`: command approval, file-change
approval, user input, or recognized unsupported Interaction. Permission and MCP
elicitation requests map to the unsupported payload; unavailable connector
approval produces no Interaction. `response_schema_id` identifies only the
accepted `ResolveInteraction.response_json` schema. Decisions use the closed
`InteractionDecision` enum. A kind/payload mismatch, unspecified enum, or
unknown future variant fails closed; no unversioned safe JSON payload exists.

Every filesystem path in a public output projection uses `PathProjection` with
exactly one UTF-8 string or opaque POSIX byte sequence. Dolgorae MUST NOT emit a
lossy Unicode replacement string. This applies to the canonical workspace root
returned by `InspectWorkspace`, workspace-change paths, command cwd,
file-change paths, and move paths.

Protected response bytes are a
bounded one-shot `ResolveInteraction` body only: never metadata, audit, trace,
journal, retry queue, or typed error detail, and buffers are zeroized where
practical. The 1-MiB raw byte bound is checked before JSON parsing. Automatic
replay is forbidden. After a lost response the caller MUST refetch the summary
and, when still pending and authorized, the full Interaction. Resolved means
complete. Pending protected input requires explicit user re-entry and MUST NOT
use a retained body. An indeterminate result is typed
`INTERACTION_OUTCOME_UNKNOWN`. A winning secret idempotency key may return the
original opaque receipt after resolution without comparing, hashing, or
replaying a supplied body; it does not authorize transparent secret replay.

Semantic failures MUST use an appropriate gRPC status code and attach versioned
`DolgoraeErrorDetail` through `google.rpc.Status`. The detail contains the
canonical Dolgorae error code, typed required-client-action enum, safe
Run/Turn/Interaction IDs, operation
ID, safe idempotency key, retry classification, recovery classification, and an
optional safe resume cursor. Clients MUST NOT parse human-readable status text.
The checked gRPC error map owns the exhaustive mapping. Secret values, secret
digests, raw carrier contents, mismatch subreason, internal paths, and hidden
payloads are forbidden in status details.

The map expands every error code to exactly one status, required action, retry
classification, and recovery classification. Method-specific overrides may
only narrow that result. In particular, `ControllerService.VerifyController` returns
`CONTROLLER_MISMATCH` with `ABORT`, `FORBIDDEN`, and `NONE` after a failed
side-effect-free verification; it MUST NOT direct the caller back into
`VerifyController` and MUST NOT reveal a mismatch subreason.

Shutdown and transport-loss details are selected by RPC semantics, not by error
code alone. `WatchRunEvents` reconnects from the committed cursor. A lost
`StartRun` may repeat its exact key because allocation replay is durable;
`SubmitTurn` first refreshes the Run snapshot and may repeat only its exact key;
`ResolveInteraction` refetches the Interaction and never replays protected
input. Other tokenless mutations and unary reads refresh their authoritative
snapshot before a retry. `DEDICATED_SERVER_START_FAILED` on `SubmitTurn` uses
the exact request key, while the same code on tokenless `ResumeRun` refreshes
the Run and retries the logical lane without inventing a key. The checked map
partitions every public RPC into server-stream, idempotent-mutation,
tokenless-mutation, or unary-read semantics and emits concrete typed details
after applying the narrower method override.

Writer recovery actions are exact: a threadless dedicated Run maps
`THREADLESS_REQUIRES_WRITE_TURN` to `SUBMIT_WRITE_TURN`; a shared read-only Run
maps `SHARED_RUN_WRITE_FORBIDDEN` to `CREATE_WRITE_CONTINUATION`; and an
unavailable or unverified in-place access transition maps
`ACCESS_TRANSITION_UNSUPPORTED` to `CREATE_WRITE_CONTINUATION`. Outcome-unknown
state requires reconciliation before any conflicting mutation. No client may
translate any of these cases into another write submission by parsing status
text.

Protobuf v1 evolution is additive. Field numbers MUST NOT be reused; removed
numbers and names are reserved; every enum starts with an `UNSPECIFIED=0` value
and appends values only. Unknown control/input enum values fail closed with
`UNSUPPORTED_SCHEMA_VERSION`; unknown output values remain representable to the
generated client and require capability or snapshot refresh. A removal or
meaning change requires a new package major version. The repository MUST
publish the `.proto` source, deterministic descriptor set, descriptor SHA-256,
minimum/maximum client range, and Buf compatibility result. Machine JSON and
Protobuf may differ on the wire but MUST pass shared semantic conformance
fixtures for equivalent operations and error outcomes.

The gateway is local same-user API, not remote authentication. It MUST validate
peer UID, socket path and record identity before serving. It MUST NOT expose the
socket on the public Internet, TCP, or Tailscale; disclose worker/App Server
transports; or accept Operator credentials. Gateway restart MUST leave durable
Runs and permitted surviving workers/App Servers unchanged. The replacement
gateway reconstructs projections from authoritative state and a mutation whose
response was lost follows the same application-level idempotency or
reconciliation contract as CLI process loss.

Dolgorae alone owns the gateway lock/record, bind, socket chmod, stale-inode
proof, unlink, and graceful cleanup. Gul creates and validates the private
parent, selects an unused absolute pathname, starts the foreground process, and
verifies the socket after readiness; it MUST NOT unlink the socket. A live
singleton collision returns typed `RPC_SERVER_ALREADY_RUNNING` and v1 does not
attach to or support multiple independently supervised gateways.
`RPC_SOCKET_UNSAFE` returns typed action `FIX_SOCKET_PATH`: Gul repairs or
replaces the private parent/path and starts a fresh gateway attempt. It MUST NOT
blindly restart with the same unsafe pathname and MUST NOT unlink a provider
socket.

Public-v1 freeze is gated by this normative runtime inventory. The checked
conformance registry may mirror these rows but may not add, remove, rename, or
reassign one. A closure artifact is valid only when its listed verifier exists,
its SHA-256 is recorded, and executing that verifier with
`--verify-evidence <artifact>` returns `ok:true` for the same case ID, owner,
evidence kind, and source revision.

| Runtime case ID | Owner | Required evidence | Evidence verifier |
|---|---|---|---|
| `slow_consumer_isolation` | `TASK-009-D1A` | `multi_run_pressure_e2e` | `tools/probes/verify_slow_consumer_isolation.py` |
| `protected_interaction_lost_response` | `TASK-009-D1A` | `secret_canary_and_fault_barrier` | `tools/probes/verify_protected_interaction_lost_response.py` |
| `gateway_restart` | `TASK-009-D1A` | `active_run_restart_e2e` | `tools/probes/verify_gateway_restart.py` |
| `socket_ownership` | `TASK-009-D1A` | `macos_uds_attack_matrix` | `tools/probes/verify_socket_ownership.py` |
| `private_boundary` | `TASK-009-E1` | `real_gul_harness` | `tools/probes/verify_private_boundary.py` |
| `run_configuration_restart` | `TASK-009-D1A` | `accepted_configuration_restart_e2e` | `tools/probes/verify_run_configuration_restart.py` |
| `start_run_allocation_replay` | `TASK-009-D1A` | `allocation_loss_conflict_and_tombstone_e2e` | `tools/probes/verify_start_run_allocation_replay.py` |
| `interaction_size_and_secret_barrier` | `TASK-009-D1A` | `preparse_bound_and_no_secret_replay_e2e` | `tools/probes/verify_interaction_size_and_secret_barrier.py` |
| `event_revision_action_barrier` | `TASK-009-D1A` | `stale_aggregate_action_e2e` | `tools/probes/verify_event_revision_action_barrier.py` |
| `lossless_non_utf8_path` | `TASK-013` | `opaque_path_cross_adapter_e2e` | `tools/probes/verify_lossless_non_utf8_path.py` |
| `threadless_first_write_runtime` | `TASK-009-D1A` | `threadless_submit_writer_activation_e2e` | `tools/probes/verify_threadless_first_write_runtime.py` |

## External Protocol References

- [Official Codex app-server documentation](https://learn.chatgpt.com/docs/app-server)
- [Official Codex subagents documentation](https://learn.chatgpt.com/docs/agent-configuration/subagents)
- [MCP 2026-07-28 Base Protocol](https://modelcontextprotocol.io/specification/2026-07-28/basic)

These references describe Codex behavior. Dolgorae-specific policy in this SOT
remains authoritative for Dolgorae.
