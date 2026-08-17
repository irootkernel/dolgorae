# Dolgorae Architecture

Status: Normative target architecture for the first supported release.

This document owns technical structure and invariants. It describes the system
Dolgorae is required to implement; it does not claim that the currently empty
repository already implements it. Product behavior is owned by
[specs.md](specs.md), rationale by
[architecture-decisions.md](architecture-decisions.md), and implementation
progress by [roadmap.md](roadmap.md).
Document roles and the required synchronization procedure are defined by the
[documentation authority map](README.md).

## System Context

Dolgorae is the sole supported local process supervisor and protocol adapter
between external masters and Codex app-server. It adds durable run identity,
controller authorization, account binding, access coordination, recovery, and
audit around Codex threads without replacing Codex conversation storage.

```text
Gul Go core / local automation
          |
          +-- Machine CLI: stable JSON/JSONL
          |
          `-- public gRPC: HTTP/2 over user-private Unix socket
                         |
                  semantic service
                         |
                   per-run worker
                    |          |
               audit ledger    | private WebSocket/UDS
                               v
             +-- shared read-only Profile Server -> Codex services
             `-- Run-owned Dedicated Server lane -> Codex services
```

The master is the only orchestrator of Independent Dolgorae Runs. A trusted
external automation broker may accept a parent model's subagent request, but the
broker remains the child Run's master and Controller and invokes only a public
adapter. The requesting model receives no Controller capability and never
controls the child directly. A profile may
permit Codex-native subagents before full lifecycle verification, but active or
unverified native state blocks every quiescence-requiring transition. Those
children remain within one Codex session tree and are never Dolgorae peer Runs,
workers, or Dedicated Lane Servers.
The 0.147.0 production initialize contract fixes
`optOutNotificationMethods:[]`. Lifecycle or correlation suppression
invalidates native support and fences every quiescence-dependent operation;
reasoning content is redacted after receipt instead.

The v1 public boundary contains the Machine CLI and optional supervised local
gRPC gateway. Both call one semantic service. The gateway is a delivery adapter,
not a state owner or worker supervisor. Worker sockets and App Server transports
remain private and inaccessible to Gul.

## Component Model

### Shared Semantic Service

The semantic service owns every public operation independently of wire format.
It accepts validated domain requests, performs Controller and Operator
authorization at the existing serialization points, coordinates workers and
durable repositories, and returns domain results or typed Dolgorae errors. The
Machine CLI converts those results to closed JSON envelopes; the gRPC gateway
converts them to Protobuf messages and typed gRPC status details. Neither
adapter may duplicate state transitions, idempotency normalization, writer
policy, recovery, redaction, or projection rules.

### CLI Front End

The visible `dolgorae` invocation is short-lived. It:

1. resolves the canonical workspace;
2. parses and validates machine-oriented input;
3. validates a credential carrier and transfers its fd for a mutation;
4. resolves the explicit run ID and performs only preliminary authorization;
5. discovers or starts the owning worker;
6. exchanges one request/response with the worker, or reads the fsynced
   projection directly for projection-only `events`;
7. emits the stable stdout envelope and exits.

It never talks directly to app-server and never writes the audit ledger while a
worker owns the run. Start-time bootstrap is the only period in which the
front-end may create the run directory and initial records before worker
ownership transfers.

### Public gRPC Gateway

`dolgorae serve --socket <absolute-path>` is an optional foreground re-execution
of the same binary. It is started and supervised by Gul or another trusted
same-user client and never installs a launchd unit. It binds only the supplied
Unix socket, checks every accepted connection with the platform peer-credential
API, and offers unary operations plus Run-scoped event streams. The gateway
serves multiple workspaces, but every request after the initial
`InspectWorkspace` bootstrap supplies an absolute workspace path and expected
workspace ID; bootstrap inspection accepts the absolute path alone and returns
the calculated identity. There is no global in-memory Run registry.

The gateway holds the installation-scoped Application Support
`Dolgorae/rpc/gateway.lock` for its lifetime and publishes `gateway.json` with
boot UUID, PID/start identity, binary digest, socket path/inode, server instance
ID, and protocol range. The lock is never acquired by an ordinary semantic
operation and therefore is outside the global operation lock hierarchy. A
second gateway returns `RPC_SERVER_ALREADY_RUNNING`.

Socket traversal is descriptor-relative and no-follow. The supplied path must
be absolute, its existing parent must be a current-uid-owned mode-0700 directory,
and the new node is mode 0600. Symlinks, non-socket collisions, foreign nodes,
unsafe permissions, and stale nodes not bound to the exact prior record return
`RPC_SOCKET_UNSAFE`. A graceful shutdown stops new calls, drains admitted unary
calls for at most five seconds, terminates open streams with
`SERVER_SHUTDOWN`, and unlinks only the inode it bound.

The ownership split is strict: Gul may create and validate the private parent,
choose an unused pathname, launch the process, and verify readiness. Dolgorae
alone owns the singleton lock/record, bind, node mode, stale proof, unlink, and
graceful cleanup. A client never unlinks the provider socket, including after a
failed start. `RPC_SOCKET_UNSAFE` instructs the client to fix or replace its
private socket parent/path before a new attempt; gateway restart with unchanged
unsafe inputs is not a remediation.

The gateway uses a bounded `tokio` runtime only to operate tonic HTTP/2, UDS
acceptance, cancellation, and per-stream delivery. Blocking semantic operations
enter a bounded worker pool. Each event stream has an independent queue limited
to 32 envelopes or 4 MiB and five seconds of stalled delivery. Pressure closes
only that stream with `SLOW_CONSUMER`; it never blocks ledger append, App Server
draining, another Run stream, or an active turn.

Gateway loss has no worker, Run, writer, or App Server lifecycle consequence.
An admitted mutation may continue after its caller loses the response, so the
client applies the operation's idempotency or reconciliation contract rather
than interpreting connection loss as failure. A new gateway reconstructs
projections from authoritative workspace state and resumes streams from the
client's durable cursor.

The adapter maps shared semantic DTOs into typed Protobuf projections. Run,
writer, policy, assurance, recovery, interaction, lineage, capability, and
required-action states use closed enums/structures. Full Controller Interaction
payloads use a typed `oneof`; only the protected response remains bounded JSON.
Run/Writer/Interaction snapshots and events carry a common revision stamp so a
client never combines incompatible aggregates to enable a mutation. Public
filesystem output uses a UTF-8/opaque-byte path `oneof`, and capability blockers
use a closed code enum. Durable event delivery uses a typed event `oneof`;
heartbeat and stream-end variants are non-durable and do not consume cursor
values. No business decision depends on parsing diagnostic text or private
worker state.

The adapter preserves `recognized_unsupported` as a distinct Interaction
support value. Profile model normalization rejects duplicate IDs, duplicate or
empty effort tokens, and zero or multiple defaults before either adapter emits
a profile; `ModelCapability.is_default` is the only default-model source.

### Per-Run Worker

