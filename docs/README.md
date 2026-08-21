# Dolgorae Documentation

This directory contains the public source of truth for Dolgorae.

## Canonical documents

- [Product specification](specs.md) owns externally observable behavior and
  semantic requirements.
- [Architecture](architecture.md) owns component boundaries, state ownership,
  process topology, and technical invariants.
- [Architecture decisions](architecture-decisions.md) records accepted choices,
  rationale, and rejected alternatives.
- [Roadmap](roadmap.md) owns delivery order and status.
- [Protocol](protocol/) owns checked wire, persisted-state, and machine-output
  shapes.

If canonical documents disagree, resolve the contradiction before changing the
implementation. For a behavior or architecture change, update the owning
document first, then synchronize affected protocol artifacts, implementation,
tests, and roadmap entries.

## Validation ownership

Rust unit and integration tests own product semantics. Python is intentionally
limited to small JSON/schema and Markdown checks plus black-box tests of the
compiled Rust executable. Run the complete repository gate with
`tools/check.sh`; setup instructions are in [CONTRIBUTING.md](../CONTRIBUTING.md).
