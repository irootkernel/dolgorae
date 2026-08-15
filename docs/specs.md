# Gomchi Product Specification

Status: Normative target specification for the first supported release.

This document owns Gomchi's externally observable behavior. Technical structure
is owned by [architecture.md](architecture.md), decision rationale by
[architecture-decisions.md](architecture-decisions.md), and delivery state by
[roadmap.md](roadmap.md). A contradiction between SOT documents is an invalid
state and must be reconciled before an implementation task becomes active.

The key words **MUST**, **MUST NOT**, **SHOULD**, and **MAY** are normative.

## Definitions

- **Master**: the user or AI agent that invokes the `gomchi` CLI and owns
  orchestration decisions.
- **Run**: one durable Gomchi session, identified by a UUIDv7 and bound to one
  Codex thread.
- **Turn**: one identified Codex execution within a run, beginning when
  `turn/start` is accepted and ending only when Codex confirms completed,
  interrupted, or failed status.
- **Worker**: the hidden per-run Gomchi process that owns the app-server child,
  the worker control socket, and the run audit writer.
- **Process generation**: one worker plus app-server lifetime within a run.
- **Target**: a user-local, named Codex execution configuration consisting of
  shell-free argv and an expected `CODEX_HOME`.
- **Reader**: a run whose turns use Codex read-only sandbox policy.
- **Writer**: the single run that holds the Gomchi writer lease for a canonical
  workspace and whose turns may use workspace-write sandbox policy.
- **Terminal turn**: a turn confirmed as completed, interrupted, or failed.
- **Forkable turn**: a terminal turn whose exact status is listed in the
  checked Codex required-subset manifest as accepted for `lastTurnId` by the
  pinned target. Terminal and forkable are intentionally not synonyms.

## SPEC-001: Product Boundary and Supported Environment

Gomchi MUST provide persistent Codex subagents through one distributable
`gomchi` executable. It MUST NOT install a global daemon, project daemon,
launchd unit, Codex binary, authentication material, or `CODEX_HOME`.

The first supported release is a personal alpha for Apple Silicon macOS
(`aarch64-apple-darwin`). Intel macOS, Linux, Windows, public installers, and
automatic updates are not supported release targets.

Gomchi depends on user-prepared Codex targets. The compatibility baseline is
Codex app-server 0.147.0.

## SPEC-002: Workspace Initialization and Discovery

`gomchi init [PATH]` MUST initialize a Git workspace. `gomchi init --non-git
[PATH]` MUST explicitly opt a general directory into Gomchi. A run MUST NOT
start in an uninitialized workspace.

In Git mode, Gomchi runs
`git -c core.quotePath=true -C <supplied-existing-directory> rev-parse --show-toplevel`
without a shell and requires exit 0 and exactly one LF-terminated stdout
result; bounded stderr is diagnostic only when exit is zero. A double-quoted result is decoded with Git's documented
C-style path quoting (including octal byte escapes); an unquoted result is the
bytes before the sole final LF. Invalid quoting, trailing output, or NUL is a
Git discovery failure. The canonical workspace is libc `realpath(3)` applied to
that decoded existing directory, even when the supplied path is a subdirectory.
In non-Git mode it is `realpath(3)` applied to the existing initialized
directory. The canonical path is the returned absolute
POSIX byte sequence with no trailing slash except for root. Gomchi performs no
additional Unicode normalization or case folding: filesystem alias resolution,
including symlink and case-insensitive lookup, belongs to `realpath(3)`.

The workspace digest is lowercase hexadecimal SHA-256 over
`"gomchi-workspace-v1\0"` followed by those canonical path bytes. The full
64-character digest is the workspace ID and writer-lock filename. The startup
filename is lowercase hexadecimal SHA-256 over
`"gomchi-startup-v1\0" || workspace_digest_bytes || run_uuid_bytes`. The short
socket name is RFC 4648 uppercase unpadded base32 of the first 160 bits of
SHA-256 over
`"gomchi-socket-v1\0" || workspace_digest_bytes || run_uuid_bytes`. The manifest
records both the canonical path bytes in the lossless path representation and
the full workspace digest. Here `workspace_digest_bytes` is the raw 32-byte
SHA-256 result, not its hex text, and `run_uuid_bytes` is the UUID's 16 bytes in
RFC 4122/network order, not its hyphenated text. Every component MUST use these
same preimages.

Each linked Git worktree has its own canonical top-level path and is therefore
a separate Gomchi workspace, run store, and writer lease. Gomchi supports one
writer lane per worktree; it does not serialize writers across worktrees that
share a common Git directory.

Dirty Git workspaces are allowed. Run creation MUST record a read-only baseline
containing HEAD, branch, tracked changes, and untracked paths. Gomchi MUST NOT
discard, reset, stash, or otherwise rewrite pre-existing changes.

Later commands discover the nearest ancestor containing `.gomchi`; an explicit
`--workspace PATH` overrides discovery. Discovery selects a workspace only. It
MUST NOT implicitly select a run.

Git-mode `.gomchi` MUST be at the canonical Git top level. `--non-git` is
rejected for a path inside any Git worktree, and a nested `.gomchi` below an
already initialized workspace is rejected. Non-Git mode records an empty Git
baseline. Absence of the `git` executable, a Git version older than 2.39, or a
Git discovery failure returns `WORKSPACE_INITIALIZATION_CONFLICT` rather than
silently falling back to non-Git mode.

In Git mode, initialization creates exactly these tracked project policy files:

```text
.gomchi/
  .gitignore
  config.toml
```

`config.toml` contains exactly `schema_version = 1`, `mode = "git"|"non_git"`,
and `default_access = "read"|"write"`. The initial default access is `read`.
Unknown or duplicate keys, wrong types, unsupported schema versions, and
malformed TOML return `CONFIG_INVALID`. The file is hand-editable, but Gomchi
never rewrites it except during first initialization. `.gomchi/.gitignore`
ignores `runs/`, `runtime/`, and `cache/`, but MUST NOT ignore `.gomchi` as a
whole.

Non-Git initialization creates the same two files and the same ignore contents,
but makes no claim that either file is tracked. The ignore file remains useful
if the directory is later placed below a version-control workspace.

Initialization uses create-exclusive temporary files, file `fsync`, rename,
and parent-directory `fsync`. Repeating `init` succeeds with `created:false`
only when the recorded mode, canonical workspace, state-root authority, schema,
and existing policy files are byte-for-byte compatible. It never overwrites an
existing tracked policy file. A partial layout, nested workspace, changed mode,
changed state root, or conflicting policy returns
`WORKSPACE_INITIALIZATION_CONFLICT`.

