# EPIC-004: Immutable Review Targets and Aquarium Activation

Roadmap Epic: [`EPIC-004`](../roadmap/README.md#epic-004-immutable-review-targets-and-aquarium-activation)

This temporary dossier owns the implementation detail needed while EPIC-004 is
active. The canonical roadmap remains the sole authority for identifiers,
ordering, dependencies, lifecycle vocabulary, and status.

## Goal and purpose

Implement reusable immutable review targets, extend Dolgorae Specialist Review
to dirty and historical Git state, and activate Dolgorae as Aquarium
independent-review's Codex backend without replacing the distinct Mulgae or
Orca review lifecycles.

The Epic must leave the completed Specialist Review `working-tree` v1 contract
compatible while adding checked, versioned capture, settlement, and scoped
review contracts for `workspace`, `staged`, `dirty`, `head`, `commit`, and exact
two-dot or three-dot `range` targets.

## Scope and approach

- Build one shared Immutable Review Target Coordinator that resolves eligible
  source bytes, detects drift, materializes an immutable external capture, binds
  safe manifest and whole-target identities, and settles only against verified
  terminal backend evidence.
- Compose that coordinator with the existing External Specialist Engagement so
  every scoped review uses one fresh managed Codex Reviewer and receives only
  the immutable target root as review context.
- Freeze one exact task-complete Dolgorae candidate, hand it to the Aquarium
  owner for activation under that repository's authority, and independently
  revalidate the returned runtime Completed Confirm before Epic closeout.
- Keep specifications, architecture, ADRs, checked protocol artifacts,
  implementation, tests, and user or operator documentation synchronized with
  the behavior they own.

## Task objectives

### Immutable review-target foundation

Implement the six accepted source-scope meanings and versioned
`review-target.capture` and `review-target.settle` Machine and CLI boundaries.
Prove source and index non-mutation, deterministic materialization, credential
and unsafe-content rejection, capture integrity, authorization-bound settlement,
idempotent replay, concurrent settlement safety, cleanup, and conservative
timeout or unknown recovery.

### Scoped Specialist Review runtime

Add the checked target `{kind, revision?}` review path without changing the v1
`working-tree` spelling or meaning. Bind each result to resolved Git identities,
capture and manifest digests, Reviewer and engagement state, settlement state,
capture integrity, and the absence of workflow-issued source mutation. Verify
all scopes, compatibility, failure paths, cleanup, and the opt-in live Codex CLI
campaign required by the roadmap.

### Aquarium activation and Completed Confirm

Freeze the exact committed Dolgorae candidate and complete the roadmap-owned
external handoff. Aquarium must activate and verify that exact candidate under
its own repository authority, return the required runtime Completed Confirm,
and prove Dolgorae-backed independent-review without Orca objects while
orca-review retains Orca lifecycle ownership. Dolgorae then revalidates every
committed and installed identity, digest, stable reference, scope result, and
failure or recovery claim before accepting the handoff.

## Required constraints

- Resolve every task, Epic, or special-request focus to exactly one supported
  source scope before capture; patch and stdin remain Mulgae-only extensions.
- Never modify the source worktree, index, refs, or Git metadata while preparing
  a review target.
- Fail closed on source drift, unresolved conflicts, escaping links, special
  files, recognized secrets, capture mutation, executable drift, incompatible
  capability, incomplete settlement, or unverifiable terminal evidence.
- Keep the settlement owner credential and its carrier outside machine results,
  provider-visible content, tracked documentation, and review evidence.
- Preserve active or unknown captures and recovery evidence; never infer
  cancellation, settlement, or replay from elapsed time.
- Treat Dolgorae implementation, Aquarium activation, installed runtime proof,
  Git commits, review publication, and roadmap state as separate authorities.

## Prohibited shortcuts and non-goals

- Do not reinterpret or replace the completed Specialist Review v1 contract.
- Do not use an Orca-created Codex terminal for Aquarium independent-review or
  remove Orca supervision from orca-review.
- Do not couple Mulgae to Dolgorae code, storage, or orchestration lifecycle.
- Do not transmit ignored files, Git metadata, private tool state, absolute
  source paths, credentials, raw prompts, transcripts, or hidden reasoning.
- Do not treat a design handoff, documentation-only change, mutable executable,
  uncommitted external diff, or prose-only success claim as runtime activation.
- Do not mutate or publish another repository without separate authority.

## Acceptance and closeout

Every member Task must satisfy the roadmap's ordinary completion gate. The
specification, architecture, ADR-032, checked contracts, implementation, and
tests must agree; the runtime Completed Confirm must bind exact committed and
installed Dolgorae and Aquarium artifacts and survive independent Dolgorae
revalidation; and the active Aquarium independent-review path must use Dolgorae
to run a fresh Codex Reviewer without creating Orca objects.

Before marking the Epic complete, promote every durable dossier statement to
its canonical owner. Then remove this file and its TODO index entry, replace the
roadmap's `Detailed SOT` link with existing `Canonical Outcomes` links, and
perform the Epic lifecycle transition in one approved closeout change. Keep no
archive or tombstone copy of this dossier.
