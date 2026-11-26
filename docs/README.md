# Dolgorae Documentation

Profile: `single-scope`. Dolgorae has one delivery scope and one canonical
roadmap. The root [README](../README.md) is the user-facing product entrypoint;
this directory is for maintainers and contributors who change product
contracts, implementation, validation, or local operations.

## Authority map

| Role | Canonical owner |
| --- | --- |
| Product specifications | [specs/README.md](specs/README.md) |
| Architecture | [architecture/README.md](architecture/README.md) |
| Architecture decision records | [architecture-decision-records/README.md](architecture-decision-records/README.md) |
| Implementation tips | [implementation-tips/README.md](implementation-tips/README.md) |
| Operations | [ops/README.md](ops/README.md) |
| Roadmap | [roadmap/README.md](roadmap/README.md) |
| TODO candidates and adopted dossiers | [todo/README.md](todo/README.md) |
| Deferred feedback | [deferred-feedback/README.md](deferred-feedback/README.md) |

The checked [protocol](protocol/) artifacts derive wire, persisted-state, and
machine-output contracts from the product specification. The
[v0.1.4 consumer acceptance specification](specs/dolgorae-consumer-acceptance-v014.md)
owns mandatory Aquarium, Podway and Gul requirements-to-evidence mappings;
the [current consumer qualification](specs/dolgorae-v014-current-consumer-qualification.md)
records the later ordinary-v3 consumer applicability without changing frozen H1;
the [alignment dossier](todo/TODO-DOLGORAE-V014-RELEASE-ALIGNMENT.md) supplies
the five-phase whole-Epic execution graph and four normal handoffs. The
[release-proof correction dossier](todo/TODO-DOLGORAE-RELEASE-PROOF-CORRECTION.md)
defines EPIC-023's bounded corrected-H1/delta-H2/Gate-A exchange before
EPIC-020 resumes. These planning documents establish no
implemented capabilities or runtime acceptance. The [Gate A acceptance](specs/dolgorae-v014-gate-a-acceptance.md) records the later
independent H2 contract/foundation acceptance and preserves its runtime limits. The
[Gate C and Gate D acceptance](specs/dolgorae-v014-gate-c-d-acceptance.md) records
historical `EPIC-022` provenance (`8fb6a3578ae3` / `b9d1750e9ec5`) and superseding
`EPIC-024` (`TASK-098`) independent three-consumer qualification (`MILESTONE-BH1`),
H3/H4 provenance (`3d3355e8481e` / `9175b40a0097`), and release-handoff limits
without mutating frozen H1. The
[review-target strategy analysis](architecture-decision-records/review-strategy-analysis.md)
is a supporting accepted design study, not runtime or completion evidence. The
root [contribution guide](../CONTRIBUTING.md) is an entrypoint to implementation
guidance, and the public [changelog](../CHANGELOG.md) owns release history.
Optional source-distributed agent guidance lives under [`skills/`](../skills/).
It consumes these authorities and current runtime capability output; it does
not redefine product behavior, grant operational authorization, or install with
the binary.

The current public-v2 Start reconciliation extension adds an explicitly
negotiated recovery method and a terminal key-closure result. Its persisted
[closure and lookup schema](protocol/dolgorae-start-operation-closure-v1.schema.json)
retains no fabricated input digest. This extends the current runtime descriptor
and bindings; the immutable historical H1 archive and its lock remain unchanged.

The ledger retains the frozen [audit v1](protocol/dolgorae-audit-record-v1.schema.json)
reader and adds [audit v2](protocol/dolgorae-audit-record-v2.schema.json) for
hash-bound historical client projections and mutation admission/completion.
[Stamped events](protocol/dolgorae-stamped-client-event-v1.schema.json) bind the
existing client event record to its append-time aggregate revisions. The
[ledger conformance registry](protocol/dolgorae-ledger-conformance-v1.json)
identifies each accepted record version and its kind extensions; these persisted
contracts do not alter the frozen public-v1 Protobuf or descriptor. Current
permission/MCP requests use [interaction v2](protocol/dolgorae-interaction-v2.schema.json)
and [protected decisions v2](protocol/dolgorae-interaction-decision-v2.schema.json).
[Public resolution v2](protocol/dolgorae-public-resolution-v2.schema.json) owns
the private actor intent and atomic durable lookup receipt.
Their opened-event vocabulary uses [event v2](protocol/dolgorae-event-record-v2.schema.json),
[stamps v2](protocol/dolgorae-stamped-client-event-v2.schema.json), and
[audit v3](protocol/dolgorae-audit-record-v3.schema.json), alongside retained
readers. [Machine v6](protocol/dolgorae-machine-v6.schema.json) carries the new
observer/detail output without changing earlier Machine envelopes.

## Precedence and synchronization

Each role owner is authoritative only for its stated domain. Specifications own
observable behavior, architecture owns structure and invariants, decision
records own accepted rationale, and the roadmap alone owns delivery identity,
order, lifecycle vocabulary, and status. Protocol artifacts, implementation,
tests, and summaries are derived where they overlap those authorities.

The checked [verification index](protocol/verification-index-v1.json) maps
Tasks and verification concerns at SPEC-section granularity. A Task listed for
a section does not own every requirement or every other Task's work in that
section, and a mapping is not executed evidence. The roadmap defines each
Task's scope and completion gate; its adopted dossier supplies detailed checks.

If canonical documents disagree, stop and resolve the contradiction in the
owning documents before changing implementation. Update affected protocol
artifacts, implementation, tests, and roadmap references only after the owners
agree.

## Roadmap identity and dossier lifecycle

The canonical roadmap namespace is `docs/roadmap/README.md`. This path migration
does not change established Epic or Task identifiers, ordering, or uppercase
lifecycle vocabulary; the roadmap's identity rules remain authoritative.
Cross-scope qualification is unnecessary while Dolgorae has one delivery scope.

Future epic-sized ideas live in the TODO owner without roadmap status. When an
idea is adopted into the roadmap as an Epic with tasks, its temporary dossier
is listed as adopted TODO work and linked from the roadmap with `Detailed SOT`.
Adoption does not set the roadmap state to `ACTIVE`: a `PLANNED` Epic may have
an adopted dossier. Keep that dossier through execution and review until Epic
closeout, which promotes durable outcomes to their owners, removes the dossier,
and replaces that link with `Canonical Outcomes`. Historical completed work
without these lifecycle fields remains unchanged.

When several Epics share an adopted dossier, each closeout promotes its own
outcomes and replaces only its own reference. Preserve the dossier until the
final referencing Epic completes and all references are settled. The v0.1.4
alignment dossier follows this shared lifecycle.

## Language and validation

Canonical repository documentation and source-distributed skills are written in
English. Run the non-writing Markdown and skill checks with:

```sh
.venv/bin/python tools/validators/validate_markdown.py
.venv/bin/python tools/validators/validate_agent_skills.py
```

Rust tests own product semantics. Python remains limited to small independent
repository checks and black-box executable tests. The complete gate and its
formatting side effect are documented in the
[implementation tips](implementation-tips/README.md).
That guide also describes immutable H1 validation, protoc and generated-source
reproduction, source-to-installed skill packaging and the
[macOS CI gate](../.github/workflows/ci.yml).
