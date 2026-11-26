# Dolgorae Gate C and Gate D: Three-Consumer Qualification and Release Handoff

Scope: `EPIC-022` (`TASK-088` and `TASK-089`) independent three-consumer
qualification (combined Gate C, unlocking actual-Gul `MILESTONE-BH1`) and final
release-ready handoff provenance (Gate D) for the Dolgorae `v0.1.4` and Gul
`v0.1.0` pair. The [roadmap](../roadmap/README.md) owns delivery lifecycle. The
frozen [H1 acceptance matrix](dolgorae-consumer-acceptance-v014.md),
[Gate A acceptance](dolgorae-v014-gate-a-acceptance.md), and
[current consumer qualification](dolgorae-v014-current-consumer-qualification.md)
retain their original snapshots and scopes.

Neither Gate C nor Gate D authorizes Git staging, commits, pushes, tags,
GitHub Releases, host installation, production cutover, external Sorage
transmission, or consumer default-backend activation. Those boundaries retain
separate explicit authorization.

Post-`EPIC-022` `v0.1.4` scope expansion (2026-11-24 to 2026-11-26): Master
subsequently adopted `EPIC-024` (`TASK-095` through `TASK-098`) into the
unreleased `v0.1.4` boundary to remove remaining Codex/Claude runtime-upgrade
fragility. All historical `EPIC-022` checksums and verdicts below remain
authoritative historical provenance for `8fb6a3578ae3` / `b9d1750e9ec5`.
`TASK-098` (`EPIC-024`) has now completed superseding Gate C and Gate D
qualification and release-ready handoff on exact H3 `3d3355e8481e` paired with
Gul H4 `9175b40a0097` / `03d62d5dd158` (committed Gul pin `3e8b7ef3...` and
promoted Gul evidence `edca6f37...`) and Codex `0.172.0` (`Claude 2.1.332` for
external hosts).

## Superseding `EPIC-024` (`TASK-098`) Gate C and Gate D Acceptance

`TASK-098` supersedes the `EPIC-022` release-pair inputs for unreleased
`v0.1.4` / Gul `v0.1.0` without mutating frozen H1 (`0927cbf8...`), public-v2
(`c34b1da6...`), or historical `EPIC-022` records. Earlier pre-floor Gul H4
handoff results (`819dd50c9e78` / `bfac15c363bd` / `4e0b739c38cf`, Gul commit
`f0c6c62`, manifest SHA-256
`607a5d83af19797df6c9aea5da31190adc5776373a74183c6cfdf67fec60c392`) remain
separate historical provenance and are not inherited as same-image evidence for
the final pair.

### Superseding Immutable Release-Pair Inputs

Zero unqualified rebuilds occurred during `TASK-098`. Every qualified binary
and archive matched its exact SHA-256 on disk across complete gates and
independent cross-verification:

