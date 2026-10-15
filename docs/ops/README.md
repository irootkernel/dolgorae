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

## Failed worker thread recovery

A reader error, or a drain, control-thread or Primary tool panic, stops the
failed worker generation. Owned control connections are closed before caller
threads are joined, including partial-frame and stalled-response peers.
Existing typed outcomes have a fixed 250 ms delivery grace; slow peers cannot
extend it. Queued
Primary tool calls cannot enter the bridge after failure is latched.
Preserve the original Run, operation key, accepted intent and receipts. A usable
journal records uncertainty; a poisoned journal remains unavailable for append.
The retained control-recovery marker belongs to that exact generation, including
a failure before startup reports ready. Use the selected build's checked
recovery operation with the original workspace and authority. Recovery must
prove the old generation's exact process absence and settle its writer/history
obligations before replacement. Do not redispatch accepted work, repair a
poisoned journal in memory, delete its history or signal an unverified PID.

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

`make test-native-integrated-acceptance` is a separately authorized acceptance
campaign. Set `DOLGORAE_RUN_NATIVE_INTEGRATED_ACCEPTANCE=1` and point
`DOLGORAE_CODEX_BIN` to the pinned Codex executable. It exercises the production
CLI with isolated homes, Profiles, credentials, and caller workspaces, using
at most 17 Turns and 34 loopback Responses requests. It never loads a real
account or sends requests to an external provider. Its isolated Profile disables
optional Codex plugins and verifies the captured setting so background plugin
downloads cannot determine startup readiness. Plugin-enabled startup is outside
this campaign's qualification. The campaign checks startup defaults,
read-only enforcement, same-thread access changes, captured instruction replay,
all direct review scopes, reusable Reviewer tasks, and accepted Primary/child
policy configurations, including the Primary's actual `request_specialist`
tool call. The modern Primary's checked Machine close must retire its child,
release the Writer and commit aggregate completion. Fresh CLI observation,
repeat close and recovery must retain its close operation and interrupt choice.
An idle writable Primary must also complete checked interruption and recovery
with its retained abort operation. This does not qualify a live active Turn abort.
A nested turnless campaign checks writer exclusion and recovery.
Cleanup uses the fixture's Operator to interrupt only its confirmed Profile,
then requires process-inventory absence before removing the fixture. It does
not project modern Runs through the frozen public-v1 adapter.
Failures retain their owned fixture for diagnosis; they do not retry
automatically. This campaign is outside the default gate.

New Roles are Markdown files at `~/.dolgorae/roles/<name>/AGENTS.md`; common
instructions come from `~/.dolgorae/home/AGENTS.md`. Treat their contents as
instructions requiring explicit installation authority. Direct starts default
to writable access and the general Role. Policy v3 names a Role and supplies
execution controls separately. New JSON policy admission fails with
`ROLE_MIGRATION_REQUIRED`; accepted legacy policies and sessions retain their
original snapshots.

Use `instructions migrate --workspace <path>` with optional `--file
<legacy-policy-input>` to enumerate source scopes, digests, and collisions.
Apply an explicitly selected checked request through `--request-stdin`.
Destination names and access choices must be explicit. Migration publishes new
files without overwriting sources or accepted records. If publication fails
partway through, inspect the new files and replan before retrying.

Common instruction directories and files require owner-only modes 0700 and
0600. A validation failure leaves installed state unchanged. Never edit an
installed snapshot, runtime registry, database, or credential carrier directly.
Validate a v3 policy input through `specialist policy validate --workspace
<path> --file <input>` before an authorized `add`. Validation observes qualified
Profile definitions without starting servers. `show` and `list` return safe
metadata and Role digests for v3 entries, and original values for accepted
legacy entries. Removal affects future selection only; accepted sessions keep
their original policy, Role captures, child configurations, and Profile bindings.

For a task-aware one-shot review, validate the v4 request against the checked
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
`DOLGORAE_CODEX_MINIMUM_BIN` pointing to Codex 0.158.0, and
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

## Native Submit lookup

When the selected binary exposes `run lookup-operation`, query the original Run
and key with its current protected Controller carrier. The Machine v5 response
contains `data.lookup`: `accepted` retains the immutable receipt and separates
current outcome; `not_accepted` carries a durable refusal and exact-key retry
permission; `unresolved` requires reconciliation; `absent` carries a complete
retained-namespace proof at its durable watermark. Do not infer absence from an
error, missing history or a foreign Controller. A current lookup rejection does
not resolve an earlier unknown Submit. Preserve the original key and inputs.

The lookup reads retained ledger facts and does not repair state, start a worker
or reopen attachment paths. Legacy operations without successor namespace
identity keep their original interfaces and receipts; lookup refuses to invent
that identity. The [durable operation specification](../specs/dolgorae-v014-durable-operations.md)
owns this diagnostic. Public-v2, H3 and actual consumer qualification remain
separate roadmap obligations.

## Retained Close and executable changes

After owner or gateway loss, repeat the same Close or use authorized
Recover/Reconcile with a current Run revision. Keep the retained operation and
interrupt choice. An ownerless `settling` record is exposed as requiring
recovery; an active owner remains responsible for settlement. An unknown effect
requires authoritative history and cannot be dismissed by a later blocked
request.

When the exact selected build supports quiescent build handoff, `run recover`
can retire a verified idle/paused old worker and publish a new generation.
Ordinary cross-build mutation remains refused. Resolve the stable
`RECOVERY_REQUIRED` blocker before retrying: settle work and writer authority,
restore exact process proof, or reconcile the original admission/effect using
the verified original build. A replaced executable pathname can invalidate
that proof; do not signal its recorded PID merely because it still exists.
The [owning recovery specification](../specs/dolgorae-v014-close-recovery.md)
defines the reasons and required actions. This source implementation leaves
public-v2, H3 and actual consumer qualification with their later owners.

