# Dolgorae Architecture Decisions

Status: Current accepted decisions for the first supported release.

This document owns decision rationale. Each ADR describes the currently
accepted decision, not an append-only historical chain. If a decision changes,
edit its ADR in place and update every affected SOT document in the same change;
Git history preserves the prior text. Contradictory active ADRs are invalid.

## ADR-001: Ship One Binary Without an Installed Daemon

Status: Accepted

### Context

Persistent subagent sessions must outlive individual CLI invocations. Dolgorae
does not need an installed supervisor daemon, while Codex can efficiently share
one app-server daemon among compatible sessions in the same account home.

### Decision

Ship one `dolgorae` executable. For every live run, re-execute that binary in a
hidden worker mode and attach a private proxy connection to the profile-scoped
Codex app-server singleton. Install no Dolgorae launchd unit, global daemon, or
project daemon. Recover workers on demand after process loss or reboot.

### Consequences

- Worker/proxy process count grows linearly with live runs; app-server count
  grows only with active profiles.
- Each run has isolated ownership and failure scope.
- Idle runs consume processes until explicitly paused or closed.
- The binary still depends on external Codex profiles and their `CODEX_HOME`.

### Rejected alternatives

- One Dolgorae global daemon: rejected because it centralizes project state and
  creates a mandatory installed service.
- One project daemon hosting many threads: rejected because one crash or upgrade
  affects every run and complicates profile isolation.
- A purely foreground CLI: rejected because turns and sessions would die with
  the invoking process.

## ADR-002: Use a Worker Unix Socket and a Private App-Server Proxy

Status: Accepted

### Context

The master needs reconnectable local control, while app-server traffic must be
fully correlated and audited by one owner. App-server supports stdio, Unix
socket, and experimental WebSocket transports.

### Decision

Use a user-private Unix domain socket between transient Dolgorae CLI invocations
and the per-run worker. Give each worker a private stdio JSONL connection through
`app-server proxy` to the profile singleton. The master never connects directly
to app-server.

### Consequences

- The worker is the only protocol client and audit interposer.
- Socket paths live under a fixed short user-private `/tmp/dolgorae-<uid>/` root
  and their actual location is recorded in workspace runtime state; discovery
  does not depend on the caller's `$TMPDIR`.
- A worker recreates an accidentally deleted private socket path and advances a
  persisted socket epoch; a foreign replacement fails closed.
- Worker loss ends only its proxy connection; recovery validates the singleton
  epoch and opens a new run generation.
- WebSocket instability and port management are avoided.

### Rejected alternatives

- Direct master-to-app-server socket: rejected because it bypasses Dolgorae state,
  writer policy, idempotency, and audit.
- Direct shared-socket consumption by workers: rejected because every worker
  would need to demultiplex and authorize unrelated frames.
- TCP/WebSocket: rejected because it is experimental, needs authentication and
  port management, and expands the attack surface.

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
thread. Restarting a worker/proxy connection changes run generation, not run or
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

## ADR-004: Operate in the Canonical Workspace With One Dolgorae Writer Lane

Status: Accepted

### Context

Automatically created dedicated worktrees or directory copies change the
user's selected workspace and require a separate merge/publication workflow.
Operating directly in the caller-selected canonical workspace is simpler but
simultaneous writers within that worktree conflict.

### Decision

Run Codex in the canonical workspace selected by the caller. A linked Git
worktree is an independent canonical workspace and supported parallel writer
lane. Allow concurrent readers and at most one Dolgorae writer app-server per canonical worktree,
coordinated by a nonblocking BSD `flock(2)` held by the worker across starting,
idle, running, and waiting writer states and released on start failure. The
canonical identity is domain-separated SHA-256 of libc `realpath(3)` bytes with
no extra case/Unicode folding; sockets and both locks reuse that digest. The
lease is close-on-exec and is never inherited by app-server. Unknown quarantine
is lease-free. Permanent lock pathnames live below
`.dolgorae/runtime/locks/` on the already-required local APFS workspace. An
unverifiable generation blocks same-thread recovery. A stale foreign-run
`writer.json` does not override the kernel acquisition attempt, but it gates
workspace writer activation, including effective-write new-run start, until its
generation is proven absent or cleaned. Writer/startup lock paths are permanent.
V1 provides no force override. Allow dirty workspaces and record their start baseline. Provide
no transactional rollback.

