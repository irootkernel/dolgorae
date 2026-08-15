# Dolgorae Architecture

Status: Normative target architecture for the first supported release.

This document owns technical structure and invariants. It describes the system
Dolgorae is required to implement; it does not claim that the currently empty
repository already implements it. Product behavior is owned by
[specs.md](specs.md), rationale by
[architecture-decisions.md](architecture-decisions.md), and implementation
progress by [roadmap.md](roadmap.md).

## System Context

Dolgorae is a local process supervisor and protocol adapter between a master and
Codex app-server. It adds durable run identity, account binding, access
coordination, recovery, and audit around Codex threads without replacing Codex
conversation storage.

```text
                          project-local profile configuration
                          argv + expected CODEX_HOME
                                     |
                                     v
master -> dolgorae CLI -> per-run worker -> per-run proxy connection
             |              |                         |
          JSON I/O       audit ledger                 v
                                     profile app-server singleton -> Codex services
```

The master is the only orchestrator of independent Dolgorae runs. Codex may
manage native subagents inside a run; those children remain within Codex's
session tree and are not Dolgorae peers.

## Component Model

### CLI Front End

The visible `dolgorae` invocation is short-lived. It:

1. resolves the canonical workspace;
2. parses and validates machine-oriented input;
3. resolves the explicit run ID when required;
4. discovers or starts the owning worker;
5. exchanges one request/response with the worker, or reads the fsynced
   projection directly for projection-only `events`;
6. emits the stable stdout envelope and exits.

It never talks directly to app-server and never writes the audit ledger while a
worker owns the run. Start-time bootstrap is the only period in which the
front-end may create the run directory and initial records before worker
ownership transfers.

### Per-Run Worker

The worker is a hidden re-execution mode of the same `dolgorae` binary. It is the
sole owner of:

- the run control socket;
- the run's exclusive app-server proxy connection;
- JSON-RPC request IDs and correlation state;
- run lifecycle and pending interactions;
- the audit append handle and materialized state;
- the writer lease when the run has write access;
- cleanup of its proxy and run-scoped descendants.

One worker serves one run. There is no shared supervisor or global in-memory
registry. Concurrent start/recovery attempts for the same run are serialized by
a per-run startup lock.

The worker is detached from the transient CLI before it accepts requests. It
runs through the hidden `__worker` argv mode after a single fork and `setsid()`,
with no controlling terminal, null stdin/stdout/stderr, and `umask(077)`. It
ignores terminal-originated `SIGINT` and `SIGHUP` for itself and handles
`SIGTERM` as a bounded shutdown request. App-server `posix_spawn` attributes use
`SETSIGDEF` for every catchable signal and `SETSIGMASK` with an empty mask so
ignored dispositions or a worker `sigwait` mask do not leak into Codex or its
descendants; there is no child-side callback. Early-start and internal diagnostics
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
parent keeps byte 0; its child's inherited startup fd is `FD_CLOEXEC` before
`__worker` re-exec. Fd 3 is explicitly preserved across that re-exec. The worker
opens one new startup fd for byte 1 and marks both it and fd 3 `FD_CLOEXEC`
before app-server launch.
The CLI creates no thread before fork. Between fork and re-exec the child
performs only async-signal-safe operations; worker threads are created only
after re-exec.
These rules make ownership handoff explicit and ensure
that Ctrl-C or command substitution cannot terminate the worker or keep the
caller's output pipe open.

### Profile App-Server Singleton and Run Proxy

The first operation that needs a profile starts or attaches to one Codex
app-server singleton keyed by canonical `CODEX_HOME` and the checked executable
snapshot. Every run worker starts a profile-matched `app-server proxy` and owns
one private JSONL connection through it. The proxy is an audit and ownership
boundary, not another app-server. A singleton may therefore serve many runs and
workspaces while each worker receives only its own connection frames.