The worker is a hidden re-execution mode of the same `dolgorae` binary. It is the
sole owner of:

- the run control socket;
- the run's private direct WebSocket connection to its immutable shared or
  dedicated execution lane;
- JSON-RPC request IDs and correlation state;
- run lifecycle and pending interactions;
- the audit append handle and materialized state;
- participation in durable writer-authority transactions;
- cleanup of its own connection and worker process, plus participation in its
  exact recorded dedicated-lane generation cleanup.

The worker does not own the shared singleton process or another run's App
Server descendants. It owns dedicated-lane lifecycle only through durable
staged records and exact census identities; the profile manager performs the spawn
transaction. Connection loss, worker loss,
`turn/interrupt` request, terminal turn evidence, singleton loss, and
background-execution absence are separate facts and are never inferred from one
another.

One worker serves one run. There is no shared supervisor or global in-memory
registry. Concurrent start/recovery attempts for the same run are serialized by
a per-run startup lock.

The worker is detached from the transient CLI before it accepts requests. It
runs through the hidden `__worker` argv mode after a single fork and `setsid()`,
with no controlling terminal, null stdin/stdout/stderr, and `umask(077)`. It
ignores terminal-originated `SIGINT` and `SIGHUP` for itself and handles
`SIGTERM` as a bounded shutdown request. Early-start and internal diagnostics
go to a 0600 log capped at 1 MiB with one rotation. The CLI holds byte 0 of the
run's startup lock before fork. As its first post-`setsid`/re-exec operation,
the worker opens that lock once, acquires byte 1, and never closes or reopens
the descriptor while serving. Startup fd 3 may emit `bound` only after byte 1
is held, the socket is bound, and the runtime identity is atomically persisted
and directory-fsynced; the CLI then releases byte 0. A later `ready` object
means replay and compatibility validation completed. A structured failure may
replace either acknowledgement. The bound wait is ten seconds; the ready wait
is the larger operation-specific startup budget. EOF before an expected object
is `TRANSPORT_FAILURE`; timeout never authorizes signalling the worker. The CLI
parent keeps byte 0; the child's inherited byte-0 startup-lock fd is
`FD_CLOEXEC` before `__worker` re-exec and is not fd 3. Startup status fd 3 is
explicitly preserved across that re-exec. The worker
opens one new startup fd for byte 1 and marks both it and fd 3 `FD_CLOEXEC`
before opening the App Server connection.
The CLI creates no thread before fork. Between fork and re-exec the child
performs only async-signal-safe operations; worker threads are created only
after re-exec.
These rules make ownership handoff explicit and ensure
that Ctrl-C or command substitution cannot terminate the worker or keep the
caller's output pipe open.

### Historical Transient Writer-Capsule Candidate (Superseded)

This section is retained to explain the evaluated candidate. It is
non-normative and superseded by **Sticky Execution-Lane Topology** below.

The profile manager is lock-serialized logic in the Dolgorae executable, not a
resident Dolgorae daemon. It computes a launch contract from canonical
`CODEX_HOME`, the absolute direct Codex executable and checked global argv,
resolved executable identity, sanitized explicit environment, deterministic
symbolic `profile_state_directory_v1` cwd policy, normalized process-static configuration, version, schema,
and feature digests. Runtime-mutable configuration is observed but excluded
from the key. Compatible profile names are aliases for one `server_key`.
Different stopped definitions for one canonical home may coexist, but a
different contract cannot start while another verified lifetime is active.

The Runtime Profile supplies deterministic `PATH`, `LANG`, and `LC_ALL`; caller
shell, virtual-environment, and locale state is never inherited. The concrete
launch directory is derived only after `server_key` is known and is not itself
hashed into that key, avoiding a fixed-point identity. Profile start, stop,
restart, migration, and repair are PREPARE/APPLY/COMMIT transactions: locks
protect only revision-bound intents and commits, while spawn, network, policy,
process, and user waits occur in APPLY with no file lock held.

The manager launches the validated executable followed by `app-server --listen
unix://<dedicated-socket>`. It never uses the official daemon or default Codex
control socket. Shared-read-only workers connect to the shared Profile Server.
A dedicated Run first owns only a durable logical lane; its first input lazily
starts that lane's physical App Server with the identical immutable launch
contract and canonical `CODEX_HOME`, a distinct short socket, UUIDv7 lane ID,
process generation, globally unique server epoch, process group, and log
drainer. Each run worker performs HTTP Upgrade and becomes a distinct masked
WebSocket client over the appropriate socket. The adapter handles
text/continuation frames, ping/pong, close, size limits and invalid frames, then
hands normalized JSON-RPC objects to the existing correlation layer. The
`app-server proxy` command is not part of the supported v1 topology because it
preserves WebSocket framing while adding another process.

Dedicated lane sockets use `/tmp/dolgorae-<uid>/c/<compact-hash>.sock`; the hash is the
first 160 bits of domain-separated SHA-256 over the full server key, lane ID,
and process generation. Internal state binds the compact name to the full preimage,
path, device/inode, and process identity. Machine projection exposes only the
resulting SHA-256 identity digest.

Each connection performs its own `initialize`/`initialized`. A newly created
run remains threadless until first turn; first input uses `thread/start`,
recovery uses `thread/resume`, and history fork uses `thread/fork`. The worker
owns only that client connection, one thread binding, correlation, lifecycle,
audit and interactions. The connected App Server owns turn execution, commands,
native subagents and its process tree. A narrowly tested user-input connection
may advertise `experimentalApi`; no other experimental feature follows.

The profile manager spawns the singleton suspended in a new process group.
Its `posix_spawn` attributes use `SETSIGDEF` for every catchable signal and
`SETSIGMASK` with an empty mask so caller signal state does not leak into Codex;
there is no child-side callback. The manager
records the direct executable identity before continuation, and publishes it
only after WebSocket compatibility validation. It owns the singleton process
identity and epoch. The singleton uses null stdin and sends stdout/stderr to a
profile-scoped Dolgorae log-drainer in its process group; it never inherits a
CLI or command-substitution pipe. The drainer applies diagnostic redaction and
maintains mode-0600 `server.log` and `server.log.1` at 1 MiB each. Its exact
identity is part of profile state, and loss fences new attachment until a
controlled restart. Run workers neither signal the singleton nor its drainer.
The profile manager owns their lifecycle. A dedicated worker requests lazy
lane-generation startup through the same staged manager logic and owns only
that generation's exact recorded lifecycle; it never signals an unrelated
process or the shared server.

### Sticky Execution-Lane Topology

The current architecture has one shared read-only server lane and zero or more
Run-owned dedicated logical lanes per profile. Lane choice is immutable. A
shared Run's thread is never loaded by a dedicated server; a dedicated Run's
thread is never loaded by the shared server or a different dedicated lane.
Read/write policy changes and workspace writer acquisition occur within one
dedicated process generation. A shared Run that later needs write creates a
lineage-linked dedicated write continuation.

