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
exact confirmed server key. Migration requires a separate Operator credential
and both the exact old and new confirmed server keys, whether or not
`--interrupt` is used. Obtain keys and membership facts from current public
status and diagnostics, never from a path name or old output.

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