## SPEC-003: Target and Account Binding

The user-global target registry lives at:

```text
${XDG_CONFIG_HOME:-$HOME/.config}/gomchi/targets.toml
```

A target contains:

- a unique name;
- shell-free executable argv;
- an absolute expected `CODEX_HOME`.

`targets.toml` has top-level `schema_version = 1` and a `targets` table keyed by
name. Each entry contains only nonempty `argv = [string, ...]` and absolute
`codex_home = string`. Unknown or duplicate keys, empty argv, relative homes,
wrong types, malformed TOML, and unsupported schema versions return
`TARGET_REGISTRY_INVALID`. Target add/remove holds a user-registry lock and uses
write-temp, file `fsync`, rename, and directory `fsync`; the registry is
hand-editable and stores no environment values.

Target names are unique. `target add` MUST reject an existing name with
`TARGET_ALREADY_EXISTS`; it MUST NOT overwrite a target implicitly. Replacement
requires an explicit remove followed by add.

Gomchi MUST set the target's `CODEX_HOME`, inherit the ordinary parent process
environment, and strip inherited Gomchi-internal variables before starting
Codex. It then injects a fresh, non-secret managed-run context marker used to
reject recursive Gomchi control from that process tree. A target MAY use wrapper
argv for additional environment preparation. The registry MUST NOT support an
arbitrary secret environment map.

`run start` MUST require an explicit target. Before use, Gomchi MUST validate
the executable, version, app-server schema, initialization handshake, login
readiness, model listing, and actual `codexHome`. A `codexHome` mismatch is a
hard failure.

Run creation snapshots the target name, argv, and expected `CODEX_HOME` into the
run manifest. Later registry edits or deletion affect new runs only. Existing
runs MUST NOT be retargeted to another account or `CODEX_HOME`. An executable
that changes at the same path is revalidated for every new process generation.

## SPEC-004: Runtime and Session Identity

A run owns no Codex thread before its first turn and exactly one thereafter.
One live run generation owns exactly one worker and one app-server child.
Gomchi imposes no artificial run-count limit.

The required control path is:

```text
master
  -> gomchi CLI (JSON on stdin/stdout)
  -> per-run worker (Unix domain socket)
  -> codex app-server child (stdio JSONL)
  -> zero Codex threads before first turn; exactly one thereafter
```

The master MUST NOT connect directly to app-server. The worker is the sole
app-server client and audit interposer. The CLI-worker socket MUST use a short
user-private runtime path derived from the canonical workspace identity and run
ID; durable state remains under `.gomchi/runs/`.

Run IDs are UUIDv7 values. V1 has no run aliases and no current-run pointer.
Every run-scoped command MUST receive the run ID explicitly.

The CLI-worker handshake includes schema version, Gomchi semantic version,
binary SHA-256, workspace/run identity, and expected process generation. A
mismatch returns `GOMCHI_PROTOCOL_MISMATCH`; upgrade does not silently mix CLI
and worker versions within one run generation.

During `starting`, the worker or app-server may not yet exist. Once started,
both remain alive while their run is idle, running, or waiting, including idle
periods. There is no automatic idle shutdown. Logout, reboot, pause, close,
outcome-unknown quarantine, or failure may stop them. No launchd recovery is
installed. Every command in the error table that acquires or attaches a worker,
acquires the writer lease, or verifies a prior generation performs the same
on-demand discovery/recovery procedure before its ordinary operation.

## SPEC-005: CLI Surface

The initial public command surface is:

```text
gomchi [--human] init [PATH] [--non-git] [--state-root <absolute-local-path>]

gomchi [--human] target add <name> --codex-home <absolute-path> -- <argv...>
gomchi [--human] target list
gomchi [--human] target show <name>
gomchi [--human] target remove <name>
gomchi [--human] target doctor <name>

gomchi [--human] run start --workspace <path> --target <name> [--access read|write] [--model <model>] [--effort <effort>] [--instructions <text> | --instructions-file <path> | --instructions-stdin]
gomchi [--human] run list [--workspace <path>]
gomchi [--human] run status <run-id> [--workspace <path>]
gomchi [--human] run send <run-id> [--workspace <path>] [--message <text>] [--image <auto|low|high>=<path>]... [--effort <effort>] [--idempotency-key <key>] [--timeout <duration>]
gomchi [--human] run submit <run-id> [--workspace <path>] [--message <text>] [--image <auto|low|high>=<path>]... [--effort <effort>] [--idempotency-key <key>]
gomchi [--human] run wait <run-id> <turn-id> [--workspace <path>] [--timeout <duration>]
gomchi [--human] run events <run-id> [--workspace <path>] [--after <cursor>] [--follow] [--raw]
gomchi [--human] run pending <run-id> [--workspace <path>]
gomchi [--human] run respond <run-id> --request-id <id> [--workspace <path>] [--response <json>]
gomchi [--human] run interrupt <run-id> [--workspace <path>]
gomchi [--human] run set-effort <run-id> <effort> [--workspace <path>]
gomchi [--human] run promote <run-id> [--workspace <path>]
gomchi [--human] run demote <run-id> [--workspace <path>]
gomchi [--human] run pause <run-id> [--workspace <path>] [--interrupt]
gomchi [--human] run resume <run-id> [--workspace <path>] [--access read|write]
gomchi [--human] run recover <run-id> [--workspace <path>] [--accept-version-change]
gomchi [--human] run reconcile <run-id> [--workspace <path>]
gomchi [--human] run fork --from <run-id> [--workspace <path>] [--fresh] [--model <model>] [--access read|write]
gomchi [--human] run close <run-id> [--workspace <path>] [--interrupt]
gomchi [--human] run delete <run-id> --confirm [--workspace <path>]
gomchi [--human] run verify <run-id> [--workspace <path>]
gomchi [--human] run export <run-id> --output <directory> [--workspace <path>]
```

`run start` creates an empty idle Gomchi session and MUST NOT allocate a Codex
thread or start the first turn. First `send`/`submit` allocates the thread and
starts the turn under one fsynced intent/idempotency transaction. Its
options include access, model, reasoning effort, and immutable run-specific
instructions. Instructions accept exactly one source: `--instructions`,
`--instructions-file`, or `--instructions-stdin`. They MUST NOT weaken Gomchi's
hard agent invariants.

