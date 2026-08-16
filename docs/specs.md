# Dolgorae Product Specification

Status: Normative target specification for the first supported release.

This document owns Dolgorae's externally observable behavior. Technical structure
is owned by [architecture.md](architecture.md), decision rationale by
[architecture-decisions.md](architecture-decisions.md), and delivery state by
[roadmap.md](roadmap.md). A contradiction between SOT documents is an invalid
state and must be reconciled before an implementation task becomes active.

Only the uppercase key words **MUST**, **MUST NOT**, **SHOULD**, and **MAY** are
normative; lowercase prose is descriptive and grants no additional authority.

## Definitions

- **Master**: a human, interactive client, workflow orchestrator, or local
  automation process that invokes the `dolgorae` CLI and owns orchestration
  decisions.
- **Controller**: the one master authorized by a durable capability binding to
  mutate a run.
- **Observer**: a same-OS-user caller allowed to read client-safe projections
  without acquiring mutation authority.
- **Run**: one durable Dolgorae session, identified by a UUIDv7 and bound to one
  Codex thread.
- **Turn**: one identified Codex execution within a run, beginning when
  `turn/start` is accepted and ending only when Codex confirms completed,
  interrupted, or failed status.
- **Worker**: the hidden per-run Dolgorae process that owns one direct App Server
  WebSocket connection, worker control socket, run lifecycle, and audit writer.
- **Server epoch**: one globally unique lifetime of any physical Codex App
  Server generation, whether the shared Profile Server or a Dedicated Lane
  Server.
- **Run generation**: one worker lifetime and its direct connection within a run.
- **Runtime Profile**: a user-local, named Codex execution configuration consisting of
  a direct absolute Codex executable, normalized global argv, canonical
  `CODEX_HOME`, and an explicit non-secret environment map.
- **Profile Server**: the shared-read-only Codex App Server singleton selected
  by one Runtime Profile launch-authority contract.
- **Dedicated Lane Server**: one physical Codex App Server generation owned by
  a Run's immutable dedicated logical lane.
- **Event Projection**: the `minimal` or `operational` delivery view over one
  durable event cursor domain.
- **Codex Config Profile**: a Codex `--profile` selection inside normalized
  global argv; it is not a Dolgorae Runtime Profile.
- **Reader**: a run whose turns use Codex read-only sandbox policy.
- **Writer**: the single run named by durable Dolgorae writer authority for a
  canonical workspace and whose turns may use workspace-write sandbox policy.
- **Terminal turn**: a turn confirmed as completed, interrupted, or failed.
- **Forkable turn**: a terminal turn whose exact status is listed in the
  checked Codex required-subset manifest as accepted for `lastTurnId` by the
  pinned profile. Terminal and forkable are intentionally not synonyms.

## SPEC-001: Product Boundary and Supported Environment

Dolgorae MUST provide persistent, controller-owned Codex Runs for direct
interactive sessions and externally managed agents through one distributable
`dolgorae` executable. A **Codex-native subagent** is an internal descendant of
one Codex Run; an **Independent Dolgorae Run** is a peer session created by a
trusted external controller. These terms MUST NOT be conflated. Dolgorae MUST
NOT install a Dolgorae global daemon, project
daemon, launchd unit, Codex binary, authentication material, or `CODEX_HOME`.
It MAY manage one Codex app-server singleton per canonical profile
`CODEX_HOME`; that Codex process is not a Dolgorae daemon.

The first supported release is a personal alpha for Apple Silicon macOS 26.0
or later (`aarch64-apple-darwin`) on local APFS. Both the workspace and its
configured state/lock root MUST report `MNT_LOCAL` and `f_fstypename ==
"apfs"`; there is no v1 override. Intel macOS, Linux, Windows, network
filesystems, non-APFS local filesystems, public installers, and automatic
updates are not supported release targets. Empirical release evidence is
valid only for the recorded OS build and MUST be refreshed on a new macOS
major version.

Dolgorae depends on user-prepared Runtime Profiles. Codex app-server 0.147.0 is
the current compatibility baseline. Background-process safety is owned by
each Sticky Dedicated logical lane across its successive process generations
and by the macOS process census; it MUST NOT depend on a future Codex
terminal-management API. A newer native API MAY supply additional evidence but
never replaces lane-generation identity, census, or cleanup.

Dolgorae MUST be the only Codex app-server supervisor used by supported
external-master integrations. An external master MUST use the stable Dolgorae
machine CLI and MUST NOT start, connect to, or control the singleton, its
dedicated App Server socket, or a private worker socket. This is a
supported-integration boundary, not a
claim that Dolgorae can prevent a hostile same-user process or an unrelated
editor from mutating the workspace. Dolgorae remains local-only: v1 MUST NOT
bind a public TCP port, provide remote authentication, or require a remote
client to remain connected.

## SPEC-002: Workspace Initialization and Discovery

`dolgorae init [PATH]` MUST initialize a Git workspace. `dolgorae init --non-git
[PATH]` MUST explicitly opt a general directory into Dolgorae. A run MUST NOT
start in an uninitialized workspace.

In Git mode, Dolgorae runs
`git -c core.quotePath=true -C <supplied-existing-directory> rev-parse --show-toplevel`
without a shell and requires exit 0 and exactly one LF-terminated stdout
result; bounded stderr is diagnostic only when exit is zero. A double-quoted result is decoded with Git's documented
C-style path quoting (including octal byte escapes); an unquoted result is the
bytes before the sole final LF. Invalid quoting, trailing output, or NUL is a
Git discovery failure. The canonical workspace is libc `realpath(3)` applied to
that decoded existing directory, even when the supplied path is a subdirectory.
In non-Git mode it is `realpath(3)` applied to the existing initialized
directory. The canonical path is the returned absolute POSIX byte sequence
with no trailing slash except for root, followed by the macOS Data-volume
alias rule below. Dolgorae performs no Unicode normalization or case folding.
Symlink and case-insensitive lookup belong to `realpath(3)`, but APFS
firmlinks do not: when the result is exactly `/System/Volumes/Data` or begins
with `/System/Volumes/Data/`, Dolgorae derives the candidate `/` or the path with
that prefix removed and substitutes it only when no-follow `stat` of both
paths yields the same `(st_dev, st_ino)`. The same rule is applied in Git and
non-Git mode before any digest is computed.

