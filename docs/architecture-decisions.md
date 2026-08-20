# Dolgorae Architecture Decisions

Status: Decision record for the first supported release. Each ADR carries its
own Accepted, Under Review, or Superseded status; this heading does not promote
an Under Review ADR to Accepted.

This document owns decision rationale. Each ADR describes the currently
accepted decision, not an append-only historical chain. If a decision changes,
edit its ADR in place and update every affected SOT document in the same change;
Git history preserves the prior text. Contradictory active ADRs are invalid.
Document roles and the required synchronization procedure are defined by the
[documentation authority map](README.md).

## ADR-001: Ship One Binary With Two Public Adapters and No Installed Daemon

Status: Accepted

### Context

Persistent subagent sessions must outlive individual CLI invocations. Dolgorae
does not need an installed supervisor daemon, while Codex can efficiently share
one reader app-server among compatible sessions in the same account home.

### Decision

Ship one `dolgorae` executable. For every live run, re-execute that binary in a
hidden worker mode and attach a private WebSocket connection to the Run's
immutable shared-read-only or dedicated execution lane. A short-lived profile manager owns
singleton creation, epoch transitions, and membership reconciliation. Install
no Dolgorae launchd unit, global daemon, or project daemon. Recover workers on
demand after process loss or reboot. Expose the public semantic contract through
two coequal adapters: the Machine CLI for scripting, automation, diagnostics,
recovery, conformance, and Operator administration; and an optional supervised
local gRPC gateway for long-lived interactive clients. Both adapters call the
same semantic application service and preserve the same authorization,
idempotency, writer, interaction, recovery, audit, and projection rules.
Neither adapter exposes private worker or App Server sockets.

### Consequences

- Worker connection count grows linearly with live Runs; App Server count is
  one shared reader plus zero or more Run-owned dedicated process generations.
- Each run isolates its worker, connection, ledger, and controller state. A
  profile singleton failure and profile-wide operator action still affect every
  member of that launch contract.
- Idle runs consume processes until explicitly paused or closed.
- The binary still depends on external Codex profiles and their `CODEX_HOME`.
- The gRPC gateway is a foreground process supervised by its same-user client;
  it is neither required for Machine CLI use nor a durable-state authority.

### Rejected alternatives

- One Dolgorae global daemon: rejected because it centralizes project state and
  creates a mandatory installed service.
- One project daemon hosting many threads: rejected because one crash or upgrade
  affects every run and complicates profile isolation.
- A purely foreground CLI: rejected because turns and sessions would die with
  the invoking process.

## ADR-002: Use Direct WebSocket Over a Dolgorae-Owned Unix Socket

Status: Accepted

### Context

The master needs reconnectable local control, while app-server traffic must be
correlated and audited per run. Codex 0.147.0 speaks WebSocket framing on its
Unix listener; `app-server proxy` only copies bytes and does not translate that
transport into JSONL.

### Decision

Keep the user-private CLI-to-worker Unix socket and framed JSONL control
protocol. Give each worker a distinct direct WebSocket client connection over a
Dolgorae-owned dedicated Unix socket to the profile singleton. The worker
implements Upgrade, masking, text/continuation, ping/pong, close and bounds, and
normalizes frames before JSON-RPC correlation. The master never connects to the
App Server.

### Consequences

- Each worker is the only protocol client and audit interposer for its run.
- Socket paths live under a fixed short user-private `/tmp/dolgorae-<uid>/` root
  and the shared listener's actual location is recorded in profile state; discovery
  does not depend on the caller's `$TMPDIR`.
- A worker may recreate only its per-run control socket. Loss or replacement of
  the shared App Server listener fails closed to the profile manager, which may
  restart the verified singleton with a new `server_epoch`; a run worker never
  binds or repairs that listener.
- Worker loss ends only its client connection; recovery validates the singleton
  epoch and opens a new run generation.
- No TCP port, WebSocket authentication scheme, or shared default Codex socket
  is exposed.

### Rejected alternatives

- Direct master-to-app-server socket: rejected because it bypasses Dolgorae state,
  writer policy, idempotency, and audit.
- `app-server proxy`: rejected for v1 because it preserves WebSocket framing and
  adds process lifecycle without adding isolation or correlation.
- JSONL over proxy: rejected because it is factually not the pinned transport.
- TCP WebSocket: rejected because it needs port/auth management and expands the
  local attack surface.

## ADR-003: Bind One Run to One Codex Thread

Status: Accepted

### Context

Allowing several threads inside a run or moving a run between threads makes
model settings, recovery, audit causality, and access state ambiguous.

### Decision

A run is one logical session. It has no Codex thread before its first turn;
first `send`/`submit` durably reserves one intent, then performs `thread/start`
and `turn/start`. A crash between those RPCs may abandon only the empty
provisional thread and retry only after absence proof plus stable history proving
that no turn was accepted. Once
the first turn is accepted, the run is permanently bound to exactly one Codex
thread. Restarting a worker connection changes run generation, not run or
thread identity. History-copying branching creates its new thread immediately;
a fresh branch creates a threadless run and allocates its thread when first used.
This lazy boundary is required because pinned Codex does not persist a
turnless `thread/start` across app-server restart.

### Consequences

- Run, thread, and audit causality remain simple after first-turn acceptance.
- One run permits one active turn at a time.
- Users create parallelism through several runs or Codex native subagents.
- A closed run cannot be reopened; continuation requires a fork.

### Rejected alternatives

- Many threads per run: rejected because run-level model, access, and audit
  semantics would no longer be singular.
- Rebinding a run to a new thread after loss: rejected because it would falsely
  claim same-session continuity.

## ADR-004: Operate in the Canonical Workspace With Durable Writer Authority

Status: Accepted, amended by ADR-025 and ADR-026; the former shared-reader/Writer-Capsule topology is
superseded by ADR-019

### Context

Automatically created dedicated worktrees or directory copies change the
user's selected workspace and require a separate merge/publication workflow.
Operating directly in the caller-selected canonical workspace is simpler but
simultaneous writers within that worktree conflict.

### Decision

Run Codex in the canonical workspace selected by the caller. A linked Git
worktree is an independent canonical workspace and supported parallel writer
lane. Start every Run with writer authority `none`: a shared Run starts as a
verified reader, while a dedicated Run starts physically absent with unknown,
unverified effective policy until its first turn. Allow concurrent readers and at most one
Dolgorae writer across every profile in a canonical worktree. Persist one
authority record whose states are `none`, `reserved`, `active`,
`handoff_prepared`, `releasing`, and `blocked_unknown`; only explicit audited transitions may grant or remove
write authority. Use nonblocking BSD `flock(2)` solely to serialize validation
and replacement of that durable record. The kernel lock is not the authority,
and worker exit or descriptor close never releases authority. The
canonical identity is domain-separated SHA-256 of libc `realpath(3)` bytes with
no extra case/Unicode folding; sockets and both locks reuse that digest. The
transaction lock is close-on-exec and is never inherited by workers or the
singleton. Permanent lock pathnames live below
`<application-support-workspace>/runtime/locks/` on the already-required local APFS state root. An
unverifiable generation blocks same-thread recovery. A stale foreign-run
`writer.json` remains authoritative until evidence proves a safe transition;
process absence and a free transaction lock are insufficient.
Idle holders may cooperatively hand off only through a controller-authorized
prepare/commit protocol bound to both run generations, writer-authority generation,
and a five-minute durable confirmation record. Cross-controller, active,
waiting, or uncertain holders cannot be taken.
Writer transaction/startup lock paths are permanent. V1 provides no force override. Allow
dirty workspaces and record their start baseline. Provide
no transactional rollback.

Every multi-resource transition follows operator, home, server, handoff,
writer, canonical run-startup, then canonical run-mutex order. Writer
activation and release use revision-bound prepare/apply/verify/commit phases so
no WebSocket or process wait occurs under a global file lock. A threadless
`acquire-write` is rejected; only its first `send|submit --write` may create a
writer-configured thread and activate authority before `turn/start`.

Codex 0.147.0 is the compatibility baseline. Under ADR-019, every dedicated Run
uses a Sticky Dedicated logical lane; a `shared_readonly` Run remains on the
shared lane and creates a lineage-linked dedicated write continuation if it needs write.
Reader and writer access are policy states of a dedicated lane and never move
the thread to the shared server. The superseded transient topology is retained
only as historical rationale. A dedicated lane may
advance to a successor process generation only after exact absence and a
durable-history barrier. Dolgorae owns each lane generation's process identity,
100-millisecond process census, exact cleanup, and five-sample empty proof.
Release, handoff, close, recovery, and generation replacement fail closed when
census evidence is incomplete or identities cannot be revalidated. A future
native Codex terminal API may add hybrid evidence but is not a release
prerequisite. The prompt rule against background work remains defense in depth,
not release evidence; neither prompt compliance nor foreground-turn completion
proves process absence.

### Consequences

- Writer changes are immediately visible to the user and readers.
- Interrupted work may leave partial changes.
- Readers have no snapshot isolation and may see an intermediate state.
- A failed confirmed handoff may intentionally leave the workspace without a
  writer; it never rolls the prior holder back to write.
- The authority coordinates Dolgorae workers only; editors and external tools remain
  outside its guarantee.
- Native Codex subagents, when the pinned profile proves them supported, remain
  inside the owning Run; Dolgorae does not serialize their internal lanes.
- For a verified but wedged current writer, same-run recovery first serializes
  through a run-keyed election, revalidates and terminates the worker outside a
  possibly-held startup lock, and confirms exit. The
  POSIX startup lock exposes its owner through `F_GETLK`; an exact wedged owner
  may be terminated and all contenders then compete for the lock. Only the
  winner acquires the transaction lock and advances durable authority only
  after cleanup is confirmed. Connection generations do not encode access.
- Startup handoff uses two POSIX byte ranges because record locks are not
  inherited across fork: the CLI owns byte 0 until a re-exec worker owning byte
  1 has bound and persisted identity. Runtime ownership is never inferred from
  an inherited lock.
- Transaction-lock identity is the held descriptor's device/inode pair and is rechecked
  against the root-relative pathname at destructive barriers, so manual unlink
  or replacement fails closed.
- Emergency continuation uses a fresh read-only run with explicit provenance;
  same-thread continuity waits until exact-generation absence is proved.

### Rejected alternatives

- Automatically create a dedicated Git worktree per run: rejected for v1 because
  it changes the product into a branch/merge manager. User-created linked
  worktrees remain supported independent workspaces.
- Copy-on-write workspace snapshots: rejected because they are expensive and
  complicate external tools and Git identity.
- Multiple optimistic writers: rejected because conflict detection after side
  effects cannot reliably prevent corruption.
- Natural-language write detection: rejected because it is nondeterministic and
  can submit a mutating turn before Dolgorae owns durable authority.
- Force takeover of an active holder: rejected because turn progress is not
  proof that workspace mutation has stopped.