The two app-server requests are not claimed to be atomic. After `thread/start`,
Gomchi appends and fsyncs the provisional thread ID before sending `turn/start`.
If the `turn/start` response is lost, recovery proves generation absence and
queries persisted history for that provisional thread. `thread/read` returning
the pinned manifest's exact absent-thread error is `Absent`; a successful read
with no accepted turn proves `NotAccepted`. Every other error, malformed
response, timeout, unknown code, or unusable status is `Unreadable`. Gomchi may
replace the thread and retry the reserved first-turn intent only for `Absent` or
`NotAccepted`. Any accepted/in-progress or `Unreadable` result follows ordinary reconciliation and may become
`outcome_unknown`; it is never retried. The permanent one-thread binding begins
when accepted history or the response supplies a turn ID. This is the sole
automatic retry exception because Codex history proves app-server did not
accept the turn.

`send` and `submit` accept exactly one text source: `--message` or stdin. Empty
text is rejected. If `--message` is absent, stdin is required and MUST NOT be a
TTY. `respond` applies the same rule to `--response` and JSON stdin. Each
repeatable `--image` value starts with the required detail token `auto=`,
`low=`, or `high=`; everything after the first `=` is the literal readable
local path, so colons and detail-like filename suffixes are unambiguous. Gomchi
stores only the canonical path and detail and MUST NOT copy image bytes. Later
replay cannot be guaranteed if the source file changes or disappears.
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
or whose verified worker fails control `hello` for ten seconds and then shows
no defined progress for the additional takeover interval. V1 has no force
override: an unverifiable prior worker, app-server, or process group returns
non-retryable `RECOVERY_REQUIRED` without signalling or starting a replacement.
`fork --fresh` is the only explicit immediate escape; it creates a threadless
new run and retains source provenance without reading the source Codex thread.
It is always read-only; combining `--fresh` with `--access write` is
`INVALID_ARGUMENT`, never an implicit downgrade.

When a generation-starting command finds a tested app-server version different
from the run manifest, it returns `COMPATIBILITY_REJECTED`. Only explicit
`run recover --accept-version-change` may accept a different version after the
full compatibility gate passes. It fsyncs the prior/new version, executable and
schema digests as a ledger event before starting the replacement generation.
The flag on an unchanged version is `INVALID_ARGUMENT`; target-wide approval
never changes an existing run.

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
version. This is distinct from the Codex-input compatibility rule, where Gomchi
records and tolerates unknown additive app-server data.

The checked [machine-output schema](protocol/gomchi-machine-v1.schema.json) and
[error contract](protocol/gomchi-error-contract-v1.json) are normative.
`command` is a closed dotted
subcommand enum and `invocation_id` is a UUIDv7. `data` is a command-tagged
union built from these reusable objects:

- `workspace`: workspace ID, lossless canonical path, mode, state-root identity,
  default access, and `created`;
- `target`: name, argv, expected/actual `codex_home`, executable/version/schema
  digests, compatibility verdict, models, and diagnostics;
- `run`: workspace/run IDs, lifecycle/access, generation, target, thread/active
  turn when present, model/effort, ledger cursor, pending count, writer/identity
  verdicts, and last terminal result;
- `turn`: thread/turn IDs, status, model/effort, usage, cursor, response, and
  bounded `workspace_changes`;
- `request`: generation-qualified ID, closed kind, redacted payload, and the
  response-schema identifier from the Codex required-subset manifest;
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

`retryable` means the identical invocation may be safely issued again unchanged;
it does not promise progress or absence of prior side effects. A request rejected
before intent reservation or external write with `RUN_BUSY` or `WRITER_BUSY` is
retryable. Validation, policy, compatibility, integrity, state, stale-request,
and outcome-unknown errors are not. `TRANSPORT_FAILURE` is retryable only when
the operation made no external write, or reconciliation using its idempotency
key proves non-acceptance. Any uncertain acceptance emits `false`.

CLI-worker frames are limited to 8 MiB, unsolicited app-server stdout protocol
lines to 16 MiB, stderr diagnostic lines to 1 MiB, and raw app-server payloads
selected for ledger representation to 2 MiB before marker escaping/redaction.
The post-transform representation allowance is 3 MiB. The terminating newline
is excluded. SHA-256 always covers the exact raw wire payload bytes before any
transform. Detection is streaming: count and hash while discarding beyond the
applicable bound. An oversized CLI
frame returns `PROTOCOL_FRAME_TOO_LARGE` only to that caller and never changes
run state. Oversized stderr records bounded metadata and continues. Oversized,
invalid, duplicate-member, or otherwise unrepresentable unsolicited stdout records bounded
metadata, stops the app-server generation, and moves an accepted active turn to
`outcome_unknown`; it never asserts `TURN_FAILED`. Observer delivery MUST read
fsynced ledger records by cursor; a slow or disconnected observer MUST NOT
delay draining app-server output or block the active turn.

A solicited `thread/read(includeTurns: true)` response is parsed by a
constant-memory streaming visitor. Before byte 16 MiB the visitor MUST yield a
unique top-level `id` matching an outstanding `thread/read`; a top-level
`method` proves an unsolicited notification. An ambiguous prefix that reaches
16 MiB is never classified from the number of outstanding requests: it fails
the compatibility/transport check and stops that app-server generation. An
accepted active turn is quarantined; a transient read fails only its caller.
After the matching ID is observed, the visitor retains only the fields required to
classify addressed turn statuses plus byte length and streaming SHA-256; it has
the 120-second operation deadline but no arbitrary total-response cap. Timeout, malformed
structure, or an unusable status fails only the requesting recovery/reconcile
command and leaves lifecycle state unchanged. Compatibility for every target
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
| 3 | Workspace, target, run, turn, or request not found |
| 4 | State/serialization conflict, policy rejection, recovery precondition, or writer conflict |
| 5 | Codex compatibility, target validation, or Gomchi protocol-version failure |
| 6 | Worker, app-server, transport, or internal runtime failure |
| 7 | A `send` or `wait` request observed its turn end failed or interrupted |
| 8 | Audit integrity verification failure |

Stable semantic error codes, exit classes, emitters, and conditions are
normative:

