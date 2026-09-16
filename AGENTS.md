# AGENTS.md

This file is the canonical local agent guidance for the Dolgorae repository.

## Core Behavior

### 1. Lead with Conclusions

- State the result or current finding first, followed by useful evidence and material limits.
- Do not repeatedly restate requirements or narrate routine work.

### 2. Reuse Verified Information

- Inspect the requested code and its named authorities before changing anything. Resolve discoverable facts before asking Master.
- Reuse established facts instead of reading or searching for them again. Recheck only the affected information when relevant state changes, evidence conflicts, or missing context makes it unreliable.
- State material assumptions and surface meaningful trade-offs. Ask when unresolved ambiguity would materially change the result, and push back on conflicts with repository authority, safety, or Master's goal.

### 3. Act on Sufficient Evidence

- Stop investigating once the evidence supports action. When the root cause is established, implement the smallest complete, durable fix within the authorized scope.
- Weigh correctness, performance, maintainability, and structural fit rather than diff size alone. If a broader design exceeds scope, complete a bounded step that satisfies current acceptance criteria.
- Reuse established patterns. Avoid speculative features, abstractions, configurability, compatibility layers, and handling for states repository invariants make impossible. Simplify complexity that the required behavior does not justify.
- Touch only what the outcome and its verification require. Preserve unrelated user work, match local style, and remove only artifacts made obsolete by this change.
- Record only independent remaining work in the canonical `deferred-feedback` owner. If none exists, propose the entry and obtain approval before creating an owner. Promote epic-sized work to a TODO candidate or roadmap unit; never defer current correctness or acceptance work.

### 4. Carry Authorization Forward

- Continue already approved work without asking for confirmation again. Ask only when a material change exceeds that authorization or an applicable rule requires a distinct approval.
- Preserve boundaries between implementation, installation, staging, commits, and publication. Check for relevant state changes before acting on an approved proposal.

### 5. Verify in Proportion to Risk

- Define success checks before implementation. Verify the affected behavior and relevant failure paths with rigor proportionate to the actual risk.
- Run focused checks first and honor required repository gates. Broaden or repeat checks when changes, failures, or unresolved concerns justify it.
- Do not add tests merely to appear rigorous or use prose matching as a substitute for behavior verification.

### 6. Finish When Complete

- Continue until deliverables and required verification are complete or a concrete blocker prevents progress.
- Once material constraints are resolved or clearly reported, provide the handoff and stop. Report the result, necessary evidence, skipped checks and their reasons, and remaining uncertainty without opening unrelated work.

### 7. Delegate Selectively

- Use a sub-agent only for an independent task when the expected benefit outweighs coordination cost.
- Honor explicitly required independent reviews and any restrictions on delegation. Keep tightly coupled work local.

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
- Use `$aquarium:dev-setup-global` to diagnose, install, or update supported user-global development tools, paired skills, services, and global MCP state. Route Aquarium plugin-only installation or updates to the host plugin-management flow.
- Use `$aquarium:dev-setup` to diagnose or configure repository-local tooling and operating guidance, including explicitly requested Sorage Project setup.
- Use `$aquarium:docs-setup` to audit, establish, adopt, or migrate canonical documentation structure and roadmap IDs.
- Use `$aquarium:test-setup` to audit or configure the common Make or Bun testing contract and evidence-backed legacy waivers.
- Use `$aquarium:release-handler` for one stable release lifecycle and `$aquarium:release-qa` for its exact committed-candidate scenario verification.
- Use `$use-dolgorae` for explicitly requested Dolgorae workspace, global Profile, immutable-target, one-shot review, External Specialist Engagement, diagnosis, lifecycle, or recovery operations.
- Use `$use-mulgae` for an authorized Mulgae review, run inspection, finding follow-up, configuration diagnosis, cleanup plan, or recovery.
- Use `$use-gaori` when a selected long or noisy check is routed through Gaori or existing Gaori evidence must be inspected.
- Use `$use-gaori-status` for Gaori-calculated duration, outcome history, and detailed timing explanations.
- Use `$use-sorage` only for explicitly requested broker operations. Session start, a new task, a Sorage mention, or Project registration does not authorize inbox or outbox discovery. Resolve requested Handoff, review, retention, deletion, and Vault operations through that skill; never edit the managed Vault or derived `.sorage/INBOX.md` directly.
- Let Aquarium workflows use Podway by default for Git-backed work unless Master opts out before the first managed-session mutation. No Aquarium skill owns a Podway session; only when starting a different session should the workflow ask whether to preserve, finish, delete, or replace the existing one.
- Use `$use-podway` directly for an explicitly requested Procedure v2 lifecycle, goal, diagnosis, recovery, cancellation, or discard operation. Route Procedure authoring to the separately installed `$create-podway-procedure` maintainer skill.
- Use `$lore-commits` for non-trivial commit messages and `$lore-query` to inspect recorded decision context.
- Use the separately installed upstream `$deslop` skill for task-owned cleanup when an Aquarium workflow requests it.
- Use the separately installed upstream `$humanizer` skill once as the final prose pass for English human-authored documentation. Preserve meaning, facts, code, commands, identifiers, URLs, citations, quotes, legal text, and generated content; retain the unchanged draft if the skill is unavailable or validation fails.
- Keep `.mulgae/**`, `.gaori/runs/**`, `.podway/runtime/**`, derived `.sorage/**`, and disposable roots as local runtime evidence. Do not cite their paths or identities as durable evidence in tracked documentation or commit messages; use an approved tracked `aquarium.promoted-evidence/v1` package under `evidence/aquarium/` only when a downstream consumer genuinely requires retained evidence. Promotion accepts only reviewed bounded non-sensitive structured evidence and never accepted reports, raw logs, excerpts, provider prose, runtime identities, or machine-specific paths.
- Repository-specific rules in Project Configuration override these defaults.

