# Dolgorae Architecture Decisions

Status: Decision record for the first supported release. Each ADR carries its
own Accepted, Under Review, or Superseded status; this heading does not promote
an Under Review ADR to Accepted.

This document owns decision rationale. Each ADR describes the currently
accepted decision, not an append-only historical chain. If a decision changes,
edit its ADR in place and update every affected SOT document in the same change;
Git history preserves the prior text. Contradictory active ADRs are invalid.

## ADR-001: Ship One Binary Without an Installed Daemon

Status: Under Review for the fourth consistency pass

### Context

Persistent subagent sessions must outlive individual CLI invocations. Dolgorae
does not need an installed supervisor daemon, while Codex can efficiently share
one reader app-server among compatible sessions in the same account home.

### Decision

Ship one `dolgorae` executable. For every live run, re-execute that binary in a
hidden worker mode and attach a private WebSocket connection to the Run's
immutable shared-read-only or dedicated execution lane. A short-lived profile manager owns
singleton creation, epoch transitions, and membership reconciliation. Install
no Dolgorae launchd unit, global daemon, or
project daemon. Recover workers on demand after process loss or reboot. The
machine CLI is the sole v1 external-master transport; a future adapter must use
the same semantic service and cannot expose private worker sockets.

### Consequences

- Worker connection count grows linearly with live Runs; App Server count is
  one shared reader plus zero or more Run-owned dedicated process generations.
- Each run isolates its worker, connection, ledger, and controller state. A
  profile singleton failure and profile-wide operator action still affect every
  member of that launch contract.
- Idle runs consume processes until explicitly paused or closed.
- The binary still depends on external Codex profiles and their `CODEX_HOME`.

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

Status: Accepted; the former shared-reader/Writer-Capsule topology is
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
`.dolgorae/runtime/locks/` on the already-required local APFS workspace. An
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
shared lane and creates a lineage-linked dedicated successor if it needs write.
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

Status: Accepted

### Context

Users may have multiple independently configured profiles. Profile edits or
executable/argument/environment changes must not silently move an existing
thread between account homes or launch contracts.

### Decision

Store profile definitions in ignored project-local `.dolgorae/local.yaml`.
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
- Project-local profile edits affect only future runs unless an operator
  explicitly migrates the shared server contract.
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
Access-dependent developer instructions are generation-immutable. Explicit
writer acquire/release keeps the same worker, logical lane, and thread. It
applies and verifies the new policy inside the current dedicated generation, or
uses a same-lane successor generation only after exact absence and a durable
history barrier. A shared-readonly compatibility Run is never promoted in
place; it creates a fresh lineage-linked dedicated successor Run. Authority
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

Status: Accepted

### Context

Pure prompt passthrough does not reliably preserve Dolgorae's reserved storage,
write authority, Git publication, background-process, and reporting boundaries.
Profile configuration must nevertheless remain useful.

### Decision

Inject a strong generation-immutable developer-instruction prefix that defines Dolgorae's
master/subagent relationship, current access, and hard safety invariants. Append immutable
run-specific instructions as subordinate context. Continue to respect profile
AGENTS files, skills, plugins, apps, and checked MCP servers unless they conflict
with the hard invariants. Native subagents additionally require a profile
capability snapshot of `supported`; the corrected exact 0.147.0 enabled campaign
passes that gate. Its disabled diagnostic produced a child and remains
`unverified`, so disablement is not an availability proof.

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

## ADR-010: Keep Independent Runs Master-Orchestrated

Status: Accepted

### Context

Independent runs could technically invoke the CLI or connect to another worker,
but peer control introduces authority escalation, cross-account access, cycles,
unbounded fan-out, audit causality, and writer-authority deadlocks. Codex already
supports native subagents within a session.

### Decision

Use a hub-and-spoke model in v1. One capability-bound controller mutates each
independent run, while same-user observers may read its client-safe projection.
Dolgorae-managed agents may use Codex native subagents only when the profile
capability snapshot reports `supported`; they must not control another
Dolgorae run or connect to its socket. Defer any future inter-run feature as
capability-scoped hierarchical delegation, not peer messaging.

### Consequences

- Independent run authority and audit provenance remain clear.
- The master explicitly carries results between runs.
- A writer cannot deadlock by waiting on another writer it attempted to spawn.
- Supported native Codex subagents remain available for bounded parallel work inside a
  run.

### Rejected alternatives

- Arbitrary peer messaging: rejected because it permits cycles and unclear
  authority.
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

## ADR-018: Own Writer-Capsule Census and Expose Bounded Artifacts and Profile Diagnostics

Status: Accepted in its bounded-artifact and profile-diagnostic portions;
superseded in its Writer Capsule portion by ADR-019

### Context

Codex 0.147.0 has no thread-scoped background-terminal authority, and waiting
for an unspecified future interface would leave the core writer guarantee
undeliverable. Final answers and exact file-change snapshots can exceed the safe
inline machine boundary. Profile startup can also fail before a Run exists, so
run-scoped events cannot represent all actionable failures.

### Decision

