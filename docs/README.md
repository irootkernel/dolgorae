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
| Serialized machine, event, credential, artifact, and persisted-state shapes | [`protocol/`](protocol/) | Examples, generated snapshots, probe fixtures |
| Cross-field run-state and projection validity | [`tools/validators/`](../tools/validators/) | JSON Schemas and validator fixtures |
| Requirement-to-verification ownership | [verification-index-v1.json](protocol/verification-index-v1.json) | Roadmap verification prose and probe reports |
| Completed-task implementation and verification evidence | [implementation-notes.md](implementation-notes.md) | Probe results and review closure artifacts |
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
- the executable semantic validators under
  [`tools/validators/`](../tools/validators/); and
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
  requirements; and
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
6. Read the linked verification-index entries and historical evidence only as
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
6. Run JSON parsing, schema/semantic validation, Markdown link checks,
   repository consistency scans, and `git diff --check`. Run the task's bounded
   live probes when it depends on OS or upstream Codex behavior.
7. Obtain an independent read-only review for the final snapshot under the
   completion gate in [roadmap.md](roadmap.md).
8. Record completed evidence in [implementation-notes.md](implementation-notes.md)
   and index its review chain in [reviews/README.md](reviews/README.md).

Git commits and pushes remain separate user-authorized actions; documentation
completion never grants either permission.

## Terminology and Normative Language

Canonical terms are defined in [specs.md](specs.md). New synonyms SHOULD NOT be
introduced in architecture, schemas, or task text. In normative Markdown, only
uppercase **MUST**, **MUST NOT**, **SHOULD**, and **MAY** carry requirement
force. The product specification owns the normative status of checked schemas
and executable semantic validators.

When an active document mentions an obsolete topology or status for rationale,
the sentence MUST identify it as historical or superseded at the point of use.
