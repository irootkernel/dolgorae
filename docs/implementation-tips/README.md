# Dolgorae Implementation Tips

This document owns non-normative guidance for changing, testing, and preparing
the Dolgorae implementation. Read the [documentation authority map](../README.md)
before changing a product contract, and update the owning specification or
architecture document before derived protocol, implementation, test, or roadmap
changes.

## Development setup

Install Rust 1.97.1, Buf 1.69.0, Go 1.26.6, and Python 3. Then create an
isolated Python environment for the small repository checks:

```sh
python3 -m venv .venv
.venv/bin/python -m pip install -r tools/validation/requirements.txt
```

The immutable H1 contract is selected from its qualified committed revision,
independently of the current package version. `test-prepare` runs
`tools/validators/validate_frozen_h1.py`, which verifies that archive and executes
all three selected H1 scripts unchanged in its extracted tree. The historical
H1 packaging guide's default-HEAD examples describe the selected H1 source;
they do not select H1 from current v0.1.4 inputs. Keep current Cargo, development
checks and current generated-source validation separate from that frozen tree.
An archive encoding/compressor mismatch fails independently of selected
lock/member-byte equality; neither check may be weakened to accommodate it.

Generated public-v2 artifacts require protoc 36.2, Buf 1.69.0,
protoc-gen-go 1.36.11 and protoc-gen-go-grpc 1.6.1. The selected protoc executable
must have its matching `include/google/protobuf/timestamp.proto` in the sibling
installation tree. `tools/validators/generate_public_v2.py` checks versions,
import bytes and generated provenance; do not replace its selected producer or
metadata to accommodate another tool arrangement.

Offline E2E fixtures use `codex` from `PATH`, or the explicit
`DOLGORAE_TEST_CODEX_BIN` override, at or above the 0.158.0 minimum. Their native
stand-in reports that executable's version and delegates schema generation to
that same executable. The Profile checks both pinned bundle digests for every
registered tested release. All releases retain required-subset checks; unlisted
newer releases report `unverified`. No older installed sibling is selected automatically. Protocol
responses still come from the independent fake, so these cases do not qualify
the selected release's live behavior.

Codex `0.166.0` is separately registered for generic Profile compatibility after
actual compatibility and access-safety campaigns and the Profile launch probe.
The checks observed history, forks, interruption, unanswered-approval recovery,
workspace and temporary-directory writes, command/file approval requests,
handshake, account/model discovery and absent-thread errors. Registration pins
both generated schema bundles. Caller-workspace and checked Reviewer admission
remain tied to their separately qualified executables and configurations.

The [CI workflow](../../.github/workflows/ci.yml) runs the same complete gate on
the standard macOS arm64 runner for pushes and pull requests. It provisions
the pinned tools, verifies downloaded asset digests and explicitly selects the
minimum Codex release as its offline schema generator to retain baseline
coverage. It uses no model account, native/live opt-in, installation target or
release operation. Local source checks do not establish that the hosted job
has executed successfully.

`examples/native_ordinary_host.rs` is a non-shipped public-v2 client and
independent observation helper. It captures inputs without registering a
binding or starting a Profile server. Its bootstrap uses the ordinary server's
provisioned StartRun and CloseRun RPCs; a normal bootstrap creates a Run and
Thread without a model Turn. Public-v2 credential carriers require three
relative components beneath `controller-carriers/`, with private owner-only
directories. The helper stores complete protobuf responses; its JSON metadata
is a partial view. Cleanup closes original Runs before non-interrupting
Profile migration and stop through the ordinary CLI. A successful bootstrap
does not qualify group execution or final H3.

A typed unresolved bootstrap Close response is retained separately from its
original pre-close observation. Keep the owned gateway alive while bounded
public observations check the original Run, Thread and Controller. Observed
completion requires Closed, no active work, Writer release, complete owned
scope absence and original Profile membership retirement. If these obligations
remain pending, prove that the exact owned gateway executor exited before
replacing it. Then issue at most one fresh continuation after observing the
original ordinary Run Closed. The product must not replay its native Close.
Original, observed-final and continuation protobuf artifacts have distinct names; a
second unresolved result preserves the fixture instead of retrying Close.

`tests/e2e/run_native_ordinary_host.py` exercises individual completion, feedback
rounds, release, fences and research groups with a separately selected
client/helper and fault library. Its product
server, native MCP, Workers and Profile drainer use the selected ordinary
binary. Select `--host codex` or `--host claude`; `--scenario consumer-group`
checks the conflict/merge/synthesis path without loading the fault library,
while `consumer-group-gaps` selects physical response loss. Both paths initialize
the delegated consumer with the actual Podway connection through the ordinary
`consumer-init` command before native collection. Host-state and preparation
initialization alone do not initialize that consumer store.
Each external case also installs a same-name Role change and a separate
configuration change before hiring from its original candidate. Those candidates
create no Task; accepted input and result checks retain the original digests.
A separately pinned `--peer-reader` may run only the existing read-only
Podway compatibility oracle; it does not initialize or operate Dolgorae on the
ordinary binary's behalf. Keep that reader's actual image and unchanged-source
applicability distinct from product execution.

The native production fixture's `--ordinary-read-merge`,
`--ordinary-synthesis-input`, `--ordinary-read-result` and
`--ordinary-managed-read-result` modes supply independent
read-only references or captured input bytes. They do not initialize, collect,
reconcile, dispatch or retire product state. Select that reader separately with
`--result-reader` and its digest. The ordinary image reads the referenced merged
artifact, accepts the separate synthesis Task, returns its complete native
result and performs the subsequent consumer Record/Accept flow. Neither reader
supplies a native delivery or semantic acceptance.

The client's `observe-images` operation records the live original member
Workers, Profile drainer and Codex server before cleanup. It checks executable
paths, bytes, physical identities and retained process fingerprints against the
selected product and runtime inputs. Keep these observations separate from
the native result pages and receipt decisions.

Ordinary managed-producer qualification uses the existing interactive Root
`run start`, `run send` and whole-session `run close` commands, with its
orchestration policy carried by the original Controller credential. Provisioning,
spawn, assignment and collection run through the actual Root Turn's managed
native tools. Proto declarations for unsupported provisioning/Broker RPCs do
not provide an alternate execution path. A private Specialist bootstrap remains
separate from the Root's qualified Profile launch. Test-side preparation and
owner-authorized reads supply original input and result comparisons only.
`tests/e2e/run_native_ordinary_managed.py` selects this route with the ordinary
`--binary`, `--helper`, pinned `--result-reader` and existing runtime/Podway
inputs. It reads every original consumer page and completes the Action separately
from receipt acceptance. Whole-session Close and Profile retirement must both
finish before the case passes.

These ordinary-image runners impose no whole-case or cleanup execution timer.
Task and Send limits remain explicit native inputs; HTTP, page and byte bounds
remain enforced. The group-gap fixture arms a separate 180-second fail-safe
only for its injected hold, immediately before group submission. A fail-safe
release fails the fault observation; it neither qualifies an unknown outcome
nor ends the remaining owner cleanup. Cleanup waits for an in-flight model
callback before changing its phase. If logical cleanup fails, retain the owned
scheduler and report its recovery handoff instead of terminating it.
On Darwin, a native host group signal can return `EPERM` after that group exits.
The harness accepts cleanup only after an independent process-group inventory
proves absence; a retained or unreadable group remains a failure.

The ordinary complete-result case observes its owned MCP inventory before
dispatch. Profile preparation may make up to 24 metadata requests across its
native probes. Actual Task execution retains a separate eight-request bound;
both counts remain visible. Two automatic policy declines and absence of the
protected question from model traffic are required.

For caller/default-startup, pool and interaction qualification, pass the same
`--binary` to `tests/e2e/run_native_caller_pipeline.py`. The selected image owns
workspace/Profile setup, caller mutations, Workers, drainers and the pool RPC
server. The example checks durable receipts and independent read-only observations.
Its replay flag is a before/after observation of the original reservation and
receipt, because the caller CLI does not return that flag. All four startup
variants preserve their original configuration and argv order, including omitted
plugin settings. Without `--binary`, this script remains a fixture-engine check
and supplies no ordinary-H3 execution evidence.