- A turnless writer thread: rejected because the pinned runtime has no proven
  persistence/restart/release contract before its first turn.
- Per-run cleanup against the shared singleton: rejected because a worker is
  not the parent of commands launched by that shared process and ancestry is not
  a safe thread-ownership discriminator. ADR-019's Sticky Dedicated lane creates
  an exclusive process boundary and combines process-group enumeration with
  persisted full identities and all-PID parent/session census.
- Start Codex globally with `--dangerously-bypass-approvals-and-sandbox`:
  rejected because it disables normalized approvals and the reader/writer policy
  boundary rather than solving background ownership.

## ADR-005: Snapshot Profile Identity and Use CODEX_HOME as Account Boundary

Status: Accepted, amended by ADR-024 and ADR-025

### Context

Users may have multiple independently configured profiles. Profile edits or
executable/argument/environment changes must not silently move an existing
thread between account homes or launch contracts.

### Decision

Store Runtime Profile definitions in mode-0600 `<application-support-workspace>/local.yaml`.
Snapshot the complete restorable non-secret launch contract into each run:
profile name, direct executable identity, normalized global argv, deterministic
launch cwd and `PWD`, sanitized environment, closed-classified process-static
configuration, initial mutable configuration observation, version, schema,
compatibility manifest, features, launch digest, server key, and expected
`CODEX_HOME`. Set that home explicitly and reject an
`initialize` response whose `codexHome` differs. Never rebind a run or fork
across profiles.

Treat server-key-changing version/configuration acceptance as an operator-only
profile migration across complete membership, not a run controller flag. Raw
digests of runtime-mutable configuration do not enter server identity; unknown
configuration fails compatibility until classified. Accepted migrations append
old/new generation contracts and preserve rollback or `migration_blocked`
evidence.

### Consequences

- Existing runs remain bound to the account that created them.
- Per-workspace Application Support profile edits affect only future Runs
  unless an Operator explicitly migrates the shared server contract.
- Executable or process-static updates require a server-key migration and do
  not change the expected home.
- Dolgorae does not install, update, or authenticate Codex.

### Rejected alternatives

- Resolve the profile name on every resume: rejected because a registry edit
  could silently change account identity.
- Store profile credentials or arbitrary secret environment variables: rejected
  because authentication belongs to Codex and v1 profiles are deterministic.
- Permit cross-profile fork: rejected because the source thread is not
  authoritative in the destination `CODEX_HOME`.
- Recalculate identity from a mutable config-file digest: rejected because the
  running App Server could invalidate its own key through normal state writes.
- Controller-only version acceptance: rejected because changing one shared
  singleton affects runs owned by other controllers.

## ADR-006: Fix Model Per Run and Change Effort Between Turns

Status: Accepted

### Context

The user needs runtime control over model and reasoning. Changing model inside a
single durable conversation weakens audit interpretation and may not preserve
model-specific behavior, while app-server supports per-turn reasoning options.

### Decision

Resolve and record one model when the run starts. Do not change it within that
run. A fork may select another model on the same profile. Allow the run's default
reasoning effort to change at runtime; a change during an active turn applies
to the next turn only and must be supported by fully paginated `model/list`.
Access-independent developer instructions are generation-immutable; current
access is supplied by the Turn-scoped dynamic access context. Explicit writer
acquire/release keeps the same Worker, logical lane, and thread. It
applies and verifies the new policy inside the current dedicated generation, or
uses a same-lane successor generation only after exact absence and a durable
history barrier. A shared-readonly compatibility Run is never promoted in
place; it creates a fresh lineage-linked dedicated write-continuation Run. Authority
advances transactionally and uses the pinned stable sandbox-policy surface when
supported. If the requested policy cannot be proved, v1 returns
`ACCESS_TRANSITION_UNSUPPORTED`; it never moves a thread between servers or
treats a connection generation as authority.

### Consequences

- Every turn has an unambiguous model.
- Model changes create a visible lineage boundary.
- Effort remains adjustable without thread replacement.
- Service tier and personality changes are outside v1.

### Rejected alternatives

- Arbitrary per-turn model changes: rejected because they weaken run identity
  and cross-turn reviewability.
- Immutable effort: rejected because it needlessly prevents runtime cost/depth
  control.
- Experimental thread settings mutation: rejected to keep v1 on stable APIs.

## ADR-007: Use One Hash-Chained Audit Ledger and Dual History Authority

Status: Accepted

### Context

Separate normalized, wire, and transcript authorities can disagree. Conversely,
Dolgorae cannot replace the Codex thread as model-visible conversation storage.

### Decision

Use one append-only `audit.jsonl` per run containing Dolgorae lifecycle and
redacted app-server-exposed wire evidence in a total order. Derive state,
transcript, events, and exports from that ledger. Canonicalize records with RFC
8785 JCS and use the versioned `sha256-jcs-v1` chain for ordinary integrity
detection. Own the canonicalizer in-repo with UTF-16 key ordering, ECMAScript
binary64 rendering, duplicate rejection, and RFC 8785 vectors; a byte change
requires a new hash-scheme version. Treat the Codex thread as continuation authority and the Dolgorae
ledger as audit authority only for information Dolgorae actually observes.
Keep the 16 MiB frame and 32 MiB reassembled-message caps because they protect
the active WebSocket stream. Treat solicited `thread/read` specially
only after its matching top-level ID appears within that prefix; this is a live
compatibility predicate. Then use a constant-memory, deadline-bounded visitor
with no arbitrary total response cap. Never infer classification from one
outstanding request.

Normalize public client events at append time and derive minimal and operational
profiles from those safe records. Suppress known reasoning notifications and
discard any unexpected reasoning text, summary, delta, or planning payload
before ledger representation; retain only method, length, digest, and
suppression reason. Public events never contain raw ledger or wire payloads.

### Consequences

- Projection conflicts resolve in favor of the ledger.
- Full verification scans one source.
- Neither history store can truthfully recreate the other's missing authority.
- Encrypted or otherwise unexposed native-subagent communication is outside the
  ledger's completeness claim and may appear only as opaque activity.
- Same-user hostile tamper resistance and signed attestation are not claimed.

### Rejected alternatives

- Separate authoritative normalized and wire logs: rejected because ordering
  and reconciliation become multi-source problems.
- Dolgorae transcript as conversation backup: rejected because it cannot recreate
  Codex's exact model-visible session state.
- Signed audit in v1: rejected because there is no external key or trust anchor.

## ADR-008: Recover Conservatively and Never Replay Unknown Work

Status: Accepted

### Context

If a worker or its App Server connection dies during a turn, filesystem mutations may have occurred
even when Dolgorae did not receive the terminal response. Automatic replay could
duplicate destructive or external side effects.

### Decision

On recovery, accept a turn outcome only when persisted Codex history proves a
terminal state. Otherwise close the connection, persist writer authority as
`blocked_unknown`, set `outcome_unknown`, block new turns, and allow only inspection, evidence-based
reconciliation, fork, or close. Never replay the input automatically. Fork only
through the newest status that the checked profile manifest proves acceptable as
`lastTurnId`; terminal-but-rejected statuses are skipped. Successful later reconciliation
moves the run to `paused`; explicit resume selects its next access mode.
Reconciliation first proves the recorded singleton identity/epoch absent, then
uses a fresh read-only connection to a compatible singleton under the accepted
contract at a new epoch and calls `thread/read` without loading or resuming the
thread. The durable result records both epochs and the absence/history/writer
verdicts. If prior process identity is unverifiable, v1
does not resume the same thread; the Master may wait for absence proof or
create a provenance-linked fresh read-only run. That fresh escape may also be
created from an unreachable running/waiting source, reads only its immutable
manifest, never reads its ledger or Codex thread, and never grants write access.

### Consequences

- Recovery prefers audit truth over convenience.
- Some sessions cannot continue on the same thread after a crash.
- Fork preserves confirmed context while explicitly recording lost uncertainty.
- Idempotency protects deliberate caller retries but is not used to infer an
  unobserved app-server outcome.

### Rejected alternatives

- Automatic input replay: rejected because model, command, and network side
  effects are not generally idempotent.
- User-declared completed/failed status: rejected because opinion would be
  recorded as authoritative execution evidence.
- Continue after acknowledging uncertainty: rejected because later history
  would conceal an unresolved causal gap.
- Forced same-thread recovery: rejected for v1 because an unverifiable old
  app-server may still own the Codex thread or leave background execution.

## ADR-009: Inject Strong Dolgorae Agent Invariants

Status: Accepted, amended by ADR-023 and ADR-026

### Context

Pure prompt passthrough does not reliably preserve Dolgorae's reserved storage,
write authority, Git publication, background-process, and reporting boundaries.
Profile configuration must nevertheless remain useful.

### Decision

Inject a strong generation-immutable developer-instruction contract that
defines the selected use-case relationship, the Primary or Independent
Specialist role when derivable from authoritative aggregate membership, and
stable behavior invariants. Do not include mutable current access, writer
ownership, writer generation, or `policy_epoch`. Every Turn receives a separate
dynamic access context derived from durable writer and verified policy state.

Do not describe every Dolgorae-managed agent as a subagent. Append immutable
Run-specific instructions as subordinate context. Continue to respect profile
AGENTS files, skills, plugins, apps, and checked MCP servers unless they conflict
with enforced invariants. Their mutable contents are not part of Dolgorae's
byte-immutable role claim. Native subagents require an explicit enabled profile
policy and a capability snapshot of `supported`; the corrected 0.147.0 campaign
passes lifecycle observation while disable enforcement remains unavailable.

### Consequences

- Every turn receives consistent governance.
- Read and write authorization derives from both request intent and run access.
- `.dolgorae`, Git publication, external effects, and background processes receive
  explicit treatment.
- Access changes require a pinned, empirically verified sandbox-policy
  transition; otherwise they fail explicitly without changing authority.
- Supported native subagents are instructed not to overlap write-heavy delegation.
- Prompt policy is defense in depth, not a hostile security boundary.

### Rejected alternatives

- Minimal passthrough: rejected because critical product invariants would be
  implicit and easy to violate.
- Disable profile tools categorically: rejected because it would make Dolgorae
  less compatible with the user's prepared Codex environments. Native-subagent
  disablement for a specific unobservable pin is reopened by ADR-019; it is not
  a categorical feature rejection.
- Mutable run instructions: rejected because they would weaken reproducibility
  and make past turn governance ambiguous.

## ADR-010: Keep Independent Runs Hub-and-Spoke Orchestrated

Status: Accepted, amended by ADR-023 and ADR-027

### Context

Independent Runs could technically invoke the CLI or connect to another Worker,
but peer control introduces authority escalation, cross-account access, cycles,
unbounded fan-out, audit causality, and writer-authority deadlocks. The product
also needs unambiguous terms for a Primary Agent, an Independent Specialist
Agent, and a Codex-native subagent. Codex already supports native subagents
within one session.

### Decision

