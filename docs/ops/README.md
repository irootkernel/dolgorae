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
at most 17 Turns and 34 loopback Responses requests, plus the existing successor
case with five Turns and 23 requests (19 base plus four required continuation
exchanges). It never loads a real
account or sends requests to an external provider. Its isolated Profile disables
optional Codex plugins and verifies the captured setting so background plugin
downloads cannot determine startup readiness. Plugin-enabled startup is outside
this campaign's qualification. Startup with the default shell snapshot is also
outside this native campaign's qualification; the roadmap assigns
default-feature Profile qualification to TASK-077. The campaign checks startup defaults,
read-only enforcement, same-thread access changes, captured instruction replay,
all direct review scopes, reusable Reviewer tasks, and accepted Primary/child
policy configurations, including the Primary's actual `request_specialist`
tool call. The modern Primary's checked Machine close must retire its child,
release the Writer and commit aggregate completion. Fresh CLI observation,
repeat close and recovery must retain its close operation and interrupt choice.
An idle writable Primary must also complete checked interruption and recovery
with its retained abort operation. This does not qualify a live active Turn abort.
A nested turnless campaign checks writer exclusion and recovery.
The successor fixture also disables the optional native shell snapshot; the
existing campaign retains its original configuration. This bounds startup
background work without relaxing process-quiescence or policy proof checks.
The successor campaign builds an opt-in example driver from the production
modules with test-only caller provenance. It uses the production settlement
port, qualified native runtime and real Broker carrier for managed dispatch;
external members use their authenticated owner carrier. Both manifest-4 members
publish a native write result and close normally. Assertions bind index
publication to the first loopback request and check verified Reader journal,
seal, writer release, owned cleanup and Profile retirement order. Five managed
preparation and recording failures check zero Submit calls, content rollback,
unchanged receipts, returned rounds and the preserved uncertain guard. Test
hooks inject owned filesystem or SQL faults after qualified acquisition.
Acquisition, dispatch and close use production code, including native
qualification.
The successor case uses three actual Primary Turns for spawn, assignment/await
and consumer inspection/Record/Accept around the two original Writer Turns.
Their real Root/Turn/call provenance comes through registered managed tools.
The pinned Codex advertises those nested tools through custom `functions.exec`.
The fixture awaits them in raw JavaScript and consumes only the correlated
custom return and its deliberately emitted results. Outer cell IDs and test
labels remain separate from native inner tool identity. Executor commands use
the same carrier and require a structured terminal exit and exact output.
Only the successor explicitly selects its declared `gpt-5.6-luna`/`low` model;
the main program keeps omitted model/effort inputs to exercise native defaults.
The five local dispatch-failure controls retain their acquisition/index hooks;
the two Writer requests retain the live Writer/index check before each model
request. The formal member captures the selected formal contract as a dependency;
the ordinary member captures the ordinary Executor guidance without that formal
dependency. Both retain their close and retirement checks.

The remaining consumer composition uses the selected read-only Aquarium source
archive and selected Podway executable as explicit inputs. Ordinary v3
assignments are prepared and rendered by Aquarium's shipped helpers; ordinary
delivery keeps its actual evidence and gaps without requiring a formal receipt.
Declared formal v1 work retains its selected instructions, complete package and
accepted native task bindings. Primary delivery acceptance, Podway's structural
receipt, Action completion and an independent semantic assessment are separate
obligations. A custom fixture Procedure alone does not qualify Aquarium.
Supply the selected executable and archive through `--podway` and
`--aquarium-source`, or `DOLGORAE_TEST_PODWAY_BIN` and
`DOLGORAE_TEST_AQUARIUM_SOURCE` when invoking the existing Make target. The
fixture checks their pinned identities and keeps extraction in its private root.
This source-defined trace still requires frozen-candidate native qualification;
its expected counts are not a passing runtime result.