Each dedicated lane has a UUIDv7 lane ID, append-only process-generation
journal, globally unique server epochs, short socket identity, exact leader and
log-drainer identity, and process census. A new Run publishes the logical lane
with null thread and absent physical server; first input starts its initial
generation. Its physical server may also be absent while the Run is paused.
Resume starts a new generation only after exact old
generation/descendant absence, five complete empty samples, no active or
unknown turn/interaction/native descendant, and a durable-history barrier.
The thread then resumes in the same logical lane. Infrastructure state,
workspace writer authority, effective Codex policy, and background workload
state are four independent facts.

One canonical workspace has at most one writer. A profile may have concurrent
dedicated writers in different workspaces; the home coordinator serializes
launch contracts, not workspace writer cardinality. Profile stop/restart and
migration enumerate the shared lane plus every dedicated-lane record. Restart
brings back the shared server and starts dedicated generations lazily on Run
resume.

`control_mode` is independent of `purpose` and lane. Direct interactive Runs
are controlled by a human CLI or interactive client and default to dedicated.
Managed Runs are controlled by an orchestrator or automation broker and must
state purpose, lane, and assurance. Purpose and its optional creation label are
immutable. Only the Controller sees and resolves full normalized interactions
through `run interaction get`; observers receive strict summaries without
payload, response-schema, artifact, thread, turn, item, or server identity. No Controller capability
enters LLM-visible data.

A brokered independent subagent is an ordinary `managed_agent` Run with an
`automation` Controller, a Dedicated Execution Lane, and opaque parent
provenance. It uses the existing credential-create, run-start, input,
observation, interrupt, writer, and close operations rather than a second
orchestration state machine. The broker owns the child credential and carries a
bounded safe result back to the parent. Broker or parent disconnection does not
imply child termination or writer release. A later MCP adapter may wrap this
composition, but it may not change its authorization or lifecycle semantics.

Instruction composition is immutable and versioned as common safety prefix,
control-mode prefix, purpose prefix, then bounded Controller instructions. A
shared Run forces Codex Plan Mode on every turn, read-only sandbox, disabled
network, and never-approve policy. Its prefix permits bounded temporary-directory
validation but forbids workspace mutation and long-lived background work. The
shared server records command items and an aggregate process census, but cannot
attribute or clean descendants per Run; only profile stop owns aggregate
cleanup. Dedicated lanes retain exact per-generation census ownership.

`run create-write-continuation` is separate from history fork. It accepts a
shared-readonly source or a dedicated reader whose write-policy transition is
unavailable or unverified. The source Controller authorizes a current-terminal-
Turn transition, while a new same-principal credential binds the destination.
Workspace, profile, and control mode are fixed; model/effort, purpose,
capability additions, and non-decreasing assurance are validated before
allocation. Common/mode/purpose instructions are recomposed and only explicit
bounded destination instructions are appended. The operation records immutable
lineage, creation reason, and a bounded handoff digest, then publishes a
dedicated logical lane with no thread or physical generation. First input starts
the destination generation. Source lane, writer authority, recovery state,
Controller instructions, reasoning, and hidden native-subagent history remain
unchanged and are never copied.

Codex 0.147.0 achieved only `best_effort_personal_alpha`. Basic same-home
coexistence and bounded storage integrity passed under the tested configuration;
long-duration/high-contention behavior and forced authentication refresh remain
unverified, and no production-grade storage guarantee is claimed. Sticky
policy transitions,
sticky policy transitions, different-workspace concurrent writers, and
closed-generation history resume passed. Cross-server same-thread migration
failed and background-terminal discovery failed. Dolgorae's live process census
and exact cleanup subsequently passed, including unrelated-process exclusion.
The retained native-subagent parser omitted exact wire item names and its
no-child conclusion is withdrawn. The corrected 0.147.0 enabled campaign proved
the complete parent/child lifecycle and restart history, so the public profile
advertises supported lifecycle observation and quiescence tracking. Disable
enforcement is unavailable: a `disabled` public profile is rejected, while the
diagnostic disabled result remains `unverified`. Polling and persisted child history fence
pause, generation replacement, profile stop, and shutdown when native state is
active or unknown. Polling remains
process-census authority and cannot claim strong containment.

### Profile Registry and Singleton Membership

`.dolgorae/local.yaml` is project-local and stores the named profile launch
definition, including explicit non-secret environment values but no credential.
The platform Application Support root contains a canonical-home coordinator at
`Dolgorae/homes/<home-key>/{home.lock,active.json}` and contract state at
`Dolgorae/profiles/<server-key>/{server.lock,state.json,membership.jsonl,members.json,epoch,server.log,server.log.1}`.
All components are current-uid-owned mode 0700/0600 and descriptor-relative.
The socket node uses the validated short path
`/tmp/dolgorae-<uid>/p/<base32-first-160-server-key-bits>.sock`; its full path
and device/inode are recorded in profile state.

Profile state records the restorable immutable launch snapshot, accepted
generation contracts, process/executable/log-drainer/socket identity, lifecycle,
compatibility, migration/quiesce revision, epoch, start time and membership
revision. The
hash-chained, directory-fsynced `membership.jsonl` is authoritative;
`members.json` is an atomic derived snapshot bound to its revision/checksum.
Membership records workspace/run/controller, worker generation, thread,
connection, lifecycle, writer, observed epoch and runtime locator. Startup
replays the journal, validates every referenced manifest/runtime record, and
rejects missing/corrupt/revision-mismatched history; it does not rebuild by
scanning incidental project directories. Operator repair verifies the valid
prefix and exact confirmed orphan and appends a tombstone/new revision. A
startup transaction holds
home-keyed `home.lock` before contract-keyed `server.lock`, validates or claims
the sole `active.json` contract, then validates or reserves a monotonically higher epoch,
persists state, registers and fsyncs membership, connects and initializes the
worker, then publishes its generation ready. A new process always consumes a
new epoch; reconnect does not.

Stop/restart uses fence, unlocked quiesce, and commit phases. Fence persists a
quiesce revision under operator/home/server locks and rejects new work; no lock
is held while turns or processes are awaited. Commit revalidates the same
revision and exact identities, then proves process-group, log-drainer, and
socket-inode absence before clearing state. Restart invalidates old connections
and forces cross-epoch member reconciliation. Corrupt, missing, or unverifiable
membership blocks the operation; an apparently empty partial index never
authorizes termination.

Server-key migration is an operator-only home transaction. Old and new server
locks are acquired in ascending decoded-key order. A home-authoritative
migration record prevents double membership, and failure before new ready
retains old membership or lands in `migration_blocked` when rollback cannot be
proved.

The global order is operator, home, server keys in binary order, handoff,
writer, run startup locks in UUID-byte order, then in-process run mutation
mutexes in the same order. Operations persist a revision-bound intent and drop
file locks before process, network, turn, or user waits; no path acquires upward.

### Persistent Run Store