The `--task094-session` mode requires `--binary` and the existing
`DOLGORAE_RUN_NATIVE_CALLER_PIPELINE=1` opt-in. It uses the pinned accountless
Codex/companion pair with loopback Responses/MCP and an ordinary public-v2
gateway process. Its cases cover a schema-4 Primary's actual write/final and
subsequent writable interactions, refusal while a native background child is
present, original identity across gateway replacement and recovery, and idle
or interrupt-consented whole-session closure. Keep logical Writer settlement
and physical generation absence as separate assertions. Preserve failed roots
and report incomplete cleanup instead of replaying a failed campaign.

The additional `--prepared-worker-loss` case requires a product compiled with
`--cfg dolgorae_native_acceptance`. It pauses after durable scope publication
without holding short locks. The fixture verifies and faults only its own
Worker, then checks public recovery, the original transaction and terminal
cleanup. This is instrumented fault evidence, separate from ordinary-H3
qualification; the barrier is absent from ordinary builds.

The RPC budget remains 30 seconds. A typed unresolved transport response does
not cancel accepted semantic work. Retain that response and keep its gateway
executor alive during bounded public observation of the same retained Close.
If settlement remains pending, verify that the exact gateway executor has
exited, replace it once, then explicitly reconcile the original operation at a
fresh revision. Gateway exit does not establish owned-generation absence or Profile
retirement. A second unresolved result retains its executor and incomplete
cleanup evidence; it never starts a new Close or a mutation retry loop.

Replacement Worker recovery keeps the original Run and accepted Thread while
advancing the Worker generation. Preserve both identities; requiring the old
generation after a restart rejects supported recovery. A concurrent ledger
capture conflict requires a fresh read; it does not authorize another Task
submission. The native session client permits at most three Watch openings
within its existing 90-second observation window. Before dispatch it stores
each request context with the original Run, Thread, Turn, cursor and stamp.
It retains the actual typed `RefreshSnapshot` error and fresh observations
separately, verifies Run and aggregate identity, then opens a new
request context using the original exclusive event cursor. The refreshed head
has not been consumed: advancing to it could skip the original Turn's terminal
event. Other errors and an exhausted read window preserve the failed case.
Observe original task completion before restoring the Worker and admitting member
release. Retry only the public API's applicable busy response on the same
original identity. An expired execution clock does not replace the cleanup
phase's separate clock or settle its state.
After observing every original member idle or released, stop and join the owned
scheduler before admitting the engagement close through the native MCP. This
keeps a concurrent scheduler lifecycle lease out of that cleanup admission.
An unresolved original close remains recovery work; a passing consumer phase
does not turn its whole failed wrapper into a pass.

Ordinary-artifact physical-response-loss qualification uses the explicitly
selected, non-shipped `tests/support/native_ack_loss.c` fault library. It may
hold one genuine Accepted send on an identity-checked Worker's original socket,
then hold the selected serve process's next lifecycle acquisition for that Run
after a real control-connection hangup or loss. Instrument the actual native
MCP producer as well as `serve`, because native hire can create a Worker before
scheduler dispatch. Verify the library's loaded witness for that exact Worker.
The bounded fixture observes the kernel's poll hangup, EOF or connection reset
and preserves original syscall results, bytes and errno. It does not change
product bytes, receipts or database state. Before submission, it can hold the
selected sender's complete request
buffer while the typed helper verifies the real Worker generation, image and
socket. It forwards the unchanged bytes after selection. The private harness
must prove terminal, real unknown, public lookup,
independent Direct progress, group gaps and original-operation recovery; loader
success alone proves none of those. Release all three holds in cleanup, with a
dead-harness fallback. Keep this instrumented configuration separate from normal
qualification, which runs without the library. Never install it with Dolgorae.

TASK-081 has an isolated consultation component mode in the existing integrated
native acceptance runner: `--collaboration-component-only`. It uses the normal
`DOLGORAE_RUN_NATIVE_INTEGRATED_ACCEPTANCE=1` opt-in and pinned Codex executable.
The mode prepares captured-policy test data and checks the Specialist dynamic
tool through native Thread/Turn/call correlation, the production Worker
dispatcher, durable admission, store-reopen replay and owned process retirement.
It does not enable the live collaboration policy or qualify the later scheduler
and result-delivery integration.

The same runner's `--successor-startup-only` mode checks actual native Worker
and Thread startup for an independently provisioned `general` successor under a
`reviewer`-only Session. It uses an enabled capture matching its qualified
Profile, checks that no collaboration tool is selected, and requires zero model
HTTP requests. It prepares the owning-service call context as fixture data;
it does not claim a Primary model tool invocation. The normal opt-in and pinned
Codex executable apply. All state is isolated, and the existing release and
cleanup paths must prove owned-process absence. The parser-required
`--caller-driver` is unused in this mode.

The `--scheduler-component-only` mode checks queued managed and external work,
canonical writes, and pooled Reader passivation and reactivation through the
same runner. It prepares accepted service calls without a Primary model tool
invocation. Its passivation target has no prior result; read-only collection
does not acknowledge a required result. The fixture settles that target's
resident marker after verifying its actual idle Worker and Reader policy,
then checks scheduler passivation and queued reactivation. Both component modes
build the current main binary and native
driver, require the matching census binary, and retain their executable hashes.
Startup and scheduler modes are mutually exclusive. These bounded fixtures do
not enable the live collaboration policy or qualify TASK-083 result delivery.

Cargo build output and the accepted-legacy/gateway executable manifests follow
`CARGO_TARGET_DIR` when it is set. The E2E helpers read those manifests from the
same directory, and Make defaults `DOLGORAE_BIN` to its `debug/dolgorae` binary.
Without the variable, the existing repository `target/` paths apply.
Make passes relative output roots and paths containing spaces unchanged from
the repository root. Native example drivers use the Cargo output root even
when `DOLGORAE_BIN` explicitly selects a different main executable.

Run the complete gate from the repository root:

```sh
make PYTHON_BIN=.venv/bin/python test
```

The Make targets define four ordered validation layers:

- `make test-prepare` applies `cargo fmt`, then runs Clippy, `cargo check`,
  architecture guardrails, Buf checks, both frozen Go consumer packages, JSON
  duplicate-key and schema meta-validation, schema-example validation, Markdown-link validation,
  source-distributed agent-skill validation, Aquarium development-channel
  producer tests, and Git whitespace checks.
- `make test-unit` runs Rust library, binary and example unit tests. The two
  actual-kernel workload fixtures run sequentially after the parallel unit
  suite because their complete process census also observes other tests'
  child exits and reparenting. Both fixtures remain required, with their
  original identity checks, empty samples and startup deadline.
- `make test-int` runs the explicitly selected Rust integration suites using
  fakes, stubs, and isolated filesystem fixtures. It does not invoke the
  Dolgorae executable, Git, a database, the network, or another external
  system.
- `make test-e2e` builds Dolgorae and runs Python black-box scenarios in an
  isolated temporary home. E2E uses a real Git installation and fails when its
  required external prerequisites are unavailable.

`make test` runs those targets in that order. Because `test-prepare` applies
formatting, it may modify Rust source files without changing their meaning. Use
`make format-check` when a read-only formatting check is required.

Run the gate with the shell's default `TMPDIR`; use `CARGO_TARGET_DIR` to isolate
build output. Fixtures that call canonical-path APIs must canonicalize their
temporary roots. Keep Unix socket fixture paths short enough for the platform
so startup reaches the failure being tested.

Put product semantics in Rust and its tests. Keep Python limited to small
independent repository checks or black-box executable tests. Any future E2E
database or external service must use test-only credentials and namespaces;
production state and the user's real home must never be reused.

Do not commit virtual environments, caches, generated review material, local
workflow state, credentials, or run/session identifiers.

The TASK-063 turnless metadata probe is separately opt-in:

```sh
DOLGORAE_RUN_LIVE_WORKSPACE_STARTUP=1 \
DOLGORAE_CODEX_PINNED_VERSION='codex-cli 0.164.0' \
make PYTHON_BIN=.venv/bin/python test-live-workspace-startup
```