## Planned v0.1.4 MCP host installation and recovery

TASK-067 must name the installation/configuration owner for each supported
Codex and Claude host route before TASK-068 contract freeze. If a trusted host
adapter or receipt-and-lookup protocol is required, its operations contract must
specify exact host/adapter versions, protected identity-state and credential
ownership, permissions, retention, restart/reconnect recovery and corruption/
missing-state behavior. Keep credentials out of model-visible MCP traffic.

An uncertain invocation retains its trusted identity and receipt linkage;
operators must not recover it by creating a new key. Installation or a typed
unsupported response does not establish required host support. Until the
pre-contract qualification passes, successor setup and Gate A remain unavailable.
Existing Machine CLI operation and historical MCP disposition remain unchanged.
This section supplies planning requirements, not an install command or runtime
authorization. The verification owner defines separately authorized host probes
and final-pair native/real-account qualification.

## Planned v0.1.4 artifact and evidence handoffs

The [alignment dossier](../todo/TODO-DOLGORAE-V014-RELEASE-ALIGNMENT.md#execution-order-and-dependencies)
defines five whole-Epic phases and four handoffs. H1 publishes the qualified
contract and three-consumer matrix; H2 returns a completed Gul contract Epic;
H3 supplies the intended runnable v0.1.4 artifact after TASK-087; H4 returns
Gul's intended v0.1.0 artifact and completed native, real-account, device/soak
and product qualification. Gul closeout does not wait for TASK-088/089.
EPIC-022 performs independent final verification on the completed artifacts,
including mandatory Aquarium/Podway compatibility. Operator installation,
account use, production cutover and stable publication retain separate authority.

Prepare provider version identity and relevant packaging before H3 and Gul's
intended artifact during Phase 4. Bind source revisions, binary/package
checksums, contract digests, pinned runtime and relevant configuration to
reproducible scenario results. Apply the approved bounded non-sensitive evidence
promotion policy when cross-repository retention is required; raw logs, provider
prose, credentials and runtime identities remain local. TASK-089 verifies the
qualified artifacts without introducing an unqualified rebuild or packaging
change. Substantive changes require all-consumer evidence-impact assessment
and affected requalification. Only existing explicitly permitted QA-neutral
metadata exceptions apply. Missing mandatory evidence or corrective work blocks
release and incomplete Epic closeout.


## Ledger capacity and checkpoint recovery

Native `STORAGE_CAPACITY_EXCEEDED` refuses ordinary admission before dispatch.
Frozen Run machine/public-v1 outputs use `CAPABILITY_UNSUPPORTED` with the
successor storage reporting boundary; inspect the original operation and use
explicitly approved native maintenance before an exact-key retry. Accepted
Turn finalization, pending interaction cleanup, Close and writer release retain
reserved capacity; integrity failures still require authoritative recovery.
Use the selected binary's `maintenance plan/apply/get` commands only with
explicit operator authority. Preserve the complete dry-run plan and its original
workspace generation and ID before Apply. Stop existing writers through their
lifecycle owners, then apply the exact plan with explicit approval. After reply
loss or interruption, Get the original ID first. A retained owner can resume the
same plan; changed history, a different selector or corrupt content requires
recovery rather than a new plan ID. Old generation files are retained.

The [ledger maintenance runtime](../specs/dolgorae-v014-ledger-maintenance.md)
owns crash ordering and bounds. This native support does not qualify public-v2,
Gul E16, or any actual consumer scenario.

For Profile journals use `maintenance plan --kind membership_compaction` with
the same protected operator and workspace arguments. Its independent workspace
maintenance generation starts at 1. Preserve the returned complete plan and
original generation; Apply and Get use the ordinary maintenance routes. Resume
the retained operation after interruption before admitting new binding or
membership changes. Active or unknown references cannot be bypassed; the
operator resolves their existing lifecycle first. Old journal generations and
original binding bytes remain, without automatic deletion.

A successful Close may report `profile_membership_cleanup_required`. Its
terminal receipt and history remain valid. Restore the reported membership
namespace, then repeat the authenticated Close to settle only cleanup. Status
and public recovery projections retain this obligation until release is proven.
Caller Machine-v4 reports registered recovery with an explicit completed-Close
reason because its frozen access projection cannot carry the cleanup fields.
See the [Profile maintenance contract](../specs/dolgorae-v014-profile-maintenance.md).

For delivered result bodies use `maintenance plan --kind retire_result_content`;
for unreferenced result storage use `--kind garbage_collection`. Use the same
protected operator, original workspace generation, complete plan, approved Apply
and original-ID Get rules. These are separate explicit effects; checkpoint or
Profile maintenance authority does not include either. Active, unknown, prepared,
undelivered and live-reader references block unsafe effects. Retirement preserves
identity and receipts and leaves a durable tombstone; garbage collection removes
only unreferenced shared bytes. Neither operation changes the selected ledger
generation or enables automatic deletion. Frozen callers report the successor
result-retention reporting boundary for a retired body. See the
[observation runtime](../specs/dolgorae-v014-observation-runtime.md).

An incomplete checkpoint or content owner keeps ordinary workspace admission
closed after interruption. Resume its original operation. A frozen database
reader may report `dolgorae.workspace-maintenance/v014`; native maintenance Get
and explicitly approved Apply retain the original recovery path.

Warm display observations reuse checked sources. A changed executable, Profile
configuration, workspace identity, database or ledger invalidates relevant reuse.
Use authoritative lifecycle and operation lookup for mutation or recovery proof;
a display result never proves that uncertain work is absent. Response deadlines
bound waiting and preserve accepted effects and their original identity.
