# Gomchi Architecture

Status: Normative target architecture for the first supported release.

This document owns technical structure and invariants. It describes the system
Gomchi is required to implement; it does not claim that the currently empty
repository already implements it. Product behavior is owned by
[specs.md](specs.md), rationale by
[architecture-decisions.md](architecture-decisions.md), and implementation
progress by [roadmap.md](roadmap.md).

## System Context

Gomchi is a local process supervisor and protocol adapter between a master and
Codex app-server. It adds durable run identity, account binding, access
coordination, recovery, and audit around Codex threads without replacing Codex
conversation storage.

```text
                          user-local target registry
                          argv + expected CODEX_HOME
                                     |
                                     v
master -> gomchi CLI -> per-run worker -> codex app-server -> Codex services
             |              |                  |
          JSON I/O       audit ledger      thread history
```

The master is the only orchestrator of independent Gomchi runs. Codex may
manage native subagents inside a run; those children remain within Codex's
session tree and are not Gomchi peers.

## Component Model

### CLI Front End

The visible `gomchi` invocation is short-lived. It:

1. resolves the canonical workspace;
2. parses and validates machine-oriented input;
3. resolves the explicit run ID when required;
4. discovers or starts the owning worker;
5. exchanges one request/response or one event stream with the worker;
6. emits the stable stdout envelope and exits.

It never talks directly to app-server and never writes the audit ledger while a
worker owns the run. Start-time bootstrap is the only period in which the
front-end may create the run directory and initial records before worker
ownership transfers.

### Per-Run Worker

The worker is a hidden re-execution mode of the same `gomchi` binary. It is the
sole owner of:

- the run control socket;
- the app-server child and its stdio pipes;
- JSON-RPC request IDs and correlation state;
- run lifecycle and pending interactions;
- the audit append handle and materialized state;
- the writer lease when the run has write access;
- cleanup of the app-server process group.

One worker serves one run. There is no shared supervisor or global in-memory
registry. Concurrent start/recovery attempts for the same run are serialized by
a per-run startup lock.

The worker is detached from the transient CLI before it accepts requests. It
runs through the hidden `__worker` argv mode after a single fork and `setsid()`,
with no controlling terminal, null stdin/stdout/stderr, and `umask(077)`. It
ignores terminal-originated `SIGINT` and `SIGHUP` for itself and handles
`SIGTERM` as a bounded shutdown request. Before every app-server exec, the child
restores those dispositions and the signal mask to defaults so ignored signals
do not leak into Codex or its descendants. Early-start and internal diagnostics
go to a 0600 log capped at 1 MiB with one rotation. The CLI holds byte 0 of the
run's startup lock before fork. As its first post-`setsid`/re-exec operation,
the worker opens that lock once, acquires byte 1, and never closes or reopens
the descriptor while serving. Startup fd 3 may emit `bound` only after byte 1
is held, the socket is bound, and the runtime identity is atomically persisted
and directory-fsynced; the CLI then releases byte 0. A later `ready` object
means replay and compatibility validation completed. A structured failure may
replace either acknowledgement. The bound wait is ten seconds; the ready wait
is the larger operation-specific startup budget. EOF before an expected object
is `TRANSPORT_FAILURE`; timeout never authorizes signalling the worker. The fd 3
write end and startup-lock descriptor are `FD_CLOEXEC` before app-server launch.
These rules make ownership handoff explicit and ensure
that Ctrl-C or command substitution cannot terminate the worker or keep the
caller's output pipe open.

### Codex App-Server Child

The worker starts target argv with app-server's stdio transport. Stdin and
stdout carry newline-delimited JSON-RPC messages; stderr is captured as bounded
diagnostic evidence and must not corrupt stdout protocol parsing.

Before launch, the worker removes inherited `GOMCHI_*` control variables and
adds a fresh managed-run context containing its own workspace and run identity.
The public CLI recognizes this context and exposes only read-only introspection
of that same run. This is an accidental-recursion guard, not an unforgeable
credential.