Use a hub-and-spoke authority model in v1. One capability-bound Controller
mutates each Independent Run, while same-user observers may read its client-safe
projection. Dolgorae-managed agents may perform Native Delegation only when the
Runtime Profile capability snapshot reports `supported`; they must not control
another Dolgorae Run, connect to its socket, or receive its credential.

ADR-027 adds bounded broker-mediated Specialist Collaboration inside one
Brokered Hierarchy. This is logical direct request and response without Primary
message relay, but it does not change Controller authority or permit physical
peer transport.

In a Dolgorae-Orchestrated Session, the internal Orchestration Broker remains
the authority hub. It owns one separate Controller credential per Independent
Specialist Run, durable membership, write-ahead spawn, accepted task,
Collaboration Exchange, mailbox, activation, result delivery, and recovery
state. The Primary Agent and Specialists may request bounded work but receive no
cross-Run capability. The Collaboration Plane carries Specialist-to-Specialist
requests without requiring the Primary Agent to relay them.

In an External Specialist Engagement, the external AI remains the hub and
controls each hired Specialist through its own authorized public-adapter calls.
Dolgorae persists only the Specialist operational boundary and does not create a
second semantic planner. Defer actual parent-held inter-Run authority as
capability-scoped hierarchical delegation, not peer messaging.

### Consequences

- Independent run authority and audit provenance remain clear.
- The internal Collaboration Plane may carry bounded results between
  Specialists without a Primary relay; the external AI control plane continues
  to carry results in an External Specialist Engagement.
- A Primary Agent may request configured specialist work without receiving
  authority over the Independent Specialist Run or learning its Controller
  capability.
- A writer cannot deadlock by waiting on another writer it attempted to spawn.
- Supported native Codex subagents remain available for bounded parallel work inside a
  run.

### Rejected alternatives

- Arbitrary direct peer messaging or unbounded group chat: rejected because it
  permits uncontrolled cycles, unclear authority, and unrecoverable delivery.
  ADR-027 accepts only bounded, durable, broker-mediated Collaboration Exchanges.
- Let agents shell out to `dolgorae`: rejected because it bypasses master intent
  and could cross profile/account boundaries.
- Add a global broker daemon: rejected because it conflicts with ADR-001 and
  still requires a delegation security model.

## ADR-011: Freeze a Minimal Cross-Version Control Protocol

Status: Accepted

### Context

Requiring an exact Dolgorae binary digest for every worker request prevents a new
binary from identifying or cleanly stopping a live worker created by the old
binary. Treating all old workers as stale would make upgrades unsafe.

### Decision

Freeze control protocol v1 with only `hello`, bounded `status`, and `shutdown`.
It validates workspace, run, generation, boot UUID, and live process identity
but deliberately tolerates Dolgorae version and binary-digest differences.
`shutdown` is identity verified and interrupts an active turn before cleanup.
All mutation, history, and app-server operations remain on the current exact
version/digest protocol and reject skew.

### Consequences

- Upgrades have a bounded, auditable path to inspect and retire old workers.
- The permanently supported surface is intentionally tiny and non-extensible
  without a new control-protocol version.
- An old worker that cannot speak control v1 is not killed merely because its
  binary differs; ordinary identity and recovery safety still apply.

### Rejected alternatives

- Exact digest for shutdown: rejected because it creates an upgrade deadlock.
- Full cross-version compatibility: rejected because mutation semantics and
  schemas cannot safely be frozen with the control surface.

## ADR-012: Own Portable Contracts and Isolate Darwin FFI

Status: Accepted

### Context

The machine envelope, Codex stable subset, RFC 8785 bytes, and macOS process
proofs are release contracts. Leaving their shapes to prose or whichever crate
is selected makes compatibility and audit identity drift with implementation.

### Decision

Check in JSON Schemas for Dolgorae machine output and the Codex required subset.
Own JCS in an in-repository conformance module. Pin Rust 1.97.1 and Cargo.lock.
Put `posix_spawn` attributes, libproc, kqueue, byte-range fcntl, `fstatfs`, and
boot-UUID sysctl behind one safe Darwin module using the `libc` crate; no other
module contains `unsafe` OS bindings. Fault barriers, monotonic time, boot UUID,
process enumeration, and identity sampling enter core logic through injectable
interfaces.

### Consequences

- Protocol and audit changes are reviewable data changes, not hidden library
  behavior.
- Deterministic tests can drive timeout and crash boundaries without sleeping.
- Darwin-specific unsafety has one auditable owner.

### Rejected alternatives

- Rely on default `serde_json` serialization: rejected because it is not JCS.
- Use an async runtime for blocking durability/process APIs: rejected because it
  adds scheduling complexity without making those APIs nonblocking.
- Allow each subsystem to call libc directly: rejected because identity and
  errno classification would drift.

## ADR-013: Fail Closed on Live Worker Ambiguity and Unsupported Filesystems

Status: Accepted

### Context

A worker may be alive and healthy while quiet inside a Codex turn. Ledger,
logging, runtime-generation, and CPU inactivity are therefore not proof that it
is safe to terminate. Separately, `MNT_LOCAL` includes filesystems whose append,
fsync, and crash-tail behavior has not been established, while network mounts
  also split per-user writer authority across hosts.

### Decision

V1 never signals a live byte-1 worker solely because control `hello` or an
activity signal timed out. A revalidated `Match` returns `RUN_BUSY`; an
`Unverifiable` identity returns `RECOVERY_REQUIRED`. Only control-v1 shutdown,
an already-authorized cleanup snapshot, or independently proven worker exit can
lead to replacement. V1 supports only local APFS workspaces and state/lock
roots and provides no filesystem override.

### Consequences

- A wedged but live worker can require external termination or `fork --fresh`;
  v1 prefers a stranded run to a false live-worker kill.
- Audit and writer guarantees have one empirically testable filesystem basis.
- Broader filesystem support and emergency recovery remain future explicit
  decisions rather than implicit weakening.

### Rejected alternatives

- Heartbeat/CPU takeover: rejected because absence of progress is not proof of
  safe termination.
- Network-filesystem opt-in: rejected because it cannot preserve the stated
  cross-process writer guarantee across hosts.

## ADR-014: Own JSON Ingest and the Independent Fake Boundary

Status: Accepted

### Context

Default JSON deserialization is last-wins for duplicate members and may discard
number lexemes, contradicting the audit anti-forgery contract. A fake that
shares the production parser can certify the same mistake twice.

### Decision

Use `serde_json::RawValue` only behind an in-repository duplicate-detecting
visitor that preserves numeric source lexemes through adaptation; never parse
untrusted protocol input directly into `serde_json::Value` or typed structs.
The safe dependency/mechanism table in architecture.md is normative, and every
new runtime dependency requires an ADR amendment. The shared fake app-server is
an independent Python WebSocket-over-Unix-socket subprocess owned by TASK-004 and driven by
manifest-validated declarative scenarios.

Project configuration uses pinned `serde_yaml_ng` 0.10 behind typed adapters
that reject duplicate and unknown keys. YAML values are never used as untyped
protocol or ledger input.

### Consequences

- Duplicate rejection and number preservation occur at ingest, before JCS.
- Fake/production parser diversity reduces self-confirming conformance tests.
- TASK-006 consumes the TASK-004 fixture rather than creating a second core.

## ADR-015: Share One Reader Server and Isolate Each Active Writer in a Capsule

Status: Superseded by ADR-019 after the pinned topology campaign

### Context

App-server can host multiple reader threads and workspaces, while commands from
different threads inside one process tree cannot be safely attributed by OS
ancestry alone. Distinct account homes must remain isolated and writer cleanup
must never signal reader or unrelated processes.

### Decision

Key one Dolgorae-exclusive shared reader singleton by canonical `CODEX_HOME` plus
the complete checked launch authority: direct absolute Codex executable,
version/compatibility identity, normalized global arguments, symbolic
`profile_state_directory_v1` cwd policy, explicit deterministic non-secret
environment including PATH/LANG/LC_ALL, and normalized process-
static configuration. Profile names with the same key are aliases.
The same home with a different live launch contract fails with
`PROFILE_LAUNCH_CONFLICT`; Dolgorae never silently joins or replaces it and
never mixes contracts. Differing stopped definitions may coexist but only one
contract may own the next verified lifetime. The active contract may run the
shared reader singleton plus at most one Writer Capsule for the current durable
writer generation; both use the identical contract and canonical home.

Dolgorae launches only the direct Codex executable as `app-server --listen
unix://...`; arbitrary wrapper and shell profiles are outside v1. It supplies a
deterministic environment assembled from the account fields required for login,
platform runtime fields, canonical `CODEX_HOME`, and the profile's explicit
allowlisted map. It constructs `PWD` from the private profile directory rather
than caller cwd. A `DOLGORAE_MANAGED` marker may aid diagnostics but grants no
authority. Each worker owns one direct WebSocket connection. The profile
manager alone owns shared-singleton lifecycle. Writer activation uses the same
staged manager mechanism to allocate a capsule with its own UUIDv7 ID, capsule
epoch, short socket, process group, log drainer, and exact identity record.
Every App Server instance receives a globally unique, never-reused
`server_epoch`. A
canonical-home-keyed lock and active-contract record serialize different server
keys before their contract-keyed locks, preventing conflicting launch contracts
from racing two servers into one home. Authority remains in Application
Support, while the socket node uses a compact deterministic name under validated
`/tmp/dolgorae-<uid>/p/` for macOS path bounds. Worker loss never terminates or
adopts the shared server. Capsule or worker loss is reconciled only from the
durable capsule record and a complete process census.

Derive the concrete launch cwd only after computing the key. Validate a short
socket candidate against the full server key and recorded profile/home,
contract digest, epoch, inode, process and executable identities; a mismatch is
`RUNTIME_PATH_COLLISION` and never authorizes attach, unlink, or signalling.
Start/stop/restart/migration use revision-bound PREPARE/APPLY/COMMIT tokens and
perform every process, network, policy, and member wait without file locks.
Bare `profile doctor` is offline and side-effect free. An explicit
`--launch-probe` performs the staged launch check and stops only the singleton
it started unless `--leave-running` is also explicit; this keeps diagnosis from
silently changing profile lifetime.

Shared singleton and capsule stdin are null. A scoped Dolgorae log drainer receives
stdout/stderr, applies the diagnostic redaction rule, and maintains a 0600,
1-MiB plus one-rotation log. This lifecycle component is accepted because a
direct unbounded file cannot enforce rotation and `/dev/null` alone discards the
startup evidence required for lifecycle diagnosis. The shared drainer is
profile-owned; the capsule drainer is capsule-owned.

Promotion closes the reader connection and resumes the same thread on a newly
verified capsule with writer policy. Release fences new work, retires and cleans
the capsule, proves five consecutive complete empty censuses, and then resumes
the same thread on the shared server with reader policy. Handoff retires the
source capsule before starting the destination capsule. Once source retirement
is proved, destination failure leaves authority `none`; uncertain cleanup is
`blocked_unknown`. No transition restarts the shared reader singleton.

