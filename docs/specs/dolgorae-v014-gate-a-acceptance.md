# Dolgorae Gate A: completed Gul foundation acceptance

Scope: TASK-069's independent acceptance of the completed Gul E15 H2 against
the immutable H1 contract. The [roadmap](../roadmap/README.md) owns lifecycle.
This acceptance opens EPIC-019's dependent provider corrections. It establishes
no production public-v2 support, H3 readiness or actual consumer qualification.

## Immutable inputs

| Input | Accepted identity |
| --- | --- |
| Dolgorae H1 source | `55ba623fce231192226d29ba718153e18b6644a3` |
| H1 lock | `docs/protocol/dolgorae-gul-consumer-v2.lock.json` |
| Raw H1 lock SHA-256 | `4af0ff5ee310d25564541004a4a45ffd600e4a1ca1c0bacfa986ba8cac80f049` |
| Gul containing H2 closeout | `ce5046879bcc5fbce2db87e7170d31035d9c22ce` |
| Gul validated foundation | `33a0910e85ca547a136fd6158600c45bf0298b92` |
| Gul validated Git tar archive SHA-256 | `003c15eefc6d6b0f0868280b8f1aa90571663219c378b3b910057029ca14e6ff` |
| Gul promoted manifest SHA-256 | `e81326467ed793dc9b965320a25a3b23e01a81d627ed0f8539209167a3659226` |
| Gul foundation payload SHA-256 | `0e9c468069b9160ca1bd1cfec9e401c89d1ab64f53ac7f49e0af00c4b5eea552` |

Read `docs/handoffs/E15-H2.md` and the
`evidence/aquarium/E15/external-handoff/003c15eefc6d6b0f0868280b8f1aa90571663219c378b3b910057029ca14e6ff/manifest.json`
package from the containing Gul commit. The payload's handoff-document digest
names the document at the validated foundation commit; the containing closeout
adds acceptance and lifecycle bookkeeping. The containing closeout and validated foundation have separate identities;
the closeout changes no foundation implementation or generated contract bytes.

Dolgorae verified the manifest and payload digests, the validated Git archive,
the local pin, scenario authority and generated lock, and all 154 H1 and 23
frozen-v1 artifacts against the producer Git objects. All eleven selected
external source checksums resolve at their committed revisions. The source
selection remains Aquarium `1e1e9293ade2f884757ee51a675d130e1acdaf04`, Podway
`4101a014a187d175e6c7cd53df9b671d613ba625` and Gul
`04ef1e577013439b9c691cd50410d665dd0c97b0`. Current checkouts and prepared
contract assets do not substitute for qualified runtime artifacts.

H1's requirements, methods, digest domains, schemas, policies, limits and
fixtures remain unchanged. Its source-bound documents and pending-delivery
fields describe the contract publication snapshot. This document and the
roadmap record the later H2 acceptance without rewriting that snapshot.

## Accepted contract and foundation scope

Gul E15, including retained E15-T1..T4 and permanent E15-T21, is completed with
its own whole-Epic acceptance and closeout. Dolgorae independently accepts the
following bounded results using its own checks of the completed H2.

