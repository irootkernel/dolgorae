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
| TODO candidates and active dossiers | [todo/README.md](todo/README.md) |
| Deferred feedback | [deferred-feedback/README.md](deferred-feedback/README.md) |

The checked [protocol](protocol/) artifacts derive wire, persisted-state, and
machine-output contracts from the product specification. The
[review-target strategy analysis](architecture-decision-records/review-strategy-analysis.md)
is a supporting accepted design study, not runtime or completion evidence. The
root [contribution guide](../CONTRIBUTING.md) is an entrypoint to implementation
guidance, and the public [changelog](../CHANGELOG.md) owns release history.
Optional source-distributed agent guidance lives under [`skills/`](../skills/).
It consumes these authorities and current runtime capability output; it does
not redefine product behavior, grant operational authorization, or install with
the binary.

The ledger retains the frozen [audit v1](protocol/dolgorae-audit-record-v1.schema.json)
reader and adds [audit v2](protocol/dolgorae-audit-record-v2.schema.json) for
hash-bound historical client projections and mutation admission/completion.
[Stamped events](protocol/dolgorae-stamped-client-event-v1.schema.json) bind the
existing client event record to its append-time aggregate revisions. The
[ledger conformance registry](protocol/dolgorae-ledger-conformance-v1.json)
identifies each accepted record version and its kind extensions; these persisted
contracts do not alter the frozen public-v1 Protobuf or descriptor.

## Precedence and synchronization

Each role owner is authoritative only for its stated domain. Specifications own
observable behavior, architecture owns structure and invariants, decision
records own accepted rationale, and the roadmap alone owns delivery identity,
order, lifecycle vocabulary, and status. Protocol artifacts, implementation,
tests, and summaries are derived where they overlap those authorities.

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
idea is adopted as an active Epic with tasks, its temporary dossier is listed
as adopted TODO work and linked from the roadmap with `Detailed SOT`. Epic
closeout promotes durable outcomes to their owners, removes the dossier, and
replaces that link with `Canonical Outcomes`. Historical completed work without
these lifecycle fields remains unchanged.

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