Each connection performs exactly one `initialize` request followed by the
`initialized` notification. A newly created run remains threadless until its
first turn because pinned Codex does not persist a turnless thread across
app-server restart. First `send`/`submit` uses `thread/start`; recovered runs use
`thread/resume`, and history forks use `thread/fork`. After first-turn
acceptance the worker maintains exactly one loaded Gomchi-owned thread.
Turns use `turn/start`, `turn/steer` only when explicitly exposed by a future
specification, and `turn/interrupt` for cancellation. V1 does not enable the
experimental app-server API capability.

The app-server is created with `POSIX_SPAWN_START_SUSPENDED` and a new process
group. While it is suspended, the worker obtains a provisional identity with
`PROC_PIDTBSDINFO`, `proc_pidpath`, and a second BSD-info sample, then writes it
with temp-file, file `fsync`, rename, and directory `fsync` before `SIGCONT`.
Failure before continuation is cleaned as a start failure. After the initialize
handshake the worker repeats this process-identity sample; only this post-exec
identity, including executable device, inode, and SHA-256, is authoritative for
the fsynced `generation_started` record. A provisional path mismatch is not
dispositive before that record. Wrapper argv and the final live executable are
distinct recorded facts.

### Target Registry

The XDG registry is user-local and independent of every workspace. It stores no
tokens and no arbitrary environment map. A run manifest contains a private
snapshot so registry edits cannot silently change an existing run's account.

### Persistent Run Store

The run store is workspace-local and ignored by Git. `audit.jsonl` is the only
event authority. `state.json`, transcripts, status views, and exports are
projections. The Codex thread remains independently stored in the pinned
`CODEX_HOME`.

## Process and Transport Topology

For N live runs, the normal baseline is N workers and N app-server children.
Commands or Codex native subagents may temporarily create additional descendant
processes.

```text
gomchi CLI invocation
  |
  | Unix domain socket: framed JSONL request/response/notifications
  v
gomchi worker [run R, generation G]
  |
  | stdin/stdout: app-server JSONL
  v
target argv ... app-server --listen stdio://
  |
  +-- Codex-owned command and native-subagent descendants
```

The worker control socket resides below `/tmp/gomchi-<uid>/s/`, whose directory
is opened once without following symlinks and accepted only when it is owned by
the current uid with mode 0700. No-symlink enforcement applies below the
resolved `/tmp` root; operations beneath the directory use descriptor-relative
`*at()` calls. Its filename is the RFC 4648 uppercase, unpadded, 32-character
base32 encoding of the first 160 bits of SHA-256 over canonical workspace path,
a NUL byte, and run ID.
The composed path must fit the macOS `sun_path` limit; overflow fails with
`RUNTIME_PATH_INVALID`. An existing path whose recorded full workspace/run
identity differs fails with `RUNTIME_PATH_COLLISION`. A sibling identity sidecar
is written atomically at bind and contains the full workspace/run identity,
boot-session UUID, worker identity, and protocol version; stale-socket collision
checks never depend on a listener answering. Every request also contains the
full workspace identity, run ID, expected worker generation, and boot UUID.
Ordinary requests additionally carry Gomchi version, CLI binary digest, and the
current mutation protocol version so cross-run and unsafe version-skewed
connections fail closed. A separate version-frozen control protocol v1 accepts
only `hello`, bounded `status`, and `shutdown` across binary-digest changes.
Those operations validate workspace, run, generation, boot, and live process
identity; all other requests reject version skew. `shutdown` is identity-bound
and interrupts an active turn before cleanup.

The actual socket path and process identity are discoverable from
`.gomchi/runtime/runs/<run-id>.json`; discovery never recomputes a path from
`$TMPDIR`. The record contains full worker and app-server identity tuples: PID,
PGID, UID, start seconds/microseconds, live executable path, executable
device/inode, and executable SHA-256, together with
the boot-session UUID, process generation, access state, socket path, Gomchi
version, binary digest, and IPC protocol version.
`.gomchi/runtime/writer.json` points to the current writer run/generation and
stores the incumbent identity plus `cleanup_in_progress`; it is replaced only
after cleanup is confirmed. Runtime records use write-temp, `fsync`, rename,
and directory `fsync`. They are recoverable coordination caches; the fsynced
`generation_started` ledger record is process-identity authority.