| Foundation obligation | Independent acceptance basis |
| --- | --- |
| G4-1 | Heartbeat heads are advisory. Delivered cursors remain unchanged; stream end, invalid envelope, heartbeat timeout and resume handling are retained. |
| G4-2 | Immutable creation mode is separate from observed access. Migration 14 retains unknown legacy creation mode; trusted creation admission cannot overwrite a different accepted choice. |
| G4-3 | Mutation effect classification checks the original method, scope, key, generation and attempt. Current rejection cannot clear prior uncertainty; missing or unknown detail remains Unknown. |
| G4-4 | Authoritative snapshots/cards commit independently of history. History failure preserves the delivered checkpoint and degrades only history; closed interrupted history is explicitly recovered. |
| G4-5 | Refresh masks survive decoding and coalescing. Usage avoids authority/history reads; bounded history pages commit independently with opaque cursors and separate budgets. |
| G4-6 | Normal encoded pages fit 4 MiB below the 8 MiB transport ceiling. Metadata/chunks fit 1 MiB; complete larger content uses length/hash-bound artifacts. SETTLING uses bounded attention observation. |
| G4-7 | Safe command projection, optional exit status and occurrence time survive wire/storage/read presentation; private paths, raw arguments, diagnostics and reasoning are excluded. |
| Allocation and local queries | Six-stream foreground preemption, leases/fairness and cancellation-before-slot-reuse are preserved. Subject-scoped Get isolates unrelated corruption and bounds Unicode previews to 200 code points. |
| Retained foundation | Backup/preservation, development isolation, gateway reconnect/admission, authenticated reads and host/file boundaries retain their existing acceptance scope. Migration 13 bytes/digests are unchanged; migration 14 is additive. |

The checked contract reproduction and typed fixtures are reproducible from
Gul's pinned toolchain. Independent scoped race verification covers the current
provider, engine, storage and retained foundation packages. The source tests
include original-key uncertainty, advisory heartbeat, independent history,
foreground allocation, immutable creation mode, v13 upgrade/rollback,
bounded encoded pages and safe activity canaries. These account-free fixtures
exercise their foundation scope; they do not exercise a production provider.

## Watch, timing, capability and harness agreement

The Gul H2 expectations are accepted under H1's existing wire/client policies:

- Cancel and join the owned receive operation before reusing its stream slot.
  Resume from the committed delivered cursor; heartbeat heads neither advance
  it nor prove a gap. RUN_TERMINAL fetches final snapshots; SERVER_SHUTDOWN
  renegotiates before resume; SLOW_CONSUMER refreshes before resume. Transport
  loss is not Run failure. TASK-075 qualifies the core path and TASK-079 the
  required event/Interaction producers; Gul E16 qualifies the actual client.
- Measure latency from first observability at the checked provider public
  interface to browser paint, separately retaining provider availability,
  Gul commit/delivery and browser timing. Correlate the same scenario/event
  across monotonic local measurements and record cross-clock correlation and
  uncertainty in the harness evidence. `occurred_at` is occurrence provenance,
  not a presumed public-availability timestamp. No unpublished wire field or
  assumed synchronized wall clock is required to claim the latency bound.
- Admit only the actual instance's applicable required-method subset and
  descriptor digest. Invalidate admission on instance change. Contract or
  fixture availability cannot advertise unimplemented runtime capability;
  the Machine CLI is a diagnostic oracle, never Gul's production fallback.
- Gul E16-T3 owns the opt-in `make test-live` native harness. H3 supplies the
  exact Gul, Dolgorae and pinned Codex app-server artifact identities and
  checksums, isolated HOME/CODEX_HOME/workspace/Gul-data roots, scripted loopback
  Responses, scenario selectors and structured result/counter output.
  TASK-087 owns final H3 inputs. Actual native execution, account use and
  device qualification retain separate authorization and evidence.

The full Gul measurement remains ten minutes after declared warm-up with ten
sessions, three clients and at most six distinct viewed sessions. Answer and
approval visibility is bounded to 1 s, commands to 2 s and background attempts
to 10 s. Idle polling is every ten minutes; closed sessions have no background
provider polling. Excess foreground demand reports capacity-degraded freshness.
TASK-075 retains the separate component read/wait/database/stream measurements
and their fixed sample method; its completion does not require future pool or
group functionality and does not establish actual-Gul latency.

## Mandatory downstream owners and entry decision