The workspace digest is lowercase hexadecimal SHA-256 over
`"dolgorae-workspace-v1\0"` followed by those canonical path bytes. The full
64-character digest is the workspace ID and writer-lock filename. The startup
filename is lowercase hexadecimal SHA-256 over
`"dolgorae-startup-v1\0" || workspace_digest_bytes || run_uuid_bytes`. The short
socket name is RFC 4648 uppercase unpadded base32 of the first 160 bits of
SHA-256 over
`"dolgorae-socket-v1\0" || workspace_digest_bytes || run_uuid_bytes`. The manifest
records both the canonical path bytes in the lossless path representation and
the full workspace digest. Here `workspace_digest_bytes` is the raw 32-byte
SHA-256 result, not its hex text, and `run_uuid_bytes` is the UUID's 16 bytes in
RFC 4122/network order, not its hyphenated text. Every component MUST use these
same preimages.

Each linked Git worktree has its own canonical top-level path and is therefore
a separate Dolgorae workspace, run store, and writer authority. Dolgorae supports one
writer lane per worktree; it does not serialize writers across worktrees that
share a common Git directory.

Dirty Git workspaces are allowed. Run creation MUST record a read-only baseline
containing HEAD, branch, tracked changes, and untracked paths. Dolgorae MUST NOT
discard, reset, stash, or otherwise rewrite pre-existing changes.

Later commands discover the nearest ancestor containing `.dolgorae`; an explicit
`--workspace PATH` overrides discovery. Discovery selects a workspace only. It
MUST NOT implicitly select a run.

Git-mode `.dolgorae` MUST be at the canonical Git top level. `--non-git` is
rejected for a path inside any Git worktree, and a nested `.dolgorae` below an
already initialized workspace is rejected. Non-Git mode records an empty Git
baseline. Absence of the `git` executable, a Git version older than 2.39, or a
Git discovery failure returns `WORKSPACE_INITIALIZATION_CONFLICT` rather than
silently falling back to non-Git mode.

In Git mode, initialization creates exactly these tracked project policy files:

```text
.dolgorae/
  .gitignore
  config.yaml
```

`config.yaml` is strict YAML and contains exactly `schema_version: 1` and
`mode: git|non_git`. Unknown or duplicate keys, wrong types, unsupported schema versions, and
malformed YAML return `CONFIG_INVALID`. The file is hand-editable, but Dolgorae
never rewrites it except during first initialization. `.dolgorae/.gitignore`
contains exactly `/local.yaml`, `/runs/`, `/runtime/`, `/evidence/`, and
`/cache/`; it MUST NOT ignore `.dolgorae` as a whole. Initialization also creates
an untracked mode-0600 `.dolgorae/local.yaml` with `schema_version: 1` and an
empty `profiles` mapping.

Non-Git initialization creates the same two files and the same ignore contents,
but makes no claim that either file is tracked. The ignore file remains useful
if the directory is later placed below a version-control workspace.

Initialization uses create-exclusive temporary files, file `fsync`, rename,
and parent-directory `fsync`. Repeating `init` succeeds with `created:false`
only when the recorded mode, canonical workspace, schema,
and existing policy files are byte-for-byte compatible. It never overwrites an
existing tracked policy file. A partial layout, nested workspace, changed mode,
or conflicting policy returns
`WORKSPACE_INITIALIZATION_CONFLICT`.

## SPEC-003: Profile, Account, and Singleton Binding

The Writer Capsule and shared↔capsule migration paragraphs retained in this
section are historical candidate text and are non-normative. `SPEC-014`
supersedes them for execution-lane cardinality, residency, server generations,
profile lifecycle, and process census.

The project-local profile configuration lives at:

```text
<canonical-workspace>/.dolgorae/local.yaml
```

A Runtime Profile contains:

- a unique name;
- an absolute Codex executable and shell-free validated global argv;
- an absolute expected `CODEX_HOME`;
- the symbolic `profile_state_directory_v1` launch-cwd policy;
- an explicit non-secret environment map.
- an explicit `native_subagents: enabled|disabled` policy, defaulting to
  `enabled` for newly defined v1 profiles.

`local.yaml` is strict YAML with top-level `schema_version: 1` and a `profiles`
mapping keyed by name. Each entry contains nonempty `argv: [string, ...]`,
absolute `codex_home: string`, `environment: {string: string}`, and optional
`native_subagents: enabled|disabled`. `argv[0]`
MUST be an absolute regular Codex executable; v1 rejects shell interpreters,
arbitrary wrappers, and argv that already contains an app-server subcommand.
Only the required-subset manifest's `profile_launch.global_arguments` are
allowed after `argv[0]`. V1 accepts canonical `--profile <name>`, repeatable
`--enable <feature>`, repeatable `--disable <feature>`, and flag-only
`--strict-config`; it rejects aliases, `--flag=value`, missing values, and every
other option. Normalization preserves argument and repetition order exactly.
The `multi_agent` Codex flag is reserved to Dolgorae and MUST NOT appear in raw
profile argv. Dolgorae injects exactly one canonical `--enable multi_agent` or
`--disable multi_agent` pair from `native_subagents`. The corrected exact-version
campaign for an enabled 0.147.0 profile proved child identity, parent
relationship, active/terminal lifecycle, persisted history, restart continuity,
and cleanup, so it advertises `native_subagents:supported`. Active or unknown
native state still blocks pause, physical-generation replacement, profile stop,
and shutdown. The disabled case also produced a child, so an explicitly disabled
0.147.0 profile advertises `unverified`, not `unavailable`.
Changing the policy changes the immutable launch contract and requires an
operator-authorized migration.
Environment names are explicit, require `PATH`, `LANG`, and `LC_ALL`, reserve `CODEX_HOME`, `HOME`,
`USER`, `LOGNAME`, `SHELL`, `TMPDIR`, and every `DOLGORAE_*` name to Dolgorae, and treat
all stored values as non-secret local configuration. Unknown or duplicate keys,
empty argv, relative homes,
wrong types, a missing required environment value, malformed YAML, and unsupported schema versions return
`PROFILE_CONFIG_INVALID`. Profile add/remove holds a workspace-local config lock and uses
write-temp, file `fsync`, rename, and directory `fsync`; the registry is
hand-editable and MUST NOT contain credentials, tokens, or other secrets.
The `.dolgorae` private directory is mode 0700 and `local.yaml` is mode
0600; creation and replacement reject a wrong-owner or more-permissive file.