Writer leases and per-run startup locks live below the workspace-recorded
persistent private local root, never a value recomputed from the current
environment. `gomchi init` resolves an explicit `--state-root` or the initial
XDG/fallback value once and records its canonical path and root device/inode in
`.gomchi/runtime/lock-root.json` and every run manifest. If the workspace record
is missing while runs exist, it is reconstructed only from unanimous manifest
values; disagreement fails closed. The root is opened and created through
descriptor-relative, no-symlink operations, must be current-uid-owned mode 0700,
and must report `MNT_LOCAL`. Later environment changes are ignored; path or
device/inode drift is `RUNTIME_PATH_COLLISION`.

Writer files are `locks/writer/<workspace-digest>` and startup files are
`locks/startup/<workspace-and-run-digest>`; the mechanisms never share an
inode. The writer lease is nonblocking BSD `flock(2)`. The startup file has two POSIX
byte-range locks: byte 0 is the transient CLI starter claim and byte 1 is the
worker lifetime claim. The file body has separate fixed-size checksummed byte-0
and byte-1 owner records; after acquiring its byte
a process updates its slot with `pwrite` on that same fd and fsyncs before
proceeding, then clears and fsyncs the slot immediately before releasing.
`F_GETLK` is
queried for both ranges; `l_pid <= 0` is `Unverifiable`, while a positive
`l_pid` is only a hint and is always checked against
the matching owner record. Normal
attachment to an answering socket takes neither byte. After ten seconds, a
contender may terminate only an exact byte-0 transient starter bound by kqueue
and revalidation. A byte-1 owner is a serving reader or writer worker and
requires control `hello` plus the additional 30-second no-progress/abort guard;
`Mismatch` or `Unverifiable` is never signalled and returns `RUN_BUSY`. After
exact exit, contenders race to acquire byte 0 and only the winner starts or
recovers. Each owner process opens the file once; identity revalidation uses
`fstat` on that held fd and never a second open, including through a hardlink.
The descriptor is never reopened or closed during ownership and is marked
close-on-exec before app-server launch.

The worker/app-server transport intentionally uses stdio even though app-server
also offers Unix and WebSocket listeners. Stdio gives the worker exclusive
ownership, lifecycle coupling, and a single audit interposition point.

App-server stdout and stderr are drained independently of every CLI observer.
Observers read fsynced ledger records by cursor and therefore cannot exert
backpressure on the active protocol stream. Limits are 16 MiB per stdout line,
1 MiB per stderr line, 1 MiB per represented ledger payload, and 8 MiB per
CLI-worker frame. CLI oversize affects only its caller; stderr oversize retains
metadata and continues. Unsolicited stdout remains capped at 16 MiB per line;
invalid or oversized unsolicited stdout stops the generation and quarantines
an accepted active turn as `outcome_unknown`. A solicited `thread/read` response
is consumed by a constant-memory streaming visitor that retains only required
turn/status fields plus length and streaming hash. It has bounded time but no
arbitrary total response-size cap; timeout or invalid structure fails only that
command and does not mutate run state.

## Workspace Identity and Local Layout

The canonical workspace ID is a digest of:

- the canonical Git top-level path in Git mode; or
- the canonical initialized directory in non-Git mode.

The repository-local layout is:

```text
.gomchi/
  .gitignore                 # tracked
  config.toml                # tracked portable policy
  runs/                      # ignored, 0700
    <uuidv7>/
      manifest.json          # 0600, fixed run facts
      audit.jsonl            # 0600, append-only authority
      state.json             # 0600, replaceable projection
      worker.log             # 0600, bounded startup/runtime diagnostics
      worker.log.1           # 0600, single rotated diagnostic log
      recovery/              # 0700, preserved crash-tail evidence
  runtime/                   # ignored, recoverable local coordination
    lock-root.json           # 0600, workspace lock-root authority
    writer.json              # 0600, current writer/cleanup pointer
    runs/<uuidv7>.json       # 0600, per-run process/socket discovery
  cache/                     # ignored, replaceable compatibility data
```