Use an exclusive Writer Capsule App Server for each active writer generation.
Spawn it suspended in a new process group, persist full process identities and
exit observation before continuation, census every 100 milliseconds and on
command notifications, clean only exact revalidated identities, and require
five complete empty samples after leader exit. Treat malformed or incomplete
census, PID reuse, unregistered survivors, unreadable identity, and detected
escape as `unverified`. A native Codex terminal API is optional `hybrid`
evidence, never the authority. Codex 0.147.0 may become release eligible after
the same-home multi-server, shared-to-capsule thread-resume, census, cleanup, and
unrelated-process non-signalling campaigns pass.

Store only `file_change_diff` and `final_response` artifacts in the run-private
mode-0600 store. Bound a file diff at 8 MiB, a final response at 32 MiB, and a
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
- Failure of the Writer Capsule live campaigns is an explicit release blocker;
  a future Codex terminal API is not.
- Artifact and diagnostic retention/authorization require dedicated tests.

### Rejected alternatives

- Infer background absence from turn completion: rejected because terminal turn
  state does not prove process absence.
- Wait for a future Codex background-terminal API: rejected because Dolgorae can
  create and police an exclusive writer process boundary now, while an external
  release has no delivery commitment.
- Run all Codex instances with
  `--dangerously-bypass-approvals-and-sandbox`: rejected because it disables the
  approval/sandbox contract and does not create thread or process ownership.
- Put large values in the audit line or stdout envelope: rejected because it
  defeats bounded parsing and replay.
- Attribute profile startup failures to a synthetic Run: rejected because no
  ready server epoch exists to bind that Run.
- Make diagnostics operator-only: rejected because same-user clients need a
  safe explanation of profile failures; operational detail remains privileged.

## ADR-019: Use Sticky Dedicated Execution Lanes and Explicit Control Modes

Status: Accepted

### Context

The transient Writer Capsule candidate moved one persistent thread from the
shared reader server to a temporary writer server and back. Exact Codex 0.147.0
testing showed that `thread/unsubscribe` acknowledged subscription removal but
left the source thread loaded after two seconds, while another App Server
rejected resume as already having an active writer. The same campaign proved
same-home server initialization and catalog stability, read/write policy
changes on one server, concurrent writers in two workspaces, ten live idle
servers, and closed-generation history resume. Background-terminal discovery
returned no entries. A subsequent Dolgorae census/cleanup campaign passed. The
later native-subagent conclusion is unusable: its semantic result reported no
collaboration item while retained wire shapes contain `subAgentActivity` and
`collabAgentToolCall`. The corrected campaign must separate Codex-native child
threads from independent Dolgorae Runs and workers.

Purpose metadata also cannot define who controls a Run, how interactions route,
where its thread resides, or what process assurance the product claims.

### Decision

Use one profile shared-read-only lane and zero or more Run-owned Dedicated
Execution Lanes. Select `shared_readonly` or `dedicated` when creating the Run
and never change it. A dedicated thread remains in its logical lane for its
entire lifetime. Read/write policy and workspace authority may change within
that lane. A stopped physical server may be replaced only after exact absence,
complete process census, native-work quiescence, and durable-history proof; the
successor is a new process generation of the same lane. A shared Run needing
write creates a lineage-linked dedicated successor.

Keep writer authority per canonical workspace. The profile may have concurrent
dedicated writers in different workspaces. Separate effective policy, writer
authority, server-lane infrastructure, and background-workload state in durable
state and projections. Profile lifecycle enumerates the shared lane and all
dedicated lane journals; dedicated servers restart lazily.

Make `direct_interactive` and `managed_agent` immutable control modes, separate
from purpose and lane. Direct mode accepts `human_cli` or `interactive_client`
and defaults to a dedicated lane. Managed mode accepts
`workflow_orchestrator` or `automation` and requires explicit purpose and lane.
Controller kind `other` cannot bind a v1 Run. Only the Controller resolves full
interactions; observers are read-only and redacted. Controller credentials
remain outside every LLM-visible channel.

Advertise only `best_effort_personal_alpha` for Codex 0.147.0. Requested
assurance is checked before allocation. Polling and exact identity revalidation
remain the background-work authority, but do not claim adversarial containment.
Treat Codex-native `multi_agent` as a profile policy independent of Dolgorae
Run/worker concurrency. The selected default permits native subagents. The
corrected exact-version campaign proves child identity, parent, terminal
lifecycle, persisted history, restart continuity, and exact cleanup, so the
enabled profile reports `supported`. Operations still refuse active or unknown
native state when they require quiescence. The disabled case produced a child
and therefore reports `unverified`, not `unavailable`.

### Consequences

- Same-thread cross-server migration is eliminated from normal and recovery
  flows; transient Writer Capsule activation/release is removed.
- Write-capable Runs consume a dedicated App Server while active. Ten idle
  servers measured about 1.21 GiB RSS, so shared read-only remains available
  and paused dedicated servers stop after the absence barrier.
- Multiple workspaces retain concurrent writers without a profile-global
  downgrade.
- Codex-native descendants and independent Dolgorae Runs are different
  concepts. Native descendants may run, but active or unknown native state and
  goals block pause, generation replacement, profile stop, and shutdown.
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
