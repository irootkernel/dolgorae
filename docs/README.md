# Dolgorae Documentation Authority Map

Status: Normative documentation-governance index.

This file is the entry point for Dolgorae's source of truth. It owns document
roles, authority boundaries, and the synchronization procedure. It does not
restate product behavior, architecture, delivery status, or evidence results.

## Authority Model

Dolgorae uses one owner per kind of fact. References and summaries in other
files are derived views and MUST NOT silently redefine their owner.

| Kind of fact | Sole owner | Derived or supporting material |
| --- | --- | --- |
| Externally observable product behavior and semantic requirements | [specs.md](specs.md) | Architecture, schemas, roadmap acceptance text, probes |
| Component boundaries, state ownership, process topology, and technical invariants | [architecture.md](architecture.md) | ADR consequences, implementation plans, diagrams |
| Accepted choices, rationale, and rejected alternatives | [architecture-decisions.md](architecture-decisions.md) | Historical review and disposition records |
| Delivery order, task state, task scope, and completion gates | [roadmap.md](roadmap.md) | Implementation-note summaries and review closure statements |
| Serialized machine, Protobuf/RPC, event, timeline, credential, artifact, and persisted-state shapes | [`protocol/`](protocol/) | Examples, generated snapshots, probe fixtures |
| Portable workspace policy, machine-local Runtime Profile registry, and workspace identity record shapes | [`dolgorae-portable-workspace-policy-v1.schema.json`](protocol/dolgorae-portable-workspace-policy-v1.schema.json), [`dolgorae-local-profile-registry-v1.schema.json`](protocol/dolgorae-local-profile-registry-v1.schema.json), and [`dolgorae-workspace-record-v1.schema.json`](protocol/dolgorae-workspace-record-v1.schema.json) | SPEC-002, SPEC-003, Workspace Identity and Local Layout, ADR-025, TASK-002 |
| Immutable Run manifest and canonical audit-record shapes and cross-field validity | [`dolgorae-run-manifest-v1.schema.json`](protocol/dolgorae-run-manifest-v1.schema.json), [`dolgorae-audit-record-v1.schema.json`](protocol/dolgorae-audit-record-v1.schema.json), and [`validate_task003a_records.py`](../tools/validators/validate_task003a_records.py) | SPEC-004, SPEC-010, Manifest and Ledger Model, ADR-007, TASK-003-A |
| Disposable Run ledger projection, append-time client events, and observer delivery validity | [`dolgorae-ledger-state-v1.schema.json`](protocol/dolgorae-ledger-state-v1.schema.json), [`dolgorae-event-record-v1.schema.json`](protocol/dolgorae-event-record-v1.schema.json), [`dolgorae-event-delivery-v1.schema.json`](protocol/dolgorae-event-delivery-v1.schema.json), and [`validate_task003b_ledger.py`](../tools/validators/validate_task003b_ledger.py) | SPEC-006, SPEC-010, Audit Ledger, Materialized State, TASK-003-B |
| Cross-field SOT, CLI/machine, comprehensive gRPC semantics, staged-method, run-state, and projection validity | archive-root [`tools/validators/`](../tools/validators/), checked [`dolgorae-grpc-validation-coverage-v1.json`](protocol/dolgorae-grpc-validation-coverage-v1.json), and checked [`protocol/validators/`](protocol/validators/) | JSON Schemas, fixtures, and full-repository validators |
| Requirement-to-verification ownership | [verification-index-v1.json](protocol/verification-index-v1.json) | Roadmap verification prose and probe reports |
| Completed-task implementation and verification evidence | [implementation-notes.md](implementation-notes.md) | Probe results and review closure artifacts |
| Derived implementation design for the staged public local RPC gateway | [local-grpc-implementation-memo.md](local-grpc-implementation-memo.md) | SPEC-015, architecture, ADR-021, ADR-030, TASK-009-D1A, TASK-010-A, checked Protobuf artifacts |
| Derived product-use-case and topology terminology guide | [agent-topology-terminology.md](agent-topology-terminology.md) | Canonical definitions in SPEC, structural mappings in architecture, rationale in ADR-023 |
| Derived External Read-Only Specialist Review Preview design | [specialist-review-preview.md](specialist-review-preview.md) | SPEC-012 one-shot adapter, architecture Review Coordinator, ADR-029, EPIC-002A, checked review tool schema |
| Derived Brokered Specialist Collaboration implementation design | [brokered-specialist-collaboration.md](brokered-specialist-collaboration.md) | SPEC-012, architecture Collaboration Plane, ADR-027, TASK-009-E0, TASK-009-E2, checked private protocol and orchestration-state artifacts |
| Durable orchestration, mailbox, activation, collaboration, and engagement state shapes and cross-object validity | [`dolgorae-orchestration-state-v1.schema.json`](protocol/dolgorae-orchestration-state-v1.schema.json), [`validate_orchestration_state_v1.py`](protocol/validators/validate_orchestration_state_v1.py) | SPEC-012, architecture store model, ADR-023, ADR-027, positive and negative fixtures |
| Controller credential and explicit Orchestration Launch Intent shape and semantic validity | [`dolgorae-controller-credential-v1.schema.json`](protocol/dolgorae-controller-credential-v1.schema.json), [`validate_controller_credential_v1.py`](protocol/validators/validate_controller_credential_v1.py) | SPEC-005, SPEC-012, Use-Case Compiler, ADR-028, TASK-000-H |
| Private run-bound Primary orchestration tool payload shape | [`dolgorae-orchestration-tool-v1.schema.json`](protocol/dolgorae-orchestration-tool-v1.schema.json) | SPEC-012, Primary Orchestration Service, ADR-028, TASK-009-D2, TASK-009-E0, TASK-009-E1 |
| Private External Specialist Engagement facade payload shape and aggregate-owner Controller authorization contract | [`dolgorae-external-specialist-facade-v1.schema.json`](protocol/dolgorae-external-specialist-facade-v1.schema.json), [`dolgorae-controller-credential-v1.schema.json`](protocol/dolgorae-controller-credential-v1.schema.json), [`validate_private_tool_examples_v1.py`](protocol/validators/validate_private_tool_examples_v1.py) | SPEC-005, SPEC-012, External Specialist Facade, ADR-028, TASK-006-C, TASK-009-D1 |
| One-shot Specialist Review CLI, external MCP payload, and per-request identity shape | [`dolgorae-specialist-review-tool-v1.schema.json`](protocol/dolgorae-specialist-review-tool-v1.schema.json), [`dolgorae-specialist-review-mcp-meta-v1.schema.json`](protocol/dolgorae-specialist-review-mcp-meta-v1.schema.json), [`dolgorae-machine-v1.schema.json`](protocol/dolgorae-machine-v1.schema.json), [`validate_private_tool_examples_v1.py`](protocol/validators/validate_private_tool_examples_v1.py) | SPEC-012 one-shot adapter, Review Coordinator, ADR-029, ADR-031, TASK-006-D, TASK-006-E0, TASK-006-E1, TASK-006-F |
| Immutable Specialist Policy snapshot shape and semantic validity | [`dolgorae-specialist-policy-v1.schema.json`](protocol/dolgorae-specialist-policy-v1.schema.json), [`validate_specialist_policy_v1.py`](protocol/validators/validate_specialist_policy_v1.py) | SPEC-012, Orchestration Broker, ADR-028, TASK-009-D2 |
| Immutable Agent Configuration shape | [`dolgorae-agent-configuration-v1.schema.json`](protocol/dolgorae-agent-configuration-v1.schema.json), [`validate_agent_configuration_v1.py`](protocol/validators/validate_agent_configuration_v1.py) | SPEC-003, Specialist Policy, Run manifest |
| Private run-bound collaboration tool payload shape | [`dolgorae-collaboration-tool-v1.schema.json`](protocol/dolgorae-collaboration-tool-v1.schema.json) | SPEC-012, architecture Collaboration Plane, ADR-027, TASK-009-E0, TASK-009-E2 |
| Development-team implementation handoff and start gate | [development-handoff.md](development-handoff.md) | Roadmap, SOT, schemas, validators, and review state |
| Deferred, explicitly non-blocking review findings | [deferred-feedback.md](deferred-feedback.md) | Review dispositions |
| Uncommitted future ideas | [todo.md](todo.md) | None; candidates grant no implementation authority |
| Preserved review chronology and historical findings | [reviews/README.md](reviews/README.md) | Review inputs, dispositions, closure reports, packages |
| Historical requirement-input decisions | [external-runtime-disposition.md](external-runtime-disposition.md), [singleton-correction-disposition.md](singleton-correction-disposition.md) | Active SPEC, architecture, ADR, protocol, and roadmap owners |