No absolute target path, socket path, PID, authentication state, or run state is
placed in tracked project policy.

## Manifest and Ledger Model

### Manifest

The manifest is created before externally meaningful app-server work and then
completed with facts learned during start. Its fixed semantic fields include:

- schema version, run ID, canonical workspace and workspace ID;
- Git/non-Git mode and start baseline;
- created timestamp and initial access;
- target name, argv, expected `CODEX_HOME` snapshot;
- actual app-server version, schema status, and actual `codexHome`;
- Gomchi version, binary SHA-256, and IPC protocol version;
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
timestamp_utc
run_id
process_generation
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

Inbound JSON is parsed with duplicate-member rejection and arbitrary-precision
number lexemes. Before any Gomchi marker is inserted, every inbound object key
matching `^\$+gomchi_` is escaped by prefixing one additional `$`. Redaction is
then applied, followed by numeric adaptation; the tokenizer never treats a
Gomchi-owned marker key as a candidate secret key. A number that cannot
round-trip exactly through IEEE-754 binary64 is replaced before JCS with
`{"$gomchi_number":"<original-lexeme>"}`. Invalid
JSON, duplicate members, and otherwise unrepresentable payloads never reach the
canonicalizer. Verification requires each stored line, excluding its newline,
to be byte-identical to the JCS serialization of its own parse. Timestamps use
UTC RFC 3339 with exactly six fractional digits and `Z`.

Each complete line is appended to an `O_APPEND` handle with `write(2)` retried
until all bytes are written. Ordinary streaming records may be group-committed
for at most 100 milliseconds. Using `fsync(2)`, the ledger is synchronized
before any byte initiating an irreversible action is written to app-server
stdin: turn intent and its idempotency reservation precede `turn/start`, and an
approval-decision record precedes the response. It is also synchronized before
Gomchi acknowledges an accepted turn ID, pending master interaction, terminal
result, or access/lifecycle change. Manifest creation and atomic state
replacement synchronize their containing directories. V1 claims process- and
OS-crash durability after these barriers, not power-loss durability.

Any malformed or invalid newline-terminated record, including the final record,
a complete record with a broken hash, or any sequence discontinuity is an
audit-integrity failure. Only nonempty bytes after the file's last newline are
a recoverable torn tail, even when those bytes parse as a complete JSON object
but lack the terminating newline. Recovery preserves them under `recovery/`,
truncates only those bytes, and appends a `ledger_tail_repaired` record. A torn
tail is never reported as ordinary tampering.

Each ledger payload is at most 1 MiB after redaction and representation. A
larger or unrepresentable payload becomes a `payload_unrepresentable` record
containing source kind, observed byte length, streaming SHA-256, JSON Pointer
when known, and reason; no original bytes or sidecar are retained. The ledger
includes CLI intent accepted by the worker, lifecycle transitions,
process generations, app-server requests/responses/notifications after
redaction, approval decisions, state reconciliation, and cleanup results. Its
completeness claim covers Gomchi lifecycle, main-turn wire traffic exposed by
app-server, approvals, access transitions, and target/account provenance. Native
subagent or other content not exposed in plaintext by app-server is retained
only as an opaque event and is not claimed as reconstructable audit. Hidden
reasoning and unexposed communication are outside the ledger's authority.

### Materialized State

`state.json` is atomically replaced only to a head at or before the last fsynced
ledger record, at durability barriers and a bounded projection interval. It
contains the current lifecycle state, process generation, thread ID, active/latest turn,
pending requests, access mode, writer lease observation, default effort, last
event cursor, and ledger head. If it is missing, stale, or invalid, the worker
replays `audit.jsonl` before accepting commands.
If its head is ahead of the durable ledger, it is a stale projection rather
than an audit-integrity failure: Gomchi rebuilds it and appends
`projection_rewound`.

## State Machine

The worker is the single state-transition authority for its run.