### Consequences

- Writer changes are immediately visible to the user and readers.
- Interrupted work may leave partial changes.
- Readers have no snapshot isolation and may see an intermediate state.
- The lease coordinates Dolgorae workers only; editors and external tools remain
  outside its guarantee.
- Native Codex subagents remain inside the owning reader or writer run; Dolgorae
  does not serialize their internal execution lanes.
- For a verified but wedged current writer, same-run recovery first serializes
  through a run-keyed election, revalidates and terminates the worker outside a
  possibly-held startup lock, and confirms exit so its flock is released. The
  POSIX startup lock exposes its owner through `F_GETLK`; an exact wedged owner
  may be terminated and all contenders then compete for the lock. Only the
  winner acquires the workspace lease, validates and removes the prior
  proxy generation, and starts a new proxy generation after cleanup is confirmed.
- Startup handoff uses two POSIX byte ranges because record locks are not
  inherited across fork: the CLI owns byte 0 until a re-exec worker owning byte
  1 has bound and persisted identity. Runtime ownership is never inferred from
  an inherited lock.
- Lease identity is the held descriptor's device/inode pair and is rechecked
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

## ADR-005: Snapshot Profile Identity and Use CODEX_HOME as Account Boundary

Status: Accepted

### Context

Users may have multiple independently configured profiles. Profile edits or
wrapper changes must not silently move an existing thread between account homes.

### Decision

Store profile definitions in ignored project-local `.dolgorae/local.yaml`.
Snapshot profile name, argv,
and expected `CODEX_HOME` into each run. Set that home explicitly and reject an
`initialize` response whose `codexHome` differs. Never rebind a run or fork
across profiles.

### Consequences

- Existing runs remain bound to the account that created them.
- Project-local profile edits affect only future runs.
- Executable updates at the same path require generation-time compatibility
  validation but do not change the expected home.
- Dolgorae does not install, update, or authenticate Codex.

### Rejected alternatives

- Resolve the profile name on every resume: rejected because a registry edit
  could silently change account identity.
- Store profile credentials or arbitrary secret environment variables: rejected
  because authentication belongs to Codex and local wrappers.
- Permit cross-profile fork: rejected because the source thread is not
  authoritative in the destination `CODEX_HOME`.

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
Access-dependent developer instructions are generation-immutable; idle
promotion/demotion keeps the same worker and startup-lock ownership while
replacing only its proxy generation, then supplies a recomposed prefix
through `thread/resume` because `turn/start` has no such field. Promotion holds
the writer lease before stopping the reader child; demotion releases it only
after the writer child is gone and the reader child is active.

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
Keep the 16 MiB cap for unsolicited stdout because it protects the active
protocol stream from unbounded frames. Treat solicited `thread/read` specially
only after its matching top-level ID appears within that prefix; this is a live
compatibility predicate. Then use a constant-memory, deadline-bounded visitor
with no arbitrary total response cap. Never infer classification from one
outstanding request.

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

If worker/proxy dies during a turn, filesystem mutations may have occurred
even when Dolgorae did not receive the terminal response. Automatic replay could
duplicate destructive or external side effects.

### Decision

On recovery, accept a turn outcome only when persisted Codex history proves a
terminal state. Otherwise stop the proxy, release any writer lease, set
`outcome_unknown`, block new turns, and allow only inspection, evidence-based
reconciliation, fork, or close. Never replay the input automatically. Fork only
through the newest status that the checked profile manifest proves acceptable as
`lastTurnId`; terminal-but-rejected statuses are skipped. Successful later reconciliation
moves the run to `paused`; explicit resume selects its next access mode.
Reconciliation uses a transient read-only app-server and `thread/read` without
loading or resuming the thread. If prior process identity is unverifiable, v1
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
  app-server may still own the Codex thread and workspace process group.

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
AGENTS files, skills, plugins, apps, MCP servers, and native subagents unless
they conflict with the hard invariants.