Run it only after explicit runtime authorization. It creates one thread in an
isolated temporary HOME/CODEX_HOME, observes three same-thread access settings
updates, restarts its own App Server and attempts to resume that binding. It
reads no account credential and submits no model Turn. The output contains
bounded structural observations rather than raw protocol or provider text.
A `metadata_observed` result leaves effective-policy enforcement unverified;
it cannot enable modern startup or replace empirical sandbox and recovery
qualification. Its admission and response-order checks run offline in the
default gate.

The separate `test-live-workspace-policy` target uses one selected account and
at most five model Turns on one thread. The first four check writable, read-only,
writable, then read-only. It checks an actual shell attempt and file bytes at each step,
updates thread settings between Turns, and omits access overrides from Turn
requests. A missing attempt or unchanged file alone does not establish a
read-only denial. It then restarts its App Server and checks the original thread,
final read-only metadata, and accepted history. The fifth Turn checks an actual
write denial after restart. No result changes a production Profile's capability
status.

Command evidence admits `agent` and `unifiedExecStartup` sources and literal
quoting of the specified command. It rejects user-shell and follow-up interaction
items, extra commands, substitutions, a different cwd, and plugin attribution.
Mismatch output contains only known source categories and structural booleans.
Turn completion and an `idle` status do not prove process quiescence; production
startup and transitions still require their separate process-census proof.

After explicit account/runtime authorization, set
`DOLGORAE_RUN_LIVE_WORKSPACE_POLICY=1`, `DOLGORAE_CODEX_BIN`,
`DOLGORAE_CODEX_PINNED_VERSION`, `DOLGORAE_CODEX_PINNED_SHA256`, and
`DOLGORAE_LIVE_AUTH_FILE` before running
`make PYTHON_BIN=.venv/bin/python test-live-workspace-policy`. The executable must
match both pins before credential admission. Only the selected current-user
0600 regular JSON credential is copied into the disposable file-backed Profile;
the probe rejects a symlink and does not use the host keyring. It does not retry
failed Turns or requests. The default gate runs only its offline admission,
budget, cleanup, evidence, and notification-order checks.