Maintain a hash-chained append-only Application Support membership journal and
a revision/checksum-bound derived index so profile-wide stop and restart can
enumerate every alias and run across projects. The manager replays the journal
and validates referenced records before acting; incomplete membership fails
closed with `PROFILE_MEMBERSHIP_INCOMPLETE`. Operator repair verifies a valid
prefix and exact identities, then appends tombstones rather than rewriting
history. Stop/restart uses a durable fence, unlocked quiesce, and identity-bound
commit so it never waits for members while holding `server.lock`. An interrupting restart pauses all
members and never resumes them automatically. Because it crosses controller
boundaries, it requires the separate local operator capability.

Version/configuration drift that changes `server_key` is an operator-only home
migration. Old/new server locks use binary key order, membership moves under one
durable migration ID, and failure rolls back before new ready or remains
explicitly blocked. A run-local `--accept-version-change` is rejected because a
single controller cannot authorize effects on other singleton members.

### Consequences

- Multiple reader workspaces and sessions share profile-level Codex state
  without sharing worker control, audit ownership, or JSON-RPC frames; the sole
  active writer has an exclusive process boundary.
- A singleton failure affects one profile, while different canonical homes use
  independent servers.
- Profile names are aliases; two names resolving to the same canonical home
  cannot create competing contracts. The accepted identical-contract capsule is
  recorded explicitly and is not a second shared singleton.

### Rejected alternatives

- Per-run app-server children for every reader: rejected because they duplicate
  a server that can safely multiplex read-only threads. A single exclusive
  writer capsule is accepted because it supplies the cleanup boundary the shared
  process tree cannot provide.
- One singleton across all profiles: rejected because it crosses the
  `CODEX_HOME` account boundary.
- An arbitrary wrapper command: rejected because its environment mutation,
  descendant ownership, and launch identity cannot be deterministic in v1.
- The pinned `app-server proxy`: rejected because it is a byte-copy bridge, not
  a JSONL boundary, and adds no per-run isolation.
- Raw mutable-config digests in `server_key`: rejected because normal Codex
  writes could make a running singleton disagree with its own identity.
- Waiting for worker quiescence under `server.lock`: rejected because member
  shutdown paths may need that lock and form a deadlock cycle.

## ADR-016: Bind Mutations to a Controller Capability and Keep Local Observation Open

Status: Accepted

### Context

An interactive client and a workflow orchestrator may discover the same run.
Process identity, a caller-supplied label, or controller metadata alone cannot
prevent one client from accidentally interrupting, answering, closing, or
handing off another client's work. Requiring credentials for every read would,
however, make local workspace discovery and writer diagnosis unnecessarily
fragile. The supported personal alpha already disclaims a hostile same-user
security boundary.

### Decision

Bind each run to a self-contained UUIDv7 controller credential with 256 bits of
random capability material. Accept the strict credential through an inherited
fd or a caller-owned mode-0600 regular file, never argv or environment. Persist
only a domain-separated SHA-256 digest and authorize every mutation before
worker discovery or effects. The CLI receives the capability through an
inherited descriptor or mode-0600 file and passes that already-open descriptor
with `SCM_RIGHTS`; the worker rereads and revalidates the credential under the
run mutation lock immediately before effects. A successful CLI check is never
the authoritative check. Do not create a global controller registry.

Allow same-uid local callers to read the complete client-safe projection,
including controller metadata and pending interactions, without a capability.
Create one separate UUIDv7 plus 256-bit local operator credential, initialized
and rotated explicitly and carried by the same fd/mode-0600-file rules. It
authorizes only the enumerated controller reset, profile-wide stop/restart/
migration, and membership-repair operations; it is not inferred from
environment, parent process, or same uid.
Rotation and use serialize on the operator lock; the consumer rereads the
already-open capability descriptor and revalidates ID, generation, and digest
under that lock before acquiring home/server/run locks or causing effects. It
uses `SCM_RIGHTS` only when the authoritative consumer is another worker.
Reset remains barred by active, pending, handoff, or unverifiable state; durable
writer authority must be safely released first.

For file-change approval, correlate the request with the pinned
`item/fileChange/patchUpdated` item. Keep small bounded diffs inline and place
larger bounded snapshots in digest-bound 0600 run artifacts; paths alone are not
an informed approval. The request shape itself is not extended upstream because
0.147.0 does not carry changes there.

For any secret user-input answer, retain first-success idempotency and an opaque
receipt without a content digest or HMAC. This intentionally gives up later
body comparison: an unkeyed digest is an offline oracle for low-entropy secrets,
while installation/controller-derived HMAC adds key rotation and reset coupling
that v1 does not otherwise need.

`run respond` accepts a response only through an inherited fd or non-TTY stdin;
response bodies are never accepted in argv. This applies to non-secret answers
too, because a schema discriminator cannot make an already exposed argv value
secret after parsing.

### Consequences

- Independent clients cannot accidentally mutate each other's runs merely by
  discovering run IDs.
- One credential reused deliberately across runs establishes same-controller
  writer handoff without a global service.
- Lost credentials fail closed until a visibly audited operator reset.
- Same-user malware can still read an inadequately protected credential file;
  the mechanism is coordination, not a multi-user privilege boundary.

### Rejected alternatives

- Controller metadata only: rejected because public metadata is forgeable.
- Reusable secret in argv or environment: rejected because process inspection
  and inheritance expose it beyond the operation.
- Controller-only observation: rejected because it obscures writer blockers and
  conflicts with the selected local full-visibility policy.
- Global registry daemon: rejected because it conflicts with ADR-001.

## ADR-017: Separate Durable Event Records From Delivery Metadata

Status: Accepted

### Context

Raw app-server events are version-specific, may contain secrets or reasoning,
and force every external client to reproduce Dolgorae's correlation and
redaction logic. Separately materialized public logs could disagree with the
hash-chained audit authority.

### Decision

Append schema-validated normalized client event records inside the one audit
ledger and schema-enforce minimal or operational projection profiles from those
records. Minimal permits only run/turn state, final response, interaction,
runtime error, writer, and recovery events; usage, workspace changes, command,
diagnostic, generation, and reasoning-suppression metadata are operational-only. A durable
record contains only event identity and payload. A delivery envelope adds the
requested projection and replay flag at read time; transport metadata is never
hashed into the durable record. Use canonical unsigned-decimal strings in the
run ledger sequence as the sole cursor domain, with `"0"` as the pre-first-event
cursor, and permit gaps caused by profile filtering.
Known reasoning methods are suppressed during initialization when supported;
all reasoning text, summaries, deltas, and internal planning content are
discarded before persistence regardless of suppression success. Retain bounded
method/length/digest metadata only. Remove the public raw-events option; an
explicit local export may retain bounded redacted non-reasoning wire evidence.

Select a final response only from authoritative completed root-turn
`agentMessage` item order: the last `final_answer` phase wins, then the last
phase-null compatibility item. Commentary and descendant-thread messages never
become `response.final`; absence is a successful null result, not a fabricated
event or a `FINAL_RESPONSE_UNAVAILABLE` error.

### Consequences

- UI and orchestrator clients consume stable record and delivery schemas and reconnect
  without understanding Codex versions or process generations.
- Audit remains the only durable event authority.
- Reasoning cannot leak through events, logs, exports, or later reprojection.
- Operational diagnostics are bounded and less complete than raw wire capture.

### Rejected alternatives

- Filter raw payloads only at read time: rejected because sensitive reasoning
  would already be durable and a future projection bug could expose it.
- Separate authoritative client-event log: rejected because crash ordering and
  repair would have two authorities.
- Public raw profile: rejected because redaction cannot make arbitrary upstream
  payloads a stable client contract.
- Treating projection membership as prose only: rejected because a syntactically
  valid minimal envelope could otherwise carry operational command data.

## ADR-018: Expose Bounded Artifacts and Profile Diagnostics

Status: Accepted, amended by TASK-000-H

### Context

Final answers and exact file-change snapshots can exceed the safe inline
machine boundary. Profile startup can also fail before a Run exists, so
run-scoped events cannot represent all actionable failures.

### Decision

Store only `user_input`, `file_change_diff`, and `final_response` artifacts in the run-private
mode-0600 store. Bound a persisted user-input artifact at 8 MiB, a file diff at 8 MiB, a final response at 32 MiB, and a
run at 256 MiB. Inline final responses are at most 1 MiB. Public show/read use
opaque IDs, digest verification, base64 chunks no larger than 1 MiB, and no
internal path; controller-authorized export is explicit. Reasoning is never an
artifact. Quota or write failure makes a final response `unavailable` without
changing a completed turn into failure.

Create a separate bounded profile diagnostic journal and cursor for pre-Run and
profile-wide operations. Same-uid `minimal` queries expose redacted status,
code, and message. Operator-authorized `operational` queries add bounded
redacted detail. Neither projection exposes credentials, raw environment,
reasoning, raw server payloads, or internal artifact paths. A Run is published
only after the Profile Server has durably published a ready non-null epoch.

### Consequences

- Large client-safe values remain retrievable without unbounded envelopes.
- Profile start failures are discoverable without inventing a failed Run.
- Artifact and diagnostic retention/authorization require dedicated tests.

### Rejected alternatives

- Put large values in the audit line or stdout envelope: rejected because it
  defeats bounded parsing and replay.
- Attribute profile startup failures to a synthetic Run: rejected because no
  ready server epoch exists to bind that Run.
- Make diagnostics operator-only: rejected because same-user clients need a
  safe explanation of profile failures; operational detail remains privileged.

## ADR-019: Use Sticky Dedicated Execution Lanes and Explicit Control Modes

Status: Accepted after the fourth consistency pass, amended by ADR-023 and ADR-026

### Context

The transient Writer Capsule candidate moved one persistent thread from the
shared reader server to a temporary writer server and back. Exact Codex 0.147.0
testing showed that `thread/unsubscribe` acknowledged subscription removal but
left the source thread loaded after two seconds, while another App Server
rejected resume as already having an active writer. The same campaign proved
same-home server initialization and catalog stability, read/write policy
changes on one server, concurrent writers in two workspaces, ten live idle
servers, and closed-generation history resume. Background-terminal discovery
returned no entries. A subsequent Dolgorae census/cleanup campaign passed. An
earlier native-subagent conclusion was unusable: its semantic result reported no
collaboration item while retained wire shapes contain `subAgentActivity` and
`collabAgentToolCall`. The corrected campaign separated Codex-native child
threads from independent Dolgorae Runs and workers.

Purpose metadata also cannot define who controls a Run, how interactions route,
where its thread resides, or what process assurance the product claims.

### Decision

