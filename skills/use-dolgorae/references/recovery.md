# Dolgorae Recovery

Read this reference after stale state, response loss, an interrupted or unknown
operation, integrity failure, incomplete membership, or unsafe profile state.
Recovery starts with current public evidence and never with direct state edits.

## Reconcile current evidence

1. Preserve the exact executable path, command arguments without secret bytes,
   process exit, complete JSON envelope, invocation ID, and returned object
   identities. A timeout, disconnect, or exit 130 does not prove rollback.
2. Re-read only the relevant public state, for example:

   ```sh
   dolgorae runtime capabilities
   dolgorae workspace inspect --workspace <path>
   dolgorae profile show <name> --workspace <path>
   dolgorae profile doctor <name> --workspace <path>
   dolgorae profile server status <name> --workspace <path>
   dolgorae profile membership verify <name> --workspace <path>
   ```

3. Parse exit and envelope independently. The stable exit classes are: 2 input,
   3 not found, 4 conflict or recovery precondition, 5 compatibility or protocol,
   6 runtime or transport, 7 failed or interrupted Turn, and 8 integrity. Exit
   130 and exits outside the documented set have no semantic machine envelope.
4. Retry an unchanged invocation only when `error.retryable` is true and current
   evidence still supports the exact same target and request. Retryability does
   not promise progress or absence of prior effects. Never weaken an identity,
   revision, digest, or credential fence to make a retry succeed.

## Review and capture uncertainty

A one-shot review creates a fresh Reviewer. Do not rerun after response loss,
timeout, interruption, malformed output, unknown engagement state, incomplete
settlement, or missing result merely because no findings were returned. Preserve
every returned review, engagement, Reviewer Run, capture, and settlement identity
and report the unresolved authority.

Low-level captures remain recovery evidence until a checked terminal receipt is
accepted. On `REVIEW_TARGET_MUTATED`, stale revision, owner mismatch, lifecycle
mismatch, missing terminal evidence, or unknown outcome, do not settle again
with changed input and do not delete the immutable root manually. Reinspect the
source and backend independently, then use only an identical supported replay or
an explicitly documented recovery action.

## Profile and process failures

- `PROFILE_CONFIG_INVALID`, `PROFILE_NOT_FOUND`, and
  `PROFILE_ALREADY_EXISTS` require correcting or explicitly replacing
  configuration; they are not transient retries.
- `PROFILE_SERVER_BUSY`, migration fences, incomplete membership, launch
  conflicts, or unverifiable process identity require current status,
  diagnostics, and membership evidence. Do not kill a process or remove a lock,
  socket, journal, or registry entry manually.
- Bare `profile doctor` is offline. Use `--launch-probe`, server control,
  membership tombstone, state reset, or operator-authorized migration only when
  that exact intervention is requested and its current preconditions are known.
- `TRANSPORT_FAILURE` is retryable only when the envelope says so; possible
  external acceptance requires reconciliation first.
- Integrity, redaction, protocol-version, and `OUTCOME_UNKNOWN` failures fail
  closed. Preserve evidence and escalate instead of repairing bytes in place.

If supported public observations cannot establish the outcome, stop with the
exact target, identities, error code, checks performed, and smallest documented
next action. Do not claim that a recovery check completed, cancelled, settled,
committed, published, installed, or removed anything it only observed.
