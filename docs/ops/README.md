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
The global `--human` flag is accepted but never reformats the readiness
envelope, which remains a supervisor-facing machine contract.

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

For a task-aware one-shot review, validate the v3 request against the checked
protocol before piping it to `specialist review --request-stdin --format json`.
Do not put workspace, Profile, credentials, or host paths in the request. Treat
`REVIEW_OUTPUT_INVALID` as a terminal rejected report: inspect the accepted
criteria and obtain a new explicitly authorized review rather than editing the
artifact or interpreting empty findings as success. A timeout or disconnect
does not authorize replay; preserve the returned engagement, capture, and
settlement identities for diagnosis. Legacy v1/v2 output is not a
completion-aware substitute.

Keep the native CLI process/result handle while waiting and collect its exit
and complete envelope. A transport wait ending does not itself cancel accepted
work, but a host timeout that interrupts or terminates the provider-backed CLI
can cancel the review or leave its outcome unknown. Before starting a recoverable
review, retain a UUIDv7 reference and a private Controller credential, then pass
`--request-ref` and `--recovery-controller-file` together. If the process handle
or response is lost, use `specialist review-inspect --workspace <path>
--request-ref <saved-reference> --format json` from a new process. An engagement
ID alone does not grant access through the external facade.

Inspection returns the original result or diagnostic and separate Reviewer,
engagement, capture, and server states. Check these states even when the initial
command reports a review error: that error takes precedence over a later cleanup
failure. An `unknown` observation means the evidence cannot establish the original
operation; it does not prove that no review ran. Preserve known evidence without
querying private databases or starting a replacement review as recovery.

For authorized cleanup, use `specialist review-recover` with the same selectors,
the original `--recovery-controller-file`, and `--action cleanup`. Cleanup closes
proven terminal members and settles their capture. With the original invocation's
`--temporary-server` opt-in, it can retire only that invocation's verified server
generation. Active or uncertain states remain blocked; cleanup never implicitly
interrupts a Turn. An interrupted stop can also remain blocked when its generation
or process evidence is incomplete. Missing server state alone does not prove
retirement. Preserve the observation for diagnosis through the existing lifecycle
interfaces; do not infer signalling authority from a PID or an idle server.
Pre-existing shared Profile Servers remain alive.