Use one profile shared-read-only lane and zero or more Run-owned Dedicated
Execution Lanes. Select `shared_readonly` or `dedicated` when creating the Run
and never change it. A dedicated thread remains in its logical lane for its
entire lifetime. A verified read/write policy change occurs in place on the
same Worker, connection, process generation, server epoch, and thread and
increments only `policy_epoch`; it does not increment `run_generation` or
`thread_generation`. Read/write policy and workspace authority may change
within that lane. A stopped physical server may be replaced only after exact absence,
complete process census, native-work quiescence, and durable-history proof; the
successor is a new process generation of the same lane. A shared Run needing
write creates a lineage-linked dedicated write continuation.

Keep writer authority per canonical workspace. The profile may have concurrent
dedicated writers in different workspaces. Separate effective policy, writer
authority, server-lane infrastructure, and background-workload state in durable
state and projections. Profile lifecycle enumerates the shared lane and all
dedicated lane journals; dedicated servers restart lazily.

Dolgorae owns the exact-identity process census for a dedicated generation. It
samples every 100 milliseconds and on command notifications, signals or cleans
only exact revalidated identities, and requires five complete empty samples
after leader exit. A malformed or incomplete census, PID reuse, an unregistered
survivor, unreadable identity, or detected escape makes background state
`unverified`. A native Codex terminal API may add `hybrid` evidence but is not
the authority. These rules survive from the rejected Writer Capsule candidate;
the capsule topology and its shared-to-capsule thread-resume campaign do not.

Use the shared lane only for lightweight read-only analysis. Compiler/test
execution, formatters, watchers, long-running validation, background processes,
or work needing reliable command ownership and cleanup selects a dedicated
lane. Routing follows expected command/write behavior and assurance, never
purpose alone.

Make `direct_interactive` and `managed_agent` immutable control modes, separate
from purpose and lane. Direct mode accepts `human_cli` or `interactive_client`;
managed mode accepts `workflow_orchestrator`, the internal Orchestration Broker,
or `automation`. The semantic service requires explicit mode, purpose, lane,
and assurance for both. A user-facing facade may resolve these fields from the
selected use case, but hidden defaults do not exist. Controller kind `other`
cannot bind a v1 Run. Only the Controller resolves full
interactions and reads interaction-derived artifacts; observers receive strict
non-sensitive summaries and observer-visible artifacts. Controller credentials
remain outside every LLM-visible channel.

Advertise only `best_effort_personal_alpha` for Codex 0.147.0. Requested
assurance is checked before allocation. Polling and exact identity revalidation
remain the background-work authority, but do not claim adversarial containment.
Basic same-home coexistence and bounded storage integrity passed only for the
retained personal-alpha scenario. Long-duration/high-contention operation and
forced authentication refresh are unverified; production-grade durability is
not claimed.
Treat Codex-native `multi_agent` as a profile policy independent of Dolgorae
Run/worker concurrency. Public profiles permit only the enabled policy; explicit
disable is rejected because the pinned binary did not enforce it. The
corrected exact-version campaign proves child identity, parent, terminal
lifecycle, persisted history, restart continuity, and exact cleanup, so the
enabled profile reports supported lifecycle observation and quiescence tracking.
Operations still refuse active or unknown native state when they require
quiescence. Disable enforcement is unavailable; the disabled case is a
diagnostic-only unverified observation.

`run create-write-continuation` preserves workspace, profile, and control mode,
accepts only a shared-readonly or access-transition-unavailable/unverified
creation reason, and may
apply validated model/effort, purpose, capability additions, and a non-decreasing
assurance request. The source Controller authorizes creation and a new
same-principal credential binds the destination. This avoids cloning authority,
Controller instructions, reasoning, native-subagent hidden history, or writer
authority while allowing an orchestrator to
specialize the successor deliberately.
The destination Run is intentionally threadless at creation. Eager thread
allocation is rejected because local publication may still fail and the sticky
lane starts its physical generation lazily. The creation projection therefore
requires a null destination thread; once first input binds it, the thread must
be non-null and different from the lineage source thread.

### Consequences

- Same-thread cross-server migration is eliminated from normal and recovery
  flows; transient Writer Capsule activation/release is removed.
- Write-capable Runs consume a dedicated App Server while active. Ten idle
  servers measured about 1.21 GiB RSS, so shared read-only remains available
  and paused dedicated servers stop after the absence barrier.
- Multiple workspaces retain concurrent writers without a profile-global
  downgrade.
- Codex-native descendants and independent Dolgorae Runs are different
  concepts. Native descendants may run, but active or unknown native state
  blocks pause, generation replacement, profile stop, and shutdown. Codex goal
  state is not a v1 quiescence operand: the pinned client method set exposes no
  goal query, so requiring it would make every quiescence-dependent transition
  fail closed.
- The v1 pre-implementation schemas are rebaselined in place; no production
  state migration is required.

### Rejected alternatives

- Transient Writer Capsule: rejected because the pinned migration gate failed
  and no explicit unload primitive was demonstrated.
- All Runs dedicated: rejected as the default because measured idle resource
  cost buys no additional safety for permanently read-only managed work.
- Single shared server: rejected because it cannot create a per-writer process
  ownership boundary.
- Infer lane from purpose: rejected because purpose is immutable descriptive
  metadata and is not an authority contract.
- Treat experimental background-terminal APIs as authority: rejected because
  live discovery returned zero entries for the background-workload probe.
- Claim verified thread-scoped or strong containment: rejected until a future
  pin proves complete descendant control or supplies kernel enforcement.

## ADR-020: Standardize Brokered Independent Subagents on Public Adapters

Status: Accepted, amended by ADR-023 and ADR-027

### Context

The existing `managed_agent` Run, automation Controller, parent reference,
durable result, and writer-authority contracts can support a trusted host that
uses an Independent Dolgorae Run as a specialist. The public feature and this
ADR title retain the phrase `brokered independent subagent`, while the
canonical Run role is now **Independent Specialist Run**. The active contract
nevertheless described only generic workflow orchestration and explicitly
deferred every MCP adapter, leaving ordinary Codex and Gul-hosted direct
sessions without a named, discoverable composition to target.

### Decision

Standardize the Independent Specialist Run composition over the public
semantic service through either the Machine CLI or local gRPC adapter. This
composition serves External Specialist Engagements and the internal Brokered Hierarchy composition of a Dolgorae-Orchestrated Session.
The trusted host creates and retains one protected `automation` Controller
credential per specialist, starts a `managed_agent` in a `dedicated` lane with
an explicitly selected Runtime Profile, purpose, assurance, and opaque parent
reference, and drives it through the existing input, observation, interaction,
writer, interrupt, and close operations. No new `subagent`, `hierarchy`, or
topology command and no alternate lifecycle are added.

Expose `features.brokered_independent_subagent_runs=true` only when that
composition and its Controller separation are implemented. The requesting model
may receive non-secret identity, status, and bounded result material, but never a
Controller capability. The child remains durable after a parent or broker
disconnect. Every child write uses the existing canonical-workspace writer
authority; external editors and non-Dolgorae Codex processes remain outside that
guarantee.

The public v1 surface does not add an MCP adapter, installed daemon,
parent-held delegation capability, nested authority transfer, or arbitrary peer
control. ADR-027 adds only a private run-bound MCP bridge and bounded durable
Collaboration Exchanges inside a Dolgorae-Orchestrated Session. A future public
stdio MCP adapter must remain a thin wrapper over the stable semantic service.

### Consequences

- An external AI can open an External Specialist Engagement, while a
  Gul-hosted Dolgorae-Orchestrated Session can enter Brokered Hierarchy
  composition, without changing Dolgorae Run semantics.
- Gul may display only the requesting turn's tool result and need not expose a
  specialist Controller, lifecycle, persistence, or navigation model to the
  Primary Agent.
- Competing Dolgorae writers from different clients still converge on the one
  durable workspace authority and `WRITER_BUSY` result.
- Capability-scoped hierarchical delegation remains a separate future design.

### Rejected alternatives

- Give the parent model a child Controller credential: rejected because it
  places mutation authority in an LLM-visible channel.
- Add a second subagent lifecycle API: rejected because the existing Run
  operations already own every required transition and failure rule.
- Reintroduce transient same-thread Writer Capsules: rejected because the
  pinned cross-server migration gate failed; brokered writers use existing
  sticky Dedicated Runs.

## ADR-021: Expose a Supervised Local gRPC Gateway Over a Private Unix Socket

Status: Accepted, amended by ADR-027

### Context

The Machine CLI remains appropriate for finite automation, diagnostics,
recovery, and conformance, but a long-lived Gul process needs to observe many
Runs without retaining one `run events --follow` subprocess per Run. It also
needs bounded-latency unary mutations, independent Run streams on one channel,
and reconnect from the durable event cursor. A mandatory installed daemon or a
public network listener would enlarge the lifecycle and authentication boundary
without improving Dolgorae authority.

### Decision

Add `dolgorae serve --socket <absolute-private-socket-path>` as a supervised
foreground process. It remains optional for finite low-level clients, but
ADR-027 requires the same process while Brokered Specialist Collaboration is
active because it hosts the reconstructable Control-Plane Runtime. V1
binds only a Unix domain socket, validates the peer UID, and exposes no TCP,
remote bind, Tailscale, HTTP authentication, client-streaming, or bidirectional
streaming contract. One gateway may run per Dolgorae user installation. A
lifetime `gateway.lock` and identity-complete `gateway.json` under the
user-private Application Support root serialize startup without entering the
ordinary operation lock hierarchy.

The supplied socket parent must already be a current-uid-owned non-symlink
directory with mode 0700. The socket is mode 0600. Descriptor-relative
no-follow traversal rejects symlinks, ownership or permission mismatch,
non-socket collisions, and unowned stale sockets. A stale socket may be removed
only when its inode matches the prior gateway record and that exact process is
proved absent. Graceful SIGTERM stops acceptance, gives admitted unary calls a
five-second drain, terminates streams with typed `SERVER_SHUTDOWN`, and unlinks
only the still-matching socket inode.

Gul owns only private-parent creation/validation, unused-path selection,
process supervision, and post-readiness verification. Dolgorae exclusively owns
the lock/record, bind, chmod, stale proof, unlink, and graceful socket cleanup;
clients never unlink the provider socket. V1 does not attach to an existing
gateway or permit multiple gateways for one installation.

Use versioned standard gRPC over HTTP/2 with unary RPCs for commands, snapshots,
timeline pages, and artifact chunks, plus Run-scoped server streaming for
events. Use `tonic` and `prost`; isolate the `tokio` runtime inside the gateway
adapter and dispatch into the shared semantic service through bounded work
queues. No adapter owns an in-memory business-rule fork. Per-stream delivery is
bounded to 32 envelopes or 4 MiB and five seconds of stalled delivery. Pressure
terminates only that Run stream with typed `SLOW_CONSUMER`; the client resumes
from its own last committed durable cursor.