| Code | Exit | Emitting commands | Condition |
| --- | ---: | --- | --- |
| `INVALID_ARGUMENT` | 2 | any command | CLI syntax, input source, duration, effort, response JSON, incompatible option combination, or pre-existing export destination is invalid |
| `WORKSPACE_NOT_INITIALIZED` | 3 | all commands except `init` and target-only commands | the addressed path has no valid Gomchi workspace |
| `CONFIG_INVALID` | 3 | workspace commands | `config.toml` is malformed, unsupported, duplicated, or has an unknown/wrongly typed key |
| `TARGET_REGISTRY_INVALID` | 3 | target commands and `run start` | `targets.toml` is malformed, unsupported, duplicated, or has an unknown/wrongly typed key |
| `TARGET_NOT_FOUND` | 3 | `target show/remove/doctor`, `run start` | the target name is absent |
| `RUN_NOT_FOUND` | 3 | every `run` command except `start/list` | the run ID is absent in the selected workspace |
| `THREAD_NOT_FOUND` | 3 | `run resume/send/submit/wait/recover/reconcile` and history-copying `run fork` | the pinned Codex history required by the operation is absent; never emitted by `fork --fresh` |
| `TURN_NOT_FOUND` | 3 | `run wait` | the turn ID is absent from both ledger and Codex history |
| `REQUEST_NOT_FOUND` | 3 | `run respond` | the request ID is absent |
| `TARGET_ALREADY_EXISTS` | 4 | `target add` | the name already exists; replacement is never implicit |
| `WORKSPACE_INITIALIZATION_CONFLICT` | 4 | `init` | re-init, nesting, Git mode, state-root, partial-layout, or policy-file facts conflict |
| `RUN_STATE_CONFLICT` | 4 | all state-changing `run` commands | the lifecycle state forbids the requested transition |
| `POLICY_REJECTED` | 4 | policy-sensitive commands and every managed-context command except own-run `status/events/verify` | workspace, hard-agent, or managed-run policy rejects the operation |
| `RUN_BUSY` | 4 | `run send/submit/wait/pending/respond/interrupt/set-effort/promote/demote/pause/resume/recover/reconcile/fork/close` | another turn owns turn-start serialization, or another contender owns per-run worker startup/attachment serialization |
| `WRITER_BUSY` | 4 | `run start` with effective write access (explicit or project default), `run promote`, `run resume --access write`, recovery of a writer run, and `run fork --access write` | another worker holds the workspace writer lease |
| `IDEMPOTENCY_CONFLICT` | 4 | `run send/submit` | a run-scoped key was reused with different normalized input |
| `STALE_REQUEST` | 4 | `run respond` | the request is known but is no longer pending |
| `OUTCOME_UNKNOWN` | 4 | `run send/submit/wait/respond/interrupt/set-effort/promote/demote/pause/resume` | the run is quarantined; this code takes precedence over `RUN_STATE_CONFLICT` |
| `RECOVERY_REQUIRED` | 4 | effective-write `run start`, `run send/submit/promote/pause/resume/recover/reconcile/close`, and ordinary or write-access `run fork` | prior same-run or workspace writer generation identity required by the operation cannot be proved safe; read-only new-run `start`, projection-only `status/events/verify`, and read-only `fork --fresh` are excluded and the code is never generically retryable |
| `TARGET_MISMATCH` | 5 | all commands that start or reconnect a worker/app-server | executable, `CODEX_HOME`, account, or immutable target identity differs from the manifest |
| `COMPATIBILITY_REJECTED` | 5 | `target doctor`, `run start/send/submit/set-effort/resume/recover/reconcile/fork` | version, model, effort, schema, login, sandbox, or app-server capability validation fails |
| `GOMCHI_PROTOCOL_MISMATCH` | 5 | every ordinary command connecting to an existing worker | workspace/run/generation identity or mutation protocol differs, or Gomchi version/binary digest differs; retry `hello`, bounded `status`, or `shutdown` through control protocol v1 |
| `TRANSPORT_FAILURE` | 6 | every command that contacts a worker or app-server | connection, stdio, or protocol transport fails |
| `PROTOCOL_FRAME_TOO_LARGE` | 6 | every command receiving a CLI-worker frame | the CLI-worker request or response frame exceeds its byte limit |
| `RUNTIME_PATH_INVALID` | 6 | `init` and every run command that starts or attaches a worker | private lock/runtime root, sidecar, or socket path fails validation |
| `RUNTIME_PATH_COLLISION` | 6 | `init` and every run command that starts or attaches a worker | a recorded root, existing short path, lease inode, or sidecar belongs to different identities |
| `REDACTION_FAILURE` | 6 | any command that would append an unclassifiable wire payload | safe redaction or serialization cannot be proved |
| `INTERNAL_ERROR` | 6 | any command | an otherwise unmapped invariant or runtime failure occurs |
| `TURN_FAILED` | 7 | `run send/wait` | the addressed turn reaches failed terminal state |
| `TURN_INTERRUPTED` | 7 | `run send/wait` | the addressed turn reaches interrupted terminal state |
| `AUDIT_INTEGRITY_FAILURE` | 8 | `run verify/export/events/status` and any state-changing run command after ledger validation | newline-terminated record structure, sequence, or hash validation fails |

`RUN_BUSY` means another turn is already active or won the serialized turn-start
race, or another process owns the run's worker startup/attachment serialization.
`RUN_STATE_CONFLICT` means the run's lifecycle state forbids the requested
operation. Every command specification and conformance fixture MUST use this
table rather than assign an exit class locally.
“State-changing run command” means start, send, submit, respond, interrupt,
set-effort, promote, demote, pause, resume, recover, reconcile, fork, close, or
delete. Status, list, wait, events, verify, and export are observers; export
embeds a failing verification result rather than refusing. Confirmed delete is
the explicit integrity-failure escape described in SPEC-010.
For a write recovery path, same-run identity safety is evaluated before
workspace lease acquisition: `RECOVERY_REQUIRED` therefore takes precedence
over `WRITER_BUSY` when both could apply.
For the same run, an answering control channel wins over startup-lock
observation. Otherwise prior-generation identity is evaluated before startup
serialization: `RECOVERY_REQUIRED` precedes `RUN_BUSY`; when identity is safe
but another byte owner wins, `RUN_BUSY` is emitted.

`run events --follow` is the only command that emits multiple JSON objects.
`--after` is an exclusive decimal ledger sequence cursor and remains stable
through torn-tail repair; `projection_rewound` never renumbers committed
records. Each success envelope has `data.kind = event|heartbeat|end`. `event`
contains cursor plus the normalized record; with `--raw` it additionally
contains the exact redacted ledger-line string rather than changing framing.
While caught up, a nonfinal run emits a heartbeat every 30 seconds containing
the current cursor and state. After `closed` or `start_failed` is caught up it
emits `end` and exits 0. A midstream failure emits one ordinary error envelope
and exits with its mapped status. Caller disconnect or SIGINT exits 130 without
altering the run. A successfully accepted `run interrupt` command itself
returns exit 0; exit 7 describes a caller that requested the interrupted turn's
result.