| State | Worker expected | App-server expected | New turn | Operation results |
| --- | --- | --- | --- | --- |
| `starting` | Maybe | Maybe | No | `idle` or `start_failed` |
| `idle` | Yes | Yes | Yes | `running`, `paused`, or `closed` |
| `running` | Yes | Yes | No | `idle`, `waiting_*`, or `outcome_unknown` |
| `waiting_approval` | Yes | Yes | No | `running`, `idle`, or `outcome_unknown` |
| `waiting_input` | Yes | Yes | No | `running`, `idle`, or `outcome_unknown` |
| `waiting_mcp` | Yes | Yes | No | `running`, `idle`, or `outcome_unknown` |
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
requests are wrapped in a Gomchi request ID that includes process generation.

Unknown responses, mismatched IDs, duplicate terminal events, invalid state
transitions, and unknown server requests are recorded and fail closed. Unknown
additive notifications do not change state and are retained as raw redacted
evidence.

A new worker/app-server lifetime increments process generation. Pending
requests and generation-scoped approvals never cross that boundary.

## Writer Lease Architecture

The lease is BSD `flock(2)` with `LOCK_EX | LOCK_NB` below the workspace-recorded
lock root. Multiple readers open no writer lease. Each canonical Git worktree
is a distinct workspace and therefore a distinct writer lane. An unresolved
`writer.json` entry for another run is audited but does not gate acquisition:
the kernel lease and the acquiring run's own generation safety determine who
may attempt it. It does gate writer activation: the held inode must match the
recorded lease inode and the foreign app-server generation must be proven absent
or identity-verified and cleaned before a new writer app-server starts.
`Unverifiable` releases the new lease with `RECOVERY_REQUIRED`.

Only the worker holds the lease. The descriptor is `FD_CLOEXEC` and is never
inherited by app-server or descendants. Lock lifetime covers starting, idle,
running, and waiting states and releases on start failure.

Recovery of every byte-1 serving worker, reader or writer, uses the same
control/progress guard. A successful control `hello` attaches and does not replace anything. If
the recorded worker is alive but `hello` does not complete within ten seconds,
the recovering CLI applies the same boot UUID, process/executable identity,
kqueue, and revalidation rules to the worker itself, then observes an
additional 30 seconds with no ledger, runtime-generation, bounded-log, or CPU
progress. Exit, generation advance, or lock-owner change aborts takeover. Only
then may a `Match` receive TERM, the five-second grace, and KILL; exact worker
exit must be observed. For a writer, this is the pre-acquisition step and its
flock must also be confirmed released. `Unverifiable` fails with
`RECOVERY_REQUIRED`. The replacement then
competes for the startup lock, rechecks the socket/runtime state, and acquires
the writer lease normally only if it wins. The run-keyed startup-lock election
serializes same-run replacements and never authorizes one run to terminate
another run's healthy writer. If either a worker during shutdown or a transient
starter is wedged while holding the startup lock, the `F_GETLK` owner recovery
above breaks that deadlock before this sequence continues.

After acquisition and at every destructive barrier, the worker compares the
held descriptor's `fstat` device/inode with a root-fd-relative `fstatat` of the
lease pathname. The pair is stored in `writer.json`; mismatch, including manual
unlink/replacement, fails closed with `RUNTIME_PATH_COLLISION`.
Startup lock files are permanent once created. Each claimant compares held-fd
and root-relative pathname device/inode before owner-slot writes; it never
recreates or proceeds through an unlink/replacement mismatch.

After lease acquisition, every path uses this order for the prior app-server
generation:

1. load the fsynced generation identity and current writer cleanup pointer;
2. compare the recorded boot-session UUID; a difference proves the entire
   generation `Absent` without consulting leader or group IDs;
3. read `PROC_PIDTBSDINFO`, then `proc_pidpath`, executable device/inode/hash,
   then BSD info again; changed start time is `Unverifiable`. An unavailable
   `proc_pidpath` for a live process is non-dispositive when the remaining
   tuple and group proof match; path replacement alone is not identity mismatch;