The public schema uses typed projections for closed semantic state and a typed
event `oneof` covering every client-safe event variant. Full Controller
Interaction payloads are also a typed `oneof`; `response_schema_id` applies
only to the bounded protected response body. Run, Writer, Interaction, and
event projections share a revision stamp, public output paths preserve UTF-8
or opaque POSIX bytes, and capability blocker codes are closed enums.
`GetCapabilities` is a
version-zero handshake and publishes the full capability and Controller-carrier
schema inventory plus exact Interaction response/payload and artifact bounds.
The carrier capability separates its canonical
Application-Support-relative root locator from a closed root-policy enum, and
uses closed enums for capability encoding and normalized-principal rules.
`SubmitTurn` returns accepted Turn plus typed Run and Writer
snapshots. `CreateWriteContinuation` returns the destination, immutable
lineage, new same-principal Controller, source-unchanged receipt, and exact-
replay indication.

`RunProjection` includes the durable accepted profile, purpose, model, required
capabilities, parent and instruction identities, and current default effort.
Lost `StartRun` responses use exact same-key replay as the primary recovery
path; allocation tombstones have no TTL and prevent post-delete key reuse from
allocating another Run. Shared-readonly and unsupported-transition failures
direct clients to `CREATE_WRITE_CONTINUATION`, while a threadless dedicated Run
continues to require its first `SubmitTurn(WRITE)`.

Controller timeline interaction items include typed kind and status plus a
bounded safe title; they never expose private prompts or raw interaction
payloads. Unsafe socket startup returns the typed client action
`FIX_SOCKET_PATH`, so the supervisor repairs or replaces the path before a
fresh start rather than treating gateway restart as sufficient remediation.

Interaction support uses a dedicated enum so Machine
`recognized_unsupported` remains distinguishable from `unavailable` over
gRPC. Profile model entries own their single default through `is_default`; a
second top-level default-model field is rejected as an ambiguous authority.

The gateway does not supervise workers or App Servers. Its exit never releases
writer authority, changes Run lifecycle, or signals those processes. Restart
reconstructs safe projections from the workspace path, expected workspace ID,
run ledger, and other authoritative Dolgorae state supplied by each request.
Gul remains responsible for every remote HTTP authentication and authorization
decision before it exposes local results.

### Consequences

- One HTTP/2 channel may carry independent streams for many Runs without
  creating a universal command stream or shared cursor/failure boundary.
- Channel establishment or successful request-byte delivery proves only
  transport delivery. A successful `SubmitTurn` response proves durable
  acceptance, while Turn completion still requires events or a snapshot.
- The Rust runtime dependency boundary grows, but asynchronous code remains an
  adapter mechanism rather than lifecycle or durability authority.
- Operator administration remains Machine CLI-only. Controller credential
  creation has no RPC, but a trusted same-user client may create a checked-
  schema carrier below its advertised client-specific descendant; overlapping
  CLI and gRPC operations require semantic conformance tests.

### Rejected alternatives

- Permanent system daemon: rejected because client supervision is sufficient
  and durable Runs already outlive the public gateway.
- TCP or direct Tailscale binding: rejected because Dolgorae does not implement
  a remote authentication boundary.
- One bidirectional command/event stream: rejected because stream loss would
  ambiguously couple mutation results to observation delivery.
- Direct Gul access to worker or App Server sockets: rejected because it
  bypasses Controller, writer, idempotency, recovery, redaction, and audit
  semantics.

## ADR-022: Adopt a Non-Breaking Agent Topology Terminology Layer

Status: Superseded by ADR-023

### Context

The existing SOT already assigns precise meanings to `Master`, `Controller`,
`control_mode`, `Worker`, Runtime Profile, Independent Dolgorae Run, and
Codex-native subagent. Informal phrases such as "master mode", "subagent mode",
"hierarchy mode", and "spawn mode" collide with those meanings. The product
needs stable language for three user-selected compositions without reopening
the completed Gul transport contract or inventing a second Run lifecycle.

### Decision

Preserve every existing authority, control, process, protocol, and schema term.
Add a descriptive Agent Topology layer with three canonical compositions:

1. **Standalone Primary Topology** for one user-facing Primary Run.
2. **External Delegation Topology** for Independent Specialist Runs serving a
   primary agent outside Dolgorae.
3. **Brokered Hierarchy Topology** for one Dolgorae Primary Run plus one or more
   Independent Specialist Runs mediated by a Hierarchy Coordinator.

Use **Primary Agent**, **Primary Run**, **Independent Specialist Agent**, and
**Independent Specialist Run** as canonical role terms. `Master` continues to
mean the external orchestration owner; **External Master** is only a clarifying
alias. Keep historical titles, public field names, and the feature identifier
`brokered_independent_subagent_runs` unchanged.

Treat **Native Subagent Policy** as orthogonal to Agent Topology. Actual use of
Codex-native descendants is **Native Delegation**. Native children remain inside
one Run and never become Independent Specialist Runs.

When using one of these primary/specialist compositions, the user or trusted
caller explicitly selects the topology and Runtime Profiles. Generic managed
workflow Runs remain valid outside this role terminology. Selection is expressed
through existing public operations,
`control_mode`, Controller kind, purpose, and optional parent provenance.
Dolgorae does not infer topology from model output or add a public
`topology_mode`, durable hierarchy object, peer-messaging channel, or alternate
lifecycle in v1. A model may request configured specialist work, but a trusted
broker or Hierarchy Coordinator owns and performs every specialist mutation.

### Supersession

ADR-023 retains the distinction among Primary Runs, Independent Specialist
Runs, and Codex-native subagents but replaces the three user-selected
topologies with two product use cases. It also makes Brokered Hierarchy state a
durable Dolgorae authority rather than external-only presentation metadata.
The terminology in this ADR remains historical evidence for TASK-000-G and is
not active product authority.

### Consequences

- Existing Gul, Machine CLI, gRPC, schema, and capability contracts remain
  byte-for-byte unchanged by this terminology decision.
- Standalone Primary and hierarchy-root Runs continue to use
  `direct_interactive`; Independent Specialist Runs continue to use
  `managed_agent` with broker-held control.
- Brokered Hierarchy describes a coordinator-mediated composition, not
  parent-held hierarchical authority. TODO-001 remains the separate candidate
  for any future delegated capability design.
- Runtime Profile remains the explicit agent-configuration mechanism and is
  never inferred from topology.
- Active documentation can distinguish Independent Specialist Runs from
  Codex-native subagents without renaming historical evidence or wire fields.

### Rejected alternatives

- Rename `Master` to Primary Agent: rejected because `Master` already owns the
  external orchestration boundary.
- Rename `control_mode` values to master/subagent: rejected because control mode
  governs authorization and interaction routing, not cognitive role.
- Add a topology enum to public v1: rejected because the current compositions
  are expressible through existing operations and the Gul interface is already
  aligned.
- Call every managed Run a subagent: rejected because managed workflow agents
  may have no parent and because the term collides with Codex-native subagents.
- Treat Native Delegation as a fourth topology: rejected because it can occur
  inside every Primary or Independent Specialist Run.

## ADR-023: Expose Two User-Facing Use Cases and Own Brokered Recovery State

Status: Accepted, amended by ADR-027 and ADR-028

### Context

Presenting Standalone Primary, External Delegation, and Brokered Hierarchy as
three user-selected topology modes exposes internal control concepts and makes
Gul responsible for reconstructing Dolgorae-created agents after failure. The
product has two materially different authority situations: either Dolgorae is
the main control plane, or another AI already is.

### Decision

Expose exactly two user-facing use cases.

1. **Dolgorae-Orchestrated Session**: Dolgorae hosts a Primary Agent and owns the
   operational orchestration loop. Gul is the canonical presentation and
   interaction client. The session begins in Standalone Primary composition and
   dynamically enters Brokered Hierarchy composition when the internal
   Orchestration Broker provisions one or more Specialist Runs.
2. **External Specialist Engagement**: another AI remains the Primary Agent and
   semantic control plane. Dolgorae provides only selectively hired Specialist
   Runs and their durable accepted task and result boundary.

Keep every Run independent. Introduce first-class durable Orchestration Session,
External Specialist Engagement, member, write-ahead spawn or hire, Specialist
task, and result-delivery records. Dolgorae is authoritative for operational
membership, owned lifecycle, idempotency, result redelivery, and recovery. It
never infers or replays an ambiguous semantic task.

Use the Primary Run ID as the Orchestration Session ID in v1. Permit one Run in
at most one active aggregate. Forbid active reparenting, in-place role
conversion, nested first-class hiring from an External Specialist Engagement,
and in-place transfer between use cases.

Keep the checked public v1 Protobuf source and descriptor unchanged. Existing
Run operations remain the low-level client contract. Future additive aggregate
queries may improve discovery but do not move state ownership back to Gul.

### Consequences

- Users choose only whether Dolgorae or an external AI owns orchestration.
- Standalone Primary and Brokered Hierarchy are internal composition states of
  one Dolgorae-Orchestrated Session.
- Gul may restart without losing Dolgorae-created Specialist membership or
  result-delivery state.
- External AI integrations do not receive a redundant planner or second control
  plane.
- Additional transactional state and crash-recovery testing are required.

### Rejected alternatives

- Keep all hierarchy state in Gul: rejected because spawn and delivery failures
  can leave duplicate or orphaned Specialists and make local recovery depend on
  a presentation client.
- Make Dolgorae a workflow engine for external AI integrations: rejected because
  the external AI already owns task planning and completion decisions.
- Merge child agents into the Primary Run: rejected because independent thread,
  Controller, lane, audit, and recovery boundaries are required.

## ADR-024: Separate Runtime Profile From Agent Configuration

Status: Accepted

### Context

The prior prose allowed Runtime Profile to mean both process launch contract and
agent personality. Two roles using one `CODEX_HOME` but different Codex
`--profile` launch arguments can trigger the same-home launch conflict even when
only their instructions differ.

### Decision

Define Runtime Profile as execution, account, tooling, process-static
configuration, and verified capability only. Define Agent Configuration as the
immutable Run-facing snapshot of Runtime Profile reference, role reference,
normalized Controller instructions, model, default effort, purpose, and
required capabilities.

Allow multiple Agent Configurations to share one Runtime Profile. Require every
selected public Runtime Profile to state `native_subagents: enabled`
explicitly. Do not claim that mutable AGENTS, skills, plugins, apps, or MCP files
are byte-immutable after Run creation; the explicit normalized instructions and
snapshot digests are the Dolgorae-owned role authority.

### Consequences

- Reviewer, tester, and architect roles can share one compatible App Server
  launch contract.
- Account or tooling isolation still uses separate Runtime Profiles.
- Run manifests and projections expose both snapshot identities clearly.

### Rejected alternatives

- Use one Runtime Profile per personality: rejected because it couples behavior
  naming to same-home process constraints.
- Infer roles from profile display names: rejected because names are not stable
  authority.

## ADR-025: Keep Mutable Authority Outside the Agent-Writable Workspace

Status: Accepted

### Context

The earlier project-local layout stored Run ledgers, writer records, locks,
profile configuration, and recovery evidence below `.dolgorae` while writer
Turns could write the canonical workspace. Prompt-only reservation cannot
prevent accidental deletion or corruption of the control plane.

