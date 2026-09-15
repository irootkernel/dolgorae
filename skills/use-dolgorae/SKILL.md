---
name: use-dolgorae
description: "Use Dolgorae safely through its local Machine CLI or an actually exposed checked tool when asked to inspect, initialize, configure, review, operate an External Specialist Engagement, diagnose, or recover Dolgorae state. Ground every action in the exact binary and advertised runtime capabilities; do not activate it for generic code review or merely because a repository contains Dolgorae files."
---

# Use Dolgorae

Dolgorae is a local durable control layer for Codex runs. This revision of the
skill covers workspace and profile readiness, immutable review targets, and
Specialist Policy operations, one-shot Specialist Review, and externally
planned reusable Specialist Engagements. It does not provide a live
Dolgorae-owned Primary tool or general persistent-Run workflow.

## Establish current authority

1. Confirm that the user explicitly asked to use, inspect, configure, diagnose,
   or recover Dolgorae. Repository policy or a `.dolgorae/` directory establishes
   availability, not authorization for a mutation or external review.
2. Resolve one executable and keep using that exact path:

   ```sh
   command -v dolgorae
   dolgorae version --json
   dolgorae runtime capabilities
   ```

   `version --json` returns exactly `{name, version}`, without the machine
   envelope; `--version` returns compact human text. Other commands emit the
   canonical machine envelope by default; do not add `--json` to them. Use
   `--human` only for interactive presentation and never parse it. If the
   executable is absent, report that fact; do not install or upgrade it
   automatically.
3. Require version JSON `name` to equal `dolgorae`, remove one leading `v`
   from its `version`, and compare it with capabilities
   `data.dolgorae_version` and any version pinned by the caller repository.
   Treat a checkout build, an installed binary, and a released artifact as
   separate identities. Stop on a mismatch instead of mixing their evidence.
4. Read `supported_transports`, `features`, `assurance`, `execution_lanes`,
   `lane_capabilities`, `profile`, and the protocol and RPC-client version
   fields from the current capability result. Use them to
   gate optional Run and orchestration behavior: a command name present in the
   CLI or specification cannot upgrade a `false`, unavailable, or unverified
   capability. The setup and one-shot review flows below are version-matched by
   distributing this skill with the Dolgorae release that provides them.
5. Resolve the target through the public interface:

   ```sh
   dolgorae workspace inspect --workspace <path>
   ```

   `WORKSPACE_NOT_INITIALIZED` is observation, not permission to run `init`.
   Read [lifecycle.md](references/lifecycle.md) only when initialization or
   another lifecycle operation was explicitly requested.

Parse process exit and the complete JSON envelope separately. Exit 0 may
represent an expected nonterminal control state. On failure, branch on stable
`error.code`, `error.retryable`, and checked `details`, not message text. Preserve
the envelope's invocation ID and every returned workspace, profile, review,
capture, engagement, Run, revision, and digest identity needed for follow-up.

## Prepare a one-shot review

1. Confirm that the requested source scope is exactly one of `workspace`,
   `staged`, `dirty`, `head`, `commit`, or `range`. Task, Epic, issue, and other
   workflow identifiers express authority or focus; they are not source scopes.
2. `workspace`, `staged`, `dirty`, and `head` reject a revision. `commit`
   requires one commit revision. `range` requires one exact `A..B` or `A...B`
   expression. Stdin may carry a v3 request, but it is not a source scope and
   cannot substitute patch semantics.
3. Inspect the named global Codex Profile and run its offline diagnosis before an
   external review when current readiness is not already established:

   ```sh
   dolgorae profile show <profile>
   dolgorae profile doctor <profile>
   ```

   `profile doctor` reports its verdict in `data.compatibility` and diagnostics;
   envelope `ok:true` means the check ran, not that the profile is compatible.
   Read [configuration.md](references/configuration.md) when a profile or
   Specialist Policy must be created or changed.
4. Treat a review as an external, potentially costly operation. Run it only
   when the user requested Dolgorae review:

   ```sh
   dolgorae specialist review \
     --workspace <path> \
     --profile <profile> \
     --target-kind <kind> \
     --format json
   ```

   Add `--revision <revision>` only for `commit` or `range`. Add
   `--deadline-seconds <1..3600>` only when the caller established a non-default
   bound. Use the legacy `--scope working-tree` carrier only when v1 compatibility
   is explicitly required; never combine it with v2-only options.
   For a task-aware change or completion review, construct the checked
   `dolgorae-specialist-review-request/v3` object and pass only that object on
   non-TTY stdin:

   ```sh
   dolgorae specialist review \
     --workspace <path> \
     --profile <profile> \
     --request-stdin \
     --format json < review-request-v3.json
   ```

   Follow `docs/protocol/examples/specialist-review-v3-request.valid.json` from
   the exact version-matched source or package. The request must carry
   `purpose` (`change` or `completion`) alongside the brief, contexts, criteria,
   expected output, and deadline. Keep workspace, Profile, credentials, and
   arbitrary host paths outside the request. Preserve exact brief and
   inline-context bytes; do not trim, normalize, or replace content with a bare
   artifact reference. Completion needs at least one uniquely identified
   criterion. Do not combine `--request-stdin` with v1/v2 source or deadline
   flags.