The run store is workspace-local and ignored by Git. `audit.jsonl` is the only
event authority. `state.json`, transcripts, status views, and exports are
projections. The Codex thread remains independently stored in the pinned
`CODEX_HOME`.

| Concept | Sole authority |
| --- | --- |
| Profile launch contract and accepted migrations | Immutable profile snapshot and accepted generation contracts |
| Shared singleton process, log drainer, and server epoch | Profile manager `state.json` under home/server serialization |
| Dedicated logical-lane generation identity, server epoch, census, and log drainer | Run lane-generation record under writer/run serialization |
| Membership | Append-only profile `membership.jsonl` |
| Run lifecycle and active turn intent | Run audit ledger reconciled with App Server history |
| Codex root thread | Run thread binding |
| Controller | Run controller record and generation |
| Workspace writer | Durable workspace `writer.json` |
| Interaction | Run interaction journal |
| Client event | Schema-validated durable event record in the run ledger |
| Projection/replay metadata | Delivery-time envelope |

Derived indexes, runtime locators, sockets, and materialized projections never
override these authorities.

Profile operations have their own bounded diagnostic journal and cursor because
startup can fail before any Run exists. Its minimal same-uid projection contains
only redacted status/code/message; operator-authorized operational projection
may add bounded redacted detail. A Run directory, ID, and audit genesis are
published only after profile state commits a ready non-null server epoch.

The run-private artifact store is a bounded projection adjunct, not an event
authority. It accepts only exact file-change diffs and final responses, writes
create-exclusive mode-0600 files, records byte length and SHA-256, and enforces
8-MiB/file, 32-MiB/final-response, and 256-MiB/run quotas. Public reads use
opaque artifact IDs and verified base64 chunks of at most 1 MiB. Inline final
responses are at most 1 MiB. Client presentation and download limits may be
stricter but never enlarge provider bounds; complete downloads verify both
length and digest. Artifact
metadata carries `observer` or `controller_only` visibility; interaction-derived
artifacts are controller-only. Internal paths and reasoning content never cross
the machine boundary.

The manifest stores controller metadata, a domain-separated capability digest,
controller generation, accepted profile/model, closed purpose and optional
label, parent metadata, required/validated capabilities, instruction-contract
versions, normalized Controller-instruction length/digest, and the initial and
current default effort. These facts reconstruct `RunConfigurationProjection`
after restart; workspace/profile defaults never overwrite an existing Run.
Raw capability bytes exist only in the caller-owned credential
carrier and are consumed before worker discovery; they never enter argv,
environment, logs, audit, runtime records, or machine output.

For public gRPC, the only accepted Controller carrier is an absolute protected
file reference below the canonical mode-0700 Application Support directory
`Dolgorae/controller-carriers/`. The Protobuf request contains the path and
expected public Controller ID/generation, never capability bytes. The semantic
service reopens the file beneath an already validated directory descriptor and
revalidates root containment, regular-file type, no-symlink identity, current
UID, mode 0600, 4-KiB bound, Controller identity/generation, and target-Run
authorization immediately before each authorized read or mutation. The
side-effect-free `VerifyController` operation runs this same check without
opening a worker or changing durable state.

The capability response publishes the checked credential schema identity and
digest plus the carrier policy. This permits a trusted Gul backend to create a
new generation-1 credential with create-exclusive semantics under
`controller-carriers/gul/<installation-id>/`; it does not grant Gul access to
Operator credentials or add a credential-generation RPC. Continuation
authorization compares normalized principals and requires a new Controller ID
and capability.

## Process and Transport Topology

For N live Runs across P active launch contracts, the normal baseline is N
workers and N WebSocket connections. Each active contract has at most one
shared-read-only Profile Server plus one physical Dedicated Lane Server for
each currently running dedicated generation. A stopped logical lane contributes
no process; a successor replaces, rather than overlaps, its prior generation.
Commands and supported Codex native-subagent work may temporarily create
additional descendants inside the selected physical generation.

```text
Machine CLI invocation ----+
                           |
Gul -- gRPC/HTTP2/UDS -----+--> shared semantic service
                                   |
                                   | private framed JSONL/UDS
                                   v
dolgorae worker [run R, worker generation G]
  |
  | HTTP Upgrade + masked WebSocket over selected private Unix socket
  v
  +-- shared_readonly -> shared Profile Server [server key K, epoch E]
  |
  `-- dedicated -> logical lane L -> Dedicated Lane Server [epoch E2]
                                      |
                                      +-- Codex-owned command/native descendants
