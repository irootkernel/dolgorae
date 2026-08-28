# Review Target Strategy Analysis

Status: Accepted EPIC-004 implementation baseline. This document records the
investigated 2026-08-25 baseline and the decisions EPIC-004 must implement. It
does not advertise a runtime capability or a completed review. Current product
behavior remains governed by the [specification](../specs/README.md) and the
checked protocol artifacts.

## Goal

EPIC-004 makes Aquarium's `independent-review` use Dolgorae to run a fresh
Codex Reviewer without creating an Orca terminal. `orca-review` should retain
its Orca and provider lifecycle, while Mulgae, Dolgorae-backed independent
review, and Orca-backed review use the same source-scope meanings. The target
must be immutable after capture so a review can cover dirty work safely and can
also address historical Git objects.

This study separates that target contract from supervision. Target selection,
capture identity, included bytes, exclusions, and drift belong to the common
review-target strategy. Reviewer creation, waiting, cancellation, result
collection, and settlement remain backend-specific.

## Verified Baseline

| Strategy | Current target forms | Current materialization | Current supervision | Verified gap |
| --- | --- | --- | --- | --- |
| Dolgorae Specialist Review | `working-tree` only | Reviewer receives the canonical workspace under read-only policy; the coordinator fingerprints the workspace before and after execution | One External Specialist Engagement and one fresh managed Codex Reviewer Run | No staged, dirty, exact commit, or range selector; no reusable immutable source capture |
| Mulgae | workspace, stage, dirty, Git diff, patch, and stdin | Captures an immutable target and controlled read-only provider workspace with manifest and digest checks | Mulgae run, provider, extraction, adjudication, and publication lifecycle | Public selector names differ, but the six common Git/workspace meanings can be mapped without a code change |
| Aquarium independent-review | staged, `HEAD`, commit, or range | Uses the original checkout; staged review reads the live index and dirty content is excluded | Fresh Codex terminal supervised by Orca | Depends on Orca and cannot target dirty content; the live index is not an immutable review identity |
| Aquarium orca-review | staged, `HEAD`, commit, or range | Uses the original checkout and the same live-target contract as independent-review | Orca Run, Task, Dispatch, terminal, and selected provider lifecycle | Must retain Orca supervision while replacing its source materialization strategy |

The Aquarium shared contract currently forbids dirty content as a target and
allows separately approved staging. That policy conflicts with a reusable
static-review target: changing the index changes repository state and a live
index can drift after dispatch. The EPIC-004 contract therefore selects existing
state without staging it and captures that state before reviewer execution.

## Accepted Common Scope Semantics

| Scope | Captured meaning | Required identity |
| --- | --- | --- |
| `workspace` | Every eligible non-ignored file in the current workspace | Content manifest and whole-target digest |
| `staged` | The captured `HEAD` to captured index transition | HEAD object, index identity, before/after manifests, and target digest |
| `dirty` | The captured `HEAD` to the combined staged, unstaged, and non-ignored untracked state | HEAD object, before/after manifests, and target digest |
| `head` | The tree of the commit resolved from `HEAD` at capture | Resolved commit and tree objects plus target digest |
| `commit` | The first-parent transition into one resolved commit; an empty tree is the base for a root commit | Resolved base, commit, and tree objects plus target digest |
| `range` | The exact requested `A..B` transition or the merge-base-to-`B` transition for `A...B` | Original expression, resolved base and head objects, and target digest |

Ignored files are excluded. Non-ignored untracked files are included in
`workspace` and `dirty`. An unresolved conflict fails capture because no single
after-state can be represented without inventing a resolution. Task, Epic, and
special-request identifiers remain review context and focus; each must resolve
to exactly one of the six source scopes before capture.

Mulgae maps `workspace`, `staged`, and `dirty` directly. Its existing Git diff
selector represents `head`, `commit`, and `range` using resolved expressions.
Mulgae-only patch and stdin targets remain extensions and do not become part of
the Aquarium common scope contract.

Eligibility is narrower than "not ignored." The EPIC-004 capture may admit only
regular in-root files and safe in-root symbolic-link representations. It must
exclude VCS metadata and private tool state, reject special files and escaping
links, and apply one versioned content-eligibility policy before bytes enter a
provider-visible snapshot. Every tracked and untracked candidate is subject to
the same check. A recognized credential, private key, token-bearing file, or
other prohibited secret fails the whole capture with a bounded path and reason;
it is not silently omitted, transmitted, or written into a partial manifest.
Explicit review consent cannot override this barrier. The EPIC-004 implementation
must test sensitive filenames and contents in both tracked and untracked state
and must describe the detector's bounded guarantees rather than claiming that
heuristics can prove arbitrary content secret-free.

