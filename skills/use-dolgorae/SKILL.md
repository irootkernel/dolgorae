---
name: use-dolgorae
description: "Use Dolgorae safely through its local Machine CLI or an actually exposed one-shot review tool when asked to inspect, initialize, configure, review, diagnose, or recover Dolgorae state. Ground every action in the exact binary and advertised runtime capabilities; do not activate it for generic code review or merely because a repository contains Dolgorae files."
---

# Use Dolgorae

Dolgorae is a local durable control layer for Codex runs. This revision of the
skill covers workspace and profile readiness, immutable review targets, and
one-shot Specialist Review. It does not provide a general orchestration,
persistent-Run, or Specialist Policy workflow.

## Establish current authority

1. Confirm that the user explicitly asked to use, inspect, configure, diagnose,
   or recover Dolgorae. Repository policy or a `.dolgorae/` directory establishes
   availability, not authorization for a mutation or external review.
2. Resolve one executable and keep using that exact path:

   ```sh
   command -v dolgorae
   dolgorae --version
   dolgorae runtime capabilities
   ```

   Dolgorae emits its canonical machine envelope by default. Do not add an
   invented `--json` flag. Use `--human` only for interactive presentation and
   never parse it. If the executable is absent, report that fact; do not install
   or upgrade it automatically.
3. Compare the version envelope with `data.dolgorae_version` from capabilities
   and with any version or artifact identity pinned by the caller repository.
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
   expression. Do not substitute patch or stdin semantics, which Dolgorae does
   not provide.
3. Inspect the named Runtime Profile and run its offline diagnosis before an
   external review when current readiness is not already established:

   ```sh
   dolgorae profile show <profile> --workspace <path>
   dolgorae profile doctor <profile> --workspace <path>
   ```

   `profile doctor` reports its verdict in `data.compatibility` and diagnostics;
   envelope `ok:true` means the check ran, not that the profile is compatible.
   Read [configuration.md](references/configuration.md) when a profile must be
   created or changed. Specialist Policy operations remain unavailable in this
   release even though their future command grammar is parsed.
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
5. Prefer the composed `specialist review` operation. It owns capture, one fresh
   Reviewer, checked result collection, integrity verification, and settlement.
   Do not replace it with manual `review-target capture` and `settle` merely to
   reproduce the same workflow.
6. Accept success only from the complete checked result. Keep the target and
   Reviewer identities, verdict and findings, engagement state, settlement
   state, capture-time source identity, integrity evidence, and
   `workflow_issued_source_mutation:false` distinct. Empty findings do not make
   a failed, unknown, active, malformed, or unsettled review successful.

The optional attached tool named `dolgorae_review` may be used only when the
host actually exposes it. Its registration is the adapter's checked disposition;
never infer availability from the binary, a prior session, an MCP process, or
conversation memory. Follow its current tool schema and preserve the host-bound
request identity. When the tool is absent, use the Machine CLI. Never start or
invoke the hidden adapter entrypoint from the shell to make the tool appear.

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
- This skill does not operate general `run`, `engagement`, workspace-writer,
  Specialist Policy, or Dolgorae-orchestrated session flows. If one is
  requested, inspect current capabilities and repository authority, report that
  it is outside this skill's present workflow, and do not improvise from
  planned command grammar.

Read [recovery.md](references/recovery.md) after stale state, response loss,
unknown outcome, integrity failure, or profile/capture recovery guidance.