## Project Configuration

### Repository Index and Authorities

- Dolgorae is a local durable control layer for persistent Codex runs. The Rust implementation leaves conversation storage with Codex while owning stable run identity, controller authorization, workspace writer coordination, recovery, and auditability.
- `docs/specs/README.md` owns externally observable behavior and semantic requirements.
- `docs/architecture/README.md` owns component boundaries, state ownership, process topology, and technical invariants.
- `docs/architecture-decision-records/README.md` records accepted decisions, rationale, and rejected alternatives.
- `docs/protocol/` owns checked wire, persisted-state, and machine-output shapes.
- `docs/roadmap/README.md` is the sole delivery-order and delivery-status authority.
- Aquarium release notes: CHANGELOG.md
- If canonical documents disagree, resolve the contradiction before changing implementation. For behavior or architecture changes, update the owning document first, then synchronize affected protocol artifacts, implementation, tests, and roadmap entries.
- The supported toolchain is Rust 1.97.1, Buf 1.69.0, and Python 3 with the validation dependencies in `tools/validation/requirements.txt`.
- The complete repository gate is `make PYTHON_BIN=.venv/bin/python test`. Its ordered layers are `test-prepare`, `test-unit`, `test-int`, and `test-e2e`.
- `make test-prepare` runs `cargo fmt` and may rewrite Rust source. Use `make format-check` when a read-only formatting check is required.
- `make test-live-specialist-review`, `make test-live-scoped-specialist-review`, `make test-live-access-safety`, and `make test-live-codex-compatibility` contact an external Codex runtime and require explicit authorization plus their documented opt-in environment. They are not part of the default complete gate.

### Commit Messages

- Do not run `git add`, `git commit`, or `git push` unless Master explicitly authorizes the specific operation. Commit authorization permits staging and committing only the approved files and never authorizes a push.
- Every commit title starts with exactly one square-bracket header followed by a concise imperative summary.
- Use the owning roadmap task ID, such as `[TASK-014] <summary>`, when one task owns the change.
- Use the owning roadmap epic ID, such as `[EPIC-004] <summary>`, only when no task owns the epic-level change.
- Use `[INT] <summary>` when no unambiguous roadmap task or epic owns the change. Do not invent an ID from branches, issues, or nearby documentation.
- Use `[REL] Release v<version>` only for the release metadata commit that closes the matching changelog cycle after release QA passes.
- When multiple roadmap items are related, use the primary owner in the title and record the others through useful Lore `Related:` trailers.
- Use `$lore-commits` for non-trivial commit messages. Lore trailers remain optional for trivial changes, but the title header is always required.

### Project-Specific Operating Rules

- Keep `.sorage/` ignored and untracked. Mulgae applies Git ignore policy during capture; use `.mulgaeignore` only for additional review exclusions.
- Use the Aquarium development channel only for explicitly requested development builds and execution. The producer entrypoints are `make aquarium-dev-describe` and `make aquarium-dev-build AQUARIUM_DEV_OUTPUT=<absolute-empty-directory>`; the builder requires a clean local `main` and writes only to the caller-created output directory outside the repository.
- Use `aquarium-dev diagnose --repository "$PWD"` for channel readiness, `aquarium-dev rebuild --repository "$PWD" --approve-build` for an authorized build and publication, and `aquarium-dev dolgorae ...` to execute the selected leased generation. Do not manually copy binaries into manager-owned generation or selector paths.
- An enrolled native post-commit hook requests a background development build after local `main` commits. Verify the published generation and checksum separately from Git commit success. Development publication does not authorize a stable release, production-tool replacement, or external review.
- `~/.aquarium-dev/` isolates development artifacts, not Dolgorae runtime state. Dolgorae still uses the fixed per-user `~/.dolgorae` home; workspace, Profile, server, and review operations retain their own authorization boundaries.

- Dolgorae agent guidance is source-distributed at `skills/use-dolgorae/SKILL.md` and installed through the README's Agent skill procedure. The skill's presence never authorizes initialization, external review, server control, credential mutation, cancellation, settlement, or repair. Specialist Policy operations remain unavailable until their owning roadmap task is implemented.

- Preserve the roadmap, Podway, Git, and external publication as separate authorities. A passing check or completed Podway session does not itself change roadmap status, create a commit, or prove publication.
- Treat tracked `.podway/procedures/aquarium-*-v2.yaml` files as portable workflow definitions. Preserve `.podway/runtime/**`, existing sessions, and runtime identifiers as local state.
- Do not commit virtual environments, caches, generated review material, local workflow state, credentials, or run and session identifiers.
- Put product semantics and their tests in Rust. Keep Python limited to small independent repository checks and black-box executable tests.
- E2E tests may use real Git only inside their isolated temporary home. Any future database or external service must use test-only credentials and namespaces and must never reuse production state or the user's real home.