Cleanup uses the fixture's Operator to interrupt only its confirmed Profile,
then requires process-inventory absence before removing the fixture. It does
not project modern Runs through the frozen public-v1 adapter.
Failures retain their owned fixture for diagnosis; they do not retry
automatically. This campaign is outside the default gate.
Successor failure cleanup releases managed/external members through their own
settlement owner, including closed Runs whose membership still needs release,
before closing the Root and retiring its Profile. It retains typed cleanup
outcomes and requires process absence; legacy Close cannot substitute for a
successor release.

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

### Current host and runtime executable selection

The private setup commands are administrative operations, not model tools:

```text
dolgorae __trusted-specialist-host owner-config-create --workspace PATH --owner-config PRIVATE_ROOT/owner.json --request PRIVATE_REQUEST --operator-fd FD --controller-fd FD
dolgorae __trusted-specialist-host namespace-setup --owner-config PRIVATE_ROOT/owner.json --operator-fd FD --controller-fd FD
dolgorae __trusted-specialist-host host-state-init --owner-config PRIVATE_ROOT/owner.json --operator-fd FD --controller-fd FD
```

Use an existing current-user-owned 0700 physical private root outside the
workspace. Keep the configured Controller file and isolated native-host root
inside it, with private permissions and no aliases. The protected request holds
route/executable selection, owner/scope, caller references and adapter provenance;
it contains no credential bytes. Every step requires the current Operator and
the matching Controller. Repeating an interrupted step validates the original
configuration/birth; inconsistent partial state requires recovery. Success is an
exit status. Failures print only a bounded safe code, without input, paths or
binding material. These commands do not install or launch a native host. Ordinary
hook/delivery/lookup never complete missing setup. Host installation, configuration
changes and actual host execution still require their own authorization.