| Artifact | Identity / Revision | Verified SHA-256 |
| --- | --- | --- |
| Dolgorae H3 candidate binary | `dolgorae-gul-e16-h3-3d3355e8481e` (clean commit `4d69005fee698e96d2f5a879189d8f359ed24f2f`, tree `ebe6851efd705b77e191ebb0191a72ae3350ab6e`, 28,097,648 B) | `3d3355e8481e08c014409b7369d0e730f885a1ee75abd96efa7e0f7e1fefd017` |
| Dolgorae H3 archive / manifest | `dolgorae-v0.1.4-h3-darwin-arm64.tar.gz` (87 members) / `manifest.json` | Archive `025e4756aea0fdfcb5378afbfefaaa9daeab81a37cacdbc2fe6c1138b1bcbc7d` / Manifest `f9ed771e6d7b423d1e145978186feb87099ee9edf999cba9d7f4aa3ee12887fa` |
| Dolgorae H3 public-v2 descriptor | `dolgorae-public-v2.descriptor.pb` (65 methods, 68 codes, unchanged) | `c34b1da680ef7f8edfd8eb744047b0981311a4076b3c05c3d4adb4986565f8f5` |
| Immutable H1 contract archive / lock | Git `0927cbf87b0a3ba37c8509016c99f14c3f9f3c29` (`validate_frozen_h1.py` exit 0, 295 members) | Lock `5900393481ed200c6ef4eadab96b034d679f3e4cc6b0d42b764c4211c94a862a` / Archive `9b8427b6958b6ef9323dfcea1a512ee3ba070a9e594076613abfc98849012e3c` |
| Gul H4 source / 3-path pin commit | Committed pin `3e8b7ef3f3840b82b9e63260a2940d75806ccd20` / evidence `edca6f37a575526003035bb98277ce08f6208a60` (embedded package source `08bd4f161a50ef9fdc25ea475abe3c30fc0ff8c1` + exact 3-path H3 pin diff `contract/current-v2/pin.json`, `internal/gateway/development_candidate.go`, `test/native/live.py`) | 3-path diff equivalence `af3895ac91fd6e401f48aae7b1cbb80ce5d08910b309565dbee4b95cc87430e2` / Contract lock `acd95a6f7aa236bd6425ed68afad5f6942dfbac51e7f5b60680e7cabcc047360` |
| Gul H4 production executable | `Gul.app/Contents/MacOS/gul` (49,953,298 B) | `9175b40a00971e34c6743de341ed7cda09d2913504ddd91c3f5986c8feaff582` |
| Gul H4 production metadata | `Gul.app/Contents/Info.plist` / `package-manifest.json` | `c99a136b69400ad91e46357a2f23508ff887543794d8803aa282ef320cb5225d` / `f9a15b36d03480e7078a65a15ee537fc3ee075cea10974b2d1bc93a8d5bb15df` |
| Gul H4 candidate executable | `Gul Candidate.app/Contents/MacOS/gul` (49,953,298 B) | `03d62d5dd1581719ebc54aa8ebe44220c6e7c5e28809adff202e417b3b2ea074` |
| Gul H4 candidate metadata | `Gul Candidate.app/Contents/Info.plist` / `package-manifest.json` | `23960846f73663b54ba4be47a9a189f005c17008c8bec4bddbe3acf08be3e268` / `a4064ff357970b4454ed8e2b2d4ee8280955864590779355c17e3077876bd350` |
| Gul H4 archive | `gul-v0.1.0-h4-for-3d3355e8-darwin-arm64.tar.gz` (8 members) | `f9cfa01ad8755ac059bd20162f208d5ebdb48688afdd987dd5e40a3cb9a6e045` |
| Pinned Codex runtime | Codex `0.172.0` (`codex` / `codex-code-mode-host` / local `codex-exec-server`) | `f30a99714dc984876864239ef60563949b1e21421a7c9180422f51fd39186bc6` / `0694f0e5c606a0015786cb94fb527b9526b4d892574cf5ba0e6bc7618cbc0d9d` / `5add3be98ac625b1a6a2b3062f058d17a13466abf12e5c8a74795764f337622e` |
| Pinned external Claude & Podway | Claude `2.1.332` / Podway `0.2.15` binary & adapter catalog | Claude `5b8214f8f082f3187420b27b4063b9b4f4d9412cd66ea436d6d1966ad582700f` / Podway `dece7fecd90eaf6130b330e2cb70e98532f3e6b73950227cc2ff935a8be8e5ae` (catalog `768adc7f0ac6bf7bf973018fa66616bef3f449e0b942ccc76ba8db4666126a1d`) |

### Superseding `EPIC-024` Promoted Evidence Packages

- Gate C (`TASK-098`):
  [manifest.json](../../evidence/aquarium/EPIC-024/gate-c-qualification/3d3355e8481e08c014409b7369d0e730f885a1ee75abd96efa7e0f7e1fefd017/manifest.json)
  (`sha256:2b5006459062568a7978a881e4ff8d66b16fb2846c1b380c5cf31f4c9b88a6dd`),
  containing
  [matrix.json](../../evidence/aquarium/EPIC-024/gate-c-qualification/3d3355e8481e08c014409b7369d0e730f885a1ee75abd96efa7e0f7e1fefd017/matrix.json)
  and
  [cross-verification.json](../../evidence/aquarium/EPIC-024/gate-c-qualification/3d3355e8481e08c014409b7369d0e730f885a1ee75abd96efa7e0f7e1fefd017/cross-verification.json).