Normative internal timeouts are: socket connect 5 seconds; startup `bound` 10
seconds; initialize and `model/list` 30 seconds each; replay `ready` 30 seconds
plus 1 millisecond per ledger record capped at 5 minutes; schema generation and
`target doctor` 120 seconds; solicited history and transient reconciliation 120
seconds; worker control hello 10 seconds followed by 30 seconds of no progress;
TERM grace 5 seconds; interrupt terminal wait 5 seconds; handoff and group
absence 10 seconds; projection publication 100 milliseconds. A user-provided
shorter command timeout limits only its caller and never weakens cleanup proof.

`send` waits without a default timeout until the turn is terminal or requires
master interaction. A caller-supplied timeout returns the current nonterminal
state without interrupting the worker. `submit` returns only after app-server
accepts `turn/start` and Gomchi fsyncs the permanent thread binding and turn ID.
`wait` requires both run and turn
IDs. Waiting and caller-timeout returns use exit 0.

An optional idempotency key is unique for the run's lifetime. Reusing a key with
the same normalized message, image paths/details, and turn options resolves to
the original turn. Reusing it with different input returns
`IDEMPOTENCY_CONFLICT`. `send` and `submit` share the same key space.
Normalization is UTF-8 message bytes, ordered `(detail,lossless-canonical-path)`
image tuples, fixed model, requested/default effort, and access-derived turn
options serialized with JCS. Before any app-server request, the intent record
fsyncs the opaque key and SHA-256 of those normalized bytes. A reservation with
no accepted turn is released only after stable history proves non-acceptance;
otherwise it remains bound to the original intent across restart.

A terminal result includes the final agent response, stable outcome code and
state, workspace/run/thread/turn IDs, fixed model, turn reasoning effort, usage
when supplied by Codex, event cursor, and:

```json
{
  "workspace_changes": {
    "observed_paths": [],
    "truncated": false,
    "attribution": "unverified"
  }
}
```

Observed paths describe workspace changes during the turn interval. In Git mode
they are the sorted unique workspace-relative paths from
`git status --porcelain=v2 -z --untracked-files=all`; ignored paths and
`.gomchi/` are excluded. In non-Git mode they are the changed regular files from
no-follow pre/post `(device,inode,size,mtime_ns)` snapshots, also excluding
`.gomchi/`. Valid UTF-8 paths are strings; other POSIX bytes use
`{"$gomchi_path_bytes":"<base64>"}` using padded RFC 4648 base64 grammar.
The machine schemas enforce the alphabet, four-character grouping, and exact
terminal padding in addition to declaring `contentEncoding`. At most 4,096 paths are retained and
`truncated` reports omission. Gomchi MUST NOT claim that Codex caused every
reported change. Full diff inspection is delegated to Git; Gomchi has no
`run diff` command.

## SPEC-007: Access and Concurrency

The default run access is the project `default_access`, initially `read`.
Readers use thread `sandbox:"read-only"`, turn
`sandboxPolicy:{"type":"readOnly","networkAccess":false}`, and
`approvalPolicy:"never"`. Writers use thread `sandbox:"workspace-write"`,
turn `sandboxPolicy:{"type":"workspaceWrite","writableRoots":[...],
"networkAccess":false,"excludeSlashTmp":false,
"excludeTmpdirEnvVar":false}`, and `approvalPolicy:"on-request"`.
`writableRoots` is the sorted unique set of the canonical workspace plus, in
Git mode, absolute `git rev-parse --git-common-dir` and
`git rev-parse --git-path .` results. These exact thread and turn carriers are
part of the required-subset manifest. Readers MAY run concurrently without a
Gomchi limit. A canonical workspace has at most one Gomchi writer.

The writer lease is BSD `flock(2)` with nonblocking exclusive semantics below
the workspace-recorded lock root, keyed by the full canonical-workspace digest.
`gomchi init` accepts `--state-root <absolute-local-path>`. Without it, init
resolves `${XDG_STATE_HOME:-$HOME/.local/state}/gomchi/locks/` once. The
canonical path and root device/inode are recorded in
`.gomchi/runtime/lock-root.json` and every run manifest. Later environment
changes are ignored. A missing workspace record is reconstructed only from
unanimous existing manifest values; conflict fails. With no existing runs it is
resolved and created. Creation and validation use root-fd-relative no-symlink
operations, validate `EEXIST`, ownership and mode 0700 with `fstat`, and require
`MNT_LOCAL` with `fstatfs`; path or device/inode drift fails with
`RUNTIME_PATH_COLLISION`. A nonlocal default requires explicit `--state-root`.
Only the
worker holds it; it is close-on-exec and MUST NOT be inherited by app-server or
descendants. A writer holds it across starting, idle, running, and all waiting
states until demotion, pause, close, entry into `outcome_unknown`, start failure,
or terminal worker cleanup. Writer acquisition never queues automatically. A
conflict returns `WRITER_BUSY`; the holding run ID and state are best-effort
nullable fields, and no prompt content is included.

Writer and startup lock files are permanent after create-exclusive creation;
normal operation never unlinks or recreates either pathname. After acquiring
the lease and before every destructive barrier, the worker
compares the held fd's `(st_dev, st_ino)` with a root-fd-relative `fstatat` of
the pathname and persists that pair in `writer.json`. Any mismatch fails closed
with `RUNTIME_PATH_COLLISION`. An unresolved `writer.json` for a different run
does not block the kernel acquisition attempt, but it does gate writer
activation. If its recorded inode differs, Gomchi first proves the recorded
generation absent, including boot mismatch or group emptiness; only then may it
atomically reconstruct `writer.json` from the held pathname/fd pair. Without
absence proof the mismatch is `RUNTIME_PATH_COLLISION`.
Before starting a writer app-server, the lease holder proves that the foreign
generation is absent or performs identity-verified cleanup using its recorded
group. `Unverifiable` releases the newly acquired lease and returns
`RECOVERY_REQUIRED`; it never starts the new app-server or resumes the foreign
thread. Thus a stale pointer does not create false `WRITER_BUSY`, while it also
cannot authorize concurrent writer app-servers.

