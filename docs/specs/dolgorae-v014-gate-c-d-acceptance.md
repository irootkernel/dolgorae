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

## Immutable Release-Pair Inputs

Zero unqualified rebuilds occurred during `TASK-088` or `TASK-089`. Every
qualified binary and package matched its exact SHA-256 on disk before and after
independent cross-verification:

| Artifact | Identity / Revision | Verified SHA-256 |
| --- | --- | --- |
| Dolgorae H3 candidate binary | `dolgorae-gul-e16-h3-8d529950e597` (base `a6d1ce0d54d6336f2cff0f2725f71f576103ea45`, 27,965,648 B) | `8d529950e59770d29e11c691e7079145861f35c250974d376881d304fc01d168` |
| Dolgorae H3 `source.patch` | Tier 1 H3 code/contract patch atop `a6d1ce0` | `27013582d220e627ca1e46846cf25faea8eeffb467ec62d213d4157d6af39212` |
| Dolgorae H3 public-v2 descriptor | `dolgorae-public-v2.descriptor.pb` (65 methods, 68 codes) | `c34b1da680ef7f8edfd8eb744047b0981311a4076b3c05c3d4adb4986565f8f5` |
| Dolgorae H3 `native_caller_pipeline` | Example binary paired with immutable H3 | `23f8a5f3823abd1b8defdb01089612d0921f03b7088ce8a083936370ac755b96` |
| Dolgorae H3 `verification.json` | Phase 1 candidate verification record | `0054c4f74ced4a33022271517d3ca708abf877de8d29624703fac9a3b1246978` |
| Immutable H1 contract archive / lock | Git `0927cbf87b0a3ba37c8509016c99f14c3f9f3c29` (`validate_frozen_h1.py` exit 0, 295 members) | Lock `5900393481ed200c6ef4eadab96b034d679f3e4cc6b0d42b764c4211c94a862a` / Archive `9b8427b6958b6ef9323dfcea1a512ee3ba070a9e594076613abfc98849012e3c` |
| Gul H4 production executable | `Gul.app/Contents/MacOS/gul` (base `fcf6e5fd6f061ee1222a2280ca8e27e22a4ac894` + Profile 2 fixes, 49,771,666 B) | `b6f459aef6a1f3c224d1dbd5b16e44e6376ba7dfee72595b9e1008215c156088` |
| Gul H4 production metadata | `Gul.app/Contents/Info.plist` / `package-manifest.json` | `c99a136b69400ad91e46357a2f23508ff887543794d8803aa282ef320cb5225d` / `d1403e09e387b00e85d5b1235d3c32e5552e1fe841fdebc5f671ad4c53b564d2` |
| Gul H4 candidate executable | `Gul Candidate.app/Contents/MacOS/gul` (49,771,666 B) | `7e7c0c8f71c30b8bb474dbb7ddf946c1009b88cf3a184d77a857d2b7991d904d` |
| Gul H4 candidate metadata | `Gul Candidate.app/Contents/Info.plist` / `package-manifest.json` | `23960846f73663b54ba4be47a9a189f005c17008c8bec4bddbe3acf08be3e268` / `b6a5a5bd6c423bb8b26ecd0b74748e8acb71d22afebfea755379df0c13a1c777` |
| Gul H4 frontend & contract locks | Embedded frontend manifest / `contract/generated/generated-lock.json` | `b5cc0b862adb9aa35364270dde1d1e8e58085839ece8c90b36a9195be8f43132` / `acd95a6f7aa236bd6425ed68afad5f6942dfbac51e7f5b60680e7cabcc047360` |
| Pinned Codex runtime | Codex `0.169.0` (`codex` / `codex-code-mode-host`) | `739cda86e15d30a6e2b40cf8dcbbfb988aaf48b305b27065573bb8e9fd41d002` / `10feae2abe764bcf2273e241ace67aecc360eb8c28a82a2988bf1d8498a9bacf` |

## Promoted Evidence Packages

Machine-readable `aquarium.promoted-evidence/v1` packages carry the complete
Gate C and Gate D records without citing local runtime directories, raw logs, or
credentials:

- Gate C (`TASK-088`):
  [manifest.json](../../evidence/aquarium/EPIC-022/gate-c-qualification/8d529950e59770d29e11c691e7079145861f35c250974d376881d304fc01d168/manifest.json)
  (`sha256:1b2a74abde054446e4f21fb028c06328f1ba1e535ba4ea6fce54517c0ece646d`),
  containing
  [matrix.json](../../evidence/aquarium/EPIC-022/gate-c-qualification/8d529950e59770d29e11c691e7079145861f35c250974d376881d304fc01d168/matrix.json)
  and
  [cross-verification.json](../../evidence/aquarium/EPIC-022/gate-c-qualification/8d529950e59770d29e11c691e7079145861f35c250974d376881d304fc01d168/cross-verification.json).
- Gate D (`TASK-089`):
  [manifest.json](../../evidence/aquarium/EPIC-022/gate-d-handoff/8d529950e59770d29e11c691e7079145861f35c250974d376881d304fc01d168/manifest.json)
  (`sha256:909ec7f8895e3017b2867552588a9dee22fcc1eff02b5ad8944fa17cf97bc3a7`),
  containing
  [provenance.json](../../evidence/aquarium/EPIC-022/gate-d-handoff/8d529950e59770d29e11c691e7079145861f35c250974d376881d304fc01d168/provenance.json)
  and
  [release-readiness.json](../../evidence/aquarium/EPIC-022/gate-d-handoff/8d529950e59770d29e11c691e7079145861f35c250974d376881d304fc01d168/release-readiness.json).
- Upstream Gul Phase 4 H4 package (verified cross-repository):
  `gul:evidence/aquarium/E16-T16/external-handoff/b6f459aef6a1f3c224d1dbd5b16e44e6376ba7dfee72595b9e1008215c156088/manifest.json`
  (4 payloads verified: `coverage.json` `da92bf3b...`, `native-inputs.json`
  `aa879d12...`, `observations.json` `9f84d4ae...`, `qualification.json`
  `740189db...`).

## Gate C (`TASK-088`) and `MILESTONE-BH1` Acceptance

Independent read-only cross-verification (`R-D`) returned `GATE_C_PASS` with
zero blocking findings on the immutable H3/H4/Codex `0.169.0` combination,
unlocking actual-Gul `MILESTONE-BH1` for this exact pair:

1. **Accountless native deterministic suite (`release-lifecycle`)**: `native_pass: true`, `driver_exit: 0`, 11 scripted loopback Responses requests, 1 MCP decision, 2 closed public Runs, `physical_absence: true` (`absence_samples: 5`), `driver.stdout` SHA-256 `4118a9f3e22648dd0fc47d0745892bcbdb2f5044860473e0a013752f77f36955`.
2. **Accountless native long-history suite (`release-history`)**: `native_pass: true`, `driver_exit: 0`, 250 consecutive turns with bounded 500-item paging and oversized original digests, 1 closed public Run (`event_cursor: "1760"`), `physical_absence: true` (`absence_samples: 5`), `driver.stdout` SHA-256 `90b2db9eaadfa427f4329f4ebb85b09515444e2f03ae9a4da88602c9a3dee43d`.
3. **Bounded isolated real-account verification**: `real_account_pass: true` on immutable H4 `Gul Candidate.app` (`7e7c0c8f...`), immutable H3 (`8d529950...`), and pinned Codex `0.169.0` (`739cda86...`), strict loopback HTTPS without Tailnet, 1 `CAN_EDIT` Session (`gpt-6.2-sol`, effort `high`), 2 user Turns: Turn 1 path-safe UI command approval (`ACCEPT_ONCE`, relative cwd, zero host-path leaks) producing exact 29-byte file `sha256:3c37b85cd7f25271b36036c8544f2e7c5dc9464dae2ec6c062feafb6688cc02b`; Gul restart and live reconnect; Turn 2 read-only follow-up; normal UI Close settling to Gul SQLite `RUN_LIFECYCLE_CLOSED`, Dolgorae `state: "closed"`, `writer_authority.state: "none"`, `server_lane.state: "absent"`, `event_cursor: "36"`, `audit.jsonl` SHA-256 `680aa67be87426b685308785ecaddaf80adbffdea8501ecd331fff5b9309533f`, and 0 surviving owned processes.

### Three-Consumer Matrix Summary (`AQ-01..06`, `PW-01..06`, `GU-01..06`)

All 18 mandatory rows pass with explicit separation of fresh H3/H4 runs from
inherited unchanged-interface qualification:

| Row Group | Verdict | Provenance and Applicability |
| --- | --- | --- |
| `AQ-01..06` (Aquarium) | `PASS` (6/6) | Inherited `TASK-086`/`TASK-087`/`d40303b4df3a` external Codex/Claude stdio MCP, ordinary-v3 (`aquarium.executor-handoff/v3`, `aquarium.evaluator-assessment/v3`), dispatch/result-loss recovery, and ownerless policy decline qualification under unchanged facade-v5/hook contracts, paired with fresh H3 `make test` (`test_trusted_host_setup_cli.py`, `test_external_engagement_cli.py`, `test_agent_skill_package.py`, `test_h1_package.py`) and 6/6 accountless `native_caller_pipeline` suites on `8d529950e597`. Dormant review backend stays dormant. |
| `PW-01..06` (Podway) | `PASS` (6/6) | Inherited `TASK-086`/`TASK-087`/`d40303b4df3a` live Podway `0.2.15`/`0.2.16` (`DOLV3-001..009`, `WORKSPACE_CONTRACT_MISMATCH`), `dolgorae.external/1` and `dolgorae.managed/1` receipts, `max_rounds`/retain/release accounting, cold-process recovery, and group-as-Action refusal under unchanged adapter v3/manifest v2/Store v15 contracts, paired with fresh H3 `make test` (`test_native_integrated_acceptance.py`), `native_caller_pipeline`, and `ReconcileStartOperation` regressions. |
| `GU-01..06` (Gul) | `PASS` (6/6) | Fresh exact H3+H4+Codex `0.169.0` `release-lifecycle`, `release-history` (250 turns), bounded isolated real-account run, immutable `Gul.app` & `Gul Candidate.app` isolated loopback HTTPS smoke/`lsof`/CSP checks, full serial Gul `make test`, and `validate_current_v2_contract.py` (65 methods, 68 codes). `GU-04` and `GU-06` retain inherited predecessor `8904e66c` 30-minute 3-client fault/reconnect soak (`1800.005s`) and v12/v13->v20 backup/restore evidence under Master's 2026-11-11 and 2026-11-17 amendments. |

## Gate D (`TASK-089`) Release-Ready Handoff and Known Limits

Both repository complete gates passed without mutating the immutable binaries:

- **Dolgorae complete gate**: `make PYTHON_BIN=.venv/bin/python TEST_THREADS=2 test` (`test-prepare`, `test-unit` [1,756 tests], `test-int`, `test-e2e`) passed exit 0, plus 6/6 accountless `native_caller_pipeline` suites on immutable H3 (`8d529950...`) and read-only validators (`validate_frozen_h1.py`, `validate_current_v2_contract.py`, `validate_markdown.py`, `validate_agent_skills.py`).
- **Gul complete gate**: serial `make test`, `bash scripts/check-sot.sh`, `node scripts/test-sot.mjs`, and `make contract-check` passed exit 0, with independent review `E16_CLOSEOUT_PASS`.

Explicit limits retained for release owners:

1. **Codex `0.169.0` effort-override behavior**: When a Run's `turn/start` supplies a `reasoning_effort` differing from the Profile `CODEX_HOME/config.toml` default, Codex `0.169.0` emits an unexpected `thread/settings/updated` notification before `turn/start` replies. Dolgorae's intentional fail-closed `CorrelationMismatch` guard classifies it as `outcome_unknown`. Match session effort to the Profile default for `v0.1.4`; effort-only relaxation is deferred post-`v0.1.4`.
2. **Explicit Codex runtime pin**: All verified runs explicitly pinned Codex `0.169.0` (`739cda86...`) and companion (`10feae2a...`) rather than relying on host `PATH`.
3. **Suspended/amended checkpoints**: Performance/latency benchmarks, profiling, load tests, physical-device campaigns, and 24-hour/OS-sleep soak remain suspended or amended per Master's 2026-11-11 and 2026-11-17 directives and are not claimed as passed.
4. **External AQ/PW host runs**: Live external Codex/Claude MCP hosts and live Podway CLI were not re-executed on `8d529950e597`; applicability relies on unchanged external facade/adapter/hook contracts verified alongside fresh H3 contract and native caller suites.
5. **No downgrade guarantee and separate release authority**: No provider downgrade guarantee is added; separate release workflows and Master authorization govern commits, pushes, tags, GitHub Releases, host installation, and production cutover.
