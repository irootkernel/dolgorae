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

## Use low-level review targets

Prefer `specialist review` for ordinary review. Use low-level capture only when
an authorized external backend genuinely owns the lifecycle and can later
provide a checked terminal receipt:

```sh
dolgorae review-target capture \
  --workspace <path> \
  --kind <kind> \
  --backend-kind <backend> \
  --backend-lifecycle-id <lifecycle-id> \
  --settlement-owner-file <new-protected-path>
```

Add `--revision` only where the target-kind contract permits it. Preserve the
capture reference, revision, backend binding, digests, and settlement-owner
carrier. Never expose the immutable root or credential to an untrusted actor.

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
conflict exit class. Preserve its capture reference, engagement state, required
action, and retryability details, then reconcile the outcome rather than
replaying or deleting it. If a process terminates without a final envelope,
treat the outcome as unknown and reconcile it through supported inspection.
Dolgorae has no generic skill-owned cleanup, uninstall, or legacy-root migration
procedure. Never map a request to clean local files onto direct deletion under
`~/.dolgorae`.