### Decision

Keep only portable tracked policy in `<workspace>/.dolgorae/`. Store
machine-local configuration and every mutable authority below
`~/Library/Application Support/Dolgorae/workspaces/<workspace-id>/`. Bind the
roots through the canonical workspace ID and lossless path record. Never include
the Application Support workspace state root in Codex writable roots or
model-visible path projections.

Preserve private short `/tmp` sockets as locators only. Their exact attach and
cleanup authority remains the Application Support state plus held locks and
process identity.

### Consequences

- An agent or formatter operating inside the workspace cannot directly modify
  Run audit, hierarchy, writer, lock, or profile authority.
- Workspace portability and versioned policy remain simple.
- Existing project-local state prose and downstream tasks require migration
  before implementation begins.

### Rejected alternatives

- Keep mutable state in `.dolgorae` and rely on instructions: rejected because
  it is not structural isolation.
- Make the workspace read-only to every writer: rejected because the product
  exists to perform controlled edits.

## ADR-026: Separate Policy Epoch From Run Generation and Avoid Cross-Controller Atomic Claims

Status: Accepted

### Context

The previous text used `run_generation` for both Worker/connection lifetime and
access-policy lifetime, while also requiring a Dedicated Run to change read and
write policy in place. Brokered Primary and Specialist Runs additionally use
different Controller credentials, so the existing same-Controller writer
handoff cannot be treated as an atomic hierarchy transfer.

### Decision

Define `run_generation` solely as one Worker lifetime and its private direct App
Server connection. Define `policy_epoch` as the monotonic version of effective
access within that generation. A verified Dedicated read/write transition keeps
the Worker, connection, thread, logical lane, process generation, and server
epoch and increments only `policy_epoch`. Immutable instructions never contain
current access; every Turn receives a dynamic access context.

For writer movement across Primary and Specialist Controller identities, use
explicit source release, authoritative verification that writer state is
`none`, and destination acquire. Do not claim this sequence is atomic. If
another Run wins the race, return `WRITER_BUSY` and let the Orchestration Broker
reschedule. Retain the existing atomic handoff only for eligible Runs controlled
by the same Controller identity.

### Consequences

- Recovery can distinguish process replacement from a policy transition.
- Pending interactions and requests are fenced by Run generation, while each
  Turn records its accepted policy epoch.
- Brokered hierarchy writer movement has an explicit race but no credential
  escalation or false atomicity claim.
- A future cross-Controller atomic handoff requires a separate capability and
  audit design.

### Rejected alternatives

- Increment Run generation for every access change: rejected because it
  contradicts same-process, same-thread transition semantics.
- Reuse same-Controller handoff across different Controllers: rejected because
  it would bypass the established capability boundary.
- Give the Primary Agent the Specialist Controller credential: rejected because
  it breaks hub-and-spoke authority isolation.

## ADR-027: Use Durable Virtual-Actor Mailboxes for Brokered Specialist Collaboration

Status: Accepted

### Context

Brokered Hierarchy Specialists need to consult one another without forcing the
Primary Agent to spend an additional model Turn relaying every request and
response. Direct Worker sockets, model-held credentials, per-Specialist database
polling, and in-memory-only queues would weaken authority, recovery, audit, and
resource behavior. A Specialist may also be logically active while its Worker
and physical lane are stopped to save resources.

### Decision

Permit bounded Brokered Specialist Collaboration only between active owned
Specialists in the same Dolgorae-Orchestrated Session. Preserve hub-and-spoke
Controller authority, but allow logical direct request and response through an
internal Collaboration Plane. Do not offer this path to External Specialist
Engagements in v1.

Use one Application Support SQLite database per workspace as the transactional
orchestration authority. Enable WAL, foreign keys, full synchronous durability,
and one mutation owner. Store Collaboration Exchanges, per-Run mailbox items,
activation operations, result-delivery state, and a hash-chained append-only
event table in the same authority. JSON and JSONL are exports only.

Treat each Independent Specialist Run as a Virtual Actor. Specialists never
poll SQLite. The Gul-supervised foreground `dolgorae serve` process hosts one
Mailbox Scheduler and Activation Manager. After a database commit it marks the
target Run dirty and sends an in-memory wake. Startup and one low-frequency
global reconciliation scan recover lost wake signals and expired pre-dispatch
claims.

A passivated Specialist preserves Run ID, thread, logical lane, Agent
Configuration, aggregate membership, mailbox, and audit state while releasing
its Worker and Run-owned physical lane. Mail under `on_mail` policy performs one
compare-and-swap activation with a bounded lease. `paused` requires explicit
resume, and terminal membership cannot be activated.

Run one target Turn at a time. A busy target queues new work and is never
preempted. Select queued work by internal recovery priority, starvation override,
inherited root priority, dependency-unblock boost, deadline, and mailbox
sequence, with source fairness and bounded queues. Specialists cannot choose a
higher priority than their root task.

Use a private run-bound MCP bridge for submit, await, and collect operations.
Bind source Run, source Turn, tool-call identity, and idempotency outside model-
controlled arguments. If the pinned shared-profile transport cannot prove that
binding, require Dedicated Lanes for collaboration-capable members. A live probe
must close this dependency before implementation.

Commit request and target-mailbox insertion before wake. Commit successful
execution, immutable result reference, source result-mailbox insertion, and
delivery-pending state in one transactional outbox boundary. Result
notification is at least once and consumption is idempotent. Unknown target Turn
acceptance or outcome is never automatically replayed.

Reject cross-session targets, nested Specialist hiring, peer lifecycle control,
writer transfer, writer-held blocking waits, wait cycles, excessive depth,
fan-out, or queue use. Collaboration carries bounded messages and artifacts, not
Controller authority, hidden reasoning, or raw protocol frames.

### Consequences

- Primary model Turns and repeated context copies are avoided for lateral
  Specialist consultation.
- A sleeping Specialist can be awakened by durable mail without a polling loop.
- Multiple requests arriving during activation produce one Worker generation and
  one ordered mailbox.
- Queue, scheduling, activation, and delivery behavior become explicit,
  testable, and crash recoverable.
- Active Dolgorae-Orchestrated Sessions depend on the Gul-supervised foreground
  control-plane runtime, but no installed daemon is introduced.
- SQLite schema migrations, claim leases, transport probes, crash injection, and
  cycle tests are required.

### Rejected alternatives

- Direct Worker-to-Worker sockets: rejected because endpoints are ephemeral and
  would expose authority, recovery, and audit hazards.
- One polling loop per Specialist: rejected because process residency and state
  ownership would leak into every actor and idle scaling would be poor.
- File-system mailboxes: rejected because cross-object atomicity, ordering, and
  claim recovery are weak.
- External NATS, Redis, or RabbitMQ: rejected because a local personal-alpha
  product does not need another operational dependency.
- In-memory channels as authority: rejected because committed work would be lost
  across restart.
- Preempt an active LLM Turn for priority mail: rejected because safe continuation
  and outcome are not provable.
- Keep the Primary Agent as a mandatory relay: rejected because it spends model
  capacity on transport rather than orchestration and duplicates context.


## ADR-028: Use Explicit Aggregate Bootstrap and Private Specialist Facades Over Public Run v1

Status: Accepted

### Context

The public Run v1 contract is already aligned with Gul. Adding public aggregate
RPCs before implementation would reopen that contract, but leaving aggregate
creation, external grouping, and Primary-to-Broker operations implicit would
produce duplicate roots, orphaned Specialists, and incompatible client
implementations after a crash or retry.

A previous draft let a raw externally hired `managed_agent` Run lazily create
an engagement from a client-generated parent reference. That made `parent_ref`
perform both presentation and creation duties and allowed low-level Run listing
to become a de facto grouping protocol. The corrected design separates explicit
empty-engagement bootstrap from each write-ahead hire operation, so aggregate
creation and child creation have independent retry and recovery boundaries.

### Decision

Keep the checked public Protobuf source and descriptor unchanged. Use explicit,
durable aggregate bootstrap and two private specialist facades over the shared
semantic Run core.

For a Dolgorae-Orchestrated Session, require a protected `human_cli` or
`interactive_client` Controller carrier containing a checked Orchestration
Launch Intent, a parentless public `direct_interactive` root `StartRun`, and an
explicit installed Specialist Policy name. Controller kind or Run mode alone
never infers the aggregate. Resolve and validate the named policy before
allocation and include its name, revision, and digest in idempotency
normalization. Preallocate one UUIDv7 for both Primary `run_id` and `session_id`,
plus a distinct Aggregate Bootstrap Operation ID used as the cross-store
correlation identifier. Commit the `creating` session and complete immutable
Specialist Policy snapshot in SQLite, then fsync the matching Run creation
intent carrying that operation ID and publish the empty root Run. Mark the
bootstrap `ready` and session `active` only after authoritative Run publication.
Exact same-key replay returns the same identities. Ambiguous cross-store
boundaries enter reconciliation and never allocate a replacement root. A root
without launch intent remains a low-level Run without aggregate membership.

For an External Specialist Engagement, use
`open_external_engagement` through the private External Specialist Facade as the
explicit aggregate-creation boundary. Dolgorae generates the engagement UUIDv7
and bootstrap-operation ID and commits an empty active engagement, immutable
external provenance, and an Aggregate Controller Binding containing public
Controller identity, generation 1, kind, normalized-principal digest, and
capability digest before returning. Exact retry returns the same aggregate identity. Opening does not
create a child Run. Every later facade call names the engagement and presents the same protected
aggregate-owner Controller credential; `hire_external_specialist` additionally
supplies a fresh per-Run Controller carrier outside model-visible payloads, and commits a separate write-ahead hire operation, member, and child
Run reservation before runtime side effects. The facade compiles each accepted
hire into the ordinary `managed_agent` Run core with the reserved presentation
parent. A raw `StartRun`, a reserved `parent_ref`, or a later Run listing never
implicitly opens, joins, or reconstructs engagement authority. Existing generic
Runs are not attached in place.

Expose Primary-to-Broker operations through the checked private run-bound
orchestration tool contract. Bind session, source Run, source Turn, tool-call
identity, inherited priority, Controller authority, and idempotency outside
model arguments. Resolve every requested role against an immutable,
schema-validated Specialist Policy snapshot. Under `user_approval_required`,
persist an approval-waiting spawn operation and one normalized Primary Run
interaction before child provisioning. Under `fully_delegated`, provision only
policy-admitted roles and access within cardinality and capability limits. The
tool never exposes a Specialist Controller capability.

Expose external engagement operations through a separate checked private CLI or
MCP payload contract. This facade owns explicit empty-engagement open, safe get,
write-ahead hire, task assignment, bounded wait, result collection,
cancellation, release, and close for Dolgorae's accepted Specialist boundary.
It never stores or executes the external AI's plan or task graph.