Every AQ-01..AQ-06, PW-01..PW-06 and GU-01..GU-06 row in the immutable
[consumer acceptance specification](dolgorae-consumer-acceptance-v014.md)
remains mandatory. Aquarium retains its host-path qualification, Primary/Judge
semantic judgment and acceptance responsibilities. Podway retains structural
receipts, current fences, outer cursor and separate progression. Dolgorae's
TASK-077/086 implement and qualify the selected external/managed adapters;
TASK-087 binds their applicability to the complete post-cleanup H3 artifact.
V2DFC-017/018 changes require impact assessment before a changed Podway
selection; DOLV3-001..009 require actual-artifact proof. Prepared Podway bytes
are not released-runtime support.

Gul E16-T1/T2 retain CAS drafts, original operation recovery, immutable unknown
material and shared-attachment protection. E16-T3/T4 own early actual native
Gate B; E16-T7..T10 own the full activity/result/delegation product surfaces;
E16-T14/T15/T16 own assembled faults, security, final runtime/account/device,
24-hour deterministic soak and exact intended Gul v0.1.0 qualification.
TASK-088 independently accepts H4 and all mandatory final-artifact evidence
for combined Gate C; TASK-089 owns Gate D and the release handoff.

Gate A's contract/foundation prerequisite is satisfied by this independently
accepted complete H2. TASK-070 may follow completed TASK-069. Gul E16 remains
Planned until completed EPIC-019, EPIC-020, EPIC-009 and EPIC-021/TASK-087 supply
the complete runnable H3 and required Aquarium/Podway runtime evidence.
EPIC-019 completion alone does not complete Phase 3. Master retains broker
transmission, final Epic acceptance and separate release/installation authority.

## TASK-092 corrected Gate A candidate

Status: `ACCEPTED` by Main under Master's EPIC-023 delegation after independent
milestone and Task reviews and isolated contract checks. TASK-092 is `COMPLETE`.
EPIC-023 remains `ACTIVE` pending Master's whole-Epic acceptance; EPIC-020 and
TASK-076 remain `PLANNED`, with no new Task activated.

### Exact inputs

| Input | Accepted identity |
| --- | --- |
| Corrected Dolgorae H1 source | `0927cbf87b0a3ba37c8509016c99f14c3f9f3c29` |
| Raw H1 lock SHA-256 | `5900393481ed200c6ef4eadab96b034d679f3e4cc6b0d42b764c4211c94a862a` |
| Reproduced H1 archive SHA-256 | `9b8427b6958b6ef9323dfcea1a512ee3ba070a9e594076613abfc98849012e3c` (966,685 bytes; 295 files) |
| Gul completed candidate | `bf6f4e7657d3094a0d007ff34ea7a8fcecb928a7` |
| Gul Git tree | `93dc9a1c8e3a6d0789a3cb5d73021506a7ba7ba7` |
| Gul native Git archive SHA-256 | `3f5d6c431c1784bbf8804447d6401f010291e0953b27e9f5a86b41c06a4bb756` |
| Complete native correction-export JSON SHA-256 | `87eae83dc58eccfc76198ca607ae3303a82f0a7a441a4224a34b9d763a5a39fe` |
| Gul canonical `docs/handoffs/E17-H2.md` SHA-256 | `685b1c14ecb2993d257307342a9d738da6b203b0ef340951a2469a83681b886f` |

The native `gul.h2-correction-export/v1` verification recomputed and compared
the entire received JSON against the exact committed Gul candidate. Original
Gate A remains bound to Gul `33a0910e85ca547a136fd6158600c45bf0298b92` and H1
`55ba623fce231192226d29ba718153e18b6644a3`; the preceding acceptance text and
its historical scope are unchanged.

### Artifact and foundation classification

The corrected H1 adds 117 artifacts, changes 17, removes none and preserves
137 existing entries. All 271 selected artifacts and 23 frozen-v1 entries
retain their bound producer bytes. The eleven external pins, native feasibility
inputs, public-v2 Proto/descriptor and bindings, 64 public methods, 45 Gul
methods, 14 keyed methods and eight digest domains/vectors are unchanged by
consumer adoption.