- Gate D (`TASK-098`):
  [manifest.json](../../evidence/aquarium/EPIC-024/gate-d-handoff/3d3355e8481e08c014409b7369d0e730f885a1ee75abd96efa7e0f7e1fefd017/manifest.json)
  (`sha256:c2e732d2b35440c0d469812b8a828ac6f5ec4fe3244ea6f7709fa79c67e6dfe8`),
  containing
  [provenance.json](../../evidence/aquarium/EPIC-024/gate-d-handoff/3d3355e8481e08c014409b7369d0e730f885a1ee75abd96efa7e0f7e1fefd017/provenance.json)
  and
  [release-readiness.json](../../evidence/aquarium/EPIC-024/gate-d-handoff/3d3355e8481e08c014409b7369d0e730f885a1ee75abd96efa7e0f7e1fefd017/release-readiness.json).
- Upstream Gul Phase 4 H4 package (verified cross-repository, pin commit `3e8b7ef3...`, evidence commit `edca6f37...`):
  `gul:evidence/aquarium/E16-T16/external-handoff/9175b40a00971e34c6743de341ed7cda09d2913504ddd91c3f5986c8feaff582/manifest.json`
  (`sha256:a3e96ad798e1a0c792d3aed07c4cb2c1281ab5ded773ff67e1a0cbfabf1095fd`).

### Superseding Three-Consumer Qualification and Gate D Limits

Independent cross-verification (`R-D`: `18/18 MET`, `READY_FROM_R_D`; `R-G`: `6/6` Gul rows `MET`, `ready_for_gate_c_d_consolidation`; and selected technical review: `18/18 MET`, `0` blocking product defects, `PROCEED_FOR_DOLGORAE_PROMOTION_AND_DOC_CLOSEOUT`) accepted the superseding pair across all 18 rows:

- **`AQ-01..06` (`PASS`, 6/6)**: Fresh upgraded external Codex `0.172.0` (`f30a9971...`) and Claude `2.1.332` (`5b8214f8...`) MCP dispatch, replay, conflict, ownerless permission/MCP declines, `14/14` ordinary-v3 plus `2/2` combined managed+external Goal Evaluator and Primary Decision cases (`db132845`), packaged H3 (`3d3355e8...`) online and cold `owner-executable-refresh` with open engagements preserving original receipts and SQLite bytes, focused retries for the two initial `--assign` failures (`rounds-retry`, fixture-only drain `eb3c7773...`), accountless Controller approval suites, and `make validate-agent-skills` (`10/10`).
- **`PW-01..06` (`PASS`, 6/6)**: Pinned Podway `0.2.15` (`dece7fec...`, `124` assets, `88` schemas, `DOLV3-001..009`) Direct lifecycle and reactivation, `0.2.16` `WORKSPACE_CONTRACT_MISMATCH` isolation, caller-scoped `Managed` (`0.172.0` fixture repin `d2c8b9f1...`) and `External` (`Claude 2.1.332`) cold post-refresh receipt preservation on packaged H3 (`3d3355e8...`), round/release accounting, and group/merge/synthesis provenance with negative group-as-Action refusal.
- **`GU-01..06` (`PASS`, 6/6)**: Exact H3 (`3d3355e8...`) + H4 (`9175b40a...` / `03d62d5d...`) loopback HTTPS smoke on both channels; deterministic `release-lifecycle` (11 Responses, 1 MCP decision, 2 writes, 2 closures, 0 processes); 250-turn `release-history` (500 items, 50 forward items, 276,066-byte original `ec45a77f...`, 1 closure, 0 processes); historical `8fb6a357` -> `3d3355e8` in-place upgrades (`U1` quiescent rollover and `U2` live-member `PROFILE_SERVER_BUSY` refusal -> owner close -> non-interrupting Operator migration -> original Gul data reopened ready); `U3` digest-only `config.toml` (`compact_prompt` stored only as `sha256:3f89773e...`, 0 raw hits across 20 + 75 scanned outputs); bounded isolated real-account interactive approval run (`CAN_EDIT`, `gpt-6.2-sol`, `effort=low` vs `high` config default, command card relative cwd `.`, settled `GetInteraction` refetch at revision 19, single UI `ACCEPT_ONCE` HTTP 200, resolved Gul attempt matching Dolgorae audit receipt `{decision: accept_once, outcome: approval}`, exact 5-byte file `sha256:f2c82dec...`, Gul restart/reattach with zero host-path exposure, read-only follow-up with unchanged hash, normal UI Close to `RUN_LIFECYCLE_CLOSED`, Dolgorae `closed` / `writer: none` / `lane: absent`, 40-record audit chain `bf1b5cb5...`, 0 surviving processes); and both repositories' complete gates exiting 0 under standard `umask 0022` (Dolgorae `1,771` unit tests + int + e2e; Gul full `make test` on `08bd4f16` + `af3895ac...`). `GU-04` and `GU-06` retain scope-limited predecessor `8904e66c` 30-minute soak and v12/v13->v20 restore provenance (`b1cdf868...`) under Master's 2026-11-11 and 2026-11-17 amendments.

