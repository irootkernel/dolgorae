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
- `make test-unit` runs Rust library and binary unit tests.
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

Put product semantics in Rust and its tests. Keep Python limited to small
independent repository checks or black-box executable tests. Any future E2E
database or external service must use test-only credentials and namespaces;
production state and the user's real home must never be reused.

Do not commit virtual environments, caches, generated review material, local
workflow state, credentials, or run/session identifiers.

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

The live v3 campaign uses the required defaults, the defective Hello World
fixture and Codex `0.157.1`. Run it only
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
`src/live_transport.rs` and
`docs/protocol/dolgorae-live-transport-selection-v1.json`. The hidden
`__live-transport-mcp` entry is test-only and must not edit a shared Profile.
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
