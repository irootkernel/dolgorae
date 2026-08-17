# Dolgorae

Status: Non-normative repository entry point.

As specified in [the product contract](docs/specs.md), Dolgorae is a local,
durable control layer for persistent Codex runs. It adds
stable run identity, controller authorization, workspace writer coordination,
recovery, and auditability while leaving conversation storage with Codex.

This repository currently contains the release contract, architecture,
machine-readable protocol, verification tools, and pre-implementation evidence.
Do not infer implementation progress from the presence of a specification or
probe. [The roadmap](docs/roadmap.md) is the sole owner of delivery status.

## Start Here

Read [the documentation authority map](docs/README.md) before changing or
implementing the system. It identifies the single owner for each kind of fact,
the required reading order, and the synchronization gate for cross-document
changes.

For a first pass:

1. [Product specification](docs/specs.md) — externally observable behavior.
2. [Architecture](docs/architecture.md) — components, state, and invariants.
3. [Architecture decisions](docs/architecture-decisions.md) — accepted choices
   and rejected alternatives.
4. [Roadmap](docs/roadmap.md) — current status, ordering, and acceptance gates.
5. [Protocol contracts](docs/protocol/) — checked wire and persisted shapes.

Historical reviews and probe results are evidence, not a second product
contract. Their role and authority are defined in
[the documentation authority map](docs/README.md).
