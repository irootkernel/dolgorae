# Dolgorae Operations

This index owns operational guidance for configuring, observing, diagnosing,
recovering, and safely updating Dolgorae's local per-user and per-workspace
runtime surfaces. Repository maintainers own this role and are the escalation
owner for missing or unsafe procedures.

The global Profile generation recovery procedure below is the only approved
operator runbook. The product
[specification](../specs/README.md) and
[architecture](../architecture/README.md) define normative operator and recovery
behavior, but they are not procedural authorization. Add a runbook only after
its target, prerequisites, safe diagnosis, bounded resolution, success checks,
rollback, and escalation path are verified against the implementation.

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