### Consequences

- Every turn receives consistent governance.
- Read and write authorization derives from both request intent and run access.
- `.dolgorae`, Git publication, external effects, and background processes receive
  explicit treatment.
- Access changes replace the proxy generation so prefix and sandbox agree.
- Native subagents are instructed not to overlap write-heavy delegation.
- Prompt policy is defense in depth, not a hostile security boundary.

### Rejected alternatives

- Minimal passthrough: rejected because critical product invariants would be
  implicit and easy to violate.
- Disable profile tools and native subagents: rejected because it would make
  Dolgorae less compatible with the user's prepared Codex environments.
- Mutable run instructions: rejected because they would weaken reproducibility
  and make past turn governance ambiguous.

## ADR-010: Keep Independent Runs Master-Orchestrated

Status: Accepted

### Context

Independent runs could technically invoke the CLI or connect to another worker,
but peer control introduces authority escalation, cross-account access, cycles,
unbounded fan-out, audit causality, and writer-lease deadlocks. Codex already
supports native subagents within a session.

### Decision

Use a hub-and-spoke model in v1. Only the master controls independent Dolgorae
runs. Dolgorae-managed agents may use Codex native subagents but must not control
another Dolgorae run or connect to its socket. Defer any future inter-run feature
as capability-scoped hierarchical delegation, not peer messaging.

### Consequences

- Independent run authority and audit provenance remain clear.
- The master explicitly carries results between runs.
- A writer cannot deadlock by waiting on another writer it attempted to spawn.
- Native Codex subagents remain available for bounded parallel work inside a
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
also split the per-user writer lease across hosts.

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
an independent Python stdio subprocess owned by TASK-004 and driven by
manifest-validated declarative scenarios.

Project configuration uses pinned `serde_yaml_ng` 0.10 behind typed adapters
that reject duplicate and unknown keys. YAML values are never used as untyped
protocol or ledger input.

### Consequences

- Duplicate rejection and number preservation occur at ingest, before JCS.
- Fake/production parser diversity reduces self-confirming conformance tests.
- TASK-006 consumes the TASK-004 fixture rather than creating a second core.

## ADR-015: Share One App-Server Singleton Per Canonical Profile Home

Status: Accepted

### Context

App-server can host multiple threads and workspaces, while spawning a complete
server for every run duplicates model/account state and complicates coordinated
profile lifecycle. Distinct account homes must still remain isolated.

### Decision

Key one Dolgorae-exclusive app-server singleton by canonical `CODEX_HOME` and
the checked executable/compatibility snapshot. Connect every run through an
exclusive worker-owned proxy connection. Track `server_epoch` globally and
`run_generation` per worker/proxy policy lifetime. Reject an incompatible live
snapshot with `PROFILE_SERVER_BUSY`; never fall back to a per-run server.

Maintain a minimal recoverable XDG membership index so profile-wide stop and
restart can enumerate runs across projects. An interrupting restart pauses all
members and never resumes them automatically.

### Consequences

- Multiple workspaces and sessions share profile-level Codex state without
  sharing worker control, audit ownership, or JSON-RPC frames.
- A singleton failure affects one profile, while different canonical homes use
  independent servers.
- Profile names are aliases; two names resolving to the same canonical home
  cannot create competing servers.

### Rejected alternatives

- Per-run app-server children: rejected because they duplicate a server that is
  designed to multiplex threads and workspaces.
- One singleton across all profiles: rejected because it crosses the
  `CODEX_HOME` account boundary.
- Direct worker access to one mixed socket: rejected because it weakens frame
  isolation and makes every worker a global demultiplexer.