4. classify the leader as `Absent`, `Mismatch`, `Match`, or `Unverifiable`;
5. for `Match`, bind an `EVFILT_PROC/NOTE_EXIT` watch and revalidate;
6. prove generation absence only when the recorded group has no possible
   surviving member by enumerating `proc_listpgrppids(recorded_pgid)`. Capacity
   starts positive and doubles whenever returned count equals capacity; `-1`,
   truncation, or invalid `pgid <= 1` is `Unverifiable`. Zero with positive
   capacity proves empty. A same-boot survivor matches only with the recorded
   uid/pgid and start time at or after the recorded leader; an earlier process
   is dismissed. `ESRCH` and zombies are absent. This predicate proves possible
   survival, not signal authority. If the leader was not first revalidated
   `Match`, possible members make the group `Unverifiable` and are not signalled;
7. while the exact leader is kqueue-bound and alive, snapshot every member's
   PID/UID/PGID/start tuple. Persist `cleanup_in_progress` and signal only
   revalidated snapshot members with `SIGTERM`; after five seconds re-enumerate
   and signal with `SIGKILL` only full-tuple matches from that snapshot. Any new
   or recycled member, or unsnapshotted member after leader loss, is
   `Unverifiable` and never signalled. `killpg` is never used;
8. confirm group absence,
   then start the new app-server and replace the writer pointer.

`Absent` and safe `Mismatch` observations are audited and proceed. Only
`Unverifiable` returns non-retryable `RECOVERY_REQUIRED`; V1 has no force
override and never resumes the same thread while an old generation may exist.

Effective-write start (explicit or project default), promotion, write resume,
write fork, and recovery of a
writer run are the complete writer-acquisition set. Each acquires the lease and
completes the same stale
same-run generation check before the run becomes usable. `RECOVERY_REQUIRED`
from that check takes precedence over a later `WRITER_BUSY`. Demotion changes future turn
policy and releases the lease only after the run is idle. Pause and close
complete child cleanup before lease release. Entry into `outcome_unknown` stops
the app-server and immediately releases the lease. Same-user hostile mutation
is not a security boundary, but manual lock-file unlink/replacement is detected
by the descriptor/path inode check and fails closed.

## Turn Execution Flow

1. Validate run state, input, optional idempotency key, model-fixed invariant,
   image readability, and requested effort.
2. Reserve the idempotency key, append intent, and fsync the ledger.
3. For a threadless run, send `thread/start`, append its provisional thread ID,
   and fsync before `turn/start`. If turn acceptance is uncertain, recover that
   exact provisional thread and retry only when stable history proves no turn
   was accepted or the thread is absent; unreadable/in-progress evidence is
   never retried.
4. Capture a best-effort pre-turn workspace observation.
5. Send `turn/start` with the fixed model, selected reasoning effort, canonical
   cwd, access-derived sandbox policy, approval policy, message/images, and
   effective developer instructions.
6. Persist the permanent thread binding and accepted Codex turn ID before
   acknowledging `submit`.
7. Stream and audit correlated notifications.
8. Surface server requests as generation-qualified pending requests.
9. On terminal notification, read back persisted thread history when necessary,
   capture a post-turn workspace observation, project the final response and
   usage, and transition to idle.

The delivery mode is not part of idempotency identity: `send` waits, while
`submit` returns after step 6.

## Recovery and Reconciliation

Recovery never auto-replays user input. The new worker first replays the ledger,
validates target identity and compatibility, and inspects the pinned Codex
thread with stable history APIs only after prior generation absence is proved.

- Confirmed idle history resumes normally.
- A terminal turn absent from Gomchi's projection is appended as reconciled
  evidence and the run returns to idle.
- An active turn without authoritative terminal evidence produces
  `outcome_unknown`; the replacement app-server is stopped and any writer lease
  is released.
- `reconcile` starts a lease-free transient worker generation, launches
  app-server with read-only sandbox policy, and calls only
  `thread/read(includeTurns: true)`. It never loads or resumes the thread and
  never starts a turn. It appends evidence and transition records, terminates
  the app-server group, and exits. Confirmed terminal evidence moves an unknown
  run to `paused`; later bare resume uses read access.
