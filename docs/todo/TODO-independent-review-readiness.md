# Independent review readiness

Adopted into [EPIC-016](../roadmap/README.md#epic-016-reliable-independent-specialist-review).
The roadmap owns IDs, execution order, status, and completion gates. This
temporary dossier holds the verification detail until Epic closeout.
Product behavior is owned by
[SPEC-012](../specs/README.md#independent-review-readiness-planned), component
ownership by the [architecture](../architecture/README.md#one-shot-specialist-review-coordinator),
and the adoption rationale by
[ADR-040](../architecture-decision-records/README.md#adr-040-qualify-structured-independent-review-through-a-corrective-epic).

## Source boundary at adoption

Inspection at commit `2885a33` established the following gaps. These are source
findings; they do not establish the outcome of a new live review.

| Concern | Source observation | Delivery owner |
| --- | --- | --- |
| Output contract | `scoped_review_prompt_v3` in `src/review.rs` lists top-level keys and refers to the v2 finding shape; `SpecialistTaskRequest::prompt` supplies accepted task data without the full nested contract. | TASK-057 |
| Diagnostics | `validate_reviewer_output_v3` in `src/specialist.rs` discards deserialization details. | TASK-057 |
| Diagnostic redelivery | `finish_completed_output` in `src/external_engagement.rs` stores only the failure code; later `task_values` projections return `safe_error_code` without the initial error details. | TASK-057 |
| Defaults | `prepare_reviewer` and `reviewer_effort` in `src/semantic.rs` fall back to the server model and first advertised effort. | TASK-057 |
| Temporary servers | Review preparation ensures a shared Profile Server; the coordinator does not own an explicit temporary-server retirement mode. | TASK-058 |
| One-shot recovery | `execute_cli` creates its request reference internally, `EphemeralCarriers::drop` deletes authority on normal/error return, and facade owner checks reject `legacy_one_shot` engagements. A lost process and response cannot be recovered by assuming ordinary external-facade access. | TASK-058 |
| Preparation failures | `prepare_reviewer` ensures the server before model/effort and Reviewer validation; credential, engagement, and capture creation can also fail before Reviewer allocation. | TASK-058 |
| Skill guidance and packaging | The source skill refers to protocol resources outside its package. The README installer copies Markdown only, and the validator checks source structure rather than installed resource resolution. Host termination is not clearly separated from transport wait expiry. | TASK-059 |
| Live v3 acceptance | The existing review drivers exercise earlier result contracts and expect `gpt-5.6-luna` with `low`; they do not prove criterion-complete v3 review with the required defaults. | TASK-060 |

Later Brokered Session cancellation and close fixes remain part of the baseline.
Codex `0.157.1` is the supported minimum, with the checked `0.157.0` schema
subset. Older campaign versions are historical evidence, not acceptance inputs
for this Epic. Existing completed Epic and Task records remain intact.

## TASK-057 verification detail

The TASK-057 implementation uses one expanded checked verdict schema in both
production prompts and the structural validator. It adds
`dolgorae-review-output-diagnostic/v1` and orchestration SQLite schema v4;
TASK-058 must project the stored one-shot diagnostic without rebuilding it.
Failure receipts in the reusable facade keep the task failed and carry no
artifact. TASK-059 must include the diagnostic schema in its installed resource
closure. TASK-060 still owns the live defect-detection campaign: the checked
selected protocol has no native structured-output carrier, so this change uses
the complete schema in the existing prompt transport.

The deterministic attack budget covers deletion of every required output field,
including nullable members at each nesting level; malformed, duplicate-key,
fenced, oversized, unknown-field, type, enum, and semantic violations; and
failure injection immediately before and after each path's terminal commit.
Each restart reuses the original task and must not dispatch a repair Turn.
Historical v1 and v3 stores exercise migration, while fresh CLI processes
exercise diagnostic retrieval and receipt replay through the public facade.
This Task introduces no new native Codex protocol field or OS process primitive.

Apply fixed defaults only to omitted one-shot Reviewer settings. Check model
and effort independently: neither missing setting may inherit server ordering,
and an unavailable explicit selection must fail without substitution. Reusable
Specialist configurations remain explicit. Bind known resolved values to both
success and failure evidence without inventing metadata for pre-launch errors.

Both production paths must deliver the canonical v3 output contract, including
finding fields, assessment/evidence shapes, enums, required nullable members,
and semantic restrictions. Prefer native structured output where the selected
protocol supports the contract; otherwise render the complete skeleton and
constraints from the checked authority. Test parity so the prompt, validator,
and schema cannot drift independently.

Exercise missing required fields, extra fields, wrong types, invalid enums,
non-JSON text, Markdown fences, oversize, criterion order/identity, evidence
references, and inconsistent overall assessments. Valid reports still support
all four criterion statuses and exact artifact redelivery. Error evidence must
identify the failure path/category and output digest while bounding and
sanitizing key metadata. Canary tests must prevent arbitrary provider strings
or credentials from appearing in diagnostics. No invalid response may trigger
another Turn automatically.

Store only the sanitized bounded diagnostic, bound to the original task and
known execution identity, consistently with its terminal failure. Publish the
first validation error only after that diagnostic is durable. Later authorized
await and collect must project the same path, category, digest, and known execution facts
without regenerating them from model output. TASK-057 owns storage in both
review paths; TASK-058 supplies public access to the one-shot record.

Test malformed output followed by loss of the first error response, process
restart, and diagnostic retrieval. Exercise crashes before and after the
terminal/diagnostic commit. Assert that every new invalid-output failure retains
its diagnostic and no additional Turn runs. Secret canaries and arbitrary model
strings must appear in neither stored nor returned diagnostics. Test old v1/v2 records without this
payload: preserve their error code and mark diagnostic detail unavailable
without fabricating it. Freeze the persisted format/migration, compatibility
projections, and absence semantics with the checked error contracts.

TASK-057 owns the affected private error schema, machine envelope, examples,
and success/failure metadata compatibility. It must freeze those shapes before
runtime edits and verify v1/v2 shapes, historical records, and public-v1
descriptor preservation. Any new native structured-output assumption needs a
separately authorized empirical check in this Task; the full defect-detection
campaign remains with TASK-060.

## TASK-058 verification detail

The implemented public carrier pairs a caller-retained UUIDv7 reference with a
private recovery Controller. `specialist review-inspect` reads the original
receipt and relational state in one database transaction; `review-recover
--action cleanup` requires that Controller and the preserved object capabilities.
The checked observation distinguishes reserved capture identity, publication,
settlement and pending source cleanup. TASK-059 must package these commands and
schemas; TASK-060 must exercise this carrier against its exact live candidate.

The deterministic attack budget covers six preparation failures (model, effort,
Reviewer policy, credential creation, engagement insertion and capture creation)
on both a new owned server and a pre-existing shared server. It also covers
response loss, malformed output, timeout, SIGINT, SIGKILL, reference/input drift,
wrong recovery authority, immutable receipt tampering, interrupted publication
and settlement, generation replacement, membership admission, and uncertain
process identity. Native checks exercise actual process identity and lock
exclusion; fault fixtures stay in isolated homes. These checks do not establish
live-provider compatibility, which remains TASK-060 work.

Define the public one-shot lookup and recovery contract before runtime edits.
The caller must retain a stable reference before server, capture, or Reviewer
effects; a reference delivered only by an initial or final response that can
be lost is insufficient. Bind it durably to the original request and to each
engagement, Reviewer, capture, and server generation as those objects become
known. Keep absent identities absent. Specify inspection authority separately
from the preserved or explicitly reauthorized capabilities for mutation.
Knowing the lookup reference must not confer cleanup or settlement permission.

The current external facade accepts `external_v1` owners, while one-shot
engagements use `legacy_one_shot` and ephemeral carriers. Implement the missing
public boundary over existing stores and lifecycle services without treating
those contracts as interchangeable or adding a general job framework. Preserve
read-only lookup: it cannot start a new review, replay a Turn, settle a capture,
or stop a server. State ambiguity and missing authority must produce a checked
unknown or recovery-blocked result. Mutation commands must require the defined
authority and current lifecycle preconditions.

Discard all responses from an invocation and terminate its CLI. Start a new
process with the reference retained before launch and use only public commands
to inspect the original operation, including any TASK-057 diagnostic. Verify a known
terminal state, an uncertain state, and denied mutation without authority.
Exercise authorized recovery where its preconditions hold, and a checked block
where they do not. Private database reads and replacement review/Turn creation
cannot count as recovery success. Read-only observation must remain usable
without silently acquiring mutation authority.

The explicit mode may retire only a Profile Server generation created
exclusively for that invocation. A server that already existed has no cleanup
authority derived from this mode. Record durable launch intent before server
creation and bind cleanup responsibility to the verified generation created.
Cover interruption during creation, before member registration, and before a
`PreparedReviewer` is returned. A process-local guard cannot provide recovery
after forced termination. Revalidate generation, native process identity, and
quiescent membership through the Profile lifecycle service before shutdown.

Inject unsupported model/effort, Reviewer policy rejection, credential creation
failure, engagement creation failure, and capture creation failure. Repeat
each case with a newly created server and a pre-existing shared server. New
owned generations must reach verified cleanup when safe, or an explicit
recoverable/blocked state when evidence is insufficient; existing servers must
survive. Before Reviewer allocation, retain known server key/generation without
inventing a Reviewer ID. Recovery must resolve the obligations of each object
already created before deciding whether server shutdown is safe.

Run deterministic failure injection and native process checks for normal
completion, invalid output, timeout, and caller interruption. Cover concurrent
member admission, server replacement, lost responses, ownership ambiguity,
and interrupted cleanup. Verify the existing shared server and unrelated
processes remain alive. Preserve terminal uncertainty for supported inspection
instead of claiming cleanup or retrying an unknown mutation.

The Task must state its adversarial attack budget and cover each new process
assumption before completion. Keep Reviewer closure, engagement closure,
capture settlement, and server shutdown as separate checked facts. Success
requires terminal evidence for the requested cleanup; ordinary review keeps
the existing shared-server lifetime. Freeze public lookup/recovery commands,
the opt-in carrier, authority persistence or reauthorization, and checked
outcomes before implementation. Broad idle-server cleanup and the general
managed-session cleanup owned by TASK-031 remain outside this Task.

## TASK-059 verification detail

Package the schemas and examples that the skill tells a host to read, including
their transitive local references. Derive copies from canonical protocol files
and check byte equality so the skill never becomes a competing schema owner.
Update the source guidance, README installation procedure, and validator
together. Installation must resolve resources from a temporary package root
without repository-relative or network fallback.

The source package derives `resources/protocol/` and `resources/manifest.json`
through `tools/validators/package_agent_skill.py sync`. Its installer validates the exact
source inventory and canonical bytes before creating a destination. The
`validate-agent-skills` Make target exercises a temporary installation and the
missing, stale, extra-resource, and existing-destination refusals. The default
E2E gate uses `test_installed_review_skill.py` to run native observation waits
and the public recovery and partial-cleanup matrices with installed schemas.

The adversarial budget covers missing transitive dependencies with and without
their inventory entry, stale bytes with and without a matching inventory digest,
extra resources, and existing directories or symlinks. Native process checks
cover an observation wait expiring without an interrupt or second Turn, then
reuse the interruption, response-loss, and partial-cleanup matrices. These
checks establish the new installer and host-wait assumptions; the live provider
assumptions remain TASK-060's responsibility.

Validate representative v2 hire and v3 assignment/review examples through the
installed resources. Exercise a missing dependency and a stale copied schema
as failures. Source structure and Markdown checks remain useful, but neither
proves that a separately installed package works.

Guide hosts to retain the original native process/result handle during waits
and collect its authoritative exit and complete envelope. Explain that a host
timeout that terminates the CLI can cancel the provider operation or lose its
outcome. Use the production CLI with a controlled fake provider and native
process supervision to verify observation-only waiting, explicit interruption,
and response loss. When the original handle is lost, follow the public commands
and authority contract implemented by TASK-058 from a new process. Document
known, unknown, and blocked outcomes and the limits of automatic recovery.
Do not substitute external-facade calls that reject one-shot authority or use
private storage inspection. Unknown acceptance or completion must not launch a
second review. Preserve the separate authorization needed for cancellation,
settlement, and server lifecycle actions.

## TASK-060 live acceptance detail

Use an exact candidate build and the corresponding skill package in isolated
test state. Live account use requires explicit authorization; record each
selected Codex executable version and immutable candidate identity in local
campaign evidence. Run the minimum `0.157.1`. Newer versions may remain
`unverified`; that metadata does not change the product's admission rules or
turn this narrow campaign into full compatibility qualification.

Commit a one-line program in an isolated fixture that prints `Hello world!`.
Submit a v3 completion request whose sole criterion `C-1` requires it to print
`Hello, world!`. Leave the Profile model and effort unset so the campaign proves
the default resolution rather than reproducing it through explicit overrides.

A passing campaign must establish:

- The checked v3 report assesses `C-1` as `unmet`, cites the defective candidate
  source line, and returns `overall_assessment: requirements_not_met`.
- The resolved model is `gpt-6-sol` and effort is `high`; the immutable
  result is collected without a repair or replacement review Turn.
- Source and Git fingerprints are unchanged, the Reviewer and engagement are
  closed, and the capture is settled.
- The opted-in temporary server is verifiably retired and an independently
  pre-existing shared server remains unaffected.
- The installed package resolves its contract resources without the checkout.

The integrated failure campaign must lose the first response and terminate the
original CLI, then use the installed workflow from a fresh process to inspect
the same review through TASK-058's public interface. Use the exact candidate
build with a controlled fake provider to produce malformed output, verify
TASK-057 diagnostic persistence and redelivery, and exercise unknown/blocked
states and authorized recovery without a new review or Turn. Rerun the
preparation-failure matrix with new and pre-existing servers. Include the
process/response-loss lookup scenario in the separately authorized live
campaign; deterministic checks cannot substitute for new external assumptions.

Run the complete `make PYTHON_BIN=.venv/bin/python test` gate and obtain an
independent read-only review through an already qualified available backend.
Resolve all blocking findings before the Task or Epic completes. Absence of
live authorization or missing terminal evidence leaves acceptance outstanding;
deterministic success cannot replace it.

## Adoption and closeout

This adoption changes documentation only. TASK-057 and TASK-058 own future
checked interface changes; TASK-059 owns source-skill packaging and installer
changes. Runtime availability, installed copies, and external publication are
verified separately from roadmap completion. The roadmap assigns this Epic to
v0.1.3, after completed EPIC-008 and before EPIC-009. All four Tasks and Epic
acceptance must pass before v0.1.3 release-candidate QA; EPIC-009 remains
planned for v0.1.4.

Keep raw reports, logs, credentials, runtime IDs, and machine-specific paths
out of tracked documentation and commits. Retain runtime evidence locally;
use an approved bounded promoted-evidence package only when a downstream
consumer requires durable evidence under repository policy.

At closeout, move durable outcomes to the specification, architecture, checked
protocol, operations, implementation guidance, and source skill as appropriate.
Remove this dossier and its TODO index entry, then replace the roadmap's
`Detailed SOT` link with `Canonical Outcomes`. Delivery status changes and
task-scoped commits follow the roadmap completion gate and their explicit
authorization.