Before launch, the worker removes inherited `DOLGORAE_*` control variables and
adds a fresh managed-run context containing its own workspace and run identity.
The public CLI recognizes this context and exposes only read-only introspection
of that same run. This is an accidental-recursion guard, not an unforgeable
credential.

Each proxy connection performs exactly one `initialize` request followed by the
`initialized` notification. A newly created run remains threadless until its
first turn because pinned Codex does not persist a turnless thread across
app-server restart. First `send`/`submit` uses `thread/start`; recovered runs use
`thread/resume`, and history forks use `thread/fork`. After first-turn
acceptance the worker maintains exactly one loaded Dolgorae-owned thread.
Turns use `turn/start`, `turn/steer` only when explicitly exposed by a future
specification, and `turn/interrupt` for cancellation. V1 does not enable the
experimental app-server API capability.

The profile singleton is started through the checked Codex daemon interface and
has a profile-wide `server_epoch`. The worker-owned proxy is created with
`POSIX_SPAWN_START_SUSPENDED`, `SETPGROUP`,
`SETSIGDEF`, and `SETSIGMASK(empty)`. While the spawn image is suspended, the
worker samples `PROC_PIDTBSDINFO`, opens the `proc_pidpath` target without
following symlinks, computes spawn-image device/inode/SHA-256 from that fd, and
repeats BSD info. A shebang or `/usr/bin/env` wrapper may expose its interpreter
here. It persists the complete ten-field provisional identity before
`SIGCONT`; missing or replaced operands fail closed without a partial record.
An entry-image to final-image exec with unchanged PID/PGID/UID/start time is
continuity, not replacement. Failure before continuation is cleaned as a start
failure. After initialize, the worker repeats the sample and fsyncs
`run_generation_started`; that record alone owns final proxy-executable identity.

### Profile Registry and Singleton Membership

`.dolgorae/local.yaml` is project-local, ignored by Git, and stores the project's
named profile map without tokens or arbitrary environment values. A run manifest
contains a private snapshot so later edits cannot silently change an existing
run's account.
The singleton membership index is separate XDG state keyed by `server_key`. It
contains only server identity/epoch and workspace/run runtime-record locators,
so stop/restart can enumerate active members across projects. It is recoverable
from those records and stores no prompt, audit payload, or credentials.
The Codex-owned daemon control socket remains below the profile's `CODEX_HOME`.
If that pathname disappears, existing proxy connections may finish but new
attachments are rejected and the profile becomes `degraded`; Dolgorae never
starts a duplicate until the recorded daemon identity is proved absent or an
identity-checked interrupting restart completes.

### Persistent Run Store

The run store is workspace-local and ignored by Git. `audit.jsonl` is the only
event authority. `state.json`, transcripts, status views, and exports are
projections. The Codex thread remains independently stored in the pinned
`CODEX_HOME`.

## Process and Transport Topology

For N live runs across P active profiles, the normal baseline is N workers, N
proxy processes/connections, and P app-server singletons.
Commands or Codex native subagents may temporarily create additional descendant
processes.

