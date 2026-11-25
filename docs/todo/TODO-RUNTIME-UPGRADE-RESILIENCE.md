# Runtime upgrade resilience and setup-bound host admission

## Control and authority

| Field | Value |
| --- | --- |
| Role | Adopted execution dossier for `EPIC-024` (`TASK-095` through `TASK-098`) |
| Adopted | 2026-11-24, under Master's approved `v0.1.4` scope expansion |
| Consumer Epic | `EPIC-024` (`ACTIVE`) |
| Release pair | Dolgorae `v0.1.4` and Gul `v0.1.0` |
| Lifecycle owner | [Roadmap](../roadmap/README.md); consult it for current execution status |
| Behavior owners | [Product specification](../specs/README.md), [delegation runtime](../specs/dolgorae-v014-delegation-runtime.md), and [three-consumer acceptance](../specs/dolgorae-consumer-acceptance-v014.md) |
| Structure and rationale | [Architecture](../architecture/README.md) and [ADR-043](../architecture-decision-records/README.md#adr-043-relax-additive-host-configuration-quiescent-upgrade-rollover-and-setup-bound-host-pins-for-runtime-upgrade-resilience) |
| Relationship to `EPIC-022` | Preserves completed historical `EPIC-022` (`8fb6a3578ae3` / `b9d1750e9ec5`) provenance in [Gate C and Gate D acceptance](../specs/dolgorae-v014-gate-c-d-acceptance.md) and gates `v0.1.4` publication on `EPIC-024`'s superseding H3/H4 pair and refreshed Gate C/D (`TASK-098`) |

Adoption records planning and target contracts only. It activates no Task,
changes no checked wire or protocol lock, and claims no implemented runtime
behavior or consumer acceptance.

## Problem statement and approved scope

Keeping `codex app-server` as Dolgorae's structured execution protocol preserves
the 45-method Gul consumer contract (`dolgorae.gul-consumer/v2` over the
65-method `current-v2` descriptor) and durable External Specialist Engagement
semantics, including clean final-answer extraction, typed Controller
interactions, event streaming, and crash reconciliation. Earlier `TASK-094`
(`28b7f74`, `ce04889`) already opened caller-workspace creation to Codex `>=
0.169.0`, dynamic sibling companion hashing, and single-shot active-turn effort
overrides.

Four concrete upgrade-fragility choke points remain in `HEAD` (`ce04889`):

1. **Closed `CODEX_HOME/config.toml` key rejection and broad `include` substring rejection**:
   unclassified top-level keys and any key matching `name.contains("include")`
   fail `configuration_snapshot` with `COMPATIBILITY_REJECTED`, breaking Profile
   operations whenever an upgraded Codex CLI writes a new setting or `include_*`
   feature toggle.
2. **Qualified/review Profile Server rollover denial**: modern caller-workspace,
   Orchestrated Session, Specialist, and direct-review admissions call
   `ensure_qualified_global_server` or `ensure_global_server_for_review` with
   `QuiescentRolloverPolicy::Deny`, returning `PROFILE_MIGRATION_REQUIRED`
   after any Codex binary or compatibility-manifest update even when zero live
   or orphan members exist.
3. **Hardcoded caller-scoped host hashes and strict `adapter_version` equality**:
   `src/trusted_host.rs` pins every caller-scoped (`caller_scope.is_some()`)
   `HostRoute::Codex` store to `0.164.0` (`d8990e3e...`), `HostRoute::Claude` to
   `2.1.310` (`ffb64331...`), and `adapter_version` to exact `CARGO_PKG_VERSION`,
   while binding `HostStore` and consumer receipts to a whole-record
   `configuration_digest` that breaks if the external host binary or Dolgorae
   version advances.
4. **Brittle version/schema/notification predicates**: `parse_version` rejects
   SemVer pre-release or build suffixes, `codex-0.157.0-required-subset.json`
   asserts `absent: true` on `TurnStartParams.developerInstructions`, and
   benign no-op `thread/settings/updated` notifications fail closed outside the
   single-shot effort-override window.

Master approved all four remediations inside the unreleased `v0.1.4` boundary
(refined across two rounds of independent `claude-opus-5-5` architecture review):

- **Release placement**: include `EPIC-024` in unreleased `v0.1.4` (and the
  cumulative `v0.2.0` boundary ahead of `EPIC-010`), requiring a superseding H3
  candidate, affected Aquarium/Podway/Gul H4 requalification, and refreshed
  Gate C / Gate D acceptance before release.
- **`config.toml` policy**: replace `contains("include")` with an explicit
  file-inclusion and reserved-key denylist (`include`, `includes`,
  `include_path`, `include_paths`, `config_include`, and reserved `_dolgorae_*`
  keys) while keeping malformed TOML and unsafe/oversized file rejections; admit
  unclassified top-level keys into `process_static_configuration` **only as
  deterministic domain-separated SHA-256 digests**
  (`_dolgorae_unclassified_sha256` over
  `"dolgorae-unclassified-config-v1\0" || utf8(key_name) || "\0" || JCS(value)`,
  keeping bare `profile doctor` offline and side-effect-free without stateful
  HMAC key-file rotation/reset coupling while preventing raw token/credential
  persistence across `state.json`, Run manifests, binding history, exports, and
  diagnostics) plus bounded key-name-only diagnostics with classification hints.
  Checked one-shot `specialist review` (`checked_review_configuration_supported`)
  still refuses a non-empty `_dolgorae_unclassified_sha256` map before Reviewer
  allocation to preserve its strict zero-unexercised-extension provenance, while
  ordinary caller-workspace, Gul, Orchestrated Session, and Specialist Runs
  admit it under `server_key` isolation.
- **Quiescent Profile Server rollover**: keep `ensure_qualified_global_server` on
  `Deny` for retained execution and `--confirm-server-key`, add a dedicated
  `ensure_qualified_global_server_for_admission` (`UpgradeWhenQuiescent`) for
  the exact new-admission call sites (`semantic.rs:3505` new `StartRun`,
  `semantic.rs:8789` new qualified external/brokered Specialist `StartRun`, and
  `semantic.rs:1134/1172` fresh MCP `prepare_reviewer`), thread `review_owner`
  through `automatic_quiescent_migration` -> `start_snapshot_for_migration` (with
  `Preexisting` on the concurrent-winner attach branch) for CLI
  `ensure_global_server_for_review`, and enforce SemVer §11 `candidate_version >=
  incumbent_version` on both `UpgradeWhenQuiescent` and legacy generic `Allow`
  (documenting that recovering a legacy pre-policy Run bound to an older version
  while a newer idle server owns the same `CODEX_HOME` requires explicit
  Operator stop/migration rather than silently downgrading the shared server).
- **Caller-scoped `trusted_host` admission and protocol resilience**: define the
  complete 4-cell `(OwnerKind x caller_scope)` matrix (`External + Some` and
  `Managed + Some` get fresh route floors `Codex >= 0.172.0` / `Claude >= 2.1.310`,
  non-executing `verify_executable` hardening, `schema_version: 2` split digest,
  and online `owner-executable-refresh`; `Managed + None` keeps exact root-Run
  `launch_snapshot` equality and refuses refresh; `External + None` stays
  refused). Preserve the non-execution boundary (operator-attested `version`
  floor + hardened `verify_executable` adding uid/root and non-group/world-writable
  checks), record `migrated_from_v1_digest` on `v1 -> v2` refresh so all five
  consumer modules (`host_invocation`, `managed_specialist`, `native_consumer`,
  `native_preparation`, `source_binding`) accept `{owner_identity_digest,
  migrated_from_v1_digest}`, gate refresh on concrete `HostStore` quiescence
  (`host.lock` acquired, no `STAGING`/sidecar/`*.pending` files, zero unsettled
  `attempted = 1` `native_calls` rows; durable open engagements never block
  refresh; overwritten old host binary on disk not required), return existing
  checked `RECOVERY_REQUIRED` (with internal `details.reason =
  "host_executable_refresh_required"`) on per-call metadata drift so zero new
  wire error codes are introduced, relax `adapter_version` to
  `recorded_adapter_version <= CARGO_PKG_VERSION` across both `v1` and `v2`
  stores, and enforce SemVer §11 / H1 checked-review / side-effect-free
  `thread/settings/updated` rules.

## Task breakdown and execution order

Execute sequentially in roadmap order: `TASK-095` -> `TASK-096` -> `TASK-097`
-> `TASK-098`.

### TASK-095: Digest-Only Unclassified Configuration Admission and Resilient Version/Schema Checks

- **Owning modules and contracts**: `src/profile.rs`, `src/global_runtime.rs`,
  `src/review.rs`, `src/turn.rs`,
  `docs/protocol/codex-0.157.0-required-subset.json`,
  `skills/use-dolgorae/SKILL.md`,
  `skills/use-dolgorae/references/configuration.md` (source-distributed in-repo
  only; no host/system skill mutation), and owning `SPEC-001`, `SPEC-003`,
  `SPEC-012`, `ADR-005`, `ADR-014`, and `ADR-043` sections. Note that editing
  `codex-0.157.0-required-subset.json` changes `compatibility_manifest_sha256`
  and recomputes `server_key` for every Profile.
- **Required behavior**:
  1. Change `profile_launch.configuration_policy.unknown_field` from
     `compatibility_rejected` to `conservative_process_static`. Replace
     `name.contains("include")` with an explicit denylist (`include`, `includes`,
     `include_path`, `include_paths`, `config_include`, and any key starting
     with reserved `_dolgorae_`) while keeping file-safety, mode-`0600`,
     ownership, size, and TOML syntax rejections. Never copy raw unclassified
     values into `ProfileSnapshot`: compute deterministic
     `sha256("dolgorae-unclassified-config-v1\0" || utf8(name) || "\0" || JCS(value))`
     per unclassified key (preserving offline side-effect-free `profile doctor`
     with zero stateful key files) and store only JCS-sorted
     `_dolgorae_unclassified_sha256: { "<key>": "<64-hex>" }` inside
     `process_static_configuration`, plus bounded key-name-only Profile
     diagnostics carrying a classification hint. Reject values that cannot be
     represented directly in canonical JSON, including nested TOML dates and times,
     non-finite floats, and numbers that would require a payload adaptation.
     Preserve JCS equivalence for accepted numeric values and omit refused
     values from diagnostics. In
     `checked_review_configuration_supported` (`checked_review_supported` and
     `accepted_checked_review_supported`), refuse any non-empty
     `_dolgorae_unclassified_sha256` map before Reviewer allocation so
     unclassified upstream extension surfaces cannot bypass checked-review
     provenance, while ordinary caller-workspace/Gul/Specialist Runs admit it.
  2. Parse SemVer `MAJOR.MINOR.PATCH[-prerelease][+build]` with SemVer §11
     precedence: compare `(major, minor, patch)` numerically (retaining existing
     leading-zero numeric component parsing such as `"0.166.00"`), order any
     pre-release strictly below its final release (`0.158.0-rc.1 < 0.158.0`,
     `0.169.0-rc.1 < 0.169.0 < 0.170.0-alpha.1`, `rc.9 < rc.10`), and ignore
     `+build` metadata for precedence while keeping the exact reported string in
     `ProfileSnapshot.codex_version`. Require an exact numeric release match
     without either suffix for `tested_runtime_schema`. Both
     `0.166.0-rc.1` and `0.166.0+local` stay `unverified`, and `0.166.00` retains
     the `0.166.0` schema pins. Refuse `-prerelease` or `+build` runtimes before launch on
     checked one-shot Specialist Review (`dolgorae-specialist-review-tool-v2.schema.json`,
     H1-locked `^[0-9]+\.[0-9]+\.[0-9]+$`) without falsifying version strings.
  3. Remove the negative `absent: true` constraint on
     `v2/TurnStartParams.json#/properties/developerInstructions` from the
     required-subset manifest while keeping Dolgorae's outbound `turn/start`
     payload free of `developerInstructions` and keeping generation-immutable
     developer instructions at `thread/start`, `thread/resume`, and
     `thread/fork`.
  4. In `TurnCoordinator`, accept `thread/settings/updated` notifications as
     audited no-ops via a side-effect-free verifier (never recording startup
     refusal predicates on probe mismatch) when `threadId` matches, the full
     workspace settings predicate set (`cwd`, `approvalPolicy`, `sandboxPolicy`
     `type`, `networkAccess`, `writableRoots`, `excludeSlashTmp`, and
     `excludeTmpdirEnvVar`) passes, `model` equals `fixed_model`, and `effort`
     equals the currently authoritative effort (the active turn's committed
     override while that turn runs, otherwise the Run's session effort),
     tolerating unknown additive `threadSettings` fields while keeping any drift
     or foreign-thread notification fail-closed. Accepted no-ops use the
     `thread_settings_update_noop` Profile diagnostic without retaining the
     notification payload. Attempt this write up to eight times per Coordinator
     lifetime. Skip it on contention or insufficient aggregate journal capacity
     and preserve Run state after any audit failure.
- **Verification**: unit and integration matrices for unclassified `config.toml`
  keys (`server_key` isolation, side-effect-free `profile doctor`, secret-canary
  absence across `state.json`, binding history, Run manifests, whole-Run export,
  and diagnostics, explicit `include`/`_dolgorae_*` rejection vs `include_*`
  toggle admission, checked-review refusal on non-empty
  `_dolgorae_unclassified_sha256` vs ordinary-Run admission, and live-member
  unclassified-key write behavior), SemVer §11 precedence + `tested` registry +
  H1 checked-review pre-release refusal, additive `TurnStartParams` fixtures,
  side-effect-free no-op vs drifting `thread/settings/updated` frames, complete
  default repository gate, and independent adversarial review.

### TASK-096: Quiescent Upgrade Rollover for Qualified and Successor Profile Servers

- **Owning modules and contracts**: `src/profile.rs`, `src/global_runtime.rs`,
  `src/semantic.rs`, `src/mcp_review_server.rs`, `src/review_recovery.rs`,
  `skills/use-dolgorae/references/lifecycle.md`,
  `skills/use-dolgorae/references/recovery.md` (source-distributed in-repo only;
  no host/system skill mutation), and owning `SPEC-003`, `SPEC-014`, `ADR-005`,
  `ADR-019`, and `ADR-043` sections.
- **Required behavior**:
  1. Keep `ensure_qualified_global_server` on `QuiescentRolloverPolicy::Deny`
     for `ensure_retained_global_server` (`semantic.rs:3407, 3503, 17788` and
     `prepare_legacy_reviewer` at `semantic.rs:1158`) and `profile server start
     --confirm-server-key` (`profile.rs:1371`). Introduce
     `ensure_qualified_global_server_for_admission` (`UpgradeWhenQuiescent`) and
     wire it at the exact new-admission call sites:
     - `semantic.rs:3505` (new caller-workspace / Orchestrated Session `StartRun`);
     - `semantic.rs:8789` (new external and brokered Specialist `StartRun` when
       `global_profile_binding.uses_qualified_launch_policy()`, keeping legacy
       non-qualified bindings on `ensure_global_server`);
     - `semantic.rs:1134/1172` (`prepare_reviewer` for fresh MCP
       `dolgorae_review`, split from `prepare_legacy_reviewer` at
       `semantic.rs:1158` via an explicit server-readiness parameter on
       `prepare_reviewer_with_binding`).
     (Provisioning validate/install remains offline and starts no server.)
     Switch `ensure_global_server_for_review` (`review_recovery.rs:212` ->
     `semantic.rs:1170`) to `UpgradeWhenQuiescent` and thread
     `review_owner: Option<&ReviewServerOwner>` through
     `automatic_quiescent_migration` -> `start_snapshot_for_migration` so
     `prepare_start` writes `Reserved` at new-epoch reservation, `commit_start`
     writes `Ready`, and the concurrent-winner attach branch (`profile.rs:2112-2124`)
     writes `Preexisting`.
  2. Under the retained singleton lock order (`home.lock` then binary-ordered
     `old/new server.lock`, carried forward in `ADR-043`), permit automatic
     durable migration from a `ready` incumbent to the requested contract only
     when:
     - the incumbent's membership journal and derived index on both keys are
       complete and contain zero live, starting, uncertain, or orphan Run
       members;
     - the incumbent's recorded process group, log drainer, and socket inode
       identities verify, and no stop reservation or `migration_blocked` fence
       is active;
     - SemVer §11 precedence satisfies `candidate_version >= incumbent_version`
       for the same canonical `CODEX_HOME` (enforced on both
       `UpgradeWhenQuiescent` and legacy generic `Allow`, which intentionally
       refuses silently downgrading a newer idle incumbent during pre-policy
       legacy Run recovery until an Operator explicitly stops or migrates that
       server).
  3. Preserve all existing migration compensation (`rolled_back` /
     `migration_blocked`) and binding-history recording guarantees. Keep `Deny`
     for `profile doctor --launch-probe`, explicit `--confirm-server-key`,
     downgrades (`candidate_version < incumbent_version`), live/orphan members,
     and retained qualified/successor Run execution.
- **Verification**: deterministic fault-barrier and multi-process E2E tests
  covering idle upgrade (`0.169.0` -> `0.169.1` / `0.170.0`) across CLI
  `StartRun`, external/brokered Specialist `StartRun`, CLI `specialist review`,
  and MCP `dolgorae_review`, manifest-change same-version rollover, downgrade
  refusal (including pre-policy legacy recovery against a newer idle incumbent
  and its explicit Operator stop/migrate recovery path), live/orphan member
  refusal, retained qualified Run bindings never reaching rollover, review
  rollover `Reserved`/`Ready`/`Preexisting` ownership + crash recovery +
  `retire_review_server` cleanup, complete default gate, and independent
  adversarial review.

### TASK-097: Minimum-Version, Non-Executing Setup-Bound Admission, and Split-Digest Refresh for Caller-Scoped Specialist Hosts

- **Owning modules and contracts**: `src/trusted_host.rs`,
  `src/host_invocation.rs`, `src/managed_specialist.rs`,
  `src/native_consumer.rs`, `src/native_preparation.rs`, `src/source_binding.rs`,
  `docs/specs/dolgorae-v014-delegation-runtime.md`, `docs/ops/README.md`,
  `docs/implementation-tips/README.md`, `ADR-041`, and `ADR-043`.
- **Required behavior**:
  1. Enforce the complete 4-cell `(OwnerKind x caller_scope)` matrix in
     `src/trusted_host.rs`:
     - `OwnerKind::External` + `caller_scope: Some(_)` (external MCP hosts) and
       `OwnerKind::Managed { session_ref }` + `caller_scope: Some(_)`
       (caller-scoped delegated sessions such as Podway on Codex or Claude):
       replace compiled-in `0.164.0`/`2.1.310` SHA-256 constants with
       operator-attested route minimum-version floors (`HostRoute::Codex >=
       0.169.0` under wire tag `codex-0.160.0`; `HostRoute::Claude >= 2.1.310`
       under wire tag `claude-code-2.1.295` using SemVer §11 precedence),
       hardened `verify_executable` setup binding, `schema_version: 2` split
       digest, and online `owner-executable-refresh`.
     - `OwnerKind::Managed { session_ref }` + `caller_scope: None` (internal
       root-Run managed specialist): retains exact equality with the root Run's
       immutable `launch_snapshot` executable (`trusted_host.rs:533-556`) and
       refuses `owner-executable-refresh`.
     - `OwnerKind::External` + `caller_scope: None`: remains unconditionally
       refused (`trusted_host.rs:432`).
  2. Preserve the documented non-execution setup boundary while hardening
     `verify_executable` (`host_invocation.rs:906-943`): neither
     `owner-config-create` nor `owner-executable-refresh` launches/execs the
     native host binary; instead, validate the operator-attested
     `executable.version` against the route floor and extend `verify_executable`
     with uid/root ownership and non-group/world-writable permission checks
     alongside its existing `O_NOFOLLOW`, `is_file`, `+x`, full-path
     `canonicalize(path) == path`, and pre/post `dev/ino/len/mtime` bracket
     around SHA-256. Run full SHA-256 hashing at setup, refresh, and
     `HostStore::launch_recipe`; on per-call `VerifiedOwner::load`/`revalidate`
     for caller-scoped `v2` stores, verify canonical path, `O_NOFOLLOW`
     ownership/permissions, and pinned `(dev, ino, len, mtime_sec, mtime_nsec)`,
     returning existing checked `RECOVERY_REQUIRED` (carrying internal
     `details.reason = "host_executable_refresh_required"`, preserving zero wire
     or error-contract drift) when the on-disk binary was updated.
  3. Advance new and refreshed caller-scoped `OwnerRecord` files to
     `schema_version: 2`, separating stable `owner_identity_digest` (canonical
     JCS SHA-256 over workspace, controller, route, owner kind, scopes, tools,
     caller scope, `native_host_root`, `controller_file`, and `adapter_id`,
     excluding `executable` and `adapter_version`) from `executable_binding`
     (`executable`, monotonic `executable_epoch`, and pinned `dev/ino/len/mtime`
     metadata) plus optional immutable `migrated_from_v1_digest: Option<String>`
     recorded when refreshing a `schema_version: 1` store. Expose
     `VerifiedOwner::matches_persisted_owner_digest` accepting
     `{owner_identity_digest, migrated_from_v1_digest}` so all five persisted
     digest consumers (`host_invocation` `metadata.owner_digest` at SQLite
     `schema_version = 1` without mutating `invocations.sqlite` on refresh,
     `managed_specialist`, `native_consumer`, `native_preparation`, and
     `source_binding`) preserve prior `v1` receipts, connections, preparations,
     partitions, and witnesses via a single atomic `owner.json` publish while
     writing `owner_identity_digest` on newly initialized `v2` stores and
     records. Relax `adapter_version` validation to `recorded_adapter_version <=
     CARGO_PKG_VERSION` (valid SemVer within the same `adapter_id`) for **both**
     `schema_version: 1` and `schema_version: 2` stores so Dolgorae upgrades do
     not strand unrefreshed `v1` stores, and retain unchanged `v1` (`0.164.0` /
     `2.1.310`) read/lookup/recovery (older binaries fail closed with
     `RECOVERY_REQUIRED` on `v2` stores).
  4. Add online administrative command
     `dolgorae __trusted-specialist-host owner-executable-refresh` (requiring
     live Operator and matching Controller descriptors, requiring no Dolgorae
     server shutdown, authenticating the stored `OwnerRecord` without requiring
     the already-overwritten old binary on disk, and gating on concrete
     `HostStore` quiescence: exclusive `host.lock` acquired, no `STAGING`/sidecar
     or `*.pending` files in `owner.root`, and zero `native_calls` rows with
     `attempted = 1` lacking a settled receipt — durable open External
     Engagements and hired Specialists never block refresh) to atomically
     publish `schema_version: 2` `owner.json` (`executable_binding` plus
     preserved `migrated_from_v1_digest` when upgrading from `v1`, leaving
     `invocations.sqlite` `metadata.schema_version = 1` untouched) to a verified
     `>=` prior version binary (or down to the route floor with explicit
     `--allow-downgrade`).
- **Verification**: unit, stdio fault-proxy, and native host-invocation E2E
  tests covering all four `(OwnerKind x caller_scope)` cells (upgraded `External
  + Some` and `Managed + Some` on Codex `>= 0.169.0` and Claude `>= 2.1.310`,
  `Managed + None` Run-binding retention and refresh refusal, `External + None`
  refusal), zero host-binary execution during setup/refresh, hardened
  `verify_executable` rejection cases, `RECOVERY_REQUIRED`
  (`host_executable_refresh_required` reason) on post-upgrade metadata drift,
  online `owner-executable-refresh` with open hired engagements and overwritten
  prior binary (`v1 -> v2` `migrated_from_v1_digest` replay across all five
  consumer modules, `--allow-downgrade` gate, unsettled `attempted = 1`
  refusal), `v1` and `v2` `adapter_version <= CARGO_PKG_VERSION` continuity,
  legacy `0.164.0` recovery, complete default gate, and independent adversarial
  review.

### TASK-098: Superseding v0.1.4 H3 Candidate, Three-Consumer Requalification, and Refreshed Gate C/D Handoff

- **Owning modules and contracts**: the Profile compatibility registry
  (`docs/protocol/codex-0.157.0-required-subset.json`), runtime admission and
  retained binding paths in `src/global_runtime.rs`, `src/profile.rs`,
  `src/trusted_host.rs` and the delegation owners, ADR-043,
  `docs/specs/dolgorae-v014-gate-c-d-acceptance.md`,
  `docs/todo/TODO-DOLGORAE-V014-RELEASE-ALIGNMENT.md`, `CHANGELOG.md`, and
  `evidence/aquarium/EPIC-024/` (preserving historical `evidence/aquarium/EPIC-022/`).
- **Required behavior**:
  1. Raise the Codex minimum to `0.172.0` for new qualified creation and
     caller-scoped host setup or refresh. Keep retained 0.169.x through
     0.171.x bindings and v2 owner records usable under their original rules.
     Register exact `0.172.0` for tested Profile compatibility with both schema
     digests and version-specific runtime evidence.
     Package the superseding intended-version `v0.1.4` H3 Dolgorae release
     candidate after `TASK-095`..`TASK-097` complete, verifying zero wire drift
     against immutable `H1` (`0927cbf8...`) and `dolgorae-public-v2.descriptor.pb`
     (`c34b1da6...`) and isolated skill packaging via `make validate-agent-skills`
     (without mutating host-installed skills).
  2. Execute and record full impact assessment and mandatory three-consumer
     requalification (`AQ-01..06`, `PW-01..06`, `GU-01..06`):
     - fresh external Codex (`>= 0.172.0`) and Claude (`>= 2.1.310`) MCP host
       qualification + online `owner-executable-refresh` with open engagements
       (`AQ-01..06`);
     - Podway `0.2.15`/`0.2.16` caller-scoped `Managed` adapter/receipt/refresh/recovery
       verification (`PW-01..06`);
     - Gul H4 adoption of the superseding H3 binary, deterministic
       `release-lifecycle` and 250-turn `release-history`, in-place upgrade from
       historical H3 (`8fb6a3578ae3`) across both quiescent automatic rollover
       and live-Gul-member operator-migration paths, digest-only unclassified
       `config.toml` smoke, and separately authorized bounded isolated
       real-account verification (`GU-01..06`).
  3. Complete independent cross-verification (`R-D` / `R-G`), promote bounded
     non-sensitive `aquarium.promoted-evidence/v1` Gate C and Gate D packages,
     update [Gate C and Gate D acceptance](../specs/dolgorae-v014-gate-c-d-acceptance.md)
     with the superseding release-ready H3/H4 pair (retaining `8fb6a3578ae3` as
     historical `EPIC-022` provenance), and close out `EPIC-024`.
- **Verification**: both repositories' complete gates exit 0 on the exact
  superseding H3/H4 binaries with zero unqualified rebuilds, `validate_frozen_h1.py`,
  `validate_current_v2_contract.py`, and `make validate-agent-skills` pass, all
  18 consumer rows pass independent review, and promoted evidence manifests
  verify cleanly.