`workspace` and the after side of `dirty` each contain one final byte sequence
per eligible path, never parallel index and worktree copies. For a tracked path,
the current worktree file wins over the index; absence in the worktree is a
deletion even when an index blob exists. A file recreated after a staged
deletion is present with its worktree bytes. Non-ignored untracked files join
that final projection. The index records staged identity and classification but
does not override a later worktree state. Renames are represented by exact-path
deletion and addition rather than an inferred rename. The `dirty` before side is
the resolved HEAD tree, while `workspace` is the final projection without a
change-base claim.

## Required Immutable Capture Boundary

TASK-014 must capture source bytes without changing the source worktree, index,
refs, or Git metadata. It must fail if relevant
source identity changes while capture is in progress. After a successful
capture, later source changes do not stale the captured target; the reviewer
result binds to the captured digest instead.

The captured view lives under Dolgorae Application Support rather than inside
the source repository. It contains `current/` for whole-tree
targets or `before/` and `after/` for transition targets, use read-only file and
directory modes, and carry a manifest with safe relative paths, sizes, SHA-256
digests, media classifications, inclusion or exclusion disposition, resolved
Git identities, and a whole-target digest. Post-execution validation must fail
closed if the captured bytes or manifest changed.

Default settlement removes captured source bytes after the backend reaches an
authoritative terminal disposition. Each capture binds one backend and lifecycle
identity plus a protected random settlement owner credential. Settlement must
present that credential, the expected capture revision, and a checked terminal
receipt bound to the same backend lifecycle and stable evidence digest. Active,
unknown, stale, foreign-owner, mismatched, or concurrently superseded requests
leave the capture unchanged. Exact accepted replay is idempotent; changed replay
is a conflict. The manifest, digest, bounded result, and settlement state may
remain. Explicit retention is a separate opt-in policy, not a default. Unknown
execution is never replayed automatically and must not trigger cleanup that
would destroy evidence needed for recovery.

Discovery of a Dolgorae executable is not execution authority. Aquarium must
bind consent to one canonical regular-file target, version, capability result,
file identity, and SHA-256 digest and revalidate all of them immediately before
source-bearing execution. Replacement or drift stops before transmission.
Bounded-wait exhaustion observes authoritative state once and preserves an
active or unknown execution. Cancellation requires explicit user authority and
never converts timeout or unknown outcome into settlement or cleanup.

Read-only materialization is not a claim of strong operating-system containment.
A same-user process may technically discover other readable paths. The backend
must pass only the captured root as review context, retain its own sandbox and
network policy, disclose this limitation, validate that the captured target and
manifest were not modified, and prove that the workflow issued no source
mutation. Source identity must remain stable through capture publication; an
unrelated later source change does not stale the captured target or its result.

## Required Backend Boundaries

- Dolgorae-backed `independent-review` will select and capture one common target,
  start one fresh Codex Reviewer through the External Specialist Engagement,
  validate the structured result, and settle the capture without Orca objects.
- `orca-review` will select and capture the same target but retain Orca Run,
  Task, Dispatch, terminal, provider, acknowledgement, and recovery semantics.
  Target settlement occurs only after the Orca lifecycle is authoritative.
- Mulgae retains its provider, extraction, adjudication, publication, and archive
  lifecycle. Conformance is semantic and digest-based; it does not require
  Dolgorae code or storage coupling.

The completed `working-tree` Specialist Review preview remains unchanged.
TASK-015 must add a versioned target request rather than reinterpret
the checked v1 request or its existing CLI spelling.

## EPIC-004 Implementation and Acceptance Conditions

EPIC-004 implementation and activation must keep the following decisions closed:

1. One versioned target selector represents exactly the six common scopes and
   preserves two-dot versus three-dot semantics.
2. Capture and settlement have checked machine results with explicit source
   drift, conflict, invalid revision, snapshot mutation, cancellation, timeout,
   unknown outcome, foreign owner, stale revision, mismatched lifecycle,
   missing terminal evidence, and concurrent settlement failures.
3. Reviewer-visible material contains no absolute source path, credentials,
   ignored files, Git metadata directory, raw prompt, transcript, or hidden
   reasoning.
4. Independent and Orca-backed reviews return the shared result envelope while
   reporting their backend lifecycles separately.
5. Compatibility, cleanup, privacy, retention, and rollback tests are owned by
   EPIC-004 rather than inferred from this analysis.
6. The failure campaign covers every ineligible path and content class,
   capture-time replacement, escaping links, special files, tracked and
   untracked secrets, absence of partial manifests or residual source bytes,
   and executable identity drift immediately before launch.
7. Activation and rollback require quiescence. Every known execution is settled;
   unknown execution and retained data remain readable and recoverable; stored
   formats remain compatible; and the exact installed Aquarium copy is moved
   atomically between the old and new contracts without mixed workers.
8. TASK-016 freezes one exact Dolgorae candidate and sends the copyable handoff
   in canonical `docs/roadmap/README.md` to the Aquarium owner. EPIC-004 remains
   blocked
   until Aquarium returns runtime proof for an exact committed and installed
   candidate and Dolgorae independently revalidates that Completed Confirm.