Runtime identity is `(boot_session_uuid, pid, pgid, uid, start_tvsec,
start_tvusec, executable_path, executable_dev, executable_ino,
executable_sha256)`. Gomchi samples BSD info, `proc_pidpath` and executable
identity, then BSD info again and rejects a changed start time. A live process
whose path is unavailable is not mismatched when the remaining tuple and group
proof match; replacement at the same path is not identity continuity. Recovery
classifies `Absent`, `Mismatch`, `Match`, or `Unverifiable`; only a revalidated
`Match` may receive an individual signal, and group absence—not leader absence
alone—is required before a new writer starts.
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
group. While continuity exists, Gomchi snapshots every member's
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

App-server launch uses spawn attributes
`POSIX_SPAWN_START_SUSPENDED | POSIX_SPAWN_SETPGROUP |
POSIX_SPAWN_SETSIGDEF | POSIX_SPAWN_SETSIGMASK`; the default set covers every
catchable signal and the child mask is empty. Before `SIGCONT`, the worker opens
the suspended final executable path without following symlinks and derives
device, inode, and SHA-256 from that same fd between two unchanged BSD-info
samples. It writes the complete ten-field provisional identity with temp-file,
`fsync`, rename, and directory `fsync`. Replacement/unavailability is
`Unverifiable`, not a partial record. The post-handshake sample is recorded by
the fsynced `generation_started` event.

Writer locks use `locks/writer/<workspace-digest>` and startup locks use
`locks/startup/<workspace-and-run-digest>` and MUST never share an inode. The
per-run startup file contains two POSIX byte-range locks: byte 0 is held by
the CLI before fork until worker `bound`; byte 1 is acquired as the worker's
first post-`setsid`/re-exec action and held for its entire serving lifetime.
The lock body contains separate version-1, 4096-byte, zero-padded owner records
for byte 0 and byte 1. Each contains range, workspace/run/generation,
boot UUID, full Gomchi process identity, executable-path SHA-256, and a SHA-256
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
contender waits at most ten seconds for ownership handoff. A timed-out byte-0
transient starter may be terminated outside the lock only after the run-keyed
file, exact Gomchi executable identity, the defined
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

When any same-run byte-1 worker's control `hello` exceeds ten seconds, recovery
applies the full identity and kqueue
rules to the worker itself without requiring the possibly-held startup lock,
then requires an additional 30 seconds with no progress in ledger head, runtime
generation, the monotonic `worker_log_bytes_written` counter, or process CPU
before takeover. `worker.log` contains generation start, bound, ready, control,
shutdown and rotation records; rotation uses rename and never resets that
counter. It aborts when
the exit watch fires, generation advances, or lock-body identity changes. Only
then does it signal a revalidated `Match` and confirm worker exit. For a writer,
it additionally confirms lease release. `Unverifiable` returns
`RECOVERY_REQUIRED`. Contenders then compete for the
startup lock, recheck socket/runtime state, and only the winner may acquire the
writer lease and clean the recorded app-server group under it. This path also
recovers an exact worker or transient starter wedged while holding the startup
lock and never permits one run to evict another run's healthy writer.

If worker `SIGTERM` arrives during an active turn, it sends `turn/interrupt`,
waits up to five seconds for a terminal event, fsyncs the observed state, and
only then performs generation cleanup. The worker holding byte 1 normally
unlinks its own socket and sidecar. On recovery, only the byte-0 election winner
may authorize unlink after prior-generation absence is proved; the replacement
worker performs it after acquiring byte 1 and before bind. Any other occupied
path fails closed. Shutdown attempts this cleanup for ten seconds and otherwise
leaves the path for that verified next owner.

Any `run start` whose effective access is write (whether explicit or project
default), promotion, `resume --access write`,
`fork --access write`, and recovery of a writer run are the complete
writer-acquisition set and acquire the lease before the run becomes usable.
Promotion and demotion are allowed only while idle. Because
`developerInstructions` is not a `turn/start` field, either transition performs
clean generation shutdown and `thread/resume` with a recomposed generation-level
instruction prefix before another turn. Promotion acquires the writer lease
before replacement activation; demotion completes cleanup before releasing it.
Resume defaults to `read`.

Readers MAY run during writer turns and may observe intermediate workspace
state. Gomchi provides no read snapshot isolation. A consistent review SHOULD
begin only after the writer turn reaches a terminal state.

Codex native subagents belong to their parent run and inherit its access
boundary. They do not acquire separate Gomchi writer leases. Gomchi therefore
serializes independent writer app-servers, not every execution lane inside one
writer session; the injected instructions require Codex to avoid overlapping
write-heavy delegation and to prefer parallel subagents for independent or
read-heavy work.

Reader requests for write or additional filesystem permission are
automatically declined. Writer approvals are never automatically accepted.
Target MCP servers, apps, and plugins remain available; side effects performed
outside Codex's sandbox are outside the one-Gomchi-writer-per-worktree
guarantee.

## SPEC-008: Run Lifecycle, Recovery, and Forking

The lifecycle states are:

- `starting`
- `idle`
- `running`
- `waiting_approval`
- `paused`
- `closed`
- `start_failed`
- `outcome_unknown`

Normal transitions are:

```text
starting -> idle -> running <-> waiting_approval
running -> idle
waiting_approval -> idle
idle <-> paused
idle -> closed
paused -> closed
starting -> start_failed
running|waiting_approval -> outcome_unknown
outcome_unknown -> paused
outcome_unknown -> closed
```

`closed` and `start_failed` are final. A failed start that allocated a run ID is
retained with its audit and any known Codex thread ID. It supports only status,
events, verify, export, and confirmed deletion.

A byte-0 election owner that proves no worker reached `bound` may use the sole
bootstrap-writer exception to append `starting -> start_failed` and its final
seal. It must hold startup serialization and fsync both records. A present but
`Unverifiable` generation is never rewritten as start failure; the operator may
wait for group emptiness or a boot-session change.

Pause and close reject running or waiting runs unless `--interrupt` is present.
With `--interrupt`, Gomchi requests interruption, waits for a terminal result,
then stops or closes the run. Pause is resumable; close is not. Neither action
reverts workspace changes.

After worker loss, an idle run may be recovered by starting a new process
generation and using `thread/resume`. Recovery of `paused` performs only
coordination cleanup and record repair; `resume` starts its next generation. If
failure occurred during an active turn, Gomchi reconciles only terminal status
confirmed by persisted Codex history. It MUST NOT replay the input automatically.