- Normal fork from `outcome_unknown` uses stable `lastTurnId` through the last
  confirmed terminal turn only after prior generation absence is proved.
  `fork --fresh` reads the immutable source manifest and read-only fsynced
  state/runtime projections needed for eligibility and provenance. It never
  reads the source Codex thread or mutates/repairs the source ledger or any
  source projection. It creates an empty threadless read-only run and
  records source run, observed lifecycle state, and unresolved-turn provenance
  without asserting an outcome. It is therefore available even when a source
  in `running` or `waiting_*` has an unreachable socket and unverifiable process
  identity. Explicit `--access write` is rejected rather than downgraded.

Every fork copies the source target snapshot and immutable run instructions,
defaults to read access, and may replace only the fixed model. It cannot change
the target or account boundary.

The original source ledger is never rewritten during reconciliation or fork.
Projection-only `status`, `events`, and `verify` return their data with an
identity-verdict field and do not fail merely because it is `Unverifiable`.
They read fsynced projections directly and never start, attach, or recover a
worker.

## Process Cleanup

The detached worker owns an app-server process group distinct from both the
transient CLI and the worker session. Pause, close, recovery, and failed start
verify every enumerated member before sending individual signals, then use a
five-second graceful phase followed by re-enumeration, forced termination of
remaining matches, and confirmed group exit. Cleanup
results are audited. Close cannot finalize a run whose prior generation remains
unverifiable. A recycled PID or PGID is never killed solely because its number
matches a stale record.

Descendants that deliberately daemonize, escape the process group, or perform
remote side effects cannot be guaranteed to stop. The agent prompt therefore
forbids background processes without explicit master direction.

On worker `SIGTERM`, bounded clean shutdown fsyncs
`generation_shutdown_requested`, rejects new control mutations, and, when a
turn is active, sends `turn/interrupt` and waits up to five seconds for terminal
history before continuing. It then completes child
group cleanup, appends and fsyncs `generation_ended` with the last known turn
state, releases the writer lease, and attempts bounded startup-lock acquisition
for socket/sidecar unlink. If that lock cannot be acquired, it leaves stale
coordination files for the next verified owner rather than blocking shutdown,
then exits zero. Failure of any step is recorded when the ledger
remains writable and produces a nonzero exit.

## Security and Trust Boundaries

Gomchi is a coordination and audit tool, not a hardened multi-user security
boundary.

It does provide:

- user-private local sockets and run files;
- fail-closed account, request, thread, turn, and generation correlation;
- Codex sandbox selection for reader/writer turns;
- a Gomchi-only writer lease per canonical worktree;
- normative recursive redaction and tamper-evident hash chaining;
- explicit approval and destructive-action boundaries;
- a managed-run context guard against ordinary recursive Gomchi control.

It does not provide:

- protection from a hostile process running as the same OS user;
- filesystem isolation from editors or non-Gomchi tools;
- rollback of partial writes;
- attribution of observed changes to Codex;
- control of external MCP/app/plugin side effects;
- serialization of multiple native Codex execution lanes inside the one writer
  app-server;
- control of descendants that deliberately leave the recorded app-server
  process group;
- cryptographic signatures or remote audit attestation;
- direct communication or trust delegation between independent Gomchi runs;
- protection when a hostile same-user child deliberately removes or forges the
  managed-run context marker.

## Compatibility Boundary

The protocol adapter is a strict subset client. Its checked manifest lists every
request, response, notification, enum variant, and required field on which
Gomchi state depends. Compatibility doctor generates the stable schema into a
temporary directory, compares that subset, then performs a live handshake and
model probe.

The tested 0.147.0 manifest is `tested`. A newer compatible version is
`unverified` and that verdict is written to every run generation. Older or
otherwise unlisted versions are rejected unless a future SOT revision adds
them to the tested set.

Runtime code tolerates additive data but never infers lifecycle progress from
uncorrelated or unknown messages. This preserves compatibility without claiming
that a schema probe can guarantee all future runtime behavior.