Use Codex CLI `0.164.0`
(`d8990e3ee7acfbc26f91265b12a1a83f0fee40d0e0f8f1f40a37d4ec21f499cf`) and Claude
Code `2.1.310`
(`ffb64331bc0ee1b69efb7c2c428bd3ab516bc31b75bf60886b356c97c1c25ffd`) for
TASK-077 qualification. Verify the executable hash before use and select an
explicit absolute path. The [runtime qualification
owner](../specs/dolgorae-v014-delegation-runtime.md#current-host-and-profile-runtime-qualification)
separates these current executable pins from the historical H1 route
identifiers. Original accepted bindings retain their executable; missing or
changed bytes require recovery without fallback. Generic Profile doctor probes,
start/restart/stop and migration retain tested/unverified compatibility admission;
exact current pins gate only new
qualified-mechanism admissions. A failed migration restores the
original snapshot under its durable migration owner. Installation, native
execution and account use retain their separate authorization boundaries.

### Explicit administrative member recovery

If recovery reports a history or transport failure, inspect the authoritative
member/task getters before another administrative action. A lost reply does not
prove that settlement failed. Do not retry blindly or create a replacement task;
retain the original accepted key and Thread/Turn. The Worker can hand its quiet
background reader to the history command without a native wake-up RPC. A partial
message that cannot finish within the operation budget closes the connection;
accepted active work follows the existing outcome-unknown recovery path.


Use the original protected owner configuration and exact accepted scope, member
and task. Pass current Operator and owner Controller credentials by inherited
private descriptors; keep those descriptors and paths out of model traffic.

```text
dolgorae __trusted-specialist-host member-recover --owner-config PRIVATE_ROOT/owner.json --scope-ref SCOPE --member-ref MEMBER --task-ref TASK --operator-fd FD --controller-fd FD
```

This explicit administrative call is effectful and remains separate from member-release
convergence and close continuation. Empty stdout and exit0 mean the call completed. Errors print only the existing bounded code on stderr and
exit4. Use the original collect/identity getters afterwards; success alone is no
result acceptance or qualified export. If the recovery reply is lost, observe
those getters before deciding on another explicit action. No automatic retry,
new assignment key or replacement task is implied.

Managed recovery requires original retained assignment provenance and the current
Root owner; it never accepts model-built context or exposes a member credential.
Missing recorded Turn remains DF-023. Ordinary terminal evidence without the
existing reconciliation/fresh-history proof remains DF-024. Storage/integrity
faults require restoring the original state; lost write streams retain their
existing unavailable marker. This command neither installs nor qualifies a host
and does not authorize unrelated native, account or consumer operations.

### Private source preparation

After explicit owner, namespace and host-state setup, use the same Operator
and Controller FDs for the private preparation commands:

```text
dolgorae __trusted-specialist-host preparation-init --owner-config PRIVATE_ROOT/owner.json --operator-fd FD --controller-fd FD
dolgorae __trusted-specialist-host provisioning-prepare --owner-config PRIVATE_ROOT/owner.json --request PRIVATE_REQUEST --output PRIVATE_OUTPUT/identity.json --operator-fd FD --controller-fd FD
dolgorae __trusted-specialist-host assignment-prepare --owner-config PRIVATE_ROOT/owner.json --request PRIVATE_REQUEST --output PRIVATE_OUTPUT/identity.json --operator-fd FD --controller-fd FD
dolgorae __trusted-specialist-host preparation-project --owner-config PRIVATE_ROOT/owner.json --request PRIVATE_LOOKUP --output PRIVATE_OUTPUT/identity.json --operator-fd FD --controller-fd FD
```

Requests are current-user-owned 0600 files in physical 0700 directories.
The output directory must be separate from the protected owner root. Publication
is exclusive; a retained identical output may be read again, while changed or
partial output is preserved and refused. Output contains only the existing
provisioning sources identity or accepted task identity, never private selection,
paths, registration identifiers or credentials. A lost output does not imply
product acceptance; projection reads the original immutable preparation.

Provisioning input contains `role_name`, explicit `role`, `dependencies`,
`selected_guidance`, the complete `profile_binding` and `admission` policy.
Each source is tagged `kind: inline` with string `value`, or `kind: file` with
an array of instruction-home components in `value`. Named sources contain
`name` and `source`. No Role-name or digest-based discovery occurs.
Assignment input contains `namespace`, canonical UUIDv7 `member_ref`, complete
`task`, `execution_intent`, `deadline_seconds` and explicit nullable `target`
with `kind` and optional `revision`. Its scope must be owned by the configured
Controller; managed scope must be the configured Root/session. Projection
lookup uses `kind: provisioning` plus `sources`, or `kind: assignment` plus
`namespace`. Pass only the projected identity in existing facade fields.

Keep original selectors and files for fresh calls. Changes after preparation
are refused before product intent; accepted replay uses retained product bytes
and the original receipt even after source removal or ref movement. Missing,
foreign or corrupt preparation state requires explicit recovery, never normal
path repair. These commands do not install host configuration, launch a host,
admit a task or qualify consumer integration.

### Native delivery and managed connection

After explicit setup and source preparation, the host launches one newline stdio process and invokes the synchronous hook before each tool call:

```text
dolgorae __trusted-specialist-host host-hook --owner-config PRIVATE_ROOT/owner.json
dolgorae __trusted-specialist-host host-mcp --owner-config PRIVATE_ROOT/owner.json
dolgorae __trusted-specialist-host host-lookup --owner-config PRIVATE_ROOT/owner.json --session ORIGINAL_NATIVE_SESSION --call ORIGINAL_NATIVE_CALL
dolgorae __trusted-specialist-host managed-connect --owner-config PRIVATE_ROOT/owner.json --operator-fd FD --controller-fd FD
```

Keep the owner path in private host configuration. The hook receives the original `PreToolUse` JSON on stdin and permits dispatch only after durable binding. Codex supplies the original `threadId` and `callId` in the tool-call metadata. Claude must use the dynamic `dolgorae` server; its hook output carries the opaque binding only in private updated input. The MCP process removes that carrier before checking public arguments. Never place a Controller file, owner path or binding token in model arguments. Installation and host configuration require separate authority.

For initialized notifications and ping, omit params or send an object; optional `_meta` must be an object. Request metadata may contain a string or integer `progressToken`; notification metadata is an untyped dictionary. Additional protocol metadata is ignored and grants no authority. Explicit null params are invalid. Tool listing accepts metadata and returns one complete page without a cursor.

MCP request IDs must be strings or integers. Null request IDs are refused before initialization or dispatch; notifications omit the ID. An error response may still use null when its request ID is unknown.

The MCP session returns its supported `2025-06-18` version to a valid version proposal, then receives `notifications/initialized` before `tools/list` or `tools/call`. Only the configured external tool names are advertised. Public requests retain the 16 MiB raw and canonical allowance and frozen field limits. Invalid framing, notifications and missing native proof cannot dispatch product operations. EOF ends transport without cancelling tasks, releasing members or stopping Profiles.

For managed calls, select a managed owner for the existing Root/session and run `managed-connect` under its current Root Controller and Operator. New Root Threads advertise `dolgorae_specialists_native` and
`dolgorae_specialist_consumer` alongside the orchestration tool. Accepted
`dolgorae_specialists` registrations retain their full public response path. Their native Worker supplies Root, Thread, Turn, tool call and generation; model scope strings must match that Root. Existing Thread tool sets are retained on reconnect; legacy Threads are not replaced. The original orchestration tool retains its behavior.

After response loss, retain the native session/call and public operation key. `host-lookup` observes the original K namespace, performs a fresh R observation or reads the original S owner. A repeated ambiguous K or S call cannot start a replacement effect. Missing or inconsistent protected state requires explicit recovery. Active delegation-identity observation still requires the consistent disposition boundary; a pending-interaction placeholder cannot prove idle. Durable released identities use the existing terminal proof. Full native and consumer qualification remains required before product support is accepted.

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
membership changes. Compaction preserves active and unknown references and
does not require closing Runs; it serializes membership changes under the
home-root and Profile Server locks. Old journal generations and
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

The selected Codex receiver limits a reassembled WebSocket message to 64 MiB. Some full collect responses allowed by the producer exceed that limit after native JSON-string wrapping. Bounded outgoing frames preserve those responses but do not resolve this receiver compatibility gap. Treat that case as unsupported pending a reviewed resolution; do not shrink the result or infer native acceptance from sender completion.

## Read-only delegation disposition

Use the existing host or managed tool's `collect_external_specialist_task` or
`collect_specialist_task` request with the original accepted member and task
references. The corresponding `get_*_delegation_identity` request observes the
same complete result and obtains the versioned identity proof. The protected
owner and trusted caller scope supply authority. Copied references confer no
authority. Each read is fresh, and its proof describes one shared instant.

Pending interaction or owned work prevents an idle export. A
`DISPOSITION_NOT_IDLE` response requires observing or awaiting the original work;
collection does not answer an interaction or settle a task. Three source motions,
unobservable history or cumulative budget exhaustion return `RECOVERY_REQUIRED`
with `RecoveryNeeded` and `restore_state`. Preserve original state and use the
owning diagnostic/recovery boundary under its existing authority. Do not delete
state, create a new key, redispatch work or assume that a failed observation
means the original task had no effect.

The observation shares 512 MiB, 1,000,000 decoded records and 300,000 ms across
at most three attempts, with the existing per-line limit. It never repairs or
releases state. Full result bytes and their digest remain part of the checked
identity; native receiver compatibility and actual consumer acceptance require
their separate qualification.

### Private consumer setup

After explicit namespace and host-state setup, use the existing
`__trusted-specialist-host consumer-init` administration entrypoint with
`--owner-config`, `--request`, `--operator-fd` and `--controller-fd`. Both
inherited authority carriers are required. The request is a mode-0600 JSON file
in a physical mode-0700 directory outside the workspace. Native-only setup is
`{"kind":"native_only"}` and opens no Podway workspace. Delegated setup uses
`{"kind":"delegated","connection":{...}}`; the connection supplies the exact
executable path and `sha256:` digest, contract manifest digest, physical
worktree, workspace/session/attempt UUIDs, placement, qualified adapter ID and
version, and permitted item IDs. Existing protected caller scope must agree.
The connection is immutable. Ordinary calls cannot create or repair missing
consumer state.

A delegated task must retain its original complete work package as an accepted
context. The composer verifies it against the current selected package before
recording or accepting. Inspect original evidence, prepare with a caller-known
request ID, inspect the frozen input, then supply the explicit `record` or
`accept` decision. Keep that request ID for response-loss recovery. Status uses
the original Podway key and receipt before current observation. An uncertain
request never starts another mutation automatically. Historical success remains
visible when current applicability becomes stale or a later read fails.

Explicit external native registration selects `specialist_native` and
`specialist_consumer` on the existing host-mcp/host-hook transport. Existing
`specialist` registrations keep their full public response path. Native
presentation retains every small authoritative candidate/member/receipt fact,
including the configuration digest needed for hire or spawn. A collected result
also supplies a task-bound inspection selection. Original pages use a compact
ASCII header and a separate original-text block, initially at most 1024 UTF8
bytes. Inspection does not require sequential acknowledgement. The private
presentation preserves semantic classification and effects through later
presentation failure.

Native registration is implemented; actual selected-host model-history and
consumer qualification remain separate acceptance obligations. The opt-in
`tests/e2e/run_native_production_host.py` owns the original eighteen scenarios
per selected host. Its trace-only mode executes no host. Execution binds native
images, product/driver bytes, model configuration and each row's trace, forwards
actual MCP negotiation, and compares current correlated next-HTTP tool results
with independent fixture originals. Its existing opted-in executable uses the
production host handlers and the same executable image for preparation, Worker
control and native delivery. The ordinary CLI image is recorded separately;
mixed-image lifecycle calls cannot qualify this path. After the writable producer
settles, its verified owner returns the writer through the existing authorized
release operation before fresh idle export. Limits remain 90 seconds, eight combined
host/executor/count-token POSTs, 32 deliveries and 4 MiB per input, plus a
separate 30-second owned cleanup. Raw observations remain private runtime data.

The integrated acceptance entrypoint also accepts
`--default-feature-startup-only`. This separate mode checks omission,
configuration enable, raw argv enable and repeated override order against the
accepted launch hash/key, actual process argv, shared and Dedicated native
feature readback and unchanged quiescent startup. It does not replace the
existing pre-disabled regression. Each variant starts and normally closes one
Run; the full caller-pipeline mode retains the separate writer/replay/recovery
regression. Qualified fixture cleanup selects the retained
accepted binding and uses the existing operator-authorized non-interrupting
stop; generic name-based Profile commands retain their generic contract.

## Successor read-only pool and capacity

Public-v2 clients negotiate protocol 2, the checked descriptor digest, credential
contract, bounds and the method subset returned by `RuntimeService.GetCapabilities`.
Start carries explicit captured instructions and provisioning identities; it uses
the same accepted startup reservation and lifecycle services as Machine startup.
Keep its original scope, Controller generation and key after response loss.
`OperationService.LookupOperation` reads the retained Start or Submit owner without
resubmitting. A Start with no retained completion receipt stays unresolved; a
missing reservation alone is not authoritative absence. Submit absence uses its
existing complete ledger/namespace proof. Renegotiate after a gateway restart. Modern Session Close/Recover/Reconcile retain the aggregate
close owner and its operation identity. Public-v1 Run methods keep their legacy
contract; mixed listing omits modern entries, while workspace Writer status can
observe a modern holder. The frozen public-v2 Run shape carries accepted
instruction/configuration identities, but has no Role-name field. Its Start input
carries the Role name; Machine-v4 continues to expose `selected_role`.

An authorized operator may install a current-user-owned mode-0600
`~/.dolgorae/pool-configuration.json` before new admissions:

```json
{
  "schema": "dolgorae.pool-configuration/v1",
  "readonly_server_limit": 1,
  "dedicated_generation_cap": 8,
  "sixth_generation_warning": 6,
  "native_subagent_default": "disabled"
}
```

K is an upper bound in 1..16. Compatible fixed read-only members currently share
one server; increasing K does not promise additional servers. Dedicated capacity
is 1..32. Accepted pending startup reservations and uncertain generations consume
capacity, including a crash before manifest publication. A sixth dedicated
admission records `sixth_dedicated_generation` in Profile diagnostics. Public-v2
`ListProfileDiagnostics` reads minimal safe records across retained bindings and
requires a refreshed page when its captured source heads change. New
admission at the cap returns `POOL_CAPACITY_EXCEEDED` before allocation or provider
effects. Lowering the cap preserves existing work, same-key replay, recovery and
finalization.

Compatibility includes the exact Profile, CODEX_HOME/account namespace,
executable, process-static configuration and immutable native-subagent policy.
Successor launch enforces project/ancestor instruction suppression and disables
native subagents by default. Explicit enabled configurations require a qualified
dedicated lane. A lease cannot elevate access. Closing one pooled member stops
its Worker participation; Profile lifecycle owns shared-generation shutdown and
complete membership/descendant proof. Do not stop a shared generation using one
Thread's quiescence. Accepted earlier Profile launches retain their original
configuration and recovery meaning; changing configuration requires the existing
explicit Profile lifecycle and migration controls.

The authorized, account-free native pool check reuses the caller pipeline:

```sh
DOLGORAE_RUN_NATIVE_CALLER_PIPELINE=1 .venv/bin/python tests/e2e/run_native_caller_pipeline.py \
  --readonly-pool --pool-limit 1 --driver /absolute/path/native_caller_pipeline --codex /absolute/path/codex
```

Run the same check with `--pool-limit 4` for the second required bound.
With the exact qualified Codex pin, the check isolates all homes and Profiles
and uses scripted loopback Responses. It measures sixteen members against
configured K, separate Profile/account/configuration bindings, fixed read-only
policy and write denial, instruction suppression, response-loss lookup without
resubmission, replay, member close,
dedicated transitions, the sixth-generation diagnostic and lowered-cap refusal.
No requests reach an external provider or real account. Group/50-task and final Gul
load qualification remain separate later work. Failures retain the original
fixture and cleanup result; successful cleanup retires owned Profiles before
removing the fixture.

### Current permission and MCP interactions

Qualified Caller-v3 managed-source and current registered External source routes
admit busy-target assignments through the central dispatcher under their
captured bounds and accepted source/caller selection. Queued work keeps its
accepted bytes, original receipts and conservative round accounting. Dispatch
uses the original owner authority. Caller-v3 Sessions permit
`reuse_any_compatible` under their accepted policy. Compatible reuse uses
accepted Role, configuration and access snapshots; author edits do not change an
accepted choice. Historical Legacy tools retain their original admission and
replay behavior. External queues grant no lateral collaboration, and queue
storage alone grants no consultation or activation authority. An operator
must use the current owning recovery path for unknown work; queue records do not
prove that a Turn was absent or authorize redispatch. Keep pending task state
when a dispatcher or owner capability is unavailable.

`DOLGORAE_CONTROL_PLANE_RECONCILIATION_REQUIRED` reports a failed control-plane
reconciliation pass. Preserve pending work and use the current owning recovery
path. The marker does not prove that a Turn was absent or permit redispatch.

Public-v2 clients list safe summaries, fetch the full typed request with the
current Run Controller, and resolve it using that Controller and the captured
request, Turn and revision. Permission grants are exact requested subsets,
limited to contained filesystem reads or currently authorized Writer paths for
one Turn. Supported MCP forms contain bounded string, enum or boolean fields.
Network expansion, special/glob filesystem grants, URL elicitation and other
unrepresentable forms receive an explicit policy decline.

A disconnected direct Controller still owns its requests. A private Specialist
with no declared answering route receives a durable typed decline during
ingestion, without waiting for a client. Fixed read-only members also decline.
The worker keeps draining until actual terminal evidence arrives; an interrupt
acknowledgement or decline alone does not prove completion. An uncertain write
retains the original recovery obligation.

Permission/MCP responses and secret user input use opaque resolution receipts.
After response loss, retry the winning key under current Controller authority
to obtain that receipt. The provider does not compare replacement protected
content or reopen its file. A different key cannot replace the winning response.
Public-v2 `OperationService.LookupOperation` recovers the safe accepted
resolution receipt by the original Run, Controller principal/generation and key.
Historical decisions without a public operation identity remain unresolved.
Observer summaries and push events never carry the request or answer body.
Machine interaction/detail output and event streams select v6 when they carry
new v2 content; retained v1 history remains readable without rewriting it.