If no terminal evidence exists, the run becomes `outcome_unknown`. Its worker
and app-server stop and any writer lease is released. `reconcile` uses a
lease-free transient read-only generation and only
`thread/read(includeTurns: true)`; it never resumes the thread or starts a turn.
Terminal evidence moves the run to `paused`, where bare `resume` uses read
access. Otherwise the run remains blocked from new turns; only status, events,
verify, export, recover, reconcile, ordinary fork after proven absence,
`fork --fresh`, and close after proven
generation cleanup are allowed.

History-copying fork is allowed from idle, paused, closed, and outcome-unknown
runs after required absence proof, but not from running or waiting runs. The
read-only `fork --fresh` escape is additionally allowed when a `running` or
`waiting_approval` source socket is unreachable and its process identity is
`Unverifiable`. The source is immutable. A fork uses the same
target snapshot, defaults to read access, and inherits the source model unless
another model is explicitly selected. It inherits the source's immutable
run-specific instructions. A cross-target fork is forbidden.

For an outcome-unknown source, the fork scans confirmed history newest first and
selects the latest status listed as forkable in the checked target manifest.
Rejected interrupted/failed statuses are skipped rather than treated as generic
terminal boundaries. If no forkable turn exists, Gomchi creates a fresh Codex
thread and records source-run and unknown-turn provenance in the new manifest.

Any fork that must copy confirmed history requires the source Codex thread to
exist in the pinned `CODEX_HOME`; the Gomchi transcript is not a substitute.
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
defaults is `COMPATIBILITY_REJECTED`. The manifest stores the initial model
capability snapshot; later successful generations append a new snapshot, and
`set-effort` while no generation is live validates against the latest fsynced
snapshot. The effort is accepted only when that snapshot advertises the exact value. An empty
value returns `INVALID_ARGUMENT`; an unadvertised value returns
`COMPATIBILITY_REJECTED`.

## SPEC-009: Pending Requests and Approvals

The checked [Codex required-subset manifest](protocol/codex-0.147.0-required-subset.json)
maps stable server requests as follows:

- `item/commandExecution/requestApproval` and
  `item/fileChange/requestApproval` are supported and become
  `waiting_approval`;
- `item/permissions/requestApproval`, `item/tool/requestUserInput`, and
  `mcpServer/elicitation/request` are recognized but unsupported in v1. Gomchi
  records their bounded shape, replies JSON-RPC method-not-found, and lets Codex
  determine the turn result. They never create a pending request or lifecycle
  state. Permissions/granular approval is live-proven to require
  `experimentalApi` on the pinned target, while SPEC-012 deliberately forbids
  that API. User-input and MCP elicitation are excluded by the v1 product scope.

The two supported approval requests are represented as structured pending requests. `run pending` returns the
generation-qualified request ID, closed kind, redacted payload, and exact
response-schema identifier from that manifest. `run respond` accepts exactly
one JSON response source, validates it against that schema, and forwards it to
the owning generation. Other known-but-unsupported stable requests receive
JSON-RPC method-not-found and are recorded; malformed frames stop the generation.

Requests from an older process generation return `STALE_REQUEST`; pending
requests never survive restart and are never silently replayed. Their accepted
turn follows ordinary persisted-history reconciliation. Waiting has
no automatic timeout. A waiting writer continues to hold its lease while a
response or interruption returns it through running/idle; only demotion, pause,
close, `outcome_unknown`, start failure, or terminal worker cleanup releases it.

Command- and file-change approval decisions exposed by Gomchi are:

- `accept_once`
- `accept_for_generation`
- `decline`
- `cancel`

For command and file-change approvals they map respectively to the pinned wire
values `accept`, `acceptForSession`, `decline`, and `cancel`.

Generation approval maps to app-server's live session-scoped approval and
expires whenever the worker/app-server generation ends. Gomchi MUST NOT present
it as durable run-wide approval and MUST NOT persist an approval policy that is
silently replayed after restart.

## SPEC-010: Audit, Retention, and Deletion

Every allocated run has a private directory at `.gomchi/runs/<run-id>/` with:

- `manifest.json`: fixed run configuration and provenance;
- `audit.jsonl`: the sole append-only audit authority;
- `state.json`: a disposable materialized view rebuilt from the ledger.
- `worker.log` and `worker.log.1`: bounded diagnostics, never audit authority;
- `recovery/`: preserved torn-tail and repair evidence.

The audit contains Gomchi lifecycle records and redacted app-server wire
records in one total order. Each record contains schema version, sequence/event
cursor, UTC timestamp, run ID, process generation, kind, payload,
`previous_hash`, and `hash`. Lines use RFC 8785 JCS. `sha256-jcs-v1` hashes the
JCS record with `hash` omitted and `previous_hash` retained; the genesis
`previous_hash` is 64 zeroes. SHA-256 chaining detects accidental corruption or
ordinary tampering; it is not a signature and does not defend against a hostile
same-user attacker.

Inbound JSON rejects duplicate object members and preserves number lexemes until
numeric adaptation. The in-repo `sha256-jcs-v1` canonicalizer orders object keys
by UTF-16 code units and formats binary64 values with ECMAScript's shortest
number representation. Verification uses that formatter rather than echoing a
preserved lexeme. Any canonicalizer byte change requires a new
`sha256-jcs-vN`; RFC 8785 published vectors and Gomchi numeric vectors are
normative fixtures. Before inserting any Gomchi marker, each inbound object key
matching `^\$+gomchi_` is escaped by prefixing one additional `$`. Processing
order is escape, recursive redaction, numeric adaptation, then JCS; the
redaction tokenizer never examines Gomchi-owned marker keys. Before JCS, a
decimal lexeme is parsed to finite binary64, rendered with the ECMAScript
shortest form, and compared by exact decimal numeric value with the original.
A nonfinite/overflowing value or unequal value becomes
`{"$gomchi_number":"<original-lexeme>"}`. Verification also requires every
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

Redaction recursively visits objects within objects and arrays. For ASCII keys,
it splits on `-` and `_`, before an uppercase letter following a lowercase
letter or digit, and before the final uppercase letter of an uppercase run when
followed by lowercase; digits attach to the token on their left. Tokens are
ASCII-case-folded, empty tokens are dropped, and compact form is their
concatenation. Non-ASCII keys never
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
`{"$gomchi_redacted":{"reason":"secret_key","original_type":"<type>"}}`,
where type is `string`, `number`, `boolean`, `null`, `object`, or `array`.
Over-redaction such as `password_hash` and `api_key_id` is intentional. JSON
encoded inside a string is not reparsed in v1 and remains within the documented
value-secret limitation. If safe classification or serialization fails, Gomchi
records only `payload_unrepresentable` metadata and never raw bytes.
Run directories use mode 0700 and sensitive files mode 0600. Prompts and command
or tool output may still contain secrets; same-OS-user confidentiality is not
guaranteed.
`.gomchi/runtime/` is mode 0700 and its records are mode 0600. `worker.log` is
limited to 1 MiB with one rotation and remains diagnostics-only.

