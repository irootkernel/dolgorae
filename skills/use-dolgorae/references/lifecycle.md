# Dolgorae Lifecycle

Read this reference only for an explicitly requested initialization, profile
server action, credential operation, low-level review-target lifecycle, removal,
or cancellation. Diagnose with public read-only commands before changing state.

## Initialize a workspace

Inspect the exact target and its Git identity first. Initialization is explicit:

```sh
dolgorae init <path>
dolgorae init <path> --non-git
```

Use `--non-git` only when the user explicitly chooses the supported non-Git
mode. A missing workspace, generic setup request, or nearby `.dolgorae/` marker
does not authorize initialization. Repeated initialization succeeds only for an
identical compatible layout and never authorizes replacement of project policy.
Afterward, run `dolgorae workspace inspect --workspace <path>` and inspect Git
status so project-policy changes remain visible.

## Manage Profile Servers

Use status before every process lifecycle action:

```sh
dolgorae profile server status <name>
```

`profile server start` may launch a long-lived local Codex App Server and
requires explicit intent. Stop and restart require a separate Operator
credential; when either operation interrupts live members, it also requires the
exact confirmed server key. On builds with quiescent upgrade rollover (`EPIC-024`
/ `TASK-096`+), fresh qualified and successor Run starts, external or brokered
Specialist typed `StartRun`, CLI `specialist review`, and MCP adapter
preparation can replace a `ready` server in the same `CODEX_HOME`.
No live, starting, uncertain, or orphan members can remain, and version
precedence must satisfy `candidate >= incumbent`. Earlier builds, downgrades (including
legacy Run recovery against a newer idle server), active or orphan membership,
`profile doctor --launch-probe`, explicit `--confirm-server-key` commands, and
retained execution of already-accepted Run snapshots remain non-rolling;
replacing a contract across those boundaries requires a separate Operator
migration with both the exact old and new confirmed server keys, whether or not
`--interrupt` is used. Obtain keys and membership facts from current public
status and diagnostics, never from a path name or old output.
The legacy MCP adapter keeps its pinned `mcp_unavailable` tool disposition.

For an explicitly selected Caller-qualified generation, use the exact key from
the owning caller's checked current preparation or accepted binding:

```sh
dolgorae profile server start <name> --confirm-server-key <key>
dolgorae profile server stop <name> --confirm-server-key <key> --operator-file <path>
```

The start records that checked binding before its deny-rollover launch. Qualified
stop is non-interrupting and refuses `--interrupt`; it rechecks the original
binding, current definition, membership and process identity. Omitting the key
keeps generic selection. Name-based status, restart and migration retain their
generic contract and do not discover a qualified generation implicitly. A key
never grants lifecycle authorization or permission to replace another launch.

Do not infer authorization for stop, restart, migrate, membership tombstone, or
profile-state reset from a failed doctor check. Read the current failure details,
verify membership and process identity through supported commands, present the
bounded effect, and require the exact lifecycle request before acting.

## Manage credentials

Operator and Controller credentials are bearer capabilities. Create or rotate
one only for an explicitly named consumer and output path:

```sh
dolgorae operator credential initialize --output <new-protected-path>
dolgorae operator credential rotate --operator-file <current-path> --output <new-protected-path>
dolgorae controller credential create \
  --kind <kind> \
  --instance-id <id> \
  --output <new-protected-path>
```

Use a new protected output file. Never overwrite, display, log, or place
credential bytes in argv. A credential file authorizes only the operation and
principal accepted by the current command; its existence is not permission to
perform another mutation. The composed one-shot Specialist Review owns its
internal credentials, so do not create replacements for it.

## Close a modern Orchestrated Session

With explicit whole-session termination authority, use the root's original
Controller with `dolgorae run close <run-id> --workspace <path>
--controller-file <path>`. Add `--interrupt` only when interruption of active
owned work is authorized. Modern roots use this checked Machine entrypoint;
accepted legacy roots retain public `CloseRun`.

Require the root's `lifecycle:closed`, `writer_state:none` and a completed or
aborted `session_close` record. Preserve its operation ID and interrupt choice.
`SESSION_CLOSE_IN_PROGRESS` requires observation; `RECOVERY_REQUIRED` requires
authorized `run recover`, and `OUTCOME_UNKNOWN` requires authoritative
reconciliation through `run reconcile`. These commands resume the retained
intent without starting a Turn or replaying an uncertain effect. A Profile
Server stop is a separate Operator action and does not prove aggregate closure.

## Delete a caller-workspace Run

For a settled Run, confirm deletion with the current Controller.

```sh
dolgorae run delete <run-id> --workspace <path> --controller-file <path> --confirm
```

Use force deletion only when the user authorizes abandoning that Run and
stopping its generation. Read the canonical workspace ID through
`workspace inspect` and use the current Operator credential.

```sh
dolgorae run delete <run-id> --workspace <path> --force --confirm \
  --confirm-workspace-id <workspace-id> --operator-file <path>
```

After a lost reply, repeat the confirmed command for the same target. Report
any ownership or handoff refusal using the command's recovery details. Both
commands preserve the Codex thread and prevent reallocation of the Run UUID.
Force deletion can also abandon a session Primary whose retained binding
matches and whose session has no Specialist dependencies or Controller close
intent. If startup ownership was incomplete and the recorded control processes
are absent, deletion can revoke the Run and its Writer authority while retaining
the runtime locator. A result with `runtime_cleanup_pending: true` means that
descendant cleanup remains unverified. The same confirmed command can retry
cleanup after the host has restarted.

## Use low-level review targets

New reviews use `specialist review` with the v4 direct-reading contract.
`review-target capture` returns `SOURCE_ISOLATION_RETIRED` before creating
source copies or credential carriers. Accepted historical captures keep their
original IDs, backend ownership, receipts and settlement authority.

Settlement is a separate authorized mutation:

```sh
dolgorae review-target settle \
  --workspace <path> \
  --capture-ref <capture-ref> \
  --expected-revision <capture-revision-integer> \
  --settlement-owner-file <protected-path> \
  --terminal-receipt-file <checked-protected-receipt-path>
```

The expected revision is the unsigned capture revision returned by the capture
operation, not a Git revision.

The backend, not the agent, must produce the authoritative checked terminal
receipt. Active, unknown, stale, mismatched, unverifiable, or concurrently lost
settlement preserves the capture and is not cleanup permission. Exact accepted
replay is idempotent; changed input is not.

## Cancellation and unsupported cleanup

Interrupt a running one-shot review only on explicit user intent. Dolgorae
handles that interrupt as a checked `REVIEW_CANCELLED` failure envelope in the
conflict exit class. Preserve the original request, review, target and
engagement identities, required action, and retryability details. For accepted
legacy work, also preserve its capture reference when one exists. Reconcile the
outcome rather than replaying or deleting it. If a process terminates without a final envelope,
treat the outcome as unknown and reconcile it through supported inspection.
Dolgorae has no generic skill-owned cleanup, uninstall, or legacy-root migration
procedure. Never map a request to clean local files onto direct deletion under
`~/.dolgorae`.