```

The worker control socket resides below `/tmp/dolgorae-<uid>/s/`. Dolgorae opens
`/tmp` without following symlinks, creates each missing component with
`mkdirat`, validates `EEXIST` with `fstatat`, and accepts the root only when each
private component is owned by the current uid with mode 0700. The root is
volatile OS-managed state and is recreated after tmp cleanup. No-symlink enforcement applies below the
resolved `/tmp` root; operations beneath the directory use descriptor-relative
`*at()` calls. Its filename is the RFC 4648 uppercase, unpadded, 32-character
base32 encoding of the domain-separated workspace-digest/run-UUID preimage in
SPEC-002.
The composed path must fit the macOS `sun_path` limit; overflow fails with
`RUNTIME_PATH_INVALID`. There is no sibling identity sidecar. The durable
`.dolgorae/runtime/runs/<run-id>.json` record is the sole identity authority for
the volatile socket: an existing path without an exact matching record fails
with `RUNTIME_PATH_COLLISION`, and only the byte-0 winner may unlink it after
the recorded generation is proved absent. Every request also contains the
full workspace identity, run ID, expected worker generation, and boot UUID.
Ordinary requests additionally carry Dolgorae version, CLI binary digest, and the
current mutation protocol version so cross-run and unsafe version-skewed
connections fail closed. A separate version-frozen control protocol v1 accepts
only `hello`, bounded `status`, and `shutdown` across binary-digest changes.
Those operations validate workspace, run, generation, boot, and live process
identity; all other requests reject version skew. `shutdown` is identity-bound
and interrupts an active turn before cleanup.

The live worker watches the socket pathname and containing private directory.
On `ENOENT`, it reopens and validates `/tmp`, recreates the private hierarchy,
binds a replacement listener at the deterministic path, records its inode,
increments `control_socket_epoch`, and atomically replaces the runtime record.
Accepted CLI connections and the App Server WebSocket are independent of that listener
replacement. An occupied or unsafe replacement path is fail-closed: the worker
interrupts an active turn, records bounded evidence, and requires recovery.

The actual socket path and process identity are discoverable from
`.dolgorae/runtime/runs/<run-id>.json`; discovery never recomputes a path from
`$TMPDIR`. The record contains the full worker identity tuple and App Server
connection identity: PID, PGID, UID, start seconds/microseconds, live executable
path/device/inode/SHA-256, together with
the boot-session UUID, run generation, access state, socket path, Dolgorae
version, binary digest, IPC protocol version, socket inode,
`control_socket_epoch`, `server_key`, `server_epoch`, and `run_generation`. A
new shared or dedicated lane-server epoch never validates a stale connection
generation.
`.dolgorae/runtime/writer.json` is durable workspace authority, not a recoverable
pointer. It stores the writer state and all facts required to reconcile a lost
worker against its selected lane-server epoch and thread/turn. Other runtime records remain
recoverable caches; the fsynced run ledger owns run history.

Writer transaction and startup locks live at fixed paths below
`.dolgorae/runtime/locks/`. The writer and handoff files are `writer.lock` and
`handoff.lock`; startup files are `startup/<run-id>.lock`. The directory is
opened through descriptor-relative, no-symlink operations, must be
current-uid-owned mode 0700, and resides on the already-required local APFS
workspace. The mechanisms never share an inode. Lock files are
create-exclusive and permanent. The writer lock serializes durable-authority
transactions; it is not a lifetime truth source. The startup file has two POSIX
byte-range locks: byte 0 is the transient CLI starter claim and byte 1 is the
worker lifetime claim. The 8192-byte file body has separate version-1,
zero-padded, SHA-256-checksummed owner records at `[0,4096)` for byte 0 and
`[4096,8192)` for byte 1; a short file is `Unverifiable`. Each record contains the
range, workspace/run/generation, boot UUID, Dolgorae process tuple and executable
path hash. Invalid/unknown/all-zero slots do not override the kernel lock;
locked ranges without a matching valid record are `Unverifiable`. After acquiring its byte
a process updates its slot with `pwrite` on that same fd and fsyncs before
proceeding, then clears and fsyncs the slot immediately before releasing.
`F_GETLK` is
queried for both ranges; `l_pid <= 0` is `Unverifiable`, while a positive
`l_pid` is only a hint and is always checked against
the matching owner record. Normal attachment to an answering socket takes
neither byte. Byte-range acquisition uses `F_SETLKWTIMEOUT` with the Darwin
`flocktimeout` layout and a ten-second relative timeout. After timeout, a
contender may terminate only an exact byte-0 transient starter bound by kqueue
and revalidation. A byte-1 owner is a serving reader or writer worker and
requires control `hello`; a live `Match` that does not answer returns
`RUN_BUSY`, and no activity-derived condition authorizes signalling it.
`Mismatch` or `Unverifiable` is never signalled. After
exact exit, contenders race to acquire byte 0 and only the winner starts or
recovers. A worker that loses byte 1 reports fd-3 `RUN_BUSY` and exits with no
socket, ledger, or runtime mutation. Each owner process opens the file once; identity revalidation uses
`fstat` on that held fd and never a second open, including through a hardlink.
The descriptor is never reopened or closed during ownership and is marked
close-on-exec before the App Server connection opens.

The worker/App Server transport is direct WebSocket over the Dolgorae-owned Unix
socket. The socket supplies local endpoint isolation; WebSocket supplies the
actual app-server framing. The worker remains the correlation and audit
interposition point without owning the shared process.

App-server WebSocket frames are drained independently of every CLI observer.
Every App Server uses null stdin and separate nonblocking stdout/stderr pipes to
its scoped bounded log drainer. A failed file sink switches the live drainer to
drain-and-drop and marks the generation degraded; loss of the drainer process
fences new attachment and requires controlled restart.
Observers read fsynced ledger records by cursor and therefore cannot exert
backpressure on the active protocol stream. Limits are 16 MiB per WebSocket
frame, 32 MiB per reassembled message, 1 MiB per diagnostic line, 2 MiB per raw selected ledger payload with a
3 MiB post-transform allowance, and 8 MiB per
CLI-worker frame. CLI oversize affects only its caller; diagnostic oversize
retains metadata and continues. Invalid or oversized WebSocket input closes the
connection and quarantines
an accepted active turn as `outcome_unknown`. A solicited `thread/read` response
is recognized only after its unique matching top-level `id` appears before byte
16 MiB; outstanding request count is never a classifier. Once recognized it is
consumed by a constant-memory streaming visitor retaining required turn/status
fields plus raw-wire length and SHA-256. It has the 120-second deadline but no
arbitrary total size cap. An ambiguous oversize prefix fails compatibility and
follows SPEC-006's active-turn quarantine rule.

## Workspace Identity and Local Layout

The canonical workspace ID is SPEC-002's full lowercase SHA-256 over the
domain-separated libc `realpath(3)` byte sequence. The same raw digest feeds the
socket derivation; no component performs case folding, Unicode normalization, or
an alternate path hash. Lock pathnames are fixed names below the per-workspace
lock root and carry no digest.

The repository-local layout is:

```text
.dolgorae/
  .gitignore                 # tracked
  config.yaml                # tracked portable policy
  local.yaml                 # ignored, 0600 local named profiles
  runs/                      # ignored, 0700
    <uuidv7>/
      manifest.json          # 0600, fixed run facts
      audit.jsonl            # 0600, append-only authority
      state.json             # 0600, replaceable projection
      worker.log             # 0600, bounded startup/runtime diagnostics
      worker.log.1           # 0600, single rotated diagnostic log
      recovery/              # 0700, preserved crash-tail evidence
  runtime/                   # ignored, recoverable local coordination
    writer.json              # 0600, durable writer authority state machine
    workspace.json           # 0600, canonical workspace/runtime facts
    locks/                   # 0700, permanent workspace lock pathnames
      writer.lock            # BSD flock transaction serializer
      handoff.lock           # cross-profile handoff serialization
      startup/<uuidv7>.lock  # two-range worker startup ownership
    runs/<uuidv7>.json       # 0600, per-run process/socket discovery
  evidence/                  # ignored, generated probe/recovery/default exports
  cache/                     # ignored, replaceable compatibility data