Audit completeness is limited to Gomchi lifecycle, app-server-exposed main-turn
wire traffic, approvals, access transitions, and target/account provenance.
Encrypted or otherwise unexposed native-subagent communication is represented
as opaque activity when observable and is not claimed as reconstructable audit.

The Codex thread in the pinned `CODEX_HOME` is conversation-continuation
authority. Gomchi's ledger is audit authority. Missing Codex history cannot be
reconstructed as the same session from the Gomchi transcript, and missing
Gomchi audit cannot be reconstructed as equivalent audit from Codex history.

Every represented ledger payload obeys the 2 MiB raw and 3 MiB post-transform
limits in SPEC-006. Larger payloads retain only
source kind, byte length, streaming SHA-256, JSON Pointer when known, and reason
in `payload_unrepresentable`; v1 creates no raw or redacted sidecar. `events`
projects normalized records. `events --raw` projects redacted wire
payloads. `verify` validates structure, sequence, and hashes but not the truth
of model or command claims. `export` creates a mode-0700 directory containing
0600 `bundle.json`, `manifest.json`, verbatim `audit.jsonl`, `state.json`,
redacted `transcript.jsonl`, and `verification.json`. `bundle.json` records
schema version, workspace/run identity, filenames, hashes, and source-derived
timestamps; lexicographic filenames and source bytes make repeated exports
content-deterministic. Runtime records, locks, logs, target registry,
`CODEX_HOME`, images, and raw torn-tail evidence are excluded. A failing audit
does not suppress export: both bundle and verification set
`verification_failed:true`, while other state-changing commands fail closed.
The bundle may contain plaintext prompts/output that key-name redaction cannot
detect. Its output
path MUST NOT already exist; Gomchi never merges or overwrites an export and
returns `INVALID_ARGUMENT` on collision.

There is no retention limit or automatic deletion. `run delete` is allowed
only for closed or start-failed runs and requires `--confirm`; it is the sole
state-changing command allowed after audit integrity failure and appends no
record to a ledger it cannot trust. It permanently
deletes the Gomchi run directory only. It MUST NOT delete the Codex thread from
`CODEX_HOME`, and Gomchi MUST NOT later auto-import that orphaned thread.

## SPEC-011: Agent Instruction and Side-Effect Policy

Gomchi injects developer instructions that are immutable for one process
generation and identify the process as a master-controlled Gomchi subagent.
They include run ID, canonical workspace, and that generation's access mode.
Promotion/demotion starts a new generation and recomposes the prefix through
`thread/resume`; immutable user run instructions remain subordinate and
unchanged. The instructions MUST establish these rules:

- `.gomchi` is reserved; the agent must not read or modify it unless the master
  explicitly requests audit or review access.
- Answer, explain, review, and diagnose requests do not authorize mutation.
- Build and fix requests may mutate only in a writer run.
- Safe, local, in-scope edits and checks may proceed autonomously when the task
  authorizes implementation.
- External side effects, destructive actions, and meaningful scope expansion
  require master direction.
- Git add, commit, and push each require explicit master authorization.
- Background process creation requires explicit master authorization.
- Native subagents must avoid overlapping write-heavy delegation and prefer
  parallelism only for independent or read-heavy work.
- The response reports outcome, material changes or findings, verification,
  and blockers without imposing a rigid JSON format on the model.

The target's own Codex configuration, AGENTS instructions, skills, plugins,
apps, MCP servers, and native subagents remain available unless they conflict
with Gomchi's hard invariants.

## SPEC-012: Orchestration Boundary and Compatibility

Independent Gomchi runs use hub-and-spoke orchestration: only the master may
create, address, interrupt, fork, pause, close, or delete them. A Gomchi-managed
agent MUST NOT invoke `gomchi` to control another run or connect to another
run's worker socket. V1 has no peer messaging or run-to-run delegation.

An app-server descendant carries `GOMCHI_MANAGED_CONTEXT`, the unpadded base64url
encoding of a UTF-8 JCS object containing schema version 1, workspace ID, run ID,
process generation, boot UUID, and worker PID. A present malformed marker,
unknown schema, nonexistent run, or foreign workspace/run is treated as managed
and rejected, never as absent. The marker is inherited only by exec descendants;
an MCP server reached through another transport may not carry it. A `gomchi`
process invoked from a valid context permits only read-only status, events, and
verification of its own run; it rejects run creation, cross-run inspection,
turn input, pending-response submission, access changes, lifecycle control,
target commands, initialization, export, and deletion with `POLICY_REJECTED`.
This guard prevents ordinary recursive use but is not a security boundary
against a hostile same-user process that deliberately removes its environment.

Codex native subagents inside one run remain allowed and are part of that
app-server-managed session tree. Their existence does not create independent
Gomchi runs or additional Gomchi writer leases.

Gomchi uses the stable app-server API surface and does not enable
`experimentalApi` for runtime operation. It validates 0.147.0 as tested. For an
unlisted newer version, Gomchi may run the version as `unverified` only when:

1. `codex app-server generate-json-schema` is available;
2. after resolving `$ref`, every JSON Pointer in the checked required-subset
   manifest has the same type/const/requiredness and every required enum value
   remains present; additive fields and enum values are allowed;
3. live initialize, `initialized`, paginated `model/list`, actual `codexHome`,
   absent-thread `thread/read`, persisted resume/read, early response-ID,
   sandbox, and required server-request probes pass.

Missing generation support, required schema, lifecycle behavior, or identity
causes fail-closed rejection. Unknown additive fields and notifications are
recorded and tolerated. Unknown server requests are recorded, receive JSON-RPC
method-not-found, and do not stop the generation; unparseable frames fail
closed. All app-server messages are correlated with request ID, thread
ID, turn ID, and process generation before affecting state.

## External Protocol References

- [Official Codex app-server documentation](https://learn.chatgpt.com/docs/app-server)
- [Official Codex subagents documentation](https://learn.chatgpt.com/docs/agent-configuration/subagents)

These references describe Codex behavior. Gomchi-specific policy in this SOT
remains authoritative for Gomchi.
