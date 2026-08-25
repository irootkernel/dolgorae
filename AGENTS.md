# AGENTS.md

This file is the canonical local agent guidance for the Dolgorae repository.

## Core Behavior

### 1. Inspect Before Acting

- Resolve repository facts and named authorities before implementation.
- State material assumptions, surface trade-offs, and ask when unresolved ambiguity would materially change the result.
- Push back when a request conflicts with repository authority, safety, or the user's stated goal.

### 2. Prefer the Smallest Complete Solution

- Implement only the verified requirement and reuse established patterns.
- Avoid speculative features, abstractions, configurability, and compatibility layers.
- Simplify an implementation whose size or complexity is not justified by its behavior.

### 3. Prefer Durable Root-Cause Solutions

- For fixes and solution proposals, prefer the smallest complete approach that addresses the verified root cause, weighing correctness, performance, maintainability, and structural fit instead of optimizing for the smallest diff.
- Prefer durable designs over symptomatic patches while keeping the current work proportional to the verified requirement and repository authority.
- When a broader ideal design exceeds the current scope, implement a bounded durable step that fully satisfies current success criteria and preserves a clear path forward.
- Record only remaining independent actionable work in the repository's canonical deferred-feedback owner. If no owner exists, report the proposed entry and obtain approval before creating one.
- Promote epic-sized work to a TODO candidate or roadmap work unit. Do not defer work required for current correctness or acceptance.

### 4. Make Surgical Changes

- Touch only what the requested outcome and its verification require.
- Preserve unrelated work and match local style.
- Remove only artifacts made obsolete by the current change.

### 5. Work Toward Verifiable Goals

- Define success checks before implementation.
- Match verification strength to the claimed behavior and relevant failure paths.
- Continue until the result is verified or a concrete blocker is established; report skipped checks and remaining uncertainty.

## Master Preferences

- Respond to Master in Korean using polite speech. When directly addressing the user, use exactly `Master`.
- Keep repository artifacts in the repository's established language and style. When no convention exists, use English unless Master requests otherwise.
- Report concise conclusions and useful evidence without exposing private chain-of-thought.

## Aquarium Development Guide

- Use `$aquarium:task-handler` for one named roadmap task.
- Use `$aquarium:epic-handler` to implement one roadmap epic as sequential task goals.
- Use `$aquarium:epic-validator` to cold-validate and remediate one completed roadmap epic.
- Use `$aquarium:new-project`, `$aquarium:new-feature`, or `$aquarium:refactor` for an explicitly requested Ouroboros-assisted project or epic design workflow.
- Use `$aquarium:war-room` to diagnose one difficult bug and stop at a task, epic, or incomplete-investigation proposal.
- Use `$aquarium:design-qa` to create, change, reactivate, or retire local Design Gates.
- Use `$aquarium:dev-setup` to diagnose or configure development tooling and repository operating guidance.
- Use `$aquarium:docs-setup` to audit, establish, adopt, or migrate canonical documentation structure and roadmap IDs.
- Use `$aquarium:test-setup` to audit or configure the common Make or Bun testing contract and evidence-backed legacy waivers.
- Use `$aquarium:release-handler` for one stable release lifecycle and `$aquarium:release-qa` for its exact committed-candidate scenario verification.
- Use `$use-mulgae` for an authorized Mulgae review, run inspection, finding follow-up, configuration diagnosis, cleanup plan, or recovery.
- Use `$use-gaori` when a selected long or noisy check is routed through Gaori or existing Gaori evidence must be inspected.
- Let Aquarium workflow owners use Podway by default for Git-backed workflows unless Master opts out before the first managed-session mutation; Aquarium workflow skills retain their stricter roadmap, ownership, and approval rules.
- Use `$use-podway` directly for an explicitly requested Procedure v2 lifecycle, authoring, diagnosis, recovery, cancellation, or discard operation.
- Use `$lore-commits` for non-trivial commit messages and `$lore-query` to inspect recorded decision context.
- Use the separately installed upstream `$deslop` skill for task-owned cleanup when an Aquarium workflow requests it.
- Keep `.mulgae/**`, `.gaori/runs/**`, `.podway/runtime/**`, and disposable roots as local runtime evidence. Do not cite their paths or identities as durable evidence in tracked documentation or commit messages; use an approved tracked `aquarium.promoted-evidence/v1` package under `evidence/aquarium/` only when a downstream consumer genuinely requires retained evidence. Promotion accepts only reviewed bounded non-sensitive structured evidence and never accepted reports, raw logs, excerpts, provider prose, runtime identities, or machine-specific paths.
- Repository-specific rules in Project Configuration override these defaults.

## Project Configuration

### Repository Index and Authorities

- Dolgorae is a local durable control layer for persistent Codex runs. The Rust implementation leaves conversation storage with Codex while owning stable run identity, controller authorization, workspace writer coordination, recovery, and auditability.
- `docs/specs.md` owns externally observable behavior and semantic requirements.
- `docs/architecture.md` owns component boundaries, state ownership, process topology, and technical invariants.
- `docs/architecture-decisions.md` records accepted decisions, rationale, and rejected alternatives.
- `docs/protocol/` owns checked wire, persisted-state, and machine-output shapes.
- `docs/roadmap.md` is the sole delivery-order and delivery-status authority.
- If canonical documents disagree, resolve the contradiction before changing implementation. For behavior or architecture changes, update the owning document first, then synchronize affected protocol artifacts, implementation, tests, and roadmap entries.
- The supported toolchain is Rust 1.97.1, Buf 1.66.1, and Python 3 with the validation dependencies in `tools/validation/requirements.txt`.
- The complete repository gate is `make PYTHON_BIN=.venv/bin/python test`. Its ordered layers are `test-prepare`, `test-unit`, `test-int`, and `test-e2e`.
- `make test-prepare` runs `cargo fmt` and may rewrite Rust source. Use `make format-check` when a read-only formatting check is required.
- `make test-live-specialist-review` contacts an external Codex runtime and requires explicit authorization plus its documented opt-in environment. It is not part of the default complete gate.

### Commit Messages

- Do not run `git add`, `git commit`, or `git push` unless Master explicitly authorizes the specific operation. Commit authorization permits staging and committing only the approved files and never authorizes a push.
- Every commit title starts with exactly one square-bracket header followed by a concise imperative summary.
- Use the owning roadmap task ID, such as `[TASK-014] <summary>`, when one task owns the change.
- Use the owning roadmap epic ID, such as `[EPIC-004] <summary>`, only when no task owns the epic-level change.
- Use `[INT] <summary>` when no unambiguous roadmap task or epic owns the change. Do not invent an ID from branches, issues, or nearby documentation.
- When multiple roadmap items are related, use the primary owner in the title and record the others through useful Lore `Related:` trailers.
- Use `$lore-commits` for non-trivial commit messages. Lore trailers remain optional for trivial changes, but the title header is always required.

### Project-Specific Operating Rules

- Preserve the roadmap, Podway, Git, and external publication as separate authorities. A passing check or completed Podway session does not itself change roadmap status, create a commit, or prove publication.
- Treat tracked `.podway/procedures/aquarium-*-v2.yaml` files as portable workflow definitions. Preserve `.podway/runtime/**`, existing sessions, and runtime identifiers as local state.
- Do not commit virtual environments, caches, generated review material, local workflow state, credentials, or run and session identifiers.
- Put product semantics and their tests in Rust. Keep Python limited to small independent repository checks and black-box executable tests.
- E2E tests may use real Git only inside their isolated temporary home. Any future database or external service must use test-only credentials and namespaces and must never reuse production state or the user's real home.