```

No absolute profile executable path, socket path, PID, authentication state, or run state is
placed in tracked project policy.

## Manifest and Ledger Model

### Manifest

The manifest is created before externally meaningful app-server work and then
completed with facts learned during start. Its fixed semantic fields include:

- schema version, run ID, canonical workspace and workspace ID;
- Git/non-Git mode and start baseline;
- created timestamp and initial access;
- profile name, argv, expected `CODEX_HOME` snapshot;
- actual app-server version, schema status, and actual `codexHome`;
- Dolgorae version, binary SHA-256, and IPC protocol version;
- fixed model and initial/default reasoning effort;
- immutable run instructions;
- controller ID/kind/instance/subject, controller generation, and the
  domain-separated capability digest (never the capability bytes);
- purpose, optional external label and parent reference;
- required capabilities and the accepted profile capability snapshot;
- Codex thread ID when allocated;
- fork provenance and last confirmed boundary when applicable;
- audit policy and compatibility verdict.

Fields that change during execution belong in ledger events and `state.json`,
not as silently mutable manifest history.

### Audit Ledger

Each JSONL record has this logical envelope:

```text
schema_version
sequence
timestamp
run_id
run_generation
kind
payload
previous_hash
hash
```

Ledger lines use RFC 8785 JSON Canonicalization Scheme (JCS) followed by one
newline. The `sha256-jcs-v1` record hash is lowercase hexadecimal SHA-256 over
the JCS bytes of the record with the `hash` member omitted and the
`previous_hash` member retained. The genesis `previous_hash` is exactly 64 ASCII
zeroes. Sequence starts at one and increases by one. The manifest records the
hash scheme and genesis. Closed and start-failed runs append a final seal event.
`state.json` stores the last projected sequence/hash so truncation or projection
lag is detectable during normal operation; verification still scans the ledger
from genesis.

Inbound JSON is parsed with duplicate-member rejection and number lexemes held
only through adaptation. The in-repo canonicalizer uses UTF-16 key order and
ECMAScript shortest binary64 rendering and is pinned by RFC 8785 plus Dolgorae
golden vectors; a byte change requires a new hash-scheme version. Before any Dolgorae marker is inserted, every inbound object key
matching `^\$+dolgorae_` is escaped by prefixing one additional `$`. Redaction is
then applied, followed by numeric adaptation; the tokenizer never treats a
Dolgorae-owned marker key as a candidate secret key. A decimal whose finite
binary64 ECMAScript rendering is not numerically equal to the original is
replaced before JCS with
`{"$dolgorae_number":"<original-lexeme>"}`. Invalid
JSON, duplicate members, and otherwise unrepresentable payloads never reach the
canonicalizer. Verification requires each stored line, excluding its newline,
to be byte-identical to the JCS serialization of its own parse. Timestamps use
UTC RFC 3339 with exactly six fractional digits and `Z`.

Each complete line is appended to an `O_APPEND` handle with `write(2)` retried
until all bytes are written. Ordinary streaming records may be group-committed
for at most 100 milliseconds. Using `fsync(2)`, the ledger is synchronized
before every externally observable effect: turn intent/idempotency precede
`turn/start`, approval decisions precede responses, cleanup intent precedes
signals, and preserved-tail evidence is fsynced before ledger truncation. It is also synchronized before
Dolgorae acknowledges an accepted turn ID, pending master interaction, terminal
result, or access/lifecycle change. Manifest creation and atomic state
replacement synchronize their containing directories. V1 claims process- and
OS-crash durability after these barriers, not power-loss durability.

Any malformed or invalid newline-terminated record, including the final record,
a complete record with a broken hash, or any sequence discontinuity is an
audit-integrity failure. Only nonempty bytes after the file's last newline are
a recoverable torn tail, even when those bytes parse as a complete JSON object
but lack the terminating newline. Recovery writes and fsyncs the deterministic
sequence/hash-named evidence, truncates and fsyncs only those bytes, then appends
and fsyncs `ledger_tail_repaired`; restart completes any prefix idempotently. A torn
tail is never reported as ordinary tampering.

Each selected payload is at most 2 MiB raw and 3 MiB after representation. A
larger or unrepresentable payload becomes a `payload_unrepresentable` record
containing source kind, observed byte length, streaming SHA-256, JSON Pointer
when known, and reason; no original bytes or sidecar are retained. The ledger
includes CLI intent accepted by the worker, lifecycle transitions,
run generations, app-server requests/responses/notifications after
redaction, normalized client events and interactions, controller resets,
approval decisions, state reconciliation, and cleanup results. Its
completeness claim covers Dolgorae lifecycle, main-turn wire traffic exposed by
app-server, approvals, writer-authority transitions, and profile/account provenance. Native
subagent or other content not exposed in plaintext by app-server is retained
only as an opaque event and is not claimed as reconstructable audit. Reasoning
text, summaries, deltas, and internal planning streams are discarded before
representation. Their method, byte length, digest, and suppression reason are
the only durable accounting.

Client events are normalized into discriminator-bound durable records and
schema-validated before their ledger append. A record owns the canonical
decimal-string cursor and server key/epoch but never a reader's projection or
replay flag. Each observer receives a separate delivery envelope carrying those
two delivery facts. The delivery schema conditionally permits state, final
response, interaction, runtime error, writer, and recovery events in `minimal`;
usage, workspace changes, command, diagnostics, generation, and reasoning-
suppression metadata are `operational` only. Both streams filter the same safe
records by one run-wide sequence cursor; they never project raw audit or app-server payloads. A filtered
record creates a cursor gap rather than a second cursor namespace. Slow or
disconnected observers replay fsynced records and never participate in
WebSocket draining or worker state transitions.

### Materialized State

`state.json` is atomically replaced only to a head at or before the last fsynced
ledger record, at durability barriers and a bounded projection interval. It
contains the current lifecycle state, run generation, thread ID, active/latest turn,
pending requests, access mode, writer-authority observation, default effort, last
event cursor, and ledger head. If it is missing, stale, or invalid, the worker
replays `audit.jsonl` before accepting ordinary mutations; the bounded control
channel remains available from `bound` throughout replay.
If its head is ahead of the durable ledger, it is a stale projection rather
than an audit-integrity failure: Dolgorae rebuilds it and appends
`projection_rewound`.

## Controller and Observer Boundary

The caller creates a self-contained controller credential rather than a global
controller registry. A helper command creates a new mode-0600 file
create-exclusively; integrations may construct the same checked object and pass
it through an inherited descriptor. The CLI validates carrier metadata and
syntax, then transfers the opened descriptor with `SCM_RIGHTS` over the private
worker socket. The request carries public controller generation, invocation,
expected state revision and idempotency facts. Under the mutation lock, the
worker reloads state, reads the bounded secret, compares its digest and all
revision operands, zeroizes it, and applies the transition. No CLI-only check or
`already_validated` claim crosses the serialization boundary.

Observer read paths validate same uid and project/runtime path safety but
require no controller credential. Their output always passes through the
client-safe projection boundary and cannot return full interactions or
controller-only artifacts. Controller read and mutation paths validate the
credential at the serialization point. A controller mismatch deliberately has one error shape so
run existence, controller-ID mismatch, and capability mismatch reveal no
additional credential facts beyond information already available to local
observers.

An installation-scoped operator credential has a separate persisted digest and
generation in the Application Support root. Initialization is create-exclusive;
rotation requires the current capability. Profile stop/restart and controller
reset accept it only by protected file/fd, while controller reset accepts the
new controller through a distinct carrier. Server key is public identity, not
authorization.

Operator reset is staged. PREPARE holds operator, then writer when applicable,
then run-startup/run serialization; it revalidates the already-open operator
and new-controller carriers, all blockers and revisions, and fsyncs an operation
token. APPLY drops every file lock before reader-policy/background verification
or worker coordination. COMMIT reacquires the same ordered locks, revalidates
the token, identities and revisions, clears writer authority when proved safe,
and atomically publishes the new controller generation. If APPLY or COMMIT
cannot prove safety, the old controller binding remains authoritative and the
recorded writer failure state is preserved. Paused and outcome-unknown runs
retain their recovery facts. Environment context markers are diagnostic only
and do not participate in authorization.

## State Machine

The worker is the normal state-transition authority. The only bootstrap
exception is a byte-0 owner that proves no worker reached `bound`; it may append
and seal `start_failed`, but only for a Run already allocated after a ready
Profile Server epoch. Failure before profile ready is a profile diagnostic and
has no Run identity. Present `Unverifiable` generations are never rewritten.

The run lifecycle table governs its worker and client connection only. Profile
singleton existence is manager-owned and independent of any one row.

| State | Worker expected | Run WebSocket expected | New turn | Operation results |
| --- | --- | --- | --- | --- |
| `starting` | Maybe | Maybe | No | `idle` or `start_failed` |
| `idle` | Yes | Yes | Yes | `running`, `paused`, or `closed` |
| `running` | Yes | Yes | No | `idle`, `waiting_interaction`, `reconciliation_required`, or `outcome_unknown` |
| `waiting_interaction` | Yes | Yes | No | `running`, `idle`, `reconciliation_required`, or `outcome_unknown` |
| `reconciliation_required` | No, except transient reconciliation | No, except a transient read-only connection | No | `paused` or `outcome_unknown` |
| `paused` | No | No | No | `idle` after resume or `closed` |
| `closed` | No | No | No | Final |
| `start_failed` | No | No | No | Final |
| `outcome_unknown` | No, except transient read-only reconciliation | No, except its transient connection | No | `paused` after reconciliation or `closed` after proven cleanup |

The same run never has two active turns. `send`, `submit`, and idempotent retries
all enter the same serialized turn-start path.
Fork creates a distinct run and never transitions or mutates its source run.

## Request Correlation and Generations

Every outbound app-server request is registered before write and correlated by
JSON-RPC request ID. Thread-scoped messages must match the run's thread ID;
turn-scoped messages must match the active or addressed turn ID. Server
requests are wrapped in a Dolgorae request ID that includes run generation.

Unknown responses, mismatched IDs, duplicate terminal events, and invalid state
transitions are recorded and fail closed. Known-but-unsupported server requests
are recorded and receive method-not-found without stopping the generation;
unparseable frames fail closed. Unknown additive notifications do not change
state and retain bounded redacted evidence unless classified as reasoning, in
which case content is discarded before ledger representation.

A new connection/access-policy lifetime increments run generation; one worker may
host successive reader and writer generations. Pending requests never cross
that boundary.

## Historical Transient Writer Authority Flow (Superseded)

The former shared↔capsule protocol is not part of the normative architecture.
Its rationale and rejected state machine remain in the historical ADR and
review records for auditability. `SPEC-014`, ADR-019, and the Sticky
Execution-Lane sections above are the only executable requirements; no capsule
state name or transition in the historical record may be implemented.

## Turn Execution Flow

1. Receive the controller fd and, under mutation serialization, validate run,
   command, invocation, controller generation, state revision, capability
   digest and idempotency key; zeroize the secret, then validate the model-fixed invariant,
   image readability, and requested effort.
2. Before any App Server request, policy change, or writer mutation, reserve the
   idempotency key, append a revision-bound operation intent, and fsync the
   ledger. If `--write` is present, the same PREPARE transaction also publishes
   `reserved` authority and a provisional thread identity when needed.
3. Release every file lock. In APPLY, a threadless write starts its thread with
   writer policy; a bound reader keeps its thread `sandbox` value and applies
   writer policy through the turn carrier alone. Verify live effective policy.
4. Reacquire writer then run serialization, revalidate the operation token and
   revisions, fsync a new thread binding when applicable, and publish `active`.
   An indeterminate APPLY never starts a turn and lands in its specified
   reserved or `blocked_unknown` recovery state.
5. For a threadless read turn, send `thread/start`, append its provisional thread ID,
   and fsync before `turn/start`. If turn acceptance is uncertain, recover that
   exact provisional thread and retry only when stable history proves no turn
   was accepted or the thread is absent; unreadable/in-progress evidence is
   never retried.
6. Capture a best-effort pre-turn workspace observation.
7. Send `turn/start` with the fixed model, selected reasoning effort, canonical
   cwd, access-derived sandbox policy, approval policy, and message/images.
   Developer instructions were supplied by thread start/resume for this
   generation because turn start has no such field.
8. Persist the permanent thread binding and accepted Codex turn ID before
   acknowledging `submit`.
9. Stream and audit correlated notifications.
10. Fsync supported server requests as generation-qualified normalized
   interactions before observer delivery.
11. On terminal notification, read back persisted thread history when necessary,
   select the last root-turn `phase:final_answer` item or last phase-null
   compatibility item in authoritative order, capture a post-turn workspace
   observation, and transition to idle. Commentary or absent messages never
   fabricate a final response.

The delivery mode is not part of idempotency identity: `send` waits, while
`submit` returns after step 8.

## Recovery and Reconciliation

Recovery never auto-replays user input. The new worker first replays the ledger,
validates profile identity and compatibility, and inspects the pinned Codex
thread with stable history APIs only after the lane-specific barrier below is
proved.

- Confirmed idle history resumes normally.
- A terminal turn absent from Dolgorae's projection is appended as reconciled
  evidence and the run returns to idle.
- An active turn without authoritative terminal evidence produces
  `outcome_unknown`; the replacement connection is closed while durable writer
  authority remains `blocked_unknown`.
- `reconcile` branches by immutable lane. For `dedicated`, it proves the
  recorded Dedicated Lane Server generation and descendants absent, satisfies
  the durable-history barrier, and attaches the same logical lane at a new
  compatible epoch. For `shared_readonly`, it never treats the shared singleton
  as run-owned or terminates it; it validates the currently recorded compatible
  shared epoch and uses profile-level singleton recovery when that server is
  actually unavailable. It then starts a writer-authority-free transient worker
  generation and calls only
  `thread/read(includeTurns: true)` over a read-only connection. It never loads
  or resumes the thread and never starts a turn. It appends old/new key/epoch,
  lane-qualified absence or shared-epoch validation, history, and
  writer-resolution evidence, closes the connection, and
  exits. Confirmed terminal evidence moves an unknown
  run to `paused`; later bare resume uses read access.
- Every history-copying fork scans newest-to-oldest and uses the latest
  status listed as forkable in the checked manifest; terminal-but-rejected
  statuses are skipped. Confirmed history with no accepted boundary returns
  `COMPATIBILITY_REJECTED`; only the no-confirmed-turn outcome-unknown fallback
  takes the fresh-thread provenance path after prior generation absence is
  proved.
  `fork --fresh` reads the immutable source manifest and read-only fsynced
  state/runtime projections needed for eligibility and provenance. It never
  reads the source Codex thread or mutates/repairs the source ledger or any
  source projection. It creates an empty threadless read-only run and
  records source run, observed lifecycle state, and unresolved-turn provenance
  without asserting an outcome. It is therefore available even when a source
  in `running` or `waiting_interaction` has an unreachable socket and unverifiable process
  identity.

Every fork copies the source profile snapshot and immutable run instructions,
defaults to read access, and may replace only the fixed model. It cannot change
the profile or account boundary.

The original source ledger is never rewritten during reconciliation or fork.
Projection-only `status`, `events`, and `verify` return their data with an
identity-verdict field and do not fail merely because it is `Unverifiable`.
They read fsynced projections directly and never start, attach, or recover a
worker.

## Process Cleanup

The detached worker owns only its worker process and private client connection.
Pause, close and recovery may request `turn/interrupt`, close that connection,
and terminate an identity-verified worker, but they cannot signal singleton
commands or native subagents. Cleanup records connection close, interrupt
request and terminal history as distinct evidence. Close cannot finalize an
active or uncertain run, nor can worker exit establish command termination.
Profile-wide operator shutdown alone may signal the verified singleton after
complete membership handling.

## Runtime and Dependency Boundary

The implementation is Rust 2024 pinned by `rust-toolchain.toml` to 1.97.1 with
rustfmt, clippy, and `aarch64-apple-darwin`; Cargo.lock is committed. Blocking
durability and process work uses dedicated OS threads rather than an async
runtime: control/protocol, stdout, stderr, ledger/state authority,
kqueue/liveness, and `sigwait` each have an explicit owner.

The public RPC gateway is the sole exception to the no-async-runtime default.
It uses `tonic`, `prost`, and an adapter-private bounded `tokio` runtime for
HTTP/2-over-UDS transport, cancellation, and server-stream delivery. Durable
state, locks, process control, worker IPC, and semantic transitions remain on
the existing blocking owners. The gateway invokes them through a bounded
blocking pool and may never hold a Tokio task, stream queue, or HTTP/2 channel
as authority evidence.

One `darwin` module is the only unsafe OS boundary. It wraps `libc` bindings for
`posix_spawn` attributes, libproc sampling/enumeration, kqueue, byte-range
fcntl/flock inspection, `fstatfs/MNT_LOCAL/APFS`, and boot-UUID sysctl. Core recovery
receives safe typed verdicts through injectable monotonic-clock, boot, identity,
enumeration, and fault-barrier interfaces. RFC 8785 canonicalization is an
in-repository safe module rather than an unspecified serializer dependency.

The approved safe-Rust mechanisms are `clap` for CLI parsing, `uuid` for
UUIDv7, `sha2` for SHA-256, `base64` plus `data-encoding` for the pinned base
alphabets, `serde_yaml_ng` 0.10 behind duplicate/unknown-key rejecting typed
configuration adapters, and `serde_json` only behind the duplicate-detecting `RawValue` ingest visitor
owned by SPEC-010. JCS serialization remains in-repository. Cargo.lock pins
exact versions; adding a runtime dependency or changing one of these mechanism
bindings requires an ADR amendment and conformance fixture.

The shared fake app-server is a test-only Python subprocess under
`tools/fake_app_server/` that speaks the real Unix-socket WebSocket boundary.
TASK-004 is
its sole owner; later tasks consume or extend it. Declarative scenarios are
validated against the checked Codex manifest, and the fake shares no production
parser or state-machine code with Dolgorae.

Dedicated lane-generation descendants are discovered by process-group enumeration plus
all-PID BSD parent/session samples, and an observed identity remains tracked
after reparenting or group/session change. Cleanup sends TERM, then KILL after
five seconds, only to exact revalidated identities and requires five complete
empty censuses within a ten-second total budget. PID reuse, truncated or failed
enumeration, unreadable identities, unregistered survivors, and detected escape
create durable `background_execution:unverified` and block release, handoff,
close, or generation replacement.
Deliberate fork/setsid/reparent escape wholly between 100-millisecond polls and
remote side effects remain outside the trusted same-user personal-alpha model;
the prompt discourages them only as defense in depth. A native Codex terminal
API is optional hybrid evidence, not a release dependency.

On worker `SIGTERM`, bounded clean shutdown appends and fsyncs `cleanup_intent`
with reason `generation_shutdown_requested`, rejects new control mutations, and, when a
turn is active, sends `turn/interrupt` and waits up to five seconds for terminal
history before closing its connection. It then appends and fsyncs
`run_generation_stopped` with shutdown reason and the last known turn state and
attempts bounded startup-lock acquisition
for socket unlink. If that lock cannot be acquired, it leaves stale
coordination files for the next verified owner rather than blocking shutdown,
then exits. Active or uncertain writer authority persists as
`blocked_unknown`; worker shutdown never releases it implicitly. Failure of any
step is recorded when the ledger remains writable and produces a nonzero exit.

## Security and Trust Boundaries

Dolgorae is a coordination and audit tool, not a hardened multi-user security
boundary.

It does provide:

- user-private local sockets and run files;
- fail-closed account, request, thread, turn, and generation correlation;
- Codex sandbox selection for reader/writer turns;
- durable Dolgorae writer authority per canonical worktree;
- normative recursive redaction and tamper-evident hash chaining;
- explicit approval and destructive-action boundaries;
- bearer-capability separation between one run controller and local observers;
- client-safe projections that discard reasoning before persistence;
- separate controller and local-operator capability boundaries.

It does not provide:

- protection from a hostile process running as the same OS user;
- remote authentication or authorization for observer projections;
- filesystem isolation from editors or non-Dolgorae tools;
- rollback of partial writes;
- attribution of observed changes to Codex;
- control of external MCP/app/plugin side effects;
- OS-level per-run ownership or termination of commands/native subagents in the
  shared App Server process tree;
- cryptographic signatures or remote audit attestation;
- direct communication or trust delegation between independent Dolgorae runs;
  externally brokered child operation remains hub-and-spoke orchestration;
- authorization based on a diagnostic environment marker.
- public Internet, TCP, or direct Tailscale exposure of the local gRPC socket;
- remote authentication or authorization inside Dolgorae; Gul owns that
  boundary before exposing local results;
- Gul access to private worker sockets, profile/dedicated App Server sockets,
  App Server protocol frames, or Operator credentials.

## Compatibility Boundary

The protocol adapter is a strict subset client. Its checked JSON manifest lists
the source schema bundle SHA, resolved JSON Pointers, methods, responses,
notifications, server requests, type/const/requiredness, required enum values,
response-schema IDs, absent-thread errors, forkable statuses, and the early-ID
behavioral observation. Compatibility doctor resolves `$ref`, performs the
normative structural comparison, then runs handshake, paginated model,
codexHome, history, sandbox, early-ID, and server-request probes.

The tested 0.147.0 manifest is `tested`. A newer compatible version is
`unverified` and that verdict is written to every run generation. Older or
otherwise unlisted versions are rejected unless a future SOT revision adds
them to the tested set.

Runtime code tolerates additive data but never infers lifecycle progress from
uncorrelated or unknown messages. This preserves compatibility without claiming
that a schema probe can guarantee all future runtime behavior.