Explicit limits retained for release owners (`RELEASE_READY_HANDOFF`):

1. **Internal dry-run provisioning floor limit ([DF-053](../deferred-feedback/README.md#df-053-align-internal-dry-run-provisioning-checks-with-the-fresh-codex-admission-floor))**: Public-v2 `ProvisioningService.ValidateProvisioning` and all install, hire, spawn, assign, and `StartRun` admission paths enforce Codex `>=0.172.0` (`CAPABILITY_UNSUPPORTED`), while internal dry-run `delegation_service::validate_provisioning`, `specialist_facade` `"validate_provisioning"`, and `source_binding::prepare_provisioning` check `>=0.158.0`. There is zero install or execution safety bypass; this dry-run mismatch is deferred to `DF-053` rather than triggering an unqualified H3 rebuild.
2. **Interaction revision double-bump UX race ([DF-054](../deferred-feedback/README.md#df-054-coalesce-approval-opened-journal-revision-bumps-to-remove-operator-refetch-race) / Gul `E16-FB-001`)**: `src/turn.rs:4792-4803` appends `ApprovalRequested` (rev 18) and `InteractionOpened` (rev 19) separately ~12 ms apart, so an immediate operator click before Gul's 5 s `GetInteraction` refetch settles rev 19 hits Gul's fail-closed `v2_interaction.go:446` `ErrConflict` pre-dispatch. Waiting for rev 19 succeeded HTTP 200 on first submission; coalescing the revision bump is deferred without rebuilding H3/H4.
3. **Upgrade and historical provenance limits**: `U1` and `U2` executed zero post-migration user turns; `U1` attempts 1–2 observed `ListProfiles` unavailable with an unestablished cause before attempt 3 passed unmodified; and predecessor `bfac15c3` metadata carried a one-character `codex-exec-server` checksum mismatch (`...b4062...` vs local `0.172.0` `...b3062...`), so `EPIC-024` pins only the verified local `0.172.0` binaries.
4. **Retained separate authorities**: Master's 2026-11-11 and 2026-11-17 suspended/amended checkpoints (performance benchmarks, physical devices, 24-hour/OS-sleep soak), no downgrade guarantee, and separate authorization for pushes, release QA, tags, GitHub Releases, installation, production cutover, Sorage transmission, and consumer default-backend activation remain unchanged.

---

## Historical `EPIC-022` (`TASK-088` / `TASK-089`) Immutable Release-Pair Inputs

Zero unqualified rebuilds occurred during `TASK-088` or `TASK-089`. Every
qualified binary and package matched its exact SHA-256 on disk before and after
independent cross-verification (superseding predecessor H3 `8d529950e597` / H4
`b6f459aef6a1` after adding single-shot policy-verified `turn/start` effort
override support for Codex `>=0.169.0`):

| Artifact | Identity / Revision | Verified SHA-256 |
| --- | --- | --- |
| Dolgorae H3 candidate binary | `dolgorae-gul-e16-h3-8fb6a3578ae3` (base `4e7f7d12c4fe93f51bd342c17b8e30b2612a5c44`, 27,965,632 B) | `8fb6a3578ae3719cf69acbb218d4ae2b1caff8fee60353846233aff79fbd532c` |
| Dolgorae H3 `source.patch` | `src/turn.rs` effort-override patch atop `4e7f7d12` | `0aa0bf22fae79f2ddae9ae62c93727065e65a7e782652bb3287da0092d5d4974` |
| Dolgorae H3 public-v2 descriptor | `dolgorae-public-v2.descriptor.pb` (65 methods, 68 codes, unchanged) | `c34b1da680ef7f8edfd8eb744047b0981311a4076b3c05c3d4adb4986565f8f5` |
| Dolgorae H3 `native_caller_pipeline` | Example binary paired with immutable H3 | `4fd3ef3ed3ed4b938341f2c134f3e7a0a79fd7285e977a305331208f9513d044` |
| Dolgorae H3 `verification.json` | Candidate verification record (`1,757` unit tests, 6/6 native suites, effort override) | `ece83dfb8879e8a54bfa1bf129d7ec2460e910f7f895f8a6b0a9ff87b7cddb19` |
| Immutable H1 contract archive / lock | Git `0927cbf87b0a3ba37c8509016c99f14c3f9f3c29` (`validate_frozen_h1.py` exit 0, 295 members) | Lock `5900393481ed200c6ef4eadab96b034d679f3e4cc6b0d42b764c4211c94a862a` / Archive `9b8427b6958b6ef9323dfcea1a512ee3ba070a9e594076613abfc98849012e3c` |
| Gul H4 production executable | `Gul.app/Contents/MacOS/gul` (base `a13fd9da78ba5af72fb256f5e67278986e4e45d9` + new H3 pin) | `b9d1750e9ec564f73fc11c92dd8be30d25c84818e6b333c837c7eeb1a70b2506` |
| Gul H4 production metadata | `Gul.app/Contents/Info.plist` / `package-manifest.json` | `c99a136b69400ad91e46357a2f23508ff887543794d8803aa282ef320cb5225d` / `64e5fe8722e8ffd4c87763a1c816a6252a77083da913bc57bd010b28b8ec6194` |
| Gul H4 candidate executable | `Gul Candidate.app/Contents/MacOS/gul` | `6cd9901ae3fefc17a5dbc01823277bade5f0a8bfa32d7313290b343ffcf541d2` |
| Gul H4 candidate metadata | `Gul Candidate.app/Contents/Info.plist` / `package-manifest.json` | `23960846f73663b54ba4be47a9a189f005c17008c8bec4bddbe3acf08be3e268` / `c2745bd95d16b51c8823b75c8dda0bbfe764d7dc7df128281251d7cd69300242` |
| Gul H4 frontend & contract locks | Embedded frontend manifest / `contract/generated/generated-lock.json` | `b5cc0b862adb9aa35364270dde1d1e8e58085839ece8c90b36a9195be8f43132` / `acd95a6f7aa236bd6425ed68afad5f6942dfbac51e7f5b60680e7cabcc047360` |
| Pinned Codex runtime | Codex `0.169.0` (`codex` / `codex-code-mode-host`) | `739cda86e15d30a6e2b40cf8dcbbfb988aaf48b305b27065573bb8e9fd41d002` / `10feae2abe764bcf2273e241ace67aecc360eb8c28a82a2988bf1d8498a9bacf` |

## Promoted Evidence Packages

Machine-readable `aquarium.promoted-evidence/v1` packages carry the complete
Gate C and Gate D records without citing local runtime directories, raw logs, or
credentials:

- Gate C (`TASK-088`):
  [manifest.json](../../evidence/aquarium/EPIC-022/gate-c-qualification/8fb6a3578ae3719cf69acbb218d4ae2b1caff8fee60353846233aff79fbd532c/manifest.json)
  (`sha256:7f8f4f0628dbec47cacc16437cd7259fef8ec747ded799637573cbdb89db9ee8`),
  containing
  [matrix.json](../../evidence/aquarium/EPIC-022/gate-c-qualification/8fb6a3578ae3719cf69acbb218d4ae2b1caff8fee60353846233aff79fbd532c/matrix.json)
  and
  [cross-verification.json](../../evidence/aquarium/EPIC-022/gate-c-qualification/8fb6a3578ae3719cf69acbb218d4ae2b1caff8fee60353846233aff79fbd532c/cross-verification.json).
- Gate D (`TASK-089`):
  [manifest.json](../../evidence/aquarium/EPIC-022/gate-d-handoff/8fb6a3578ae3719cf69acbb218d4ae2b1caff8fee60353846233aff79fbd532c/manifest.json)
  (`sha256:b3a9cb2082c060540a32d0fc195a5e341121b8b3264504f6a7df2e1e97241c56`),
  containing
  [provenance.json](../../evidence/aquarium/EPIC-022/gate-d-handoff/8fb6a3578ae3719cf69acbb218d4ae2b1caff8fee60353846233aff79fbd532c/provenance.json)
  and
  [release-readiness.json](../../evidence/aquarium/EPIC-022/gate-d-handoff/8fb6a3578ae3719cf69acbb218d4ae2b1caff8fee60353846233aff79fbd532c/release-readiness.json).
- Upstream Gul Phase 4 H4 package (verified cross-repository):
  `gul:evidence/aquarium/E16-T16/external-handoff/b9d1750e9ec564f73fc11c92dd8be30d25c84818e6b333c837c7eeb1a70b2506/manifest.json`
  (`sha256:adc0963f7ce8130162caed32bb62572a68a1d8e9e541f85ed18a6abaf2aed5f6`).

## Gate C (`TASK-088`) and `MILESTONE-BH1` Acceptance

Independent read-only cross-verification (`R-D`) accepted the immutable
H3/H4/Codex `0.169.0` pair with zero blocking findings, unlocking actual-Gul
`MILESTONE-BH1` for this exact pair:

1. **Accountless native deterministic suite (`release-lifecycle`)**: `native_pass: true`, `driver_exit: 0`, 11 scripted loopback Responses requests, 1 MCP decision, 2 closed public Runs, `physical_absence: true` (`absence_samples: 5`), `driver.stdout` SHA-256 `43469f8175ad3a0a5150928abad812cb5bfe24a16b7d8c1e4f76ff4a6abb3d1a`.
2. **Accountless native long-history suite (`release-history`)**: `native_pass: true`, `driver_exit: 0`, 250 consecutive turns with bounded 500-item paging and oversized original digests, 1 closed public Run, `physical_absence: true` (`absence_samples: 5`), `driver.stdout` SHA-256 `3e0bd332d585890572f6ae5d7a8f6d2f08569d620feae8cc5e48bf3d53398d53`.
3. **Accountless real Codex `0.169.0` effort-override loopback verification**: `CODEX_HOME/config.toml` `model_reasoning_effort = "high"`, Run started with `--effort low`, Turn 1 (`low`) -> Turn 2 (`medium`) -> Turn 3 (`high`) -> clean `run close` (`seen_responses_efforts: ["low", "medium", "high"]`, all `completed`).
4. **Bounded isolated real-account verification with `effort="low"` (≠ `config.toml` default `high`)**: `real_account_pass: true` on immutable H4 `Gul Candidate.app` (`6cd9901a...`), immutable H3 (`8fb6a357...`), and pinned Codex `0.169.0` (`739cda86...`), strict loopback HTTPS without Tailnet, 1 `CAN_EDIT` Session (`gpt-6.2-sol`, `effort: "low"`), 2 user Turns: Turn 1 path-safe UI command approval (`ACCEPT_ONCE`, relative cwd, zero host-path leaks) producing exact 40-byte file `sha256:465824fa473d20e8189c9fee8d033b5886fb608909cfe250621f6d2d44e3a6e3`; Gul restart and live reconnect; Turn 2 read-only follow-up; normal UI Close settling to Gul SQLite `RUN_LIFECYCLE_CLOSED`, Dolgorae `state: "closed"`, `writer_authority.state: "none"`, `server_lane.state: "absent"`, `event_cursor: "36"`, `audit.jsonl` (36 records) SHA-256 `355e6019674c9af8314834e8c5a15b18b9d69a7af013ac5c76fb7298683574e0`, and 0 surviving owned processes.

### Three-Consumer Matrix Summary (`AQ-01..06`, `PW-01..06`, `GU-01..06`)

All 18 mandatory rows pass with explicit separation of fresh H3/H4 runs from
inherited unchanged-interface qualification:

| Row Group | Verdict | Provenance and Applicability |
| --- | --- | --- |
| `AQ-01..06` (Aquarium) | `PASS` (6/6) | Inherited `TASK-086`/`TASK-087`/`d40303b4df3a` external Codex/Claude stdio MCP, ordinary-v3 (`aquarium.executor-handoff/v3`, `aquarium.evaluator-assessment/v3`), dispatch/result-loss recovery, and ownerless policy decline qualification under unchanged facade-v5/hook contracts, paired with fresh H3 `make test` (`1,757` unit + int + e2e) and 6/6 accountless `native_caller_pipeline` suites on `8fb6a3578ae3`. Dormant review backend stays dormant. |
| `PW-01..06` (Podway) | `PASS` (6/6) | Inherited `TASK-086`/`TASK-087`/`d40303b4df3a` live Podway `0.2.15`/`0.2.16` (`DOLV3-001..009`, `WORKSPACE_CONTRACT_MISMATCH`), `dolgorae.external/1` and `dolgorae.managed/1` receipts, `max_rounds`/retain/release accounting, cold-process recovery, and group-as-Action refusal under unchanged adapter v3/manifest v2/Store v15 contracts, paired with fresh H3 `make test` and `native_caller_pipeline` suites on `8fb6a3578ae3`. |
| `GU-01..06` (Gul) | `PASS` (6/6) | Fresh exact H3+H4+Codex `0.169.0` `release-lifecycle`, `release-history` (250 turns), accountless effort-override (`low`/`medium`/`high`), bounded isolated real-account run with `effort="low"` (≠ `config.toml` default `high`), immutable `Gul.app` & `Gul Candidate.app` isolated loopback HTTPS smoke/`lsof`/CSP checks, full serial Gul `make test`, and `validate_current_v2_contract.py` (65 methods, 68 codes). `GU-04` and `GU-06` retain inherited predecessor `8904e66c` 30-minute 3-client fault/reconnect soak (`1800.005s`) and v12/v13->v20 backup/restore evidence under Master's 2026-11-11 and 2026-11-17 amendments. |

## Gate D (`TASK-089`) Release-Ready Handoff and Known Limits

Both repository complete gates passed without mutating the immutable binaries:

- **Dolgorae complete gate**: `make PYTHON_BIN=.venv/bin/python TEST_THREADS=2 test` (`test-prepare`, `test-unit` [1,757 tests], `test-int`, `test-e2e`) passed exit 0, plus 6/6 accountless `native_caller_pipeline` suites, accountless Codex `0.169.0` effort-override verification, and read-only validators (`validate_frozen_h1.py`, `validate_current_v2_contract.py`, `validate_markdown.py`, `validate_agent_skills.py`).
- **Gul complete gate**: serial `make test`, `bash scripts/check-sot.sh`, `node scripts/test-sot.mjs`, and `make contract-check` passed exit 0, with independent review `E16_CLOSEOUT_PASS`.

Explicit limits retained for release owners:

1. **Explicit Codex runtime pin**: All verified runs explicitly pinned Codex `0.169.0` (`739cda86...`) and companion (`10feae2a...`) rather than relying on host `PATH`.
2. **Suspended/amended checkpoints**: Performance/latency benchmarks, profiling, load tests, physical-device campaigns, and 24-hour/OS-sleep soak remain suspended or amended per Master's 2026-11-11 and 2026-11-17 directives and are not claimed as passed.
3. **External AQ/PW host runs**: Live external Codex/Claude MCP hosts and live Podway CLI were not re-executed on `8fb6a3578ae3`; applicability relies on unchanged external facade/adapter/hook contracts verified alongside fresh H3 contract and native caller suites.
4. **No downgrade guarantee and separate release authority**: No provider downgrade guarantee is added; separate release workflows and Master authorization govern commits, pushes, tags, GitHub Releases, host installation, and production cutover.