There is no precedence rule that makes a contradiction acceptable. If two
active owners disagree, the repository is invalid and affected implementation
work MUST stop until the owners and every checked derivative are reconciled.
Historical evidence may describe an older contract only when it is explicitly
marked historical or superseded.

## Contract and Evidence Boundary

The active contract consists of:

- [specs.md](specs.md), [architecture.md](architecture.md), and accepted ADRs in
  [architecture-decisions.md](architecture-decisions.md);
- the current checked artifacts under [`protocol/`](protocol/);
- the executable archive-local validators under [`../tools/validators/`](../tools/validators/),
  the reproducible entry point [`../tools/validate_sot.sh`](../tools/validate_sot.sh),
  checked validators under [`protocol/validators/`](protocol/validators/), and
  any stricter repository-root validators in the full source tree; and
- task status and acceptance ownership in [roadmap.md](roadmap.md).

The following are not independent contract authorities:

- `prompt.md`, which is user-owned, ignored input whose accepted effects must be
  promoted into the active contract;
- `docs/reviews/`, which preserves what reviewers found and how it was closed;
- historical disposition documents, which record how an input was promoted at
  one checkpoint without owning the resulting contract;
- `docs/probes/`, `docs/probes/results/`, and closure packages, which prove only
  the bounded observations they record;