The source skill includes canonical schema copies and examples under
`resources/protocol/`, with a generated inventory in `resources/manifest.json`.
Use the [README installation procedure](../../README.md#agent-skill) from the
matching source tag. It rejects missing or stale resources and preserves an
existing destination. Resolve schema IDs through the installed local registry;
do not fetch missing dependencies from the network or a checkout.
`make validate-agent-skills` checks canonical byte equality, temporary
installation, local reference resolution, and missing/stale resource failures.
The default E2E gate runs native wait, interruption, public recovery, an
isolated defective-Hello-World v3 scenario against the installed resources,
and a credential-free check of the live campaign's admission guards.

After separate live-account authorization, run `make test-live-independent-review`
with `DOLGORAE_RUN_LIVE_INDEPENDENT_REVIEW=1`,
`DOLGORAE_CODEX_MINIMUM_BIN` pointing to Codex 0.157.1, and
`DOLGORAE_LIVE_AUTH_FILE` pointing to the approved credential file. The campaign
uses the exact local candidate binary and installs its matching skill package
into isolated test state. It copies credentials into protected isolated Codex
homes, leaves Profile model and effort unset, and runs both the ordinary review
and a public recovery after losing the original CLI response. Its output contains
bounded version, executable, candidate, result, and source digests plus the
recorded capability verdict; `unverified` is not a full compatibility claim.
Keep raw reports, logs, credentials, and runtime identifiers out of tracked
evidence. Installed-package and deterministic checks do not establish live
acceptance; follow [EPIC-016](../roadmap/README.md#epic-016-reliable-independent-specialist-review)
for the completion gate.

The source-distributed [`use-dolgorae` skill](../../skills/use-dolgorae/SKILL.md)
provides capability-adaptive agent guidance for the currently supported setup,
configuration, immutable-target, one-shot review, public-provider, and
externally planned reusable Specialist Engagement surfaces. It is not an
operator runbook or a semantic authority, and loading it never authorizes a
mutation, external review, live account use, engagement lifecycle change,
installation, or recovery action.

## v0.1.3 provider operation

Use this runbook for the checked `dolgorae.gul-consumer/v1` provider profile,
not for the deferred collaboration surface or actual Gul acceptance. Resolve
one exact Dolgorae executable, verify its version and runtime capabilities, and
run `profile doctor` for the selected Profile. Starting a Profile Server,
installing a Specialist Policy, creating a Controller, submitting a live Turn,
and closing a Session remain separately authorized mutations. A deterministic
fake-Codex test does not authorize a live account or token use.

Start `dolgorae serve` as described in the gateway recovery runbook above. A
generated client first sends protocol zero to `GetCapabilities`, then requires
protocol 1, the checked descriptor SHA-256, and all 27 methods in the frozen
consumer profile. Additional advertised optional methods do not invalidate the
required subset. Stop before allocation if the digest, required methods, limits,
or accepted client range differs. The supported v0.1.3 slice has no
busy-member queue, lateral collaboration, activation/passivation, or
`reuse_any_compatible`; those capabilities must remain unadvertised.

Create one generation-1 `interactive_client` Controller carrier with the
intended orchestration policy. Keep the bearer bytes in Dolgorae's owner-only
carrier file and pass only its checked reference in public requests. Never put
the capability, private socket, database path, child Controller, worker
identity, or host artifact path in model input, logs, or tracked evidence.

The normal call order is:

1. Inspect the workspace and Profile, verify the Controller, and call
   `StartRun` with a fresh operation-scoped idempotency key.
2. Call `SubmitTurn` with the current Run revision. While that Turn is active,
   retain later human input as a client draft; v0.1.3 rejects a second live
   submission instead of queueing or steering it.
3. Let the Primary use its host-bound `dolgorae_orchestration` tool to request a
   Specialist. Under `user_approval_required`, list and fetch the root Run's
   pending approval and resolve the exact `specialist_approval` interaction
   with the root Controller. Under `fully_delegated`, require the immutable Role
   to permit automatic approval.
4. Let the Primary list a ready member, assign one task, await or collect its
   result, and read every result page. Retire members through whole-session
   `CloseRun`; the live Primary tool cannot release an individual member.
5. Observe the aggregate through `GetOrchestratedSession`. Discover published
   Primary-owned artifact references only through
   `ListOrchestratedSessionResults`, then call `GetArtifact` and
   `ReadArtifactChunk`. Concatenate bounded chunks and verify the advertised
   byte length and SHA-256 before using the result.
6. Call root `CloseRun` only with explicit whole-session-close authority.
   Preserve its close operation identity until the aggregate reports a final
   disposition.

For example, a client starts negotiation with a request context whose protocol
version is zero and accepts the server-selected version only after checking the
descriptor and method set. Subsequent protected calls carry a Controller
reference, not bearer bytes. A result reader first records the result-list page
head, then downloads the listed artifact from offset zero in chunks no larger
than the advertised maximum until the returned total length is reached; it
rejects any digest or length mismatch.

Treat a transport timeout as an unknown response, not rejection. For an
idempotent mutation, refresh `GetRun` or the aggregate and reconcile the
original key before issuing conflicting work. Exact accepted task replay
returns its original receipt; a fresh assignment to a busy member is rejected
before effects. An await timeout leaves accepted work running. Preserve the
durable task deadline; restarting a client or gateway does not reset it.
`SESSION_CLOSE_IN_PROGRESS` means observe or recover the recorded close, not
submit another close intent.

Resume `WatchRunEvents` strictly after the last durable cursor. A stream ending
with `SERVER_SHUTDOWN` describes the gateway, not the Run; restart the gateway,
recheck capabilities, refresh projections, and reconnect from that cursor.
Never derive retry behavior from human-readable status text.

Success requires the exact public profile, complete expected timeline and
session/result projections, verified artifact bytes, and no leaked secret or
private identifier. Provider conformance establishes `MILESTONE-BH1-P` only.
Stable release, tag, publication, installation, Personal Alpha, and actual Gul
integration require their own authorization and evidence.

Development, testing, and release-engineering guidance belongs in the
[implementation tips](../implementation-tips/README.md), not this operations
owner. Never record credentials, tokens, private keys, live secret values, or
secret-bearing command output here.