Use public Run projections, parent metadata, events, and Controller Interactions
as Gul's limited presentation surface only. Durable aggregate bootstrap,
membership, operation, delivery, and recovery state remains authoritative in the
orchestration database. A future additive typed aggregate query may improve
observability without changing ownership.

Place the live run-bound tool transport probe after the transport-independent
durable broker implementation and immediately before live collaboration-tool
integration. Earlier foundation work does not depend on that probe.

### Consequences

- Both user-facing use cases have one explicit, recoverable entry path without a
  public Protobuf change.
- An Orchestrated Session cannot be published without a matching bootstrap
  record, and recovery uses the original preallocated Primary identity.
- An external AI first opens one empty engagement idempotently, then hires each
  Specialist through a separate idempotent operation; Dolgorae returns stable
  server-generated aggregate and Run identifiers at their respective boundaries.
- Raw `managed_agent` Runs remain available as low-level primitives but do not
  accidentally acquire engagement semantics.
- Primary orchestration behavior, external facade behavior, and Specialist role
  limits have checked machine-readable payloads.
- Parent metadata remains safe presentation and provenance rather than a hidden
  authority channel.
- Full typed hierarchy observability remains an additive future extension.

### Rejected alternatives

- Add public aggregate RPCs before implementation: rejected because the current
  Gul contract is aligned and the semantic service can bootstrap the root behind
  existing `StartRun`.
- Infer engagement creation from a raw `StartRun` or presentation parent:
  rejected because creation authority and retry identity would be ambiguous.
- Combine engagement open and the first hire in one operation: rejected because
  aggregate creation and child provisioning have different retry, credential,
  and recovery boundaries, and an empty durable engagement gives the external
  controller a stable identity before any Run is created.
- Let the external client generate the authoritative engagement ID: rejected
  because Dolgorae owns the durable aggregate and idempotency namespace.
- Infer membership by grouping reserved `parent_ref` values or `ListRuns`:
  rejected because presentation metadata is not a transactional registry.
- Attach an existing generic managed Run to an engagement: rejected because it
  would retroactively change lifecycle and recovery ownership.
- Let the Primary call public Run mutations with Specialist credentials:
  rejected because it exposes authority and bypasses policy and broker state.
- Encode orchestration state in free-form diagnostics: rejected because
  presentation strings are not typed recovery contracts.

## ADR-029: Deliver Read-Only Specialist Review Before the Full Orchestration Stack

Status: Accepted

### Context

The highest-priority dogfooding need is an independent Specialist review from an
existing Codex CLI session. The previous roadmap delayed that capability until
writer authority, interaction handling, complete recovery, the Dolgorae Primary
control plane, and Brokered Hierarchy were all implemented. Those systems are
not required when the external Codex CLI already owns semantic orchestration and
needs only one read-only Reviewer.

A throwaway shell wrapper could deliver a quick demonstration, but it would
bypass the accepted External Specialist Engagement, Controller, idempotency,
audit, and result contracts and would need to be replaced later. Conversely,
requiring the later run-bound internal MCP probe would conflate an external AI
adapter with a tool invoked from inside a Dolgorae Run.

### Decision

Insert `EPIC-002A` immediately after the independent Run, Codex App Server,
thread, Turn, and Controller foundations. Its completion is the first usable
milestone, `MILESTONE-SR1`.

Implement the review slice on the final shared semantic Run core and a restricted
External Specialist Engagement. Support one independent read-only Reviewer,
one working-tree task, one immutable structured result, and explicit cleanup.
Provide both a one-shot Machine CLI command and one narrow external stdio MCP
tool named `dolgorae_review`. Bind workspace, Runtime Profile, Controller
credentials, provenance, and idempotency in the trusted adapter rather than
model arguments.

Do not require writer authority, user approval flows, Dolgorae Primary
orchestration, Brokered Hierarchy, internal run-bound source identity, mailbox
scheduling, or Specialist collaboration for this milestone. Unknown Turn
acceptance or outcome is fail-closed and never replayed. The Reviewer profile
must not contain the external review MCP adapter, and the semantic layer rejects
nested first-class hiring.

After the preview, harden External Specialist Engagements first, then implement
the transport-independent Dolgorae Orchestration Session and Brokered Hierarchy,
then integrate the live Primary run-bound tool, and finally add durable lateral
Specialist collaboration.

### Consequences

- Codex CLI can dogfood independent Specialist review as soon as `EPIC-002A`
  completes.
- The early feature is a real product slice, not a disposable alternate
  architecture.
- The external review MCP adapter is not blocked by the later run-bound
  transport probe.
- Read-only scope substantially reduces the safety and recovery surface of the
  first milestone.
- Long-lived reuse, writes, Primary orchestration, Brokered Hierarchy, and
  collaboration remain explicit later milestones with their own completion
  gates.

### Rejected alternatives

- Wait for the complete Orchestration Broker and Brokered Hierarchy: rejected
  because it delays the requested external review use case without adding value
  to that path.
- Implement a temporary shell-only Reviewer outside the engagement and Run
  contracts: rejected because it creates a second lifecycle, authority, and
  recovery model that must later be discarded.
- Reuse the internal run-bound MCP bridge for the external Codex CLI adapter:
  rejected because an external control plane has no Dolgorae source Run or Turn
  to bind and does not need that transport dependency.
- Allow the Reviewer to write fixes in the first milestone: rejected because it
  introduces writer coordination and host-write races before the review value
  has been validated.


## ADR-030: Establish the Supervised Gul Runtime Before the Brokered Hierarchy Milestone

Status: Accepted

### Context

The specialist-review-first roadmap correctly delivers the external read-only
Reviewer before the Dolgorae Primary control plane. A sequencing defect remained
later in the roadmap: `MILESTONE-BH1` claimed that Gul could operate a live
Dolgorae-Orchestrated Session at the end of the live Primary-tool Epic, while
`dolgorae serve`, the local gRPC adapter, and `ControlPlaneRuntime` ownership were
still assigned to a later operator-interface Epic. The aggregate core could be
proved through fake adapters, but Gul had no production transport through which
to start or observe that aggregate.

The frozen public-v1 descriptor contains more methods than the minimum BH1 path
needs. Moving the entire later observation/operator Epic ahead of hierarchy
work would unnecessarily delay the main control-plane milestone.

### Decision

Split the former gateway Task into two delivery stages without changing the
checked Protobuf source or descriptor.

`TASK-009-D1A`, before the durable Brokered Hierarchy core, implements the
foreground `dolgorae serve` process, private UDS and peer-UID boundary,
singleton record and lock, reconstructable `ControlPlaneRuntime`, pinned gRPC
code generation, and the checked minimum public-v1 method path needed by a live
Gul Orchestrated Session. It routes each implemented method to the shared
semantic service and advertises only those methods in capabilities.

`TASK-010-A` later completes the remaining descriptor methods and extended
observation/operator-safe conformance. Generated stubs may exist before a method
is implemented, but an unadvertised method is unavailable and must fail closed.
`MILESTONE-BH1` requires `TASK-009-D1A`, the durable hierarchy core, and live
Primary-tool integration. It therefore denotes actual Gul use, not a
Gul-shaped fake harness.

### Consequences

- The first Specialist Review milestone remains unaffected and does not wait for
  gRPC.
- The Brokered Hierarchy core can still be tested with fake adapters, but the
  live BH milestone has a real supervised client path.
- `GetCapabilities.supported_methods` becomes a stage-aware inventory rather
  than an unconditional copy of every descriptor method.
- The public descriptor remains byte-identical; capability advertisement, not
  descriptor mutation, expresses staged availability.
- Later timeline, diagnostics, advanced Run operations, writer
  handoff, deletion, verification, and full pressure coverage retain a separate
  completion gate.
- BH1 includes `ArtifactService.GetArtifact` and
  `ArtifactService.ReadArtifactChunk`; a real Gul client must retrieve at least
  one artifact-backed result before the milestone can complete.

### Rejected alternatives

- Downgrade BH1 to a Gul-shaped harness: rejected because the milestone is
  explicitly a user-usable Gul control plane.
- Move all of EPIC-004 before the hierarchy core: rejected because the minimum
  Run path is sufficient and the remaining operator surface does not determine
  hierarchy semantics.
- Advertise every descriptor method and return placeholder success: rejected
  because capabilities must describe implemented behavior, not generated
  stubs.

## ADR-031: Use Explicit Per-Request MCP Metadata for External Review Idempotency

Status: Accepted

### Context

The first Specialist Review roadmap derived idempotency from an MCP connection
and tool-call boundary. MCP 2026-07-28 is request-stateless: a server must not
infer task, conversation, or session continuity from a connection or stdio
process, and state spanning requests requires an explicit identifier on every
request. JSON-RPC request IDs correlate one in-flight response but do not prove
that two attempts represent the same logical review.

The review request identity must remain outside model-controlled arguments. MCP
provides per-request `_meta`, including vendor extension keys, for this purpose,
but the pinned Codex CLI must be proven to preserve the custom field on the
supported retry and reconnect paths.

### Decision

Add a checked host-controlled metadata fragment with key
`xyz.rootkernel.dolgorae/externalRequestRef` under `tools/call params._meta`.
The value is one UUIDv7 generated once per logical tool invocation. The model
cannot supply or override it. Replay-safe MCP mode requires the pinned host to
repeat the same value on every attempt. Dolgorae persists the value and
normalized request digest before opening the engagement; exact replay returns
the original review identity and changed input conflicts.

Add `TASK-006-E0` before the external MCP adapter. It selects exactly one of two
checked dispositions: replay-safe metadata or MCP unavailable. Replay-safe mode
requires the pinned host to preserve the same trusted reference across every
supported retry and reconnect boundary. If that behavior is not proven, SR1
exposes only the Machine CLI carrier and Codex CLI invokes it through its shell
tool. Same-reference input drift returns `IDEMPOTENCY_CONFLICT`; there is no
best-effort or connection-derived fallback.

Connection identity, stdio process lifetime, and JSON-RPC request ID are never
durable request identity.

### Consequences

- The CLI review milestone remains available even if the pinned Codex MCP host
  cannot transport custom metadata.
- A reconnect-safe MCP claim is empirical and version-pinned rather than
  assumed.
- Model-visible tool arguments remain free of workspace, credential, and
  idempotency authority.
- A future non-replayable carrier requires a stable host-visible submission
  receipt, durable request reference, review lookup operation, and unambiguous
  terminal-state query before it may re-enter the active roadmap.
- Unknown delivery is not converted into duplicate Reviewer execution.

### Rejected alternatives

- Use the MCP connection or process as a session: rejected by the protocol's
  stateless request model.
- Use the JSON-RPC request ID as a durable idempotency key: rejected because it
  is scoped to request/response correlation and need not survive a retry.
- Let the model provide an idempotency key: rejected because the model is not a
  trusted authority carrier.
- Silently retry without a stable identifier: rejected because it can create a
  duplicate Specialist Run after response loss.