Profile names are unique within one project. Every profile command MUST resolve
an initialized workspace through `--workspace` or normal upward discovery.
`profile add` MUST reject an existing name with
`PROFILE_ALREADY_EXISTS`; it MUST NOT overwrite a profile implicitly. Replacement
requires an explicit remove followed by add.

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

`run start` MUST require an explicit profile. Before use, Dolgorae MUST validate
the executable, version, app-server schema, initialization handshake, login
readiness, model listing, and actual `codexHome`. A `codexHome` mismatch is a
hard failure.

Run creation stores a complete immutable Runtime Profile snapshot, not only its digest.
The snapshot contains exactly the profile name, canonical `CODEX_HOME`,
normalized argv, `launch_cwd_policy`, derived concrete launch cwd, sanitized environment, enabled
and disabled features, normalized process-static configuration, initial configuration
observation, executable identity, Codex version, generated App Server schema
digest, compatibility-manifest digest, launch-contract digest, and initial
server key. It contains sufficient non-secret bytes and explicit-absence markers
to reconstruct the accepted launch contract after registry edit or deletion.
Existing runs MUST NOT be rebound to another account or `CODEX_HOME`.

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
ID; durable state remains under `.dolgorae/runs/`.

The actual worker socket node is the sole per-run exception to project-local
runtime storage and lives below `/tmp/dolgorae-<uid>/s/`; its identity authority
lives in `.dolgorae/runtime/runs/<run-id>.json`. A live worker MUST detect a
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
and the profile capability snapshot accepted at creation. These fields are
durable run identity; worker, connection, singleton, and CLI restart MUST NOT discard
or reinterpret them.

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

dolgorae [--human] runtime capabilities
dolgorae [--human] controller credential create --kind <kind> --instance-id <id> [--subject-id <id>] --output <new-path>
dolgorae [--human] operator credential initialize --output <new-path>
dolgorae [--human] operator credential rotate [--operator-file <path> | --operator-fd <fd>] --output <new-path>

dolgorae [--human] workspace inspect [--workspace <path>]
dolgorae [--human] workspace writer status [--workspace <path>]
dolgorae [--human] workspace writer handoff-prepare --workspace <path> --from <run-id> --to <run-id> --expected-generation <n> [--controller-file <path> | --controller-fd <fd>]
dolgorae [--human] workspace writer handoff-commit --workspace <path> --handoff-id <id> --expected-generation <n> [--controller-file <path> | --controller-fd <fd>]
dolgorae [--human] workspace writer handoff-cancel --workspace <path> --handoff-id <id> [--controller-file <path> | --controller-fd <fd>]

dolgorae [--human] profile add <name> [--workspace <path>] --codex-home <absolute-path> [--native-subagents <enabled|disabled>] [--env <name=value>]... -- <argv...>
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

dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] start --workspace <path> --profile <name> [--control-mode <direct-interactive|managed-agent>] [--execution-lane <shared-readonly|dedicated>] [--required-assurance <best-effort-personal-alpha|verified-thread-scoped-control|strong-process-containment>] [--model <model>] [--effort <effort>] [--purpose <purpose>] [--purpose-label <label>] [--parent-namespace <value> --parent-kind <value> --parent-id <value>] [--require-capability <name>]... [--instructions <text> | --instructions-file <path> | --instructions-stdin]
dolgorae [--human] run list [--workspace <path>]
dolgorae [--human] run status <run-id> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] send <run-id> [--workspace <path>] [--write] [--message <text>] [--image <auto|low|high>=<path>]... [--effort <effort>] --idempotency-key <key> [--timeout <duration>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] submit <run-id> [--workspace <path>] [--write] [--message <text>] [--image <auto|low|high>=<path>]... [--effort <effort>] --idempotency-key <key>
dolgorae [--human] run wait <run-id> <turn-id> [--workspace <path>] [--timeout <duration>]
dolgorae [--human] run events <run-id> [--workspace <path>] [--after <cursor>] [--follow] [--projection <minimal|operational>]
dolgorae [--human] run pending <run-id> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] respond <run-id> --request-id <id> --idempotency-key <key> [--workspace <path>] [--response-fd <fd>]
dolgorae [--human] run artifact show <run-id> <artifact-id> [--workspace <path>]
dolgorae [--human] run artifact read <run-id> <artifact-id> --offset <n> --length <n> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] artifact export <run-id> <artifact-id> --output <path> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] interrupt <run-id> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] set-effort <run-id> <effort> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] acquire-write <run-id> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] release-write <run-id> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] pause <run-id> [--workspace <path>] [--interrupt]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] resume <run-id> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] recover <run-id> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] reconcile <run-id> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] fork --from <run-id> [--workspace <path>] [--fresh] [--model <model>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] create-successor --from <run-id> --from-turn <turn-id> --control-mode <direct-interactive|managed-agent> --purpose <purpose> [--purpose-label <label>] --required-assurance <level> [--handoff-summary-fd <fd>] [--artifact-ref <artifact-id>]... --idempotency-key <key> [--workspace <path>]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] close <run-id> [--workspace <path>] [--interrupt]
dolgorae [--human] run [--controller-file <path> | --controller-fd <fd>] delete <run-id> --confirm [--workspace <path>]
dolgorae [--human] run verify <run-id> [--workspace <path>]
dolgorae [--human] run export <run-id> [--output <directory>] [--workspace <path>]
dolgorae [--human] run [--operator-file <path> | --operator-fd <fd>] controller reset <run-id> [--workspace <path>] --confirm <run-id> [--new-controller-file <path> | --new-controller-fd <fd>]
```

`run start` creates an empty idle Dolgorae session and MUST NOT allocate a Codex
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

`run start` MUST bind a controller credential. `human_cli` and
`interactive_client` derive or accept `direct_interactive` and default purpose
to `{kind:"interactive",external_label:null}`, lane to `dedicated`, and
assurance to `best_effort_personal_alpha`. `workflow_orchestrator` and
`automation` MUST explicitly provide `managed_agent`, purpose, lane, and
required assurance; none may silently inherit interactive defaults. Purpose,
including its creation-time external label, is immutable. Parent-reference
arguments are all-or-none. Required
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
non-null epoch. A dedicated Run is published after durable logical-lane
allocation even while its physical state is absent; only a Turn requires a
ready non-null server epoch. Failure to start a physical dedicated generation
leaves the logical Run idle and returns `DEDICATED_SERVER_START_FAILED`; it does
not fabricate a Turn or change lanes.

`run create-successor` requires a `shared_readonly` source, its exact current
terminal Turn, no active Turn, and no unresolved interaction. It creates a new
Run ID and dedicated lane under the same Controller ID/generation/digest. It
never mutates the source lane, copies hidden reasoning, or inherits writer
authority. The immutable lineage records source Run/thread/terminal-Turn IDs,
creation reason, source/destination Controller kinds, timestamp, workspace
baseline, at most 64 artifact references, and the SHA-256 of an optional UTF-8
handoff summary of at most 65,536 bytes. The successor remains threadless and
physically absent until first input; its first instruction composition may
include the bounded summary and selected artifact references. Cross-controller
successors are rejected in v1.

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
and [operator-credential schema](protocol/dolgorae-operator-credential-v1.schema.json)
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
| 8 | Audit integrity verification failure |

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
| `PROFILE_ALREADY_EXISTS` | 4 | `profile add` | the name already exists; replacement is never implicit |
| `PROFILE_SERVER_BUSY` | 4 | profile lifecycle and run connect/recovery | another serialized profile operation or active-member condition temporarily blocks the command |
| `PROFILE_LAUNCH_CONFLICT` | 4 | `profile doctor/server start`, `run start/resume/recover/fork` | the canonical `CODEX_HOME` already has a different active launch contract |
| `PROFILE_MIGRATION_REQUIRED` | 4 | generation-starting run commands | compatible accepted drift changes the shared server key and requires operator-authorized profile migration |
| `PROFILE_MEMBERSHIP_INCOMPLETE` | 4 | profile lifecycle, migration, and membership repair | the manager cannot prove its durable membership journal complete; details name the operator repair action when available |
| `PROFILE_SERVER_EPOCH_MISMATCH` | 4 | run attach/recovery and writer operations | a connection or writer record names a different selected shared/dedicated lane-server epoch |
| `WORKSPACE_INITIALIZATION_CONFLICT` | 4 | `init` | re-init, nesting, Git mode, partial-layout, or policy-file facts conflict |
| `RUN_STATE_CONFLICT` | 4 | all state-changing `run` commands | the lifecycle state forbids the requested transition |
| `POLICY_REJECTED` | 4 | policy-sensitive commands | workspace or hard-agent policy rejects the operation |
| `RUN_BUSY` | 4 | state-changing run commands | another turn owns turn-start serialization, or another contender owns per-run worker startup/attachment serialization |
| `WRITER_BUSY` | 4 | `run send/submit --write`, `run acquire-write` | durable workspace authority names another run or is blocked unknown; details identify the holder and whether a same-controller idle handoff may be prepared |
| `WRITER_HANDOFF_NOT_ALLOWED` | 4 | writer handoff commands | source, destination, or workspace state blocks safe handoff |
| `CROSS_CONTROLLER_RELEASE_REQUIRED` | 4 | writer handoff commands | source and destination have different controllers |
| `STALE_WRITER_GENERATION` | 4 | writer handoff commands | a prepared handoff no longer matches writer or run generations |
| `CONTROLLER_MISMATCH` | 4 | controller-authorized mutation | the supplied capability does not own the run |
| `CONTROLLER_RESET_NOT_ALLOWED` | 4 | `run controller reset` | active work, a pending interaction, handoff, or unverifiable writer state blocks reset |
| `OPERATOR_MISMATCH` | 4 | operator credential rotation, profile stop/restart, controller reset | the supplied separate local operator capability is absent, stale, or invalid |
| `CAPABILITY_UNSUPPORTED` | 4 | `run start`, projection and interaction commands | a required Dolgorae or profile feature is unavailable |
| `ACCESS_TRANSITION_UNSUPPORTED` | 4 | write acquire/release and handoff | the tested profile cannot safely apply the requested policy to the existing thread; fork a new run/thread |
| `BACKGROUND_EXECUTION_UNVERIFIED` | 4 | writer release/handoff/close and recovery | the Dedicated lane-generation census or exact cleanup cannot prove the supported process scope empty |
| `IDEMPOTENCY_CONFLICT` | 4 | `run send/submit/respond` | a run-scoped key was reused with different normalized input |
| `INTERACTION_ALREADY_RESOLVED` | 4 | `run respond` | another valid response already won |
| `INTERACTION_STALE` | 4 | `run respond` | the interaction belongs to an older or cleared generation |
| `INTERACTION_RESPONSE_INVALID` | 4 | `run respond` | the response does not satisfy the recorded normalized schema |
| `FILE_CHANGE_ARTIFACT_UNAVAILABLE` | 4 | `run pending/respond` | the exact correlated proposed change cannot be represented or its artifact is missing, oversized, or digest-stale |
| `ARTIFACT_NOT_FOUND` | 3 | artifact show/read/export | the artifact ID is absent or does not belong to the addressed Run |
| `ARTIFACT_RANGE_INVALID` | 2 | artifact read | offset/length is outside the artifact or the 1-MiB call bound |
| `ARTIFACT_INTEGRITY_FAILURE` | 8 | artifact show/read/export and run verify | stored bytes do not match authoritative artifact metadata |
| `PROJECTION_PROFILE_UNSUPPORTED` | 4 | `run events` | the requested client-safe projection is unavailable |
| `OUTCOME_UNKNOWN` | 4 | state-changing run commands | the run is quarantined; this code takes precedence over `RUN_STATE_CONFLICT` |
| `RECOVERY_REQUIRED` | 4 | writer acquire/release and lifecycle/recovery commands | prior same-run identity or a `blocked_unknown` workspace writer generation cannot be proved safe; new reader runs and projection-only commands are excluded and the code is never generically retryable |
| `PROFILE_MISMATCH` | 5 | all commands that start or reconnect a worker/app-server | executable, `CODEX_HOME`, account, or immutable profile identity differs from the manifest |
| `COMPATIBILITY_REJECTED` | 5 | `profile doctor`, `run start/send/submit/set-effort/resume/recover/reconcile/fork` | version, model, effort, schema, login, sandbox, or app-server capability validation fails |
| `DOLGORAE_PROTOCOL_MISMATCH` | 5 | every ordinary command connecting to an existing worker | workspace/run/generation identity or mutation protocol differs, or Dolgorae version/binary digest differs; retry `hello`, bounded `status`, or `shutdown` through control protocol v1 |
| `PROTOCOL_VERSION_UNSUPPORTED` | 5 | machine commands | the caller requests an unsupported machine or event schema version |
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

`RUN_BUSY` means another turn is already active or won the serialized turn-start
race, or another process owns the run's worker startup/attachment serialization.
`RUN_STATE_CONFLICT` means the run's lifecycle state forbids the requested
operation. Every command specification and conformance fixture MUST use this
table rather than assign an exit class locally.
“State-changing run command” means start, send, submit, respond, interrupt,
set-effort, acquire-write, release-write, pause, resume, recover, reconcile,
fork, close, delete, controller reset, or writer handoff. Status, list, wait,
events, pending, writer status, verify, and export are observers; export
embeds a failing verification result rather than refusing. Confirmed delete is
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

An idempotency key is REQUIRED for `send`, `submit`, and `respond` and is unique
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
with an append-only metadata index. Supported kinds are `file_change_diff` and
`final_response`; reasoning content is forbidden. A file-change artifact is at
most 8 MiB, a final response at most 32 MiB, and total retained artifact bytes
per Run are at most 256 MiB. Retention equals Run lifetime; an unresolved
interaction's artifact cannot be removed. `artifact show` returns metadata only.
`artifact read` uses raw-byte offsets, requires `1 <= length <= 1048576`, returns
base64 content plus actual range and EOF, and verifies the full artifact digest
before first access in an invocation. `artifact export` is controller-authorized,
uses safe create-exclusive destination handling and streaming verification.
Same-uid observers may show/read only artifacts referenced by their client-safe
Run projection; no command exposes the internal artifact path.

Observed paths are populated only when `measured` is true and describe workspace
changes during the terminal turn interval. In Git mode
they are the sorted unique workspace-relative paths from
`git status --porcelain=v2 -z --untracked-files=all`; ignored paths and
`.dolgorae/runs/`, `.dolgorae/runtime/`, `.dolgorae/evidence/`, and
`.dolgorae/cache/` are excluded, while
tracked policy-file changes remain visible. In non-Git mode they are the changed regular files from
no-follow pre/post `(device,inode,size,mtime_ns)` snapshots, also excluding
those four internal directories. Valid UTF-8 paths are strings; other POSIX bytes use
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
`approvalPolicy:"never"`. Writers use thread `sandbox:"workspace-write"`,
turn `sandboxPolicy:{"type":"workspaceWrite","writableRoots":[...],
"networkAccess":false,"excludeSlashTmp":false,
"excludeTmpdirEnvVar":false}`, and `approvalPolicy:"on-request"`.
`writableRoots` is the sorted unique set of the canonical workspace plus, in
Git mode, libc `realpath(3)` of `git -C <canonical-workspace> rev-parse
--path-format=absolute --git-common-dir` and `git -C <canonical-workspace>
rev-parse --path-format=absolute --git-path .`. Every member MUST be absolute
and is deduplicated after resolution. These exact thread and turn carriers are
part of the required-subset manifest. Readers MAY run concurrently without a
Dolgorae limit and MAY observe a writer's intermediate files; there is no
snapshot isolation or rollback. A canonical workspace has at most one Dolgorae
writer across every run and profile.

Writer acquisition is lazy and explicit. `run send|submit --write` MUST activate
durable writer authority before submitting any prompt. `run acquire-write` is
valid only after a thread is durably bound; a threadless run returns
`RUN_STATE_CONFLICT` with reason `threadless_requires_write_turn`. It never
creates a turnless writer thread or treats a reservation as an idle writer.
Dolgorae MUST NOT infer write intent from natural language or use mid-turn
permission escalation. A failed activation MUST NOT start the turn.
Authority remains active across idle, running, waiting, and worker loss until an
explicit release or safe terminal/absence reconciliation completes. Acquisition
never queues automatically.

The writer serializer is BSD `flock(2)` with exclusive semantics on
`.dolgorae/runtime/locks/writer.lock`; a free lock is never evidence that no
writer exists. The authoritative `.dolgorae/runtime/writer.json` is a
versioned, atomically replaced and directory-fsynced state machine with
`none`, `reserved`, `active`, `releasing`, `handoff_prepared`, and
`blocked_unknown` states. It
records workspace/run IDs, controller ID/generation, writer and worker
generations, profile server key/epoch, thread/active-turn IDs, lifecycle,
pending interaction count, last durable event cursor, and recovery state.
Per-run startup locks are
`.dolgorae/runtime/locks/startup/<run-id>.lock`; the handoff serializer is
`.dolgorae/runtime/locks/handoff.lock`. Creation and validation use
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
For a threadless first write, the worker starts the Run's Dedicated Lane Server,
then calls `thread/start` there with writer policy, verifies the
effective policy, and reacquires home, server, writer, then run locks to fsync the lane/thread
binding and `active` state; only afterward may it release locks and call
`turn/start`. An existing dedicated reader uses the same
prepare/apply/verify/commit shape in its current physical generation. If that
generation is absent, it may use `thread/resume` only on a proved successor
generation of the same logical lane. A shared reader cannot use this protocol
and must create a dedicated successor. An existing
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
epoch have been reconciled. A writer crash with an active or uncertain turn
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

The recorded lock root already denotes the directory ending in `locks/`.
Writer locks use `writer/<workspace-digest>` and startup locks use
`startup/<workspace-and-run-digest>` relative to that root and MUST never share an inode. The
per-run startup file contains two POSIX byte-range locks: byte 0 is held by
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
sibling sidecar: `.dolgorae/runtime/runs/<run-id>.json` is the sole socket
identity authority. On recovery, only the byte-0 election winner may authorize
unlink after an exact matching runtime record and prior-generation absence are
proved; the replacement worker performs it after acquiring byte 1 and before
bind. An occupied path with no matching record fails closed. Shutdown attempts
this cleanup for ten seconds and otherwise leaves the path for that verified
next owner.

Acquire or release never restarts the selected shared/dedicated lane server and never substitutes a
connection generation for authorization. A known unavailable/unverified
transition returns `ACCESS_TRANSITION_UNSUPPORTED` before reservation, leaving
the record unchanged. Once a supported transition begins, it follows the
`reserved`/`releasing` crash-safe landings above; failure never falsely restores
the pre-transition state. For read-to-write, the required action is a
new lineage-linked writer run/thread that establishes write policy before
authority activation. For write-to-read, the incumbent cannot simply fork and
release: the user must pause/close it, prove terminal turn and protocol-supported
background-execution absence, retire its connection, transactionally clear
authority, and then create or resume a separate reader. When the pinned protocol
cannot prove that absence, authority remains `blocked_unknown`. Unsupported handoff
fails before commit and uses that same writer-retirement path; it never leaves
two runs believing they own authority. Worker byte-1 ownership does not change
during an actually supported in-place transition.
Start, resume, fork, and recovery otherwise create readers and do not acquire
writer authority.

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
workspace changes.

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
  recognized but unsupported in v1. Dolgorae persists an immediately resolved
  `unsupported_request` interaction, replies JSON-RPC method-not-found, and
  keeps draining the turn. They never appear in `run pending`. Connector
  approval remains unavailable.

Supported requests are represented by the discriminator-bound interaction
kinds `command_execution_approval`, `file_change_approval`, and `user_input`.
`run pending` returns the generation-qualified request ID,
run/thread/turn/item IDs, server epoch, status, exact kind-bound payload,
decisions, response-schema ID, timestamps, and resolution. Approval responses
contain only `decision`. User-input responses contain an `answers` map keyed by
question ID, each with a nonempty string array; Dolgorae validates IDs, option
membership, and `isOther` semantics before translating to the pinned Codex
response. If any answered question is secret, all answers exist only in the
controller request and upstream write buffer and are zeroized. The durable
resolution stores `contained_secret:true`, answer count, the winning
idempotency key, and an opaque UUIDv7 resolution receipt; it stores no plaintext,
unkeyed digest, HMAC, or other content-binding value. Unknown decisions,
question IDs, response schemas,
or raw Codex tokens return `INTERACTION_RESPONSE_INVALID`. Malformed frames
stop the generation. Other known-but-unsupported requests receive JSON-RPC
method-not-found and are recorded.
After method-not-found, Dolgorae keeps draining the generation until Codex emits
a terminal turn event. If Codex instead leaves the turn nonterminal, the Master
sees the run as `running` with no Dolgorae pending request and uses `run interrupt`
as the bounded escape.

The interaction is fsynced before any observer delivery. Controller disconnect
does not affect it, the first valid idempotent response wins, an identical
non-secret retry returns the recorded result, and another response returns
`INTERACTION_ALREADY_RESOLVED`. A response from a non-controller returns
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
snapshot is rejected rather than displayed. Each upstream change preserves the
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
content returns `FILE_CHANGE_ARTIFACT_UNAVAILABLE` and cannot be approved. The
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

Reader auto-decline is implemented solely by `approvalPolicy:"never"`; Dolgorae
does not install a second approval interception mechanism. A server request
that nevertheless arrives is handled by the recognized-unsupported rule above.

The pinned server's session-scoped approval value remains an observed wire
capability but is not exposed as v1 public input.

## SPEC-010: Audit, Retention, and Deletion

Every allocated run has a private directory at `.dolgorae/runs/<run-id>/` with:

- `manifest.json`: fixed run configuration and provenance;
- `audit.jsonl`: the sole append-only audit authority;
- `state.json`: a disposable materialized view rebuilt from the ledger.
- `worker.log` and `worker.log.1`: bounded diagnostics, never audit authority;
- `recovery/`: preserved torn-tail and repair evidence.

The audit contains Dolgorae lifecycle records and redacted app-server wire
records in one total order. Each record contains schema version, sequence/event
cursor, UTC timestamp, run ID, run generation, kind, payload,
`previous_hash`, and `hash`. Lines use RFC 8785 JCS. `sha256-jcs-v1` hashes the
JCS record with `hash` omitted and `previous_hash` retained; the genesis
`previous_hash` is 64 zeroes. SHA-256 chaining detects accidental corruption or
ordinary tampering; it is not a signature and does not defend against a hostile
same-user attacker.

The v1 audit-kind enum is closed:
`workspace_initialized`, `run_created`, `turn_intent`, `thread_bound`,
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
`.dolgorae/runtime/` is mode 0700 and its records are mode 0600. `worker.log` is
limited to 1 MiB with one rotation and remains diagnostics-only.

Audit completeness is limited to Dolgorae lifecycle, app-server-exposed main-turn
wire traffic, approvals, writer-authority transitions, and profile/account provenance.
Encrypted or otherwise unexposed native-subagent communication is represented
as opaque activity when observable and is not claimed as reconstructable audit.

Reasoning text, reasoning summaries, reasoning deltas, and internal planning
streams MUST NOT be persisted in the ledger, projections, logs, diagnostics, or
exports. The worker SHOULD request notification suppression for every reasoning
method pinned by the required-subset manifest and MUST independently filter an
unexpected reasoning method before representation. It appends only the method,
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
content-deterministic. Runtime records, locks, logs, project-local profile configuration,
`CODEX_HOME`, images, and raw torn-tail evidence are excluded. A failing audit
does not suppress export: both bundle and verification set
`verification_failed:true`, while other state-changing commands fail closed.
The bundle may contain plaintext prompts/output that key-name redaction cannot
detect. Its output
path MUST NOT already exist; Dolgorae never merges or overwrites an export and
returns `INVALID_ARGUMENT` on collision.

Automatically retained probe, recovery, and diagnostic evidence MUST live under
`.dolgorae/evidence/`. An export without an explicit output path defaults to a
create-exclusive child of that directory. A user MAY explicitly request an
external export destination; that copy is user output, not runtime authority.

There is no retention limit or automatic deletion. `run delete` is allowed
only for closed or start-failed runs and requires `--confirm`; it is the sole
state-changing command allowed after audit integrity failure and appends no
record to a ledger it cannot trust. It permanently
deletes the Dolgorae run directory only. It MUST NOT delete the Codex thread from
`CODEX_HOME`, and Dolgorae MUST NOT later auto-import that orphaned thread.

## SPEC-011: Agent Instruction and Side-Effect Policy

Dolgorae injects developer instructions that are immutable for one run
generation and identify the process as a master-controlled Dolgorae subagent.
They include run ID and canonical workspace; actual write authority derives
from the durable authority record plus enforced sandbox policy, not prompt
text. Writer acquire/release does not restart the worker, connection, or shared
singleton. Immutable user run instructions remain subordinate and unchanged.
The instructions MUST establish these rules:

- `.dolgorae` is reserved; the agent must not read or modify it unless the master
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

The `.dolgorae` reservation is prompt-enforced policy, not a sandbox deny-list or
same-user security boundary. Changes to `.dolgorae/config.yaml` and
`.dolgorae/.gitignore` remain observable workspace changes; only worker-owned
run/runtime/evidence/cache paths are filtered.

The profile's own Codex configuration, AGENTS instructions, skills, plugins,
and apps remain available unless they conflict with Dolgorae's hard invariants.
MCP servers follow the checked launch snapshot. Native subagents remain
available only when the profile snapshot reports them `supported`. The pinned
0.147.0 default enables `multi_agent`; its corrected live campaign proved the
complete child lifecycle and reports `supported`. Its disabled diagnostic still
created a child and therefore reports `unverified`, never `unavailable`.

## SPEC-012: Orchestration Boundary and Compatibility

Independent Dolgorae runs use hub-and-spoke orchestration: only the master may
create, address, interrupt, fork, pause, close, or delete them. A Dolgorae-managed
agent MUST NOT invoke `dolgorae` to control another run or connect to another
run's worker socket. V1 has no peer messaging or run-to-run delegation.

The shared singleton has one profile-generation environment, so Dolgorae MUST
NOT claim that a per-run `DOLGORAE_*` marker reaches commands, MCP servers, or
native subagents. A diagnostic marker, if observed, is advisory only and cannot
authorize or reject any command. A command launched by Codex that invokes
Dolgorae without the run's controller capability can use the ordinary same-uid
client-safe observer surface but cannot mutate that or another run. Controller
or operator credentials are never placed in the workspace, prompts, argv,
singleton environment, or client-safe events.

When a profile reports native subagents `supported`, their children belong to
the parent run and app-server-managed session tree; they do not create
independent Dolgorae runs or writer authorities. `unavailable` rejects their
use. `unverified` fails closed for any operation that requires proof of native
quiescence and MUST NOT be presented to callers as working support.

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

## SPEC-013: External Runtime and Controller Contract

The machine CLI is the sole required v1 integration transport. CLI, worker, and
any future adapter MUST call one semantic application service with identical
state transitions, authorization, idempotency, errors, and projections. The
private per-run worker socket is not a public contract. V1 provides no public
local socket, WebSocket, HTTP, gRPC, MCP adapter, or workspace-multiplexed event
stream.

`runtime capabilities` MUST return finite machine/event protocol versions,
supported transports, projection profiles, stable Dolgorae feature flags, and
known interaction kinds. It also exposes `access_policy_transition` as
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
It also exposes profile-specific `native_subagents` as `supported`,
`unavailable`, or `unverified`. A feature flag or successful root turn alone
MUST NOT produce `supported`; the pinned probe must observe child identity,
parent relationship, active/terminal lifecycle, and restart behavior. A binary-
level query without a profile returns `unverified`. The exact 0.147.0 enabled
probe passed that complete gate and reports `supported`. The disabled diagnostic
also produced a persisted child, so it reports `unverified` rather than
`unavailable`. A later pin must rerun the same gate; a policy change still
requires operator-authorized profile migration. Binary-level support
does not override a rejected or incapable profile. A run declaring a required
capability MUST fail before allocation when that profile does not provide it.

A controller credential is a strict object conforming to the checked v1 schema.
It contains a UUIDv7 `controller_id`, one of `human_cli`,
`interactive_client`, `workflow_orchestrator`, `automation`, or `other`, a
nonempty instance ID, optional subject ID, and exactly 32 random capability
bytes encoded as unpadded base64url. Instance and subject IDs are at most 128
and 256 UTF-8 bytes and reject NUL and control characters. The credential file
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

Controller authorization applies before worker attachment or any external
effect to `send`, `submit`, `respond`, `interrupt`, `set-effort`, write acquire
or release, pause, resume, recover, reconcile, fork, close, delete, and writer
handoff. Controller equality requires both the controller ID and a constant-time
comparison of the persisted capability digest. A normal fork inherits the
controller. A mismatch is non-retryable and MUST NOT reveal whether controller
ID or capability comparison failed. All same-uid local callers may list runs
and read status, wait results,
pending interaction payloads, client-safe events, writer status, verification,
and exports without a capability. Controller metadata is visible; capability
bytes and their digest are never visible. External applications own any
authentication and authorization applied before exposing those observer results
remotely.

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
| Handoff prepare/commit | Handoff, writer, source/destination run locks in UUID order | None |
| Handoff cancel | Handoff, then affected run locks in UUID order | None |
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

Purpose and parent metadata are opaque to Dolgorae authority behavior.
`purpose.kind` is the closed enum `interactive`, `planning`, `implementation`,
`review`, `research`, `discussion`, `workflow_stage`, or `other`; an optional
external label is at most 128 UTF-8 bytes.
`parent_ref.namespace`, `kind`, and `id` are all-or-none, limited to 128, 64,
and 256 UTF-8 bytes, and reject NUL/control characters. They may be used only
for filtering, display, and audit provenance; Dolgorae MUST NOT implement
mission, task, role, finding, review-disposition, or workflow semantics.

Client-safe event cursors are canonical decimal-string run-ledger sequences and
survive observer disconnect, worker replacement, and server epoch changes.
Every record identifies its originating server key/epoch. Multiple observers
never apply backpressure to the App Server connection, hold writer authority, or affect pending
interaction persistence. Retention equals the run ledger lifetime in v1.
Duplicate transport delivery is permitted, but an event ID plus cursor MUST
make deduplication deterministic and replay MUST NOT execute a side effect.

## SPEC-014: Control Modes, Execution Lanes, and Assurance

Every Run MUST durably record immutable `control_mode` and `execution_lane`
values at creation. `control_mode` is `direct_interactive` or `managed_agent`;
`execution_lane` is `shared_readonly` or `dedicated`. `purpose` is one of
`interactive`, `planning`, `implementation`, `review`, `research`,
`discussion`, `workflow_stage`, or `other`. The canonical purpose is the
immutable object `{kind,external_label}`; the nullable label is also fixed at
creation. Purpose is descriptive metadata and MUST NOT select or change the
execution lane, writer authority, controller, or assurance policy.

A direct interactive Run accepts only a `human_cli` or `interactive_client`
Controller and defaults to purpose `interactive`, lane `dedicated`, and
required assurance `best_effort_personal_alpha`. It may explicitly request
`shared_readonly`, which is permanently read-only. A managed Run accepts only
`workflow_orchestrator` or `automation`; control mode, purpose, lane, and
required assurance MUST all be supplied. Controller kind `other` MUST NOT bind a v1 Run. Kkotge-like
interactive clients contain no LLM and are direct controllers, not managed
agents.

Instructions use `dolgorae.instructions/v1`: common prefix version 1 plus mode
prefix version 1, purpose prefix version 1, and bounded Controller instructions.
The direct prefix routes
normalized approvals and user input to its interactive Controller. The managed
prefix routes low-level interactions only through its orchestrator or
automation Controller and tells the model not to search for controller
credentials. The Controller capability MUST remain outside prompts, developer
instructions, model/tool input, environment, machine output, audit, events,
diagnostics, and persisted Run state. For a shared Run, every turn additionally
carries Codex
`collaborationMode:{mode:"plan",settings:{model:<selected>,developer_instructions:<composed>}}`,
read-only sandbox, `networkAccess:false`, and `approvalPolicy:"never"`. Its
immutable prefix prohibits workspace modification, privilege escalation, and
long-lived background work, permits bounded validation under the OS temporary
directory, and directs workspace-writing validation to a dedicated successor.
This is a personal-alpha behavior boundary, not per-Run process containment.
Observers receive only authorized
redacted projections and MUST NOT resolve an interaction. V1 has no single-use
observer delegation.

One profile owns one shared read-only logical lane and zero or more dedicated
logical lanes. A shared Run's persistent thread is loaded only in the shared
server and its effective policy MUST remain verified read-only. It MUST NOT
acquire workspace writer authority or be promoted in place. When it needs
write, Dolgorae creates a lineage-linked dedicated successor or returns
`SHARED_RUN_WRITE_FORBIDDEN`. Read-only foreground commands are allowed in Plan
Mode. Shared background control is `profile_aggregate_only`: Run close cannot
claim per-Run descendant cleanup, and profile stop owns complete aggregate
census and cleanup.

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
has passed, no active or unknown goal, interaction, turn, or native descendant
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
a shared Run needing write uses a fresh dedicated successor. The public
`run create-successor` operation requires the source's current terminal Turn,
no active Turn or pending interaction, the same Controller binding, and an
idempotency key. It records immutable lineage and creates a threadless,
physically absent dedicated Run. Cross-controller successors are unsupported in
v1; ordinary `run fork` remains a separate history-copy operation.

Lane-specific errors are used only for distinct recovery semantics.
`EXECUTION_LANE_UNSUPPORTED` rejects a profile that cannot host the selected
lane; `EXECUTION_LANE_IMMUTABLE` rejects an attempted in-place lane change;
`SHARED_RUN_WRITE_FORBIDDEN` requires a dedicated successor;
`THREAD_RESIDENCY_CONFLICT` requires residency reconciliation;
`SAME_HOME_MULTI_SERVER_UNSAFE` rejects a profile whose pinned campaign failed;
`ASSURANCE_LEVEL_UNAVAILABLE` permits only lowering the pre-allocation request;
`DEDICATED_HISTORY_BARRIER_FAILED` blocks generation replacement;
`PROFILE_LANE_MIGRATION_REQUIRED` requires operator migration of the complete
lane set; and `DEDICATED_SERVER_START_FAILED` permits a bounded retry of the
same logical lane. Identity uncertainty still uses `RECOVERY_REQUIRED`, active
goal/native work uses `RUN_BUSY`, workload uncertainty uses
`BACKGROUND_EXECUTION_UNVERIFIED`, and writer/handoff conflicts keep their
existing errors.

## External Protocol References

- [Official Codex app-server documentation](https://learn.chatgpt.com/docs/app-server)
- [Official Codex subagents documentation](https://learn.chatgpt.com/docs/agent-configuration/subagents)

These references describe Codex behavior. Dolgorae-specific policy in this SOT
remains authoritative for Dolgorae.