`test-native-workspace-policy-fixture` checks one ambient positive-control Turn
and the same five access steps with a scripted loopback Responses endpoint.
Each Turn receives one exact `exec_command`
call. The fixture checks the matching call ID in the native tool return; it never
supplies a tool result. Codex 0.159.3 can return a
[sandbox error before creating a command-history item](https://github.com/openai/codex/blob/rust-v0.159.3/codex-rs/core/src/tools/handlers/unified_exec/exec_command.rs),
so this check can verify a terminal native return even without that item.
Every fixture attempt, including the positive control, requires that return.
When command history is also present, its exit code and output must agree with
the correlated return. Conflicting evidence stops the campaign.
The probe requires a file read canary, a zero exit code and matching bytes
for each successful write. Each read-only step requires the read canary, a
nonzero exit code, a known operating-system permission denial, and unchanged
bytes. A generic tool error does not establish a read-only denial.
On command-result verification failure, the probe reports the evidence source,
whether the exit code was zero, the recognized command status, whether output contained a
known permission denial, whether the read canary was present, and whether the
file matched the expected bytes. The matcher ignores capitalization in
"Permission denied", "Operation not permitted" and "Read-only file system",
including lowercase zsh errors, and reports only the bounded error category.
It also reports whether the correlated return is present and agrees with command
history when both are available. It retains no tool output, commands, paths,
or file contents. Missing observations
cannot be reconstructed from an earlier campaign.

The positive control uses a separate thread with project discovery enabled
inside a Git root under the disposable HOME. It must observe common/Role and
Profile/project/ancestor markers before that App Server closes. The controlled
thread then checks the same sources with `project_doc_max_bytes=0`.
Only booleans and counts are reported. Interpret the per-path observations
against CW-AMBIENT: Profile markers are permitted user guidance; project or
ancestor markers disprove the required suppression for that configuration.
The result distinguishes Profile guidance from project/ancestor suppression.
Missing positive-control markers stop the campaign; controlled instruction
observations remain separate from access observations. Neither observation
alone establishes modern-admission qualification.

After explicit native-runtime authorization, set
`DOLGORAE_RUN_NATIVE_WORKSPACE_POLICY_FIXTURE=1`, `DOLGORAE_CODEX_BIN`,
`DOLGORAE_CODEX_PINNED_VERSION`, and `DOLGORAE_CODEX_PINNED_SHA256`, then run
`make PYTHON_BIN=.venv/bin/python test-native-workspace-policy-fixture`.
The v3 campaign matches the coordinator's approval settings: writable uses
`on-request`, and read-only uses `never`. Its connection opts into
`experimentalApi`, as the caller-workspace coordinator must for the pinned
[settings request and notification](https://github.com/openai/codex/blob/01fc69f4026735edfdf6789820549727a4867b11/codex-rs/app-server-protocol/src/protocol/common.rs#L651).
That opt-in adds no dynamic tools and does not qualify the mechanism.
Before the first controlled Turn, it
calls `thread/name/set`, checks empty history, restarts the App Server and
requires the original writable Thread with empty history. This tests the
metadata persistence barrier independently of a task Turn. The pre-Turn resume
supplies the frozen launch inputs explicitly, including discovery suppression
and the selected approval and sandbox settings. Thread ID alone does not prove
that startup settings survived; identity mismatches report separate booleans
for the Thread, workspace, model, provider and approval policy. This writable
mechanism probe does not authorize automatic writer acquisition during recovery.
The later restart supplies those inputs again with explicit read-only policy
and `never` approval, matching product recovery. It requires the original
Thread's completed history and a fresh read-only attempt; it does not assume
that the last settings update survived in native storage.
The pinned [resume implementation](https://github.com/openai/codex/blob/01fc69f4026735edfdf6789820549727a4867b11/codex-rs/app-server/src/request_processors/thread_processor.rs#L4033)
honors explicit approval and permission overrides when loading persisted settings.
Before each settings update, two complete loaded-Thread observations around
an idle-status read must contain only the original Thread. This checks the
native metadata API used by the conservative child guard; it does not prove
operating-system process absence or child permission inheritance.

One campaign allows at most six native Turns, four App Server processes, two
threads, and twelve loopback Responses POSTs. One thread is the positive control;
the other is the original access-test binding and must survive both restarts.

The corrected v3 native campaign confirmed original-Thread recovery with empty
history when the pre-Turn resume supplies the accepted launch inputs. It observed
all six Turns and twelve loopback POSTs, including two writable file effects,
three read-only denials and original completed history after read-only recovery.
Its positive control loaded Profile, project and ancestor instructions; the
controlled Thread retained Profile guidance and excluded project and ancestor
guidance. These observations qualify the mechanism on the pinned executable.
Complete Dolgorae startup, Writer commit and recovery need their own acceptance
checks.

The custom provider requires no account
auth, disables WebSockets and retries, and accepts only its numeric loopback
endpoint. The disposable home has no credential file; inherited account and
proxy environment variables are excluded. Tool output and HTTP bodies remain
in memory and results contain only bounded structural observations. The native
runtime, endpoint, and disposable files are closed or removed on completion or
failure. The default gate checks this machinery with offline clients. An opt-in
result establishes native sandbox observations for the selected executable; real
provider behavior, process quiescence, and production qualification still need
their own evidence.

For task-aware Specialist work, keep reusable Role behavior in the Agent
Configuration and executable brief/context/criteria in `src/task_request.rs`.
Exercise one-shot composition in `src/review.rs` and reusable facade behavior in
`src/external_engagement.rs`; both must persist the accepted JCS request before
dispatch and validate `structured_review_v3` before artifact commit. Test
Korean, CRLF, quotes, Markdown, and shell metacharacters as data, plus every
count and byte boundary. Fake-runtime E2E must assert that task bytes appear in
the Turn input but not `developerInstructions`, and must not be described as a
live-provider acceptance result.

## Independent review readiness verification

The [independent review requirements](../specs/README.md#independent-review-readiness),
[architecture](../architecture/README.md#independent-review-readiness), and
[EPIC-016](../roadmap/README.md#epic-016-reliable-independent-specialist-review)
record the shipped boundary and its verification. For future changes, synchronize
the private checked contracts and examples before runtime changes; preserve the
public-v1 descriptor and legacy results.

Test the full contract through both one-shot and reusable production paths.
Model/effort tests must vary advertised ordering and explicit overrides.
Malformed-output tests must exercise safe structural diagnostics and secret
canaries as well as rejection. Lose the first validation-error response, restart
the process, and assert identical sanitized diagnostics from authorized
await/collect. Cover the terminal/diagnostic commit boundaries and historical
records with no diagnostic payload; never reconstruct missing detail from raw
model output.

Retain a reference before effects and use the public one-shot lookup/recovery
authority contract. Terminate the original CLI and discard its responses, then
inspect the same operation through public commands in a new process. Check
read-only observation, authorized recovery,
unknown/blocked outcomes, and rejection of mutation without authority. An
engagement ID alone cannot bypass the current `legacy_one_shot`/`external_v1`
boundary. No private database query or replacement review may count as recovery.

Native cleanup tests must start at server creation, before model/effort checks
or `PreparedReviewer` publication. Inject Reviewer policy, credential,
engagement, and capture failures and interruption before member registration
with both newly created and pre-existing servers. Cover concurrent membership,
server replacement, and uncertain process identity under a stated adversarial
attack budget. Durable ownership must survive forced termination; destructors
alone do not prove that property.

Install the skill into an isolated temporary root and validate its required
schemas, examples, and local reference closure without a checkout. Compare
packaged contract bytes with their canonical sources. Test wait, interruption,
and unknown-outcome guidance against the production CLI and a controlled fake
provider, using TASK-058's implemented commands after process-handle loss.
Prose assertions alone cannot establish the behavior.

The access-safety driver's `DOLGORAE_CODEX_PINNED_VERSION` override selects an
explicit exact `codex-cli x.y.z` qualification candidate and reports its observed
version; the default remains the original `0.158.0` campaign.

The live v3 campaign uses the required defaults, the defective Hello World
fixture and Codex `0.158.0`. Run it only
with explicit live authority. The existing v1/v2 drivers and a passing default
gate do not supply this evidence. Complete each Task's designated checks and
independent review, then run the full repository gate and live acceptance for
TASK-060. Its integrated checks must retain the diagnostic-loss/restart and
preparation-failure scenarios, and its authorized live campaign must include
new-process lookup after CLI and response loss. Keep raw logs and runtime
identities local under the repository's evidence-retention policy.

## Orchestration core verification

Keep Role and policy semantics in `src/specialist_policy.rs` and durable
aggregate behavior in `src/orchestration.rs`. The orchestration adapter is the
effect boundary: tests should inject deterministic failures immediately before
and after SQLite commit, Primary intent publication, child reservation, Worker
publication, thread creation, task dispatch, result append, and delivery
receipt. A retry may resume only when durable state proves that no external
effect was accepted; ambiguous publication or Turn acceptance must remain
`recovery_required` or `interrupted_unknown`.

Use isolated temporary state roots for broker tests and check both SQLite and
the protected credential carrier. Raw capabilities must not appear in the
database, events, tool results, or diagnostics; the carrier must be mode 0600
and disappear only after authoritative Specialist release. Schema examples and
the fake adapter prove the transport-independent core. They do not prove the
later live model-facing tool transport, and the opt-in live Codex targets remain
outside the complete repository gate.

## Live provider implementation

EPIC-008 completed its amended twelve-task roadmap and established
`MILESTONE-BH1-P`. TASK-053's checked contract remains frozen through
`docs/protocol/dolgorae-gul-consumer-v1.lock.json`; TASK-054 provides complete
history, TASK-055 read-only aggregate/result queries, TASK-052 whole-session
closure, TASK-056 unchanged-consumer regression, and TASK-026 provider
acceptance. Follow the canonical [provider specification](../specs/README.md#v013-live-provider-slice),
[Gul consumer contract](../specs/gul-consumer-v1.md), and
[operations runbook](../ops/README.md#v013-provider-operation). Actual Gul
acceptance, release, publication, and installation remain separate.
Preserve TASK-025's completed private contract and transport selection. Its
isolated probes do not implement production bridge/provisioning/dispatch.
The public 27-method consumer lock is a checked TASK-053 artifact; do not treat
its descriptor presence or generated stubs as runtime availability. General collaboration and
busy/mail-count reuse descriptions are EPIC-009 targets, not instructions to
create placeholder services in EPIC-008.

Reuse the existing Broker, Run/Worker, writer, artifact, and process inventory
owners. Share task-content helpers and applicable result validators without
using the External Specialist CLI/facade as the Broker backend or forcing review
criteria onto ordinary tasks. Do not mutate shared global Profiles for one
Run's tool registration. TASK-025 selected native `item/tool/call` on the isolated-home live campaign
pin (locally installed Codex CLI 0.155.1). That historical campaign did not
change the then-current Codex App Server 0.153.4 compatibility baseline.
Deterministic probes and the checked selection artifact live under
`examples/support/live_transport.rs` and
`docs/protocol/dolgorae-live-transport-selection-v1.json`. The separate
`live_transport_probe` example owns the test-only stdio MCP entry; the ordinary
binary exposes no `__live-transport-mcp` command. It must not edit a shared Profile.
The optional Python live campaign creates its own Python MCP and remains
independent of this Rust example and its historical selection evidence.
`DOLGORAE_RUN_LIVE_TRANSPORT_PROBE=1` runs the isolated live probe against the
local `codex` binary. The campaign creates its own Codex home and must not use
`~/.codex` or `~/.dolgorae`. It is outside the complete repository gate.
Production TASK-047 authenticates live `item/tool/call`; TASK-049/050 split
admission from completion and must not keep the fake `CompletedTask` seam.
Commit authenticated busy rejections for exact replay. Compare assignment
intent with admitted member access before writer movement.

Separate durable task acceptance, Turn acceptance, completion, and delivery.
Replay an accepted receipt before fresh busy checks. Validate immutable policy,
member access, and writer/working-root conditions before reservation or effects.
No SQLite transaction or global mutation owner spans a model Turn, approval,
or bounded wait. Route broker approvals to their spawn operation, not a Codex
pending request. Use injected clocks and retain the original durable deadline.
Transport timeout or disconnect is not cancellation; interrupt acknowledgement
alone is not terminal proof. Never replay possibly accepted work. Assignment
always returns its durable accepted receipt. For fresh `blocking: true` calls,
wait only until terminal state or the earlier of acceptance plus 60 seconds and
the durable deadline; exact retry returns the receipt without re-waiting. A new
await/collect call obtains terminal state. Test these response shapes without
implementing the production dispatch/wait path ahead of its owning Task.

Check accepted-request identity before its output discriminator. Validate the
requested output before publishing completion. An artifact ID must name real
immutable bytes with matching length and digest. Use write-ahead reconciliation
between existing artifact storage and SQLite, not a cross-store atomicity claim.
Expose only a permitted Primary result projection or bounded private reader,
never child credentials or arbitrary child files. Preserve cursor redelivery.
TASK-055 exposes published references through the root-authorized result query.
A conformance client must discover them there before artifact reads; do not
inject IDs from private fixtures or fabricate Primary final-response events.
For text pages, offset must be a UTF-8 boundary and content the longest complete
prefix within the requested byte limit. Reject an undersized next-character
limit or invalid offset; never return an empty pre-EOF page or replacement
characters. Preserve exact CRLF, hash the complete immutable readable artifact,
and test Korean, emoji, EOF, malformed ranges, and lossless page concatenation.
The contract fixtures are reference checks, not evidence of the TASK-051 reader.

Restore pinned Run/thread/Profile/access, isolated work roots, writer state,
deadlines, and receipts on restart. The existing external-engagement isolated-
write branch does not automatically cover brokered members. Reuse TASK-046 for
any new bridge process and test teardown; healthy shared servers survive client
exit. Do not add unsolicited Primary Turns or auto-resume paused Runs.

History is distinct from mutation replay. Store exact accepted Primary text
before acknowledgement and implement the full Controller timeline, including
long input artifacts. Test same-key retry versus same-text new request, pages,
concurrent append, restart, interruption and close. New ordinary human input is
rejected while a Turn is active; retain drafts without queue/steering/auto-send
and keep current Interaction answers available.

Whole-session close uses the root Controller and Broker-owned child control.
Persist intent before effects; reject non-interrupting close while owned work
is active. Account for in-flight spawn/dispatch and uncertain results before
reporting closed. Use aggregate snapshots for bounded-call reconciliation, not
Gul child mutations or blind retries. Keep history/files/unrelated sessions and
healthy shared Profile Servers intact; Primary pause is not aggregate pause.

Publish exact contract and credential-schema hashes. TASK-056 runs the old
consumer unchanged rather than regenerating it to match each new candidate.
The immutable TASK-053 descriptor is the Buf compatibility baseline; the
pre-TASK-053 descriptor remains only the source for the older low-level client.
The candidate must advertise the SHA-256 of its actual checked descriptor bytes,
and both generated clients execute against the production UDS gateway before
and after restart.
Keep contract-ready, runtime-ready, released-provider and actual-Gul evidence
separate. Future Podway observation remains read-only and outside this release.

Extend the existing native gateway fixtures with generated public-v1 clients
against real dolgorae serve and production semantics. Label fake Codex scenarios;
separately authorized pinned-live evidence must prove actual Primary calls,
both approval modes, Specialist execution, and actual result consumption.
Plan approval does not authorize live Profile/account or token use. Missing
required live evidence prevents Task completion. Scope live checks to new or
changed external behavior; reuse earlier evidence only when its tested
conditions and coverage still apply. `tests/e2e/test_private_boundary.py` is
the default-gate deterministic campaign: it starts the production gateway and
uses public-v1 clients and production semantics, but deliberately substitutes a
fake Codex boundary. It must cover exactly the frozen 27-method profile and
must never be reported as live-model evidence.

The separately authorized command below runs the assembled provider against
the isolated campaign home and pinned Codex CLI. It copies only the selected
authentication file, never uses the user's Dolgorae home, and emits a bounded
sanitized summary rather than raw model or credential output:

```sh
DOLGORAE_RUN_LIVE_PROVIDER_ACCEPTANCE=1 \
  make PYTHON_BIN=.venv/bin/python test-live-provider-acceptance
```

The live campaign must pass both `user_approval_required` and
`fully_delegated`, prove an actual Primary tool call and Specialist execution,
make the Primary consume a result above the private inline-page bound, and have
the generated public client discover and verify that result through the public
result-list and artifact APIs. Planned paths and static driver checks are not
live evidence. Keep raw logs, provider prose, credentials, local runtime
identifiers, and model output out of tracked documentation.

Use non-writing Markdown/schema/example/descriptor/skill checks for plan changes.
Implementation completion still requires the full deterministic gate and
applicable live evidence. v0.1.3 requires both EPIC-008 and EPIC-016 completion
under the roadmap release boundary. Exact-candidate build/installation QA,
tag, publication, and runtime installation retain separate authority. v0.1.3
remains a Milestone Preview, not Personal Alpha or proof of actual Gul integration.

## Planned v0.1.4 qualification

The [consumer acceptance matrix](../specs/dolgorae-consumer-acceptance-v014.md)
binds every release-scoped Aquarium, Podway and Gul requirement to current
contracts, implementing Tasks, external owners, runtime paths and final gates.
TASK-067 refreshes source revisions and resolves external implementation plans;
TASK-068 publishes qualified immutable H1 after pre-contract host feasibility.
The [seven-case host matrix](../todo/TODO-DOLGORAE-V014-RELEASE-ALIGNMENT.md#pre-contract-external-mcp-host-feasibility)
covers initial invocation, exact replay, changed-input conflict, concurrency,
reconnect, MCP process restart and response loss around acceptance/receipt
commit. Host behavior requires runtime evidence. Probes need explicit runtime
authorization and opt-in. Preserve the historical mcp_unavailable record and
ADR trust boundary; typed unsupported cannot satisfy required host support.

Validate sequential handoffs: EPIC-018 closes with H1; Gul's contract/foundation
Epic closes with H2; TASK-069 opens EPIC-019 and records Gate A. Phase 3 closes
EPIC-019/020/009/021 with implemented provider capabilities and mandatory
Aquarium/Podway runtime qualification. TASK-087 prepares intended v0.1.4 identity
and packaging and checks/requalifies earlier TASK-077/086 results against the
post-cleanup H3 candidate. Record source revision, binary/package checksums,
contract digests, runtime pin and relevant configuration. Missing required
external work blocks closeout and requires an explicit corrective schedule.
TASK-093 owns the separately identified H3 replacement for the public-v2
Observation gap. Compare all intervening source changes with the original H3,
requalify affected native bindings on the exact replacement image, and retain
unaffected evidence with its original scope and failures. Deliver a separate
correction Handoff; do not silently rebuild, repin or overwrite H1.

Gul performs Phase 4 product/qualification Epics using fixed H3, including E16-T5's
early isolated packaging/recovery rehearsal and final requalification on its
intended v0.1.0 artifact. Native Gate B and final campaign occur within this
phase. Gul owns client behavior/UX and closes its product Epic with H4 evidence
before TASK-088/089. Phase 5 TASK-088 independently validates reproducibility,
artifact applicability and provider facts, runs necessary actual-Gul cases using
the completed Gul artifact, and verifies/revalidates all required Aquarium and
Podway runtime evidence against the same final provider artifact. No active or
reopened Gul development Task is required. Gate C needs both accepted results;
TASK-088 cannot close by acknowledging a report.

Keep component and final measurements separate: TASK-075 owns core warm reads,
ledger tailing, bounded waits, database access and streams; TASK-078 owns the
16-member K-server bound and configuration/account/read-only isolation;
TASK-084 owns the 50-task group, bounded queues and complete result/coverage
accounting. Gul Phase 4 produces integrated load, reconnect, interaction visibility
and latency evidence; TASK-088 independently checks/reproduces necessary cases.
TASK-075 must not wait for later pool/group implementation. Fresh authority,
process and history proofs remain necessary at mutation, writer release and
generation replacement boundaries regardless of display-cache latency.

Keep four evidence classes distinct. Deterministic integration and fault tests
use actual Gul, actual Dolgorae and the pinned actual Codex app-server with
scripted loopback Responses. Separately authorized real-account smoke on the
exact final pair covers create, bounded task, approval, complete result
collection/display, follow-up and normal close. Gul Phase 4 executes and records
client/UX outcomes; TASK-088 independently validates provider facts and reproduces
necessary cases with separate authorization. Device usability and the 24-hour
three-client soak remain Gul evidence; paid execution need not cover the entire
soak. Aquarium/Podway supported host, adapter, identity, receipt, round accounting
and recovery require their own actual-path evidence. Mocks, fixtures and handoff
documents cannot replace these runtime obligations.

Record exact artifact, contract, runtime/model/effort, harness/scenario revision,
configuration and per-step results. Agree commands, opt-in and bounded execution
budget before runtime execution; this amendment advertises no new command.
Use isolated state. Retained cross-repository evidence must follow the approved
bounded non-sensitive aquarium.promoted-evidence/v1 policy under evidence/aquarium;
never promote raw logs, provider prose, credentials or runtime identities.

TASK-089 consumes the complete accepted three-consumer matrix, both repository
gates, final QA and exact artifact provenance. It verifies the already qualified
artifacts and introduces no unqualified rebuild or packaging change. Only
existing explicitly permitted QA-neutral metadata exceptions apply. Substantive
artifact, contract, runtime or relevant configuration changes require evidence
impact assessment across all three consumers and affected requalification before
Gate D or either publication. Defects block incomplete Epics and require explicit
corrective scheduling. Planning validation proves neither implementation nor
runtime support; runtime use and separate release workflows remain authorized
activities in their own right.

### Current runtime qualification

Select Codex CLI `0.164.0`
(`d8990e3ee7acfbc26f91265b12a1a83f0fee40d0e0f8f1f40a37d4ec21f499cf`) explicitly
for the native integrated acceptance, workspace-policy fixture and review-reader
campaigns. These campaigns use private Codex state and a scripted loopback
Responses provider that requires no account credentials. In product-launched
Profiles, `HOME`, `USER`, `LOGNAME`, `SHELL` and `TMPDIR` come from the OS account
and platform. A temporary driver `HOME` does not change that captured
environment. Check the child environment against the authorized test scope;
private `CODEX_HOME` alone does not prove absence of user-home access.

The integrated campaign includes caller-pipeline coverage; run the standalone
caller only to isolate missing coverage or a failure. Keep the existing
`shell_snapshot=false` regression and the process-quiescence checks. The
default-feature campaign is separate.

The existing `--scheduler-component-only` command runs correction proof and
ordinary queued-work qualification in independent subphases. Each uses its own
private state, the existing driver bounds and a separate process census. The
correction subphase starts no model Turn; the ordinary subphase retains the
managed/external writes and pooled passivation/reactivation checks. The command
succeeds after both subphases complete. Activation, pre-effect claim and task
expiry observations use controlled fixture clocks; they do not extend an
accepted deadline or promise automatic retry.

Run `--collaboration-component-only` only with explicit native authorization and
its documented opt-in. The bounded qualification uses two isolated
approval-policy cases, ordinary launch-intent Primary/native Thread creation,
two physical Dedicated members and foreground current-main `serve` ownership.
The fully-delegated trace has 17 POSTs/7 Turns and the approval-required trace
has 18 POSTs/7 Turns; bind each observed count to its exact script and inputs.
The separate 17-Turn/34-POST integrated campaign retains its own checks and
budget, with its own foreground main `serve` Child and strict process census.
Earlier seven-POST prepared-Primary evidence retains its narrower scope.

The component uses the current main CLI for Profile and policy publication,
explicit exact-key qualified server start/stop, credential setup and all Run
mutations. Its driver only observes an unrecorded Profile candidate. Source
Pause/Resume uses that member's original fixture-owned Broker carrier through
main CLI `--controller-fd`; no Primary or peer capability substitutes for it.
The carrier stays private and never enters a model request. Cleanup preserves
the first failure, independently attempts ordinary owner closure and exact
foreground Child termination, then records the current-main strict census on
both success and failure. A cleanup error cannot establish absence.

The trace covers normal Primary policy-task busy-target assignment, lateral nonblocking
submit/blocking await/collect, and ordinary owner Pause/independent captured
Kernel absence/Resume before collection on the original Thread. The target
artifact exceeds 64 KiB. One bounded Source read supplies its UTF-8 prefix in
the next native request; after restart, a new read supplies its tail. Each
current call and label binds the page bytes and offset to the same original
artifact's full byte length and digest. Page-level input provenance is recorded
separately from the driver's whole-artifact byte verification. Preserve each task's
original deadline and the existing 180-second driver, build, outer and cleanup
bounds. Direct unit coverage retains blocking submit; this physical trace does
not prove blocking-submit interruption, shared collaboration binding or runtime
subagent-disable enforcement. Pool/default-feature and frozen-consumer
qualification remain separate.

Keep normal `dolgorae` processes, including foreground serve, on fixture HOME
so DolgoraeHome::system resolves only private fixture state. Product-launched
Codex child account HOME/platform TMPDIR are a distinct explicitly authorized
environment exception. Report no account login, account credential copy or
external provider requests separately from that HOME policy; private CODEX_HOME
does not prove absence of real-home file access. Ordinary member release,
Profile retirement, foreground Child termination and current-main owned-root
process census must finish before success. Failed roots, unknown effects and
unproved absence remain explicit.

Historical TASK-077 MCP host qualification additionally selects Claude Code `2.1.310`
(`ffb64331bc0ee1b69efb7c2c428bd3ab516bc31b75bf60886b356c97c1c25ffd`); planned
`EPIC-024` (`TASK-095` through `TASK-098`) transitions caller-scoped host setup
(`External` and caller-scoped `Managed`) to `Codex >= 0.169.0` and `Claude >=
2.1.310` hardened non-executing setup binding with split-digest `schema_version:
2` online refresh alongside digest-only unclassified `config.toml` capture and
dedicated-entrypoint quiescent same-or-newer server rollover. The
[runtime qualification
owner](../specs/dolgorae-v014-delegation-runtime.md#current-host-and-profile-runtime-qualification)
owns historical host qualification pins, `EPIC-024` external-host floors, and
the new-creation minimum of Codex 0.169.0. Newer runtimes may be attempted
without per-version registration and retain unverified compatibility until
separately tested. Schema generation at 0.159.3 and 0.164.0 establishes
protocol differences, while native campaigns establish behavior. The historical
H1 qualification remains unchanged.

Caller Primary asynchronous admission keeps one existing slot available for
Controller approval responses, even when blocking tools occupy the ordinary
slots. Do not raise the total cap or apply that reservation to unrelated Run
families. External queue removal preserves its original ordering record in the
owning SQL transaction and event chain. Historical immediate admissions
retain normal priority under their original admission contract. A task with
queue-admission evidence requires its original captured or retained priority;
if that priority is unavailable, materialization refuses it and preserves the
previous output. Removing or lacking the current queue row never supplies a
normal-priority default. Prepare and validate full immutable content outside the short
publication write fence, then recheck database motion, selected metadata and the
pending/output inode identities. One failing workspace must not starve later
independent refreshes.

A separate native policy-Writer fixture uses Caller3 members and the main-owned
runtime, with fixture-owned trusted-host task admission. It exercises the initial
Writer and a later task-attributed acquisition and release. Its synthetic source
calls qualify owner wiring and physical lifecycle behavior; they do not qualify
canonical-write assignment through the live Primary bridge or all native
model-source authorization. The existing live-bridge refusal remains. Each source
phase retains its original bounded deadline; failed or ambiguous qualification
never widens the expected request or Turn count. Runtime qualification requires
separate explicit native execution and its actual evidence.

## Source preparation checks

Keep actual consumer composition in the existing integrated/native fixtures.
Extract only the selected Aquarium helpers and referenced instruction files into
the fixture's private source directory. Retain their exact content identities
and use the shipped ordinary preparer/renderer rather than copying their
validation logic. Pass their prepared assignment and original instructions to
the actual task. Preparation is source plumbing, not native or consumer proof.
Formal mapping must use the managed Root, member and task's authoritative
identity; an external Engagement or synthetic fixture identity is not a
substitute. Keep all original campaign bounds until their owner resolves a
demonstrated trace conflict.

The production-host runner preserves its eighteen original scenario budgets.
Its separate `consumer-complete`, `consumer-rounds`, `consumer-release`,
`consumer-fences`, `consumer-group` and `consumer-group-gaps` cases use declared
bounds for full source pages, prepared inputs, original outcomes and receipts.
The group cases map only the genuine synthesis Task to a receipt. Use
`--trace-only --rows <names> --hosts codex claude` to inspect the planned bounds;
this executes no qualification. `--verify-distribution-only --podway-archive
<archive> --podway <executable>` verifies selected packaged bytes and closed
schemas without running the consumer. It proves no native row.

Pass the same archive with `--podway-archive` to the actual production-host
campaign and provide the selected Aquarium full archive with
`--aquarium-archive`. `--direct-contract-only` uses the selected public CLI and
production client for Direct progression, reactivation and an unsupported
workspace generation. It requires the normal explicit runtime opt-in. Keep
short canonical scratch roots for macOS sockets and preserve failed receipts.
A physical worker-loss case must prove original Task settlement before retiring
its Profile; unresolved history stops cleanup for explicit recovery.

For a frozen managed campaign, `--successor-driver` and
`--successor-driver-sha256` select the already-built exact driver. The runner
checks its bytes before making a private copy, which its workers and Profile
drainer also use. Omit these options only when an explicitly authorized source
build is intended. Keep the main binary, each driver, input artifacts and source
manifest together. A single-image campaign must use that exact image set for
every row. Separately identified passing phases may form a current evidence
composition only after comparing each original command, coverage, input,
dependency and environment with the current candidate. Preserve original image
identities and failed campaign outcomes; never present that composition as one
single-image campaign. Changed guarantees require affected current execution,
and unknown applicability requires requalification. TASK-087 still requires
qualification against its exact post-cleanup packaged artifact.

For current qualification, select `--managed-producer-only` in the integrated
runner. It retains a `producer-facts.json` after genuine managed Root dispatch,
original page reads, structural receipt and owned cleanup. The old
`--successor-only` mode retains its historical formal consumption scope; it is
not the current ordinary-v3 qualification entrypoint. Supply a frozen driver
with the pair above; current qualification does not build at runtime.

`tests/e2e/run_native_aquarium_consumer.py` consumes the exact producer facts from
the external and managed cases. Its independent opt-in is
`DOLGORAE_RUN_NATIVE_AQUARIUM_CONSUMER=1`. Pass `--producer-facts`,
`--task-criteria`, the selected Aquarium/Podway archives and native executables,
both model names and a short canonical `--scratch`. `--hosts codex claude`
selects actual same-host collection/Evaluator roles. The cases are `current`,
`wrong_candidate`, `wrong_goal`, `wrong_attempt`, `incomplete`, `superseded` and
`wrong_schema`. Each case has a 180-second, 32-POST, 4 MiB-input bound, at most
three actual native children, 20 Primary requests and three count-token requests.
The ordinary Actions use no fabricated formal receipts. Preserve the actual
native handle, entire returned report and observed Primary decision; negative
cases leave the affected placement unchanged. The runner never claims a global
Task pass or semantic model reasoning.

The Codex official distribution also requires its matching sibling
`codex-code-mode-host`. Preserve both exact executable hashes and the archive
provenance when freezing the campaign. Advertisement and tiny child-capability
checks are preparation evidence only. A current wording correction to the
advertisement script permits its one metadata-only `functions.exec` call;
historical frozen probes retain their original bytes and outcomes.

The existing successor program has five Turns: Primary spawn (two POSTs),
Primary assignment/await (three), managed Writer (two), Primary consumer
(ten plus four required continuation exchanges), and external Writer (two).
Its expected total is 23. The consumer reads these fixture views through current
correlated tool returns. These are source-derived page allocations; qualification
must confirm the actual bytes and counts:

| View | Payload / page allocation |
| --- | --- |
| Complete result | One modified file, no output attachments: 1111 bytes, two pages |
| Formal original response | Fixed-width actual UUID/digest bindings: 1857 bytes, two pages |
| Before / after | 33 / 8 bytes, one page each |
| Selected package | 901 bytes, one page |
| Prepared Record | 452 bytes, one page |
| Original Record outcome | One item result and bounded selected envelope, one page |
| Prepared Accept | 1348 bytes, two pages |
| Original acceptance outcome | Bounded selected envelope, one page |
| Original receipt | Selected envelope and full receipt, three pages |

Read the second result/response pages in one exchange, the second Accept-input
page in one, and the remaining receipt pages in two. These four exchanges are
`K`, not spare capacity. Public wrapper UUIDs/timestamps and the physical root
change bytes but stay within the declared envelope page classes; the original
whole-body digests remain bound. A changed page class, missing view, wrong
call/operation/request ID, changed original byte, incomplete read or stale receipt
fails qualification. Original response bytes come from the scripted executor;
complete-result metadata comes from the owning collected result; change bytes
come from the independent fixture before/after values. Prepared inputs are
checked against the selected closed schemas and current public fences, retaining
their actual generated original keys. Outcomes are compared with public key
lookup, and receipt semantics with a separate public receipt read. No original
body receives credit from a user prompt or unrelated history.

The main integrated program remains 17 Turns/34 POSTs. Its inherited 2 MiB
per-HTTP-input guard is unchanged. The separate two-host eighteen-row campaign
keeps its original 8 combined POSTs, 32 deliveries, 90-second row, 30-second
cleanup and 4 MiB per-input bounds. A source-derived trace is not native proof.

For the selected Codex Code Mode carrier, each independent batch is one custom
`functions.exec` call that awaits the registered tools and emits labeled results.
Correlate the outer custom return, require the observed separate `Script completed`
header and JSON result blocks, and preserve each nested result without trimming
or searching history. A running cell or a session without a terminal exit is not
completion. Keep logical test labels separate from actual inner native call IDs.
Caller, policy, reader and integrated checks share this bounded carrier handling;
the host campaign's Codex producer does too. Claude/MCP direct host paths keep
their own framing. The successor's threaded request handling lets its Writer wait
for the genuine assignment acknowledgment while the Primary request is served;
request handlers join before fixture shutdown. Preserve actual input sizes, page
digests/offsets and correlated returns in the result before successful cleanup
removes the fixture.

Writer task prompts contain one accepted JSON object followed by the producer's
execution-intent and deadline text. Decode that object and require the exact
suffix and typed task/context for the case; trailing text is not arbitrary.
Preserve the first fixture error when a later failed-task observation follows it.
Cleanup observations retain settlement-port code/reason before service error
normalization, forwarding arguments and results unchanged.

The state-control fixture takes the close revision from the owning Engagement's
public getter. Its membership counter is a different fence. Duplicate close
checks require the first result to show a closed Engagement and retain the
actual first and replay replies before asserting equality. A missing emission
record or a native transport error remains a failed row.

The private source-binding adapter is tested separately from native transport.
Use the source-binding Rust corpus, owning service replay/admission corpus and
the cold setup CLI test to check preparation publication, provenance, drift and
identity comparison. The [operator recipe](../ops/README.md#private-source-preparation)
uses isolated files and inherited authority FDs. Preparation fixtures do not
prove native host or actual consumer support.

Preparation storage bounds apply to separate components. Public carrier data
retains its 16 MiB raw/canonical ceiling; assignment selection and registration
metadata reserve another 64 KiB. A snapshot retains the existing 64 MiB bound,
and a target manifest retains the capture producer's 8 MiB bound. Private source
selection reserves 64 MiB for Profile authority, sixfold JSON encoding of
129 sources with at most 65,536 inline bytes or 64 components of 255 bytes each,
and 1 MiB for aliases/policy metadata. Reads check length before allocation and
read through at most limit+1, with checked arithmetic. These ceilings do not
relax any source, task, count or per-field contract bound.

Within one changes capture, clean paths with the same captured content digest
and length can reuse an already verified Git before image. Keep references into
the bounded retained-side buffer rather than another copy of each blob. Every
path still retains its own modes and contributes to the aggregate content bound.
Read current tracked and untracked names in separate Git listings: deleted names
can remain in the index while their replacements are untracked. Each command
keeps the existing output bound, and the merged domain keeps the existing
added-path and complete-result change bounds.

Within a native collection call, use the owning service's verified typed result
and consume the materialization returned with the selection. Do not reconstruct
the same JSON or walk all change content again for that response. Content reads
still have opening and closing observations; later inspection and Record/Accept
calls perform their own checks, including fresh export proof before mutation.

## Complete delegation observation checks

The focused Rust corpus exercises the shared allowance, complete history,
source/authority changes, pending interactions, both owning services and pinned
Engagement transactions:

```sh
cargo +1.97.1 test --locked --offline --lib complete_observation -- --test-threads=2
```

Use a new isolated target and inherit the shell's default `TMPDIR`; canonicalize
fixture roots where their owning path contract requires it. A zero-test filter
is not evidence. The WAL checkpoint case proves that closing source checks run
while the database snapshot is actually pinned. Scoped mutation controls need
separate immutable sources and targets; their artifacts must never enter an
ordinary verification target. These offline checks do not replace native,
consumer or complete-candidate qualification.

## Research group admission and recovery

`delegation_service::research_group` owns the group lifecycle shared by checked
managed/external facades and public-v2 GroupService. Public admission verifies
the actual aggregate Controller and retains its own origin; it supplies no
native Turn/tool provenance or Podway caller scope. Native facade assignments
keep their configured caller accounting and limits.

Exercise `assign_group` through real protected Codex and Claude host bindings,
and through the managed Worker entrypoint, rather than calling only the group
service. Its K mirror must verify the retained original namespace, normalized
input, caller scope and native proof on re-delivery of the original logical call.
Keep the full accepted envelope identity unchanged: a different protected
invocation UUID conflicts even under the same key. Test a Controller-origin
null-invocation key collision separately from fresh R lookup and managed
source-call checks. A changed managed `source_tool_call_id` must not rewrite the
accepted group or child provenance; it does not supply an external invocation UUID.
`await_group` and `collect_group` remain fresh R observations, with no assignment
or accounting writes. Original-key lookup returns the group receipt and leaves
each child's progress to coverage. Exercise 0, 64 and 65 entries through the
Controller-origin service/store fixture: accept the full 64-entry manifest and
refuse invalid counts before intent, capture, quota or birth reservation.
The focused `task084_` tests exercise these routes. The managed test port replaces
only native provisioning/settlement after the normal Worker authority checks.

Generation 7 fences writers that do not account for group captures and result
component bindings. Preserve the historical 5-to-6 migration's literal target;
the separate 6-to-7 transaction changes only the writer generation and extension
marker. Read-only admission accepts retained generation 6 without upgrading it.
The six base tables, namespace birth and orchestration extension 3 remain intact.

Validate all ordered JCS inputs against installed provisioning before accepting
the group. The acceptance transaction links chunked captures, actual member Run
allocations, any required future Run births, quota reservations and the group
receipt. Keep the `GroupQuotaPlan` locks through that commit and release them
before starting a member. Artifact publication and group reservation share
the Run quota lock. A failure or unknown effect retains the reservation.

Group recovery reads retained chunks and uses deterministic original child
keys to admit undispatched children. Accepted, refused and unresolved children
remain with their existing owner. Never reopen a selected input
file or turn an unknown child into a replacement dispatch. Managed and external
recovery validate the retained Controller-group origin separately from native source
provenance. Complete results retain exact content identities and their
publication-time artifact bindings; collection creates no new result identity.

Queued successor expiry prepares its body-free complete result before taking
the SQLite write transaction. Acquire the real Run's quota lease first, then
commit publication, terminal state and accounting together. Test both owners at
normal capacity and the exact 256 MiB boundary, including rollback and repeated
expiry. At capacity, retain the real result identity without artifact references;
never repair historical expiry from collection.

Run the focused `research_group`, `group_quota` and `structured_findings`
Rust filters before the complete repository gate. The accountless native
50-task harness is a separate opt-in measurement of actual queueing, member
reuse, server bounds and durable recovery; fixture assertions alone do not
establish that measurement.

The immutable H1 package pins its Makefile and example validator. Run the added
manifest and offline harness checks separately:

```sh
.venv/bin/python -B tools/validators/validate_result_component_manifest.py
.venv/bin/python -B tests/e2e/test_native_research_group.py
```

With explicit native runtime authorization, build the instrumented driver in a
task-owned external `CARGO_TARGET_DIR` and copy it and the ordinary product binary
to separate frozen paths before starting workers:

```sh
cargo rustc --locked --example native_research_group -- --cfg test --cfg dolgorae_native_acceptance
DOLGORAE_RUN_NATIVE_RESEARCH_GROUP=1 .venv/bin/python -B tests/e2e/run_native_research_group.py --driver /absolute/frozen-driver --binary /absolute/frozen-dolgorae --codex /absolute/pinned-codex --pool-limit 1
```

For the findings-merge extension, add `--with-merge` to that runner command.
It keeps the same fifty-task, at-most-fifty-POST and existing time/resource
bounds, then merges the complete retained coverage through the Controller public
path. It reads the real output artifact, replays the original merge key and
looks up its original receipt before cleanup. These reads and the merge add no
model Turn. The report records this extension separately from the original group
measurements. Protected external carrier and active managed Root behavior also
require their focused Rust cases and separately authorized native observations;
this Controller campaign alone does not qualify those paths.

Run a separate campaign with `--pool-limit 4`. The runner checks the native pin,
uses an isolated home and accountless loopback responses, preserves original
unknown coverage before official cleanup reconciliation, and retains failed
roots. The driver records its selected Profile binding and proves the initial
durable queues before starting the gateway-owned control-plane runtime. Its
startup scan then recovers the work committed before those wake hints had a
receiver. The driver holds normal Responses after the first POST so one pinned
SQLite read can prove executing and queued tasks together. Profile process
measurement uses the actual `ready` lifecycle.
Keep frozen executables until owner-service cleanup and process absence
are verified. This instrumented source campaign leaves TASK-087's packaged H3
qualification due.

If the original cleanup reconciliation failed, a separately authorized
`--recover-original-task ROOT MEMBER TASK` driver call attempts only that recorded
fault target's original history recovery. It verifies the fixture, accepted
group, current Controller and unchanged Turn, records one exclusive attempt,
and writes a separate result. It never resubmits work or overwrites the failed
cleanup record. A successful terminal result allows ordinary owner cleanup to
continue while retaining both records; an existing attempt prevents blind retry.

### Exact findings merge implementation

`delegation_service::research_merge` consumes one checked original source at a
time. Keep coverage separate from the accumulator, retain complete finding
content for exact equivalence, and budget encoded output as variants and source
links grow. Summing raw source lengths is not a merge-capacity check: large
equivalent sources can produce a small valid output. Persist actual result and
manifest references instead of copying every large CompleteResult identity.
Body-free identity-only sources retain their real ID, exact small identity and
explicit discovery-unavailable reason.

Keep the real Run quota lease before the publication transaction. The new merge
charge includes the complete retained output; existing captures, component
artifacts and previous merges remain cumulative. Competing original-key calls
must return the winning publication and owner without allocating a second
charge. Synthesis uses the ordinary task acceptance and caller accounting. Its
retained input binding and shared renderer must govern initial dispatch,
Worker verification and recovery. Public structured task bytes are distinct from
an execution projection and remain unchanged, including constraints.

Run `cargo test --lib task085`, the selected facade/consumer corpus checks and
historical generation-7 tests before the complete gate. Preserve failed receipts
and bind final focused evidence to the final candidate. Accountless fixture
checks establish these code paths; native runtime and consumer qualification
remain separate authorities.

The default integrated-native offline suite also runs the loss-witness guards in
`test_native_task085.py`. These checks start no native runtime.

The production-host harness also has a separate `--task085` mode. It leaves the
original eighteen scenarios and their budgets unchanged. Its three isolated
cases are protected Codex, protected Claude, and a real managed Codex Primary.
Each prepares one native findings Task, merges through the selected native tool,
reads the original key, then explicitly assigns and collects one native synthesis
Task. No Podway acceptance operation is part of this mode. The managed case
withholds the first merge value in the native code wrapper after the real Worker
call completes, then reads the caller-known original namespace and replays that
key. This is a discarded wrapper return, not a physical Worker crash. The Root
Worker, Thread and Profile identities must remain unchanged throughout.
The harness checks that merge preserves the accepted Task count and source Turn.
Explicit synthesis must add exactly one Task with its own Turn. Managed calls
must stay in the original Primary Turn.
Each case keeps a real fixture Control Plane and Gateway alive for Task dispatch.
Its owned process counts toward the existing sixteen-process bound. The parent
joins it during the same thirty-second owner cleanup, including preparation
failures; a failed shutdown prevents qualification. Cleanup releases successor
members through their owner before Root Close and Profile retirement. Even a
Closed member needs release verification. Logical cleanup and physical process
absence must both pass; a physical cleanup cannot clear a failed release.
After orphan cleanup, the public process inventory must have no candidates.
A healthy owned process also prevents passage; absence of orphans alone is
insufficient. This observation shares the existing orphan-cleanup deadline.
Protected Codex calls use the selected runtime's advertised Code Mode surface.
One bounded metadata-only call identifies the deferred MCP tool; each returned
value must match the actual correlated MCP reply. This discovery stays within
the existing HTTP limits and uses the selected release's pinned companion.

Each case permits at most twelve loopback POSTs, eight semantic tool calls,
thirty-two backend deliveries, four MiB per HTTP input, a 180-second scenario
plus thirty seconds for owner cleanup and thirty seconds for verified orphan
cleanup. Execution requires explicit runtime authorization, frozen matching
driver/product images and the existing
Codex/Claude pins. Use `run_native_production_host.py --task085` with the existing
binary, driver, native executable, model and scratch arguments; Podway arguments
are unnecessary for this mode. The original fifty-task campaign is separate.
