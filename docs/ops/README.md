# Dolgorae Operations

This index owns operational guidance for configuring, observing, diagnosing,
recovering, and safely updating Dolgorae's local per-user and per-workspace
runtime surfaces. Repository maintainers own this role and are the escalation
owner for missing or unsafe procedures.

The procedures below are the approved operator runbooks. The product
[specification](../specs/README.md) and
[architecture](../architecture/README.md) define normative operator and recovery
behavior, but they are not procedural authorization. Add a runbook only after
its target, prerequisites, safe diagnosis, bounded resolution, success checks,
rollback, and escalation path are verified against the implementation.

## Supervised public gateway recovery

Run `dolgorae serve` only under a supervisor that retains the foreground
process, its single readiness envelope, and its exit status. Before launch,
verify that the Dolgorae home uses the supported generation and choose an
absolute socket path whose existing parent is owned by the current user with
mode 0700. The socket name must be unused or must be the exact stale socket
recorded for a provably absent prior gateway.

Launch the gateway with `--socket <absolute-path>`. A supervisor may also pass
one inherited writable descriptor with `--ready-fd <fd>`; in that case the
readiness envelope is written there instead of standard output. Success is one
`ok: true` envelope containing the selected socket path, a new server instance
ID, protocol range, and public descriptor digest. Connect only as the same OS
user, call `GetCapabilities`, and use only the advertised methods.

Diagnose a failed start from that one machine envelope:

- `RPC_SERVER_ALREADY_RUNNING` means another process still holds the
  installation gateway lock. Connect to the reported existing gateway or ask
  its supervisor to send `SIGTERM`, then wait for that process to exit. Do not
  start a competing socket or remove its lock, record, or socket.
- `RPC_SOCKET_UNSAFE` means Dolgorae could not prove the socket path, parent,
  lock, record, or prior process identity. Correct an unsafe parent's ownership
  or mode, or select a fresh private parent and unused path. Preserve symlinks,
  foreign files, unrecorded sockets, replaced inodes, and malformed or
  unverifiable gateway state for escalation; a client must never unlink them.
- A stale socket from an ungraceful stop needs no manual deletion when its
  device and inode still match the private gateway record and the recorded
  process is provably absent. Restart with the same path and let Dolgorae
  validate and replace it. If either identity differs, stop and escalate.

For a planned stop, send `SIGTERM` through the supervisor and allow at least
five seconds for admitted unary calls to drain. Open streams end with
`SERVER_SHUTDOWN`; a clean exit unlinks only the socket inode that gateway
created. After a crash, durable Runs, workers, writer authority, and App Server
lifetimes remain independent. Restart the gateway, verify a new readiness
identity and capabilities response, then resume event streams from each
client's durable cursor and retry mutations only through their documented
idempotency or reconciliation contract.

Success means the prior process is absent, the replacement reports readiness,
the socket is a current-user mode-0600 Unix socket, and `GetCapabilities`
matches the checked public descriptor. If startup still fails, leave the
observed path and `~/.dolgorae/rpc` state untouched and escalate the complete
machine error and supervisor exit status to the repository maintainers. There
is no in-place rollback beyond stopping the replacement; the prior gateway can
be relaunched only after its own process and socket identity are revalidated.

## Global Profile generation recovery

After the EPIC-013 activation, `LEGACY_STATE_UNSUPPORTED` means Dolgorae found a
nonempty unmarked, legacy, partial, mixed, malformed, or unsupported fixed home.
Dolgorae does not repair or migrate that state. The operator must:

1. Stop every Dolgorae process and verify no command is using
   `~/.dolgorae`.
2. Back up or move the complete `~/.dolgorae` directory as one unit. Do not
   copy individual registry, Run, server, writer, engagement, or aggregate
   files into the replacement home.
3. Run `dolgorae init` against one intended project to atomically establish a
   fresh `global-profile-v2` home and register that workspace.
4. Recreate each Codex Profile explicitly with its native executable, canonical
   `CODEX_HOME`, arguments, environment, and capability policy.
5. Run `dolgorae init` in each remaining compatible project. Existing tracked
   `.dolgorae/config.yaml` files are retained byte-for-byte and their workspace
   IDs are recomputed from the canonical paths; fresh machine-local workspace
   state is created.

Success means the new marker validates, every recreated Profile passes doctor,
and each intended workspace reports its original deterministic workspace ID
with fresh empty machine-local state. If any check fails, stop all Dolgorae
processes, remove the incomplete replacement home, and restore the moved home
as a complete unit. That rollback restores the legacy condition for diagnosis;
it does not make the state compatible. Escalate any identity mismatch or
unexpected mutation to the repository maintainers before retrying.

## Specialist Role and policy operation

Treat project Role sources as executable project-authored instructions only
after the user explicitly authorizes policy installation. Before adding a
policy, inspect the selected global Profiles, verify the Role source scope and
name, ensure each selected Profile Server is already running, and run the
read-only compiler:

```sh
dolgorae specialist policy validate \
  --workspace /absolute/project \
  --file /absolute/policy-input.json
```

Common Role directories and files require owner-only modes 0700 and 0600.
Project Role paths must be owned by the current user and must not be writable by
group or others. `CONFIG_INVALID` leaves the installed registry unchanged;
correct the named source or policy input and validate again. Never repair the
registry, an installed snapshot, the orchestration database, or a broker
credential carrier by editing its files directly.

Validation observes the running server's model, effort, and capability catalog;
it does not start or repair a stopped server or record a Profile binding.

After an authorized `specialist policy add`, use `show` to verify the returned
name, revision, Role source digests, and resolved Agent Configuration snapshots.
Adding an existing name fails closed. Replacement is an explicit `remove`
followed by `add`; removal changes only future session selection. Existing
Orchestrated Sessions retain their complete policy snapshots and remain
recoverable without the source or registry entry.

The source-distributed [`use-dolgorae` skill](../../skills/use-dolgorae/SKILL.md)
provides capability-adaptive agent guidance for the currently supported setup,
configuration, immutable-target, one-shot review, and externally planned
reusable Specialist Engagement surfaces. It is not an operator runbook or a
semantic authority, and loading it never authorizes a mutation, external
review, engagement lifecycle change, installation, or recovery action.

Development, testing, and release-engineering guidance belongs in the
[implementation tips](../implementation-tips/README.md), not this operations
owner. Never record credentials, tokens, private keys, live secret values, or
secret-bearing command output here.