The export's `generated_delta` covers only `contract/generated`: its file
inventory changes `policy/h1-source.json` and preserves the other 25 outputs;
`contract/generated/generated-lock.json` records that selection. Separately,
Gul `a0a6ac54037342610f06d7ac40d4f434e5c4cb8e` adds `SESSION_MODE_UNKNOWN` to
Gul's own `api/proto/gul/v2/gul.proto`, its generated Go/TypeScript clients and
the shared web bundle. Those Gul API/frontend changes are not Dolgorae public-v2
wire changes. Unknown creation provenance maps to Gul's explicit unknown mode.
Legacy history reduces an oversized page at the same
cursor within the original deadline, preserving the single-item refusal bound.

Gul `2ed2abdb6f5e2d47f9e7757f40ee5ccf41ee4bb5` settles retained image inputs
from the single engine's checked snapshots, retaining uncertain submissions
through ordinary idle observations. The focused foundation checks include image
settlement, unknown-mode presentation and bounded legacy-history regressions.

### Independent checks and inherited evidence

The following checks ran from a clean isolated clone of the actual `bf6f4e7`
delivery commit with the selected Python dependencies. All 294 unique selected
files, including the lock, matched committed Dolgorae `0927cbf` bytes.

| Check | Actual result |
| --- | --- |
| `python -I -B scripts/export-e15-h2.py --revision bf6f4e7657d3094a0d007ff34ea7a8fcecb928a7 --verify <received-json>` | Entire native export and native archive identity matched |
| `python -I -B scripts/test-export-h2.py` | All four cases passed from clean committed inputs |
| `scripts/h1-schema-check.sh check` and `scripts/h1-schema-check.sh test` | 112 fixed release cases and five integration cases passed |
| Direct calls to the delivered fixture's `ExportTests.commit` helper | Unchanged and repeated snapshots retained the delivery HEAD; a changed snapshot committed once and remained clean; an actual Git index-lock failure propagated |

Required P2 is resolved by Gul `bf6f4e7`. Its parent `8eebc625` failed before
any of the four export cases ran because setup attempted an empty commit.
The correction commits only when staged inputs differ. Direct Git comparison
confirmed that `scripts/test-export-h2.py` is the only changed file. The revised
native JSON differs only in `target` and `target_encoding`; its contract payload
and canonical `docs/handoffs/E17-H2.md` are unchanged. No Gul source was patched
during this independent qualification.

The following independent results carry forward from exact parent
`8eebc6254cdd34e4d8c826bd7b4bfa6bc454186a`. Their source and generated inputs
are byte-identical in `bf6f4e7`. They used Go 1.27.1, Node 26.7.0 and Bun 1.4.2
without dependency installation.

| Check | Actual result on the unchanged parent inputs |
| --- | --- |
| `make contract-check` | Generation drift, contract race tests and typed fixtures passed |
| `bash scripts/check-sot.sh` and `node scripts/test-sot.mjs` | SOT integrity and 55 negative cases passed |
| TESTING.md's nine foundation packages plus `internal/files`, with `go test -race -count=1` | All ten packages passed |
| `bun test frontend/src/read.test.tsx` and `bun run typecheck` | Five read tests and frontend/API typechecking passed |
| `node api/scripts/api-command.mjs check`, `node contract/port/generate-errors.mjs check` and `node frontend/scripts/check-bundle.mjs` | Gul API, error catalog and shared bundle drift checks passed |

Gul supplies a full serial `make test` exit-zero result from a committed snapshot
with the same `93dc9a1` tracked tree, before creation of the delivery commit.
That full gate was not independently repeated here. The previous three consumer
reviews predate the fixture correction. Subsequent independent TASK-092 reviews
and isolated contract checks support Main's delegated acceptance of this selection;
that member-Task acceptance does not establish final Epic acceptance. The dependency
remains Gul consuming the already delivered Dolgorae H1. Runtime release
settlement, Rust/DB implementation, H3/H4,
consumer hosts, accounts, devices, soak and production activation retain their
existing owners and qualification gates.