- `implementation-notes.md`, which records completed evidence without creating
  requirements;
- `local-grpc-implementation-memo.md`, which is a decision-complete handoff but
  cannot override SPEC, architecture, ADR, protocol, or roadmap owners;
- `agent-topology-terminology.md`, which is a derived communication guide and
  cannot override the two canonical use cases, aggregate ownership, or internal
  Run terminology owned by the active SOT;
- `specialist-review-preview.md`, which is a decision-complete derived
  implementation memo for the first usable milestone but cannot override SPEC,
  architecture, ADR, protocol, or roadmap owners;
- `brokered-specialist-collaboration.md`, which is a decision-complete derived
  implementation memo but cannot override SPEC, architecture, ADR, protocol, or
  roadmap owners; and
- `todo.md`, whose candidates are neither scheduled nor approved.

A measured upstream behavior becomes a Dolgorae dependency only after the
required subset manifest, product contract, architecture, verification index,
and owning roadmap task agree. A review closure proves the reviewed snapshot;
it does not permanently certify later changes.

## Reading Order

Before implementing a task:

1. Read this authority map.
2. Read the task and every prerequisite in [roadmap.md](roadmap.md).
3. Read the SPEC entries named by that task.
4. Read the corresponding architecture sections and accepted ADRs.
5. Read the checked protocol artifacts and semantic validators the task must
   implement.
6. For work involving the Gul-facing control plane, external AI integrations,
   Primary Runs, Specialists, Brokered Hierarchies, or native subagents, read
   [agent-topology-terminology.md](agent-topology-terminology.md).
7. For `EPIC-002A`, the one-shot review command, or the external Codex CLI MCP
   adapter, read [specialist-review-preview.md](specialist-review-preview.md).
8. For Brokered Specialist Collaboration, mailbox scheduling, passivation, or
   activation, read
   [brokered-specialist-collaboration.md](brokered-specialist-collaboration.md).
9. For a new implementation handoff, read
   [development-handoff.md](development-handoff.md) and obey its start gate.
10. Read the linked verification-index entries and historical evidence only as
    needed to understand the acceptance boundary.

Do not begin a later task because a review or implementation note sounds
complete. Only the task's status in `roadmap.md` authorizes its position in the
sequence.

## Change Synchronization Gate

A change that affects behavior or architecture is complete only when all
applicable steps below are satisfied in the same change set:

1. Name the owning SPEC, architecture section, ADR, roadmap task, protocol
   artifact, and verification entry before editing.
2. Change the sole owner first, then update every derived description and
   checked artifact. Do not copy a new normative rule into an evidence file as
   a substitute for changing its owner.
3. Preserve historical review inputs. Add a disposition or explicit
   supersession marker instead of rewriting what a reviewer originally saw.
4. Update protocol schemas and executable semantic validators together when a
   persisted or projected invariant changes. Schema-only validation is not
   sufficient for cross-field rules.
5. Update `verification-index-v1.json` and the owning roadmap verification text
   when a requirement, error, command, or compatibility dependency changes.
6. Create the validation environment with `python3 -m venv .venv`, install
   [`../tools/validation/requirements.txt`](../tools/validation/requirements.txt),
   run `PYTHON_BIN=.venv/bin/python tools/validate_sot.sh` from the package root,
   then run Markdown link checks, full-repository consistency scans, and
   `git diff --check`. Run the task's bounded live probes when it depends on OS
   or upstream Codex behavior.
7. Obtain an independent read-only review for the final snapshot under the
   completion gate in [roadmap.md](roadmap.md).
8. Record completed evidence in [implementation-notes.md](implementation-notes.md)
   and index its review chain in [reviews/README.md](reviews/README.md).

Git commits and pushes remain separate user-authorized actions; documentation
completion never grants either permission.

## Terminology and Normative Language

Canonical terms are defined in [specs.md](specs.md). The derived
[use-case and topology terminology guide](agent-topology-terminology.md)
provides the approved communication map without owning behavior. New synonyms SHOULD NOT be
introduced in architecture, schemas, or task text. In normative Markdown, only
uppercase **MUST**, **MUST NOT**, **SHOULD**, and **MAY** carry requirement
force. The product specification owns the normative status of checked schemas
and executable semantic validators.

When an active document mentions an obsolete topology or status for rationale,
the sentence MUST identify it as historical or superseded at the point of use.