```text
dolgorae CLI invocation
  |
  | Unix domain socket: framed JSONL request/response/notifications
  v
dolgorae worker [run R, generation G]
  |
  | private stdin/stdout: app-server proxy JSONL
  v
profile argv ... app-server proxy -> profile app-server singleton
  |
  +-- Codex-owned command and native-subagent descendants
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
Accepted connections and the proxy connection are independent of that listener
replacement. An occupied or unsafe replacement path is fail-closed: the worker
interrupts an active turn, records bounded evidence, and requires recovery.

The actual socket path and process identity are discoverable from
`.dolgorae/runtime/runs/<run-id>.json`; discovery never recomputes a path from
`$TMPDIR`. The record contains full worker and proxy identity tuples: PID,
PGID, UID, start seconds/microseconds, live executable path, executable
device/inode, and executable SHA-256, together with
the boot-session UUID, run generation, access state, socket path, Dolgorae
version, binary digest, IPC protocol version, socket inode,
`control_socket_epoch`, `server_key`, `server_epoch`, and `run_generation`. A
new singleton epoch never validates a stale proxy generation.
`.dolgorae/runtime/writer.json` points to the current writer run/generation and
stores the incumbent identity plus `cleanup_in_progress`; it is replaced only
after cleanup is confirmed. Runtime records use write-temp, `fsync`, rename,
and directory `fsync`. They are recoverable coordination caches; the fsynced
`run_generation_started` ledger record is process-identity authority.

Writer leases and startup locks live at fixed paths below
`.dolgorae/runtime/locks/`. The writer and handoff files are `writer.lock` and
`handoff.lock`; startup files are `startup/<run-id>.lock`. The directory is
opened through descriptor-relative, no-symlink operations, must be
current-uid-owned mode 0700, and resides on the already-required local APFS
workspace. The mechanisms never share an inode. Lock files are
create-exclusive and permanent. The writer lease is nonblocking BSD `flock(2)`. The startup file has two POSIX
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
close-on-exec before app-server launch.

The worker/app-server transport intentionally uses stdio even though app-server
also offers Unix and WebSocket listeners. Stdio gives the worker exclusive
ownership, lifecycle coupling, and a single audit interposition point.

App-server stdout and stderr are drained independently of every CLI observer.
Observers read fsynced ledger records by cursor and therefore cannot exert
backpressure on the active protocol stream. Limits are 16 MiB per unsolicited
stdout line, 1 MiB per stderr line, 2 MiB per raw selected ledger payload with a
3 MiB post-transform allowance, and 8 MiB per
CLI-worker frame. CLI oversize affects only its caller; stderr oversize retains
metadata and continues. Unsolicited stdout remains capped at 16 MiB per line;
invalid or oversized unsolicited stdout stops the generation and quarantines
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
writer filename, startup filename, and socket derivations; no component performs
case folding, Unicode normalization, or an alternate path hash.

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
    writer.json              # 0600, current writer/cleanup pointer
    workspace.json           # 0600, canonical workspace/runtime facts
    locks/                   # 0700, permanent workspace lock pathnames
      writer.lock            # BSD flock writer lease
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
redaction, approval decisions, state reconciliation, and cleanup results. Its
completeness claim covers Dolgorae lifecycle, main-turn wire traffic exposed by
app-server, approvals, writer lease transitions, and profile/account provenance. Native
subagent or other content not exposed in plaintext by app-server is retained
only as an opaque event and is not claimed as reconstructable audit. Hidden
reasoning and unexposed communication are outside the ledger's authority.

### Materialized State

`state.json` is atomically replaced only to a head at or before the last fsynced
ledger record, at durability barriers and a bounded projection interval. It
contains the current lifecycle state, run generation, thread ID, active/latest turn,
pending requests, access mode, writer lease observation, default effort, last
event cursor, and ledger head. If it is missing, stale, or invalid, the worker
replays `audit.jsonl` before accepting ordinary mutations; the bounded control
channel remains available from `bound` throughout replay.
If its head is ahead of the durable ledger, it is a stale projection rather
than an audit-integrity failure: Dolgorae rebuilds it and appends
`projection_rewound`.

## State Machine

The worker is the normal state-transition authority. The only bootstrap
exception is a byte-0 owner that proves no worker reached `bound`; it may append
and seal `start_failed`. Present `Unverifiable` generations are never rewritten.

| State | Worker expected | App-server expected | New turn | Operation results |
| --- | --- | --- | --- | --- |
| `starting` | Maybe | Maybe | No | `idle` or `start_failed` |
| `idle` | Yes | Yes | Yes | `running`, `paused`, or `closed` |
| `running` | Yes | Yes | No | `idle`, `waiting_approval`, or `outcome_unknown` |
| `waiting_approval` | Yes | Yes | No | `running`, `idle`, or `outcome_unknown` |
| `paused` | No | No | No | `idle` after resume or `closed` |
| `closed` | No | No | No | Final |
| `start_failed` | No | No | No | Final |
| `outcome_unknown` | No, except transient read-only reconciliation | No, except its transient app-server | No | `paused` after reconciliation or `closed` after proven cleanup |

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
state and are retained as raw redacted evidence.

A new proxy/access-policy lifetime increments run generation; one worker may
host successive reader and writer generations. Pending requests never cross
that boundary.

## Writer Lease Architecture

The lease is BSD `flock(2)` with `LOCK_EX | LOCK_NB` on
`.dolgorae/runtime/locks/writer.lock`. Multiple readers open no writer lease. Each canonical Git worktree
is a distinct workspace and therefore a distinct writer lane. An unresolved
`writer.json` entry for another run is audited but does not gate acquisition:
the kernel lease and the acquiring run's own generation safety determine who
may attempt it. It does gate writer activation: the held inode must match the
recorded lease inode, or that historical pair must be reconstructed only after
the recorded generation is proved absent. The foreign proxy generation must be proven absent
or identity-verified and cleaned before a new writer proxy starts.
`Unverifiable` releases the new lease with `RECOVERY_REQUIRED`.

Only the worker holds the lease. The descriptor is `FD_CLOEXEC` and is never
inherited by proxy, singleton, or descendants. Lock lifetime covers idle,
running, and waiting writer states until explicit idle release or safe lifecycle
cleanup.

Recovery of every byte-1 serving worker, reader or writer, uses the same
control/identity guard. A successful control `hello` attaches and does not
replace anything. If a recorded worker is alive but `hello` does not complete
within ten seconds, the recovering CLI applies boot UUID,
process/executable identity, kqueue, and revalidation rules. A live `Match`
returns retryable `RUN_BUSY` without a signal; `Unverifiable` returns
non-retryable `RECOVERY_REQUIRED`. Activity counters and CPU inactivity never
authorize byte-1 takeover. After independently proven exit, the replacement
competes for the startup lock, rechecks socket/runtime state, and acquires the
writer lease normally only if it wins. The run-keyed startup-lock election
serializes same-run replacements and never authorizes one run to terminate
another run's healthy writer. If either a worker during shutdown or a transient
starter remains alive while holding the startup lock, identity inspection may
explain the contention but does not create signal authority. The contender
returns `RUN_BUSY` for a revalidated `Match` or `RECOVERY_REQUIRED` for
`Unverifiable`; only independently proven exit permits the sequence to continue.

After acquisition and at every destructive barrier, the worker compares the
held descriptor's `fstat` device/inode with a root-fd-relative `fstatat` of the
lease pathname. The pair is stored in `writer.json`; mismatch, including manual
unlink/replacement, fails closed until recorded-generation absence authorizes
pointer reconstruction. Writer and startup lock files are permanent once
created. Each claimant compares held-fd
and root-relative pathname device/inode before owner-slot writes; it never
recreates or proceeds through an unlink/replacement mismatch.

After lease acquisition, every path uses this order for the prior app-server
generation:

1. load the fsynced generation identity and current writer cleanup pointer; no
   generation record yields `Absent`, while a present unreadable record is
   `Unverifiable`;
2. compare the recorded boot-session UUID; a difference proves the entire
   generation `Absent` without consulting leader or group IDs;
3. read `PROC_PIDTBSDINFO`, then `proc_pidpath`, executable device/inode/hash,
   then BSD info again; changed start time is `Unverifiable`. An unavailable
   `proc_pidpath` for a live process is non-dispositive when the remaining
   tuple and group proof match; path replacement alone is not identity mismatch;
4. classify the leader as `Absent`, `Mismatch`, `Match`, or `Unverifiable`;
5. for `Match`, bind an `EVFILT_PROC/NOTE_EXIT` watch and revalidate; `ESRCH`
   while binding proves `Absent`;
6. prove generation absence only when the recorded group has no possible
   surviving member by enumerating `proc_listpgrppids(recorded_pgid)`. Entry
   capacity starts positive, the buffer size is `capacity * sizeof(pid_t)`, and
   capacity doubles whenever the returned PID count equals capacity; `-1`,
   truncation, or invalid `pgid <= 1` is `Unverifiable`. Zero with positive
   capacity proves empty. A same-boot survivor matches only with the recorded
   uid/pgid and start time at or after the recorded leader; an earlier process
   is dismissed. `ESRCH` and zombies are absent. This predicate proves possible
   survival, not signal authority. If neither the leader nor a persisted
   snapshot member is kqueue-bound and revalidated `Match`, possible members
   make the group `Unverifiable` and are not signalled;
7. while continuity is kqueue-bound, snapshot every member's
   PID/UID/PGID/start tuple. Persist `cleanup_in_progress` and the tuples, then signal only
   revalidated snapshot members with `SIGTERM`; after five seconds re-enumerate
   and signal with `SIGKILL` only full-tuple matches from that snapshot. Any new
   or recycled member, or unsnapshotted member after leader loss, is
   `Unverifiable` and never signalled. A later process must rebind/revalidate a
   recorded member before inheriting continuity. `killpg` is never used;
8. confirm group absence within ten seconds,
   then start the new proxy and replace the writer pointer.

`Absent` and safe `Mismatch` observations are audited and proceed. Only
`Unverifiable` returns non-retryable `RECOVERY_REQUIRED`; V1 has no force
override and never resumes the same thread while an old generation may exist.

Every run starts and resumes as a reader. Only `acquire-write` and
`send|submit --write` acquire the lease, before proxy policy replacement or turn
submission. Successful `release-write` activates a reader generation before
unlocking. Pause and close complete proxy cleanup before release; uncertain
writer crashes record a logical `blocked_unknown` that kernel unlock alone does
not clear. `RECOVERY_REQUIRED` from same-run safety checks takes precedence over
`WRITER_BUSY`.

On contention, `writer.json` and the holder runtime record supply nullable
run/profile/state plus lease epoch. An idle holder permits generation-bound
one-shot token issuance. The confirmed requester serializes with `handoff.lock`,
cooperatively activates the holder as a reader, fsyncs its pointer, releases its
lease, then acquires it. Requester failure leaves no writer. Starting, running, waiting, or
unknown holders return retryable `WRITER_BUSY`; they are never signalled, killed,
or queued. Same-user hostile mutation is not a security boundary, but manual
lock-file unlink/replacement is detected by descriptor/path inode checks and
fails closed.

## Turn Execution Flow

1. Validate run state, input, optional idempotency key, model-fixed invariant,
   image readability, and requested effort.
2. If `--write` is present, acquire or confirm the workspace lease and activate
   the writer generation; on conflict, return before recording turn intent.
3. Reserve the idempotency key, append intent, and fsync the ledger.
4. For a threadless run, send `thread/start`, append its provisional thread ID,
   and fsync before `turn/start`. If turn acceptance is uncertain, recover that
   exact provisional thread and retry only when stable history proves no turn
   was accepted or the thread is absent; unreadable/in-progress evidence is
   never retried.
5. Capture a best-effort pre-turn workspace observation.
6. Send `turn/start` with the fixed model, selected reasoning effort, canonical
   cwd, access-derived sandbox policy, approval policy, and message/images.
   Developer instructions were supplied by thread start/resume for this
   generation because turn start has no such field.
7. Persist the permanent thread binding and accepted Codex turn ID before
   acknowledging `submit`.
8. Stream and audit correlated notifications.
9. Surface server requests as generation-qualified pending requests.
10. On terminal notification, read back persisted thread history when necessary,
   capture a post-turn workspace observation, project the final response and
   usage, and transition to idle.

The delivery mode is not part of idempotency identity: `send` waits, while
`submit` returns after step 7.

## Recovery and Reconciliation

Recovery never auto-replays user input. The new worker first replays the ledger,
validates profile identity and compatibility, and inspects the pinned Codex
thread with stable history APIs only after prior generation absence is proved.

- Confirmed idle history resumes normally.
- A terminal turn absent from Dolgorae's projection is appended as reconciled
  evidence and the run returns to idle.
- An active turn without authoritative terminal evidence produces
  `outcome_unknown`; the replacement proxy is stopped and any kernel writer
  lease is released while the workspace pointer remains `blocked_unknown`.
- `reconcile` starts a lease-free transient worker generation, launches
  app-server with read-only sandbox policy, and calls only
  `thread/read(includeTurns: true)`. It never loads or resumes the thread and
  never starts a turn. It appends evidence and transition records, terminates
  the app-server group, and exits. Confirmed terminal evidence moves an unknown
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
  in `running` or `waiting_approval` has an unreachable socket and unverifiable process
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

The detached worker owns an proxy process group distinct from both the
transient CLI and the worker session. Pause, close, recovery, and failed start
obtain leader-or-persisted-member continuity before signalling only snapshot
members, then use a five-second graceful phase, revalidation, and individual
forced termination of remaining snapshot matches. New or recycled members are
never signalled; ten-second absence failure is `RECOVERY_REQUIRED`. Cleanup
results are audited. Close cannot finalize a run whose prior generation remains
unverifiable. A recycled PID or PGID is never killed solely because its number
matches a stale record.

## Runtime and Dependency Boundary

The implementation is Rust 2024 pinned by `rust-toolchain.toml` to 1.97.1 with
rustfmt, clippy, and `aarch64-apple-darwin`; Cargo.lock is committed. Blocking
durability and process work uses dedicated OS threads rather than an async
runtime: control/protocol, stdout, stderr, ledger/state authority,
kqueue/liveness, and `sigwait` each have an explicit owner.

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
`tools/fake_app_server/` that speaks the real stdio JSONL boundary. TASK-004 is
its sole owner; later tasks consume or extend it. Declarative scenarios are
validated against the checked Codex manifest, and the fake shares no production
parser or state-machine code with Dolgorae.

Descendants that deliberately daemonize, escape the process group, or perform
remote side effects cannot be guaranteed to stop. The agent prompt therefore
forbids background processes without explicit master direction.

On worker `SIGTERM`, bounded clean shutdown appends and fsyncs `cleanup_intent`
with reason `generation_shutdown_requested`, rejects new control mutations, and, when a
turn is active, sends `turn/interrupt` and waits up to five seconds for terminal
history before continuing. It then completes child
group cleanup, appends and fsyncs `run_generation_stopped` with shutdown reason and
the last known turn state, releases the writer lease, and attempts bounded startup-lock acquisition
for socket unlink. If that lock cannot be acquired, it leaves stale
coordination files for the next verified owner rather than blocking shutdown,
then exits zero. Failure of any step is recorded when the ledger
remains writable and produces a nonzero exit.

## Security and Trust Boundaries

Dolgorae is a coordination and audit tool, not a hardened multi-user security
boundary.

It does provide:

- user-private local sockets and run files;
- fail-closed account, request, thread, turn, and generation correlation;
- Codex sandbox selection for reader/writer turns;
- a Dolgorae-only writer lease per canonical worktree;
- normative recursive redaction and tamper-evident hash chaining;
- explicit approval and destructive-action boundaries;
- a managed-run context guard against ordinary recursive Dolgorae control.

It does not provide:

- protection from a hostile process running as the same OS user;
- filesystem isolation from editors or non-Dolgorae tools;
- rollback of partial writes;
- attribution of observed changes to Codex;
- control of external MCP/app/plugin side effects;
- serialization of multiple native Codex execution lanes inside the one writer
  app-server;
- control of descendants that deliberately leave the recorded app-server
  process group;
- cryptographic signatures or remote audit attestation;
- direct communication or trust delegation between independent Dolgorae runs;
- protection when a hostile same-user child deliberately removes or forges the
  managed-run context marker.

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