5. Prefer the composed `specialist review` operation. It owns capture, one fresh
   Reviewer, checked result collection, integrity verification, and settlement.
   Do not replace it with manual `review-target capture` and `settle` merely to
   reproduce the same workflow.
6. Accept success only from the complete checked result. Keep the target and
   Reviewer identities, verdict and findings, engagement state, settlement
   state, capture-time source identity, integrity evidence, and
   `workflow_issued_source_mutation:false` distinct. Empty findings do not make
   a failed, unknown, active, malformed, or unsettled review successful.
   For v3, also require every input criterion exactly once in input order,
   retain each status, evidence basis, remaining gap, and `evidence_limits`, and
   report `overall_assessment` as Reviewer evidence rather than business
   approval. `REVIEW_OUTPUT_INVALID` is terminal and never a clean result.

The optional attached tool named `dolgorae_review` may be used only when the
host actually exposes it. Its registration is the adapter's checked disposition;
never infer availability from the binary, a prior session, an MCP process, or
conversation memory. Follow its current tool schema and preserve the host-bound
request identity. When the tool is absent, use the Machine CLI. Never start or
invoke the hidden adapter entrypoint from the shell to make the tool appear.

## Operate an External Specialist Engagement

Use this mode only when the user explicitly asks an external AI host to open,
inspect, hire into, assign, wait for, collect from, cancel, release, complete,
abort, or recover one engagement. The external host remains the semantic
planner; do not infer a task graph, retry unknown work, attach an existing Run,
or let a Specialist hire or contact another Specialist.

1. Read the checked
   `docs/protocol/dolgorae-external-specialist-facade-v2.schema.json` contract
   from the exact version-matched source or package. For a v3 accepted-task
   assignment, also read
   `docs/protocol/dolgorae-external-specialist-facade-v3.schema.json` and its
   `docs/protocol/examples/external-engagement-v3-assign.valid.json` example. For hire requests, follow
   `docs/protocol/examples/external-engagement-v2-hire.valid.json`: use
   `agent_configuration.schema_version: 2` and `selected_profile`. Supply the
   optional optimistic `global_profile_binding_sha256` only from the actual
   selected Profile binding; never invent a digest. Construct exactly one
   request variant and pass it through a protected regular non-TTY descriptor:

   ```sh
   dolgorae engagement call \
     --workspace <path> \
     --controller-fd <aggregate-owner-fd> \
     --request-fd <request-fd>
   ```

   Treat hire `objective` only as non-executable rationale; put actual work in
   the later task assignment. A v3 assignment carries its exact brief, inline
   context and provenance, criteria, and expected output inside `task`, with
   `execution_intent` and `deadline_seconds` beside it. When it
   requests `structured_review_v3`, accept only the criterion-complete report
   returned by collection. For `isolated_write`, that report is the
   `final_response` member beside `isolated_change`; read criterion assessments
   and `overall_assessment` from `final_response`, not from the outer result.

2. The aggregate owner must be a generation-1 `workflow_orchestrator` or
   `automation` Controller. Preserve its carrier across reconnects; an
   engagement ID or opaque external reference is never authority. Add one
   distinct `--new-controller-fd <member-controller-fd>` only for
   `hire_external_specialist`. Never persist or expose either credential.
3. Reuse the exact operation-scoped idempotency key only with byte-equivalent
   semantic input. A host disconnect or transport wait expiry does not cancel
   accepted work. Reconnect with `get_external_engagement`, then wait or collect;
   never resubmit a task whose acceptance or outcome is unknown.
4. Treat `completed_not_delivered` as a durable result awaiting collection and
   preserve `next_after_sequence` only after consuming the returned immutable
   result. Treat `interrupted_unknown` as terminal uncertainty requiring an
   external planning decision, never automatic retry.
5. `isolated_write` confines model writes to a separate Git worktree.
   `canonical_workspace_write` is an assertion that the external host has
   quiesced its own writer; Dolgorae still rejects a competing Dolgorae writer.
   Use explicit cancellation, release, complete, or abort authority immediately
   before those lifecycle operations.

## Preserve authorization and state boundaries

- Require explicit user intent for `init`, profile changes, launch probes,
  server start/stop/restart/migrate, credential creation or rotation, low-level
  capture or settlement, cancellation, cleanup, reset, removal, and repair.
- Use only public CLI or an attached checked tool. Do not edit `.dolgorae/`,
  `~/.dolgorae/`, ledgers, registries, credentials, sockets, locks, or runtime
  databases directly. The fixed per-user root has no legacy migration,
  fallback, or alternate-root mode.
- Keep Controller, Operator, and settlement-owner credentials in protected
  files or inherited descriptors. Never print their bytes, place them in argv,
  copy them into a prompt, or treat an identity or digest as the credential.
- After every authorized mutation, re-read the affected public state and report
  only what the new envelope proves. A successful review, profile check, or
  lifecycle command never authorizes Git commit, push, release, installation,
  or another mutation.
- This skill does not operate general `run`, workspace-writer, or live
  Dolgorae-orchestrated Primary tool flows. If one is
  requested, inspect current capabilities and repository authority, report that
  it is outside this skill's present workflow, and do not improvise from
  planned command grammar.

Read [recovery.md](references/recovery.md) after stale state, response loss,
unknown outcome, integrity failure, or profile/capture recovery guidance.
