# Gomchi Pre-Implementation Technical Review — Round 4

Date: 2026-08-15
Snapshot note: this is the immutable round-4 input review. Owner dispositions
belong in `docs/reviews/` (per the established convention), so later fixes do
not rewrite the review that motivated them. At round start the round-3 input
review was relocated verbatim from this slot to
`docs/reviews/round-3-input.md` (SHA-verified identical) and the three inbound
references were updated; that archive and those link edits are uncommitted and
should be committed with this round's artifacts.
Scope: the full SOT at HEAD `266f8a2` (`docs/specs.md` 850 lines,
`docs/architecture.md` 637, `docs/architecture-decisions.md` 428,
`docs/roadmap.md` 347, `docs/todo.md` 71), the probe evidence
(`docs/probes/task-000.md` 268) and probe suite (`tools/probes/`, 11 Python
files + 1 C file), and the round-2/3 review record (`docs/reviews/`).
Weighted on technical and implementation feasibility, per request. This is the
first round to review the roadmap's task/verification design, the
implementation substrate, and the machine-output contract as first-class
surfaces.
Method: eight independent review lanes (concurrency/coordination, durability/
audit, Codex integration, mechanical matrices, completeness, implementation
substrate, roadmap/verification feasibility, round-3 disposition-truth audit),
followed by adversarial verification of every SOT-design candidate finding in
seven batches. Verification was refutation-first: every load-bearing quote
re-read at source, counter-definitions searched with recorded terms and hit
counts, a steelman written before any verdict, severity re-derived
independently. OS-level claims were checked empirically on the target platform
(macOS 26.5.2 arm64, SDK headers, scratch-space measurements only; repository
untouched): `F_SETLKWTIMEOUT`, `EVFILT_PROC`/`NOTE_EXIT` against live, zombie,
and reaped PIDs, `fstatfs`/`MNT_LOCAL`, and `proc_listpgrppids` buffer
semantics. Roadmap-layer findings were spot-checked against roadmap text.
Verification refuted two candidate claims outright and downgraded or
materially narrowed a dozen more; refuted claims are excluded, the demotions
are applied below, and the strongest refutations are recorded in §7 as
verified-sound design. Two findings below (H-11's contradiction, H-18's units
defect) were discovered by the verification pass itself.

## Verdict

**The round-3 patch held where round 3 aimed it.** The disposition-truth audit
verified 50 of 60 disposition claims outright with zero absent (§1), and the
concurrency/recovery mechanisms survived adversarial re-examination well:
byte-1 exclusivity genuinely prevents a second serving worker, the takeover
guard's CPU-progress predicate prevents wrong kills during replay, macOS
provides a direct timed-lock primitive (`F_SETLKWTIMEOUT` — measured), and the
two-byte handoff, four-verdict identity model, reconcile design, and ledger
design all stand at the mechanism level. Several round-4 candidate findings
against that layer were refuted in verification and are not in this review.

**The unreviewed surfaces did not hold.** The defect mass sits in four places
this project has never pointed a review at: (1) the **Codex-facing contract**
— the access-mode→sandbox/approval policy mapping, the pending-interaction
wire surface, the fork-boundary rule, and the required-subset manifest are
asserted but not written down, and the entire approval subsystem (three
lifecycle states, SPEC-009, TASK-008) rests on wire shapes no probe has ever
observed (C-2); (2) the **machine-output contract** — a machine-first CLI
whose per-command `data` payloads, `retryable` semantics, cursor/stream
contract, and export bundle are undefined (H-21..H-29); (3) **workspace
identity** — the digest that keys the single-writer promise has no algorithm,
preimage, encoding, or case/Unicode normalization rule, so two spellings of
one path can produce two "exclusive" writers (C-1); (4) the **verification
plan** — approvals are certified against a self-built fake five tasks before
the fake can emit server requests, seeds are named as a determinism mechanism
for kill races they cannot control, and no injectable clock exists while
TASK-013 bans the sleeps its siblings mandate (§3F, §6).

Feasibility **holds at B**. Nothing requires re-architecting: every fix below
is bounded document work plus a small probe campaign (~8 live probes, hours
each), and the substrate lane's verdict is that a runtime model satisfying all
18 derived constraints exists (threaded, no async runtime — advisory sketch in
§7). The distance to B+ is no longer concurrency correctness; it is contract
completeness — writing down what the design already intends — and a
verification plan that can actually fail for the right reasons.

Severity: **Critical** = cannot be implemented as written or defeats a core
promise; **High** = wrong-behavior path or blocks deterministic
implementation; **Medium** = rework/operational pain; **Low** = polish.
Tags: `patch-surface` (text the round-3 patch introduced or reshaped),
`pre-existing`, `planning` (roadmap/probe layer).

---

## Findings index (Critical and High)

| ID | Sev | One line | Primary doc | Tag |
| --- | --- | --- | --- | --- |
| C-1 | Critical | Workspace-identity digest/normalization undefined; one path, two writers | architecture.md | pre-existing |
| C-2 | Critical | Pending-interaction wire surface entirely unobserved; SPEC-009 unimplementable as written | specs.md + roadmap.md | pre-existing |
| H-1 | High | Access-mode→sandboxPolicy/approvalPolicy mapping absent (0 wire fields named) | specs.md | pre-existing |
| H-2 | High | Developer-instruction "immutable" vs promote/demote contradiction; param carriage unstated | specs.md | pre-existing |
| H-3 | High | Fork boundary says "terminal"; own probe evidence proves interrupted turns rejected | specs.md | pre-existing |
| H-4 | High | Solicited-vs-unsolicited undecidable at the 16 MiB cap; H-11 patch never delivered the distinction | specs.md | patch-surface |
| H-5 | High | `model/list` semantics undetermined: identity field, absent-`--model` default, pagination, effort accessor | specs.md | pre-existing |
| H-6 | High | Absent-vs-unreadable thread evidence undefined; sole retry exception may never fire | specs.md | patch-surface |
| H-7 | High | Required-subset manifest does not exist; "contains" undefined over JSON Schema | architecture.md | pre-existing |
| H-8 | High | SPEC-007 cites a delegation instruction SPEC-011's MUST list omits | specs.md | pre-existing |
| H-9 | High | 1 MiB payload cap: measurement point contradicts itself; measured real frame already exceeds it | specs.md | pre-existing |
| H-10 | High | Group cleanup requires a live leader; impossible in both shapes it exists for | specs.md | patch-surface |
| H-11 | High | Writer-activation gate: three-way scope contradiction (error table vs SPEC-007 vs ADR-004) | specs.md | patch-surface |
| H-12 | High | Worker inherits CLI's lock descriptor; closing it silently drops byte 1 | architecture.md | patch-surface |
| H-13 | High | No socket unlink authority for the next owner; EADDRINUSE dead end | architecture.md | pre-existing |
| H-14 | High | `start_failed` transition and its mandatory seal have no authorized writer | architecture.md | patch-surface |
| H-15 | High | Recorded-lease-inode clause traps every future writer after legitimate lock-file recreation | specs.md | patch-surface |
| H-16 | High | Re-`init` undefined; can silently replace the lock root under a live writer | specs.md | patch-surface |
| H-17 | High | Provisional identity record cannot satisfy the tuple it is the sole source for | specs.md | patch-surface |
| H-18 | High | `proc_listpgrppids` units defect defeats truncation detection; ESRCH-at-kqueue-bind misclassified | specs.md | patch-surface |
| H-19 | High | RFC 8785 byte-identity is write-your-own in Rust; four verified pitfalls, one self-blinding | specs.md | pre-existing |
| H-20 | High | posix_spawn mandated alongside a child-side signal reset no child can perform | architecture.md | pre-existing |
| H-21 | High | Per-command `data`/`details` payloads undefined across ~half the command surface | specs.md | pre-existing |
| H-22 | High | `retryable` has no semantics and no per-code value | specs.md | pre-existing |
| H-23 | High | `run events --follow` stream contract and record-kind enum undefined | specs.md | pre-existing |
| H-24 | High | Timeout sweep: three named waits unvalued, six waits unnamed | specs.md | pre-existing |
| H-25 | High | `config.toml`/`targets.toml`: no schema, unknown-key, parse-error, or write protocol | specs.md | pre-existing |
| H-26 | High | `gomchi init` preconditions/idempotence undefined; no covering error codes | specs.md | pre-existing |
| H-27 | High | Git/non-Git mode unpersisted; two canonicalization rules can disagree | specs.md | pre-existing |
| H-28 | High | `run export` bundle contents, ledger inclusion, permissions, and residual disclosure undefined | specs.md | pre-existing |
| H-29 | High | `observed_paths` has no derivation, scope, or bound; can silently break its own envelope | specs.md | pre-existing |
| H-30 | High | Transition chain omits `running -> idle`; capability table and turn flow disagree with it | specs.md | patch-surface |
| H-31 | High | SPEC-004's 3-command recovery-trigger list vs the 9-command emitter reality | specs.md | patch-surface |
| H-32..H-37 | High | Roadmap verification-feasibility findings (table, §3F) | roadmap.md | planning |
| H-40 | High | Wholly-absent generation record reads `Unverifiable`; the only reboot-immune case (§3B) | specs.md | patch-surface |
| H-38 | High | Round-4 findings have no lawful landing vehicle in the status model (§6) | roadmap.md | planning |
| H-39 | High | Round-3's mandated review method is absent from the reusable Task Completion Gate (§6) | roadmap.md | planning |

Medium (M-1..M-23) and Low (L-1..L-2) are tabled in §4. (H-40 sits with the
§3B findings thematically; it was promoted from the Medium table at
cold-read.)

---

## 1. Round-3 disposition verification

Every row of `docs/reviews/round-3-disposition.md` was audited individually
against the current SOT, including each lettered sub-item and each item inside
the rolled-up M/L rows. Result: **50 Verified, 10 Partial, 0 Absent** across
60 audited items. The rolled-up "M-1 through M-10, M-12" row — the predicted
hiding place for partial work — verified **fully**; the round-3 fixes landed.

| Verdict | Items |
| --- | --- |
| Verified against current text | C-1, C-2, H-1(a-d), H-2, H-3(a-b), H-4, H-5, H-6, H-8, H-9(a-d), H-10, H-11, H-12, H-13, H-15(a-c), M-1..M-10, M-12, L-1..L-5, L-7, the lazy-thread-allocation compatibility finding, M-11's cited ADR text (present in ADR-007 verbatim), probe prefilter fix, probe diagnostics, `supportedReasoningEfforts` schema gate, resume + interrupt probes |
| **Partial** | **H-7** — the disposition says "Implemented," but only the kernel-flock step was unblocked; app-server *activation* is still refused with `RECOVERY_REQUIRED` for an unverifiable foreign generation. The substitution is defensible (see H-11 steelman) but undisclosed; the evidence sentence is true and non-responsive. **H-14** — probes landed, but the promised advertised/unadvertised effort fixture was never added to TASK-006's verification list. **L-6** — the count fix landed; round-2-follow-up's rows still have no IDs and round-2-disposition still self-contradicts (lines 5-6 vs 55-56). **Probe centralization** — `JsonRpcAppServer` now exists and 6 of 8 live-RPC probes use it; `task000_frame_probe.py` and `task000_subagent_visibility_probe.py` still hand-roll their own reader/correlator. Plus stale post-closure status text (§6) and NOTE-001 citing only the first of the two commits that constitute completion. |

Two Partial items are re-raised with new substance as H-11 and §3F/H-36; the
bookkeeping residue is M-20..M-23.

---

## 2. Critical findings (2)

### C-1. The workspace-identity digest is unspecified, and unnormalized path spellings split the single-writer lane  [Critical, pre-existing]

**Where:** `architecture.md` § Workspace Identity and Local Layout (~242-248)
and § Process and Transport Topology (~202-203); `specs.md` § SPEC-002
(~51-55), § SPEC-007 (~408-411, 486-487).

**Problem.** The socket name is specified to the bit — "the RFC 4648
uppercase, unpadded, 32-character base32 encoding of the first 160 bits of
SHA-256 over canonical workspace path, a NUL byte, and run ID" — but the
workspace identity that keys the *writer lease* gets only "The canonical
workspace ID is a digest of: the canonical Git top-level path". No algorithm,
preimage, or encoding is defined for `<workspace-digest>` or
`<workspace-and-run-digest>`; SPEC-007 says "keyed by the full
canonical-workspace digest" without saying what "full" is. Normalization stops
at symlinks ("Symlink spellings of the same directory MUST resolve to the same
workspace identity"); case spellings on case-insensitive APFS are not covered,
and `realpath(3)` preserves caller case, so `/x/Proj` and `/x/proj` digest
differently — two lock files, two "exclusive" writer leases, one worktree.
That defeats ADR-004's core promise ("at most one Gomchi writer" per
canonical worktree). No path-case/Unicode normalization rule exists anywhere
in the SOT (every `normaliz*` hit in specs/architecture-decisions concerns
idempotency input or transcript authority, and roadmap's three concern test
names — none paths). The lock path composition also reads doubled (root
resolves to `.../gomchi/locks/`, files at `locks/writer/<digest>`).

**Fix.** Take the path branch unconditionally: workspace identity =
lowercase-hex SHA-256 over the NFC-normalized, Unicode-simple-case-folded
canonical path bytes — volume-independent, so identity is never a function of
filesystem state (over-merging two genuinely case-distinct directories costs
only a shared writer lane; under-merging breaks the single-writer promise).
Do not anchor on `(st_dev, st_ino)` (breaks on restore/migration and
conflicts with the path-based socket digest). State that the identical
normalized preimage feeds the socket-name digest, while the manifest records
the `realpath` spelling verbatim; define both lock digests' preimage and
encoding; state the exact root-relative lock paths once.

### C-2. The pending-interaction wire surface is entirely unobserved, and the plan certifies it against its own guess  [Critical, pre-existing + planning]

**Where:** `specs.md` § SPEC-009 (~663-684); `architecture.md` § Request
Correlation (~403), § Turn Execution Flow step 8 (~518), and the "It does
provide" list (~603); `roadmap.md` TASK-008 (~215-224), TASK-013 (~311-314),
TASK-015 (~336-340); `docs/probes/task-000.md` Scope Limitations (~257).

**Problem.** Chain: (1) SPEC-009 is normative — "`run pending` returns the
generation-qualified request ID, kind, redacted payload, and accepted
response schema. `run respond` … validates it against that schema" — yet
repo-wide (docs and tools) there are **zero** server→client method names,
zero request/response shapes, and exactly one occurrence of "response schema"
whose source is never named. `run respond` cannot be implemented as written.
(2) Nothing states which incoming request selects `waiting_approval` vs
`waiting_input` vs `waiting_mcp`, what the four decisions serialize to, or
whether unanswered requests survive an app-server restart;
"`accept_for_generation` maps to app-server's live session-scoped approval"
is an unverified assertion about Codex stated normatively. (3) The probe
evidence says "The probes do not exercise approval flows", and every probe
ran `approvalPolicy: "never"`. (4) The roadmap compounds it: TASK-008 is
verified by "fake server-request coverage for each kind and decision" — a
fake built from these assumed shapes — while server-request emission is a
TASK-013 deliverable five tasks later, and the only live evidence is opt-in
TASK-015's "approval round trip". A green TASK-008 gate certifies Gomchi
against Gomchi's own guess, and `architecture.md`'s "It does provide" list
stakes "explicit approval and destructive-action boundaries" — a core promise
— on it. The external-references section (`specs.md:844-850`) legitimately
delegates Codex-owned wire shapes, but the Gomchi-owned decisions (schema
source, kind→state mapping, decision→wire mapping, restart survival) are
delegated to nothing, and the "checked manifest" architecture promises as the
landing surface does not exist (H-7).

**Fix.** Run one live approval/elicitation probe before TASK-008 (writer turn
under each non-`never` `approvalPolicy` enum variant; a scratch `CODEX_HOME`
with a trivial stdio MCP server for elicitation; restart with an outstanding
request to observe expiry). Pin the observed method names, params, response
shapes, decision vocabulary, and the kind→`waiting_*` mapping into SPEC-009;
name the response-schema source. Move server-request emission from TASK-013
into the shared fake app-server core (TASK-006 today; wherever H-32 re-sites
it), and make the probe a blocking precondition on TASK-008 in the roadmap.

---

## 3. High findings

### A. The Codex-facing contract (the least-evidenced layer)

**H-1. The access-mode → sandbox/approval policy mapping is absent.**
[pre-existing] `specs.md` names only the thread-level enum in prose ("Codex
read-only sandbox policy", "workspace-write sandbox policy");
`architecture.md:512` says "access-derived sandbox policy, approval policy".
Zero of the wire fields the probes actually sent (`sandboxPolicy`,
`writableRoots`, `networkAccess`, `excludeSlashTmp`, `excludeTmpdirEnvVar`,
`approvalPolicy`) appears in any SOT document, and no `approvalPolicy` value
is ever named. Two implementations will differ on network access, exclusions,
and — sharpest — `writableRoots`: with the only probed value
(`[cwd]`), a *linked worktree's* real gitdir (`<main>/.git/worktrees/<name>`
+ shared object DB) lies outside the writable root, so SPEC-011's
master-authorized `git commit` is silently dead in every linked worktree
while SPEC-002 advertises per-worktree writer lanes. *Fix:* a normative
SPEC-007 table: access mode → exact thread-level `sandbox` string and
turn-level `sandboxPolicy` fields (naming both carriers), plus the exact
`approvalPolicy` value per mode; for linked worktrees include the resolved
`git rev-parse --git-common-dir` and per-worktree gitdir in `writableRoots`,
or document commit-unavailability. Escalates to Critical if `[cwd]` is the
intended literal value.

**H-2. Developer instructions: "immutable" contradicts promote/demote, and
the parameter carriage is unstated.** [pre-existing] Three documents
collide: SPEC-011 injects "immutable developer instructions … include … access
mode"; SPEC-007 allows idle `promote`/`demote` (access mode changes); ADR-009
rejects "mutable run instructions". No text says which RPC re-supplies the
prefix (schema places `developerInstructions`/`model` on thread-level params;
whether `turn/start` accepts them is unprobed — `architecture.md:512`'s "Send
`turn/start` with the fixed model … and effective developer instructions" is
unverifiable as written). *Fix:* scope "immutable" to a process generation;
state that promote/demote takes effect at the next generation (or a
documented `thread/resume` re-supply point) with the prefix recomposed; probe
`TurnStartParams`' actual property set and the binding effect of thread-level
fields on resume. Escalates to Critical if `turn/start` cannot carry `model`
(ADR-006's per-turn fixed-model send becomes unexecutable).

**H-3. The fork boundary says "terminal"; the project's own evidence proves
an interrupted turn is not a valid boundary.** [pre-existing] Definitions:
"Terminal turn: a turn confirmed as completed, interrupted, or failed."
SPEC-008/architecture/ADR-008 all fork "through the last confirmed terminal
turn". The crash probe recorded: supplying the crashed (persisted
`interrupted`) turn as `lastTurnId` failed — "Codex still classified that
boundary as in progress" — and the evidence then claims the completed-turn
choice "matches Gomchi's normative fork contract", which it does not: no SOT
sentence says "completed". Worse, SPEC-008's only fallback ("If none exists")
is keyed on *no terminal turn existing*; in the canonical crash an
interrupted terminal turn exists, so a rejected fork has no defined error,
fallback, or table row. *Fix:* replace "terminal" with "completed" for
`lastTurnId` selection in all three documents; define the fallback ladder
(next-older completed turn, else fresh-thread path); correct
`task-000.md:177-178`; probe fork against completed / cleanly-interrupted /
crash-interrupted / failed boundaries and record exact errors.

**H-4. Solicited-vs-unsolicited cannot be decided at the moment the 16 MiB
cap applies.** [patch-surface] Two mutually exclusive per-line rules
(unsolicited: cap + generation stop; solicited `thread/read`: no total cap)
require the JSON-RPC `id` to classify, but `specs.md:284` mandates "count and
hash while **discarding** beyond the bound" — once discarded, a genuinely
solicited >16 MiB response is unrecoverable, which is the exact trap the
round-3 H-11 patch existed to remove. The patch's envelope scanner
(`specs.md:300`) runs only on a *complete invalid* line, is permissive,
failure-scoping-only, and unbounded — it does not answer classification,
though the disposition marked round-3 H-11 "Implemented" against a fix text
naming this distinction as the deliverable. *Fix:* make the 16 MiB counter
itself the decision point: a stdout line reaching the cap without the
streaming visitor having yielded a top-level `id` matching an outstanding
request is unsolicited and capped; one that has is solicited and continues
under the operation deadline. Probe whether 0.147.0 emits `id` before
`result`; if it does not, add the fallback in the same paragraph: a line
reaching the cap with no top-level `id` yet observed is classified solicited
iff exactly one request is outstanding on that generation and its method is
`thread/read`, otherwise unsolicited — writable regardless of the probe
result.

**H-5. Model fixation and effort membership rest on undetermined
`model/list` semantics.** [pre-existing] `--model` is optional at `run start`
while ADR-006 requires exactly one model resolved and recorded — no
resolution rule exists for the absent-flag case (probes used `isDefault`; the
SOT has never heard of it). No text names the identity field `--model`
matches (probes hedge `item.get("model") or item.get("id")`), the
`supportedReasoningEfforts` element accessor (string vs object — probes hedge
both), or pagination (`ModelListResponse` carries `nextCursor`; no probe or
SOT text follows it). *Fix:* state in SPEC-003: exhaust `nextCursor` before
any verdict; the identity field; the absent-`--model` default rule; the exact
effort accessor; which cached listing `set-effort` validates against when no
generation is live. Probe pagination reality and field shapes.

**H-6. "Absent" vs "unreadable" thread evidence is undefined, so the sole
automatic retry exception may never fire.** [patch-surface] The lazy
first-turn recovery may retry "only when history proves no turn was accepted
or the provisional thread is absent. Any accepted/in-progress or unreadable
result … is never retried" — "unreadable" is used twice and defined nowhere,
and no mapping exists from app-server error shapes to the two verdicts.
`specs.md:297-299` ("Timeout, malformed structure, or an unusable status
fails only the requesting … command") textually supports the pessimistic
reading under which an errored `thread/read` can never establish absence.
The probe gap is precisely located: the "no rollout found" evidence came from
`thread/resume`, but SPEC-008 restricts reconciliation to
`thread/read(includeTurns: true)` — the exact recovery call has never been
exercised against an absent thread. *Fix:* a closed normative list mapping
error shapes to absent-vs-unreadable, defaulting unlisted shapes to
`unreadable`; probe `thread/read` against a never-persisted and a deleted
thread ID.

**H-7. The required-subset manifest does not exist, and "contains" is
undefined over JSON Schema.** [pre-existing] `architecture.md:624-625`
promises, in the present tense, "Its checked manifest lists every request,
response, notification, enum variant, and required field on which Gomchi
state depends." Repo-wide file search: no such artifact. SPEC-012 gates every
unlisted-version acceptance on "the generated stable schema contains Gomchi's
required … subset", with "contains" undefined ($ref resolution? types? enum
variants? requiredness?) — the schema probe implements it as top-level
property-name presence only, so a retyped `sandboxPolicy` or a dropped
`interrupted` status variant would pass. The SOT names zero notifications
while the turn flow depends on them. Roadmap side: TASK-005 must implement
"required stable-subset comparison" while the "full required method/field
manifest" first appears as TASK-013 fixtures, eight tasks later. *Fix:* author
the manifest as a checked-in normative artifact (TASK-001/002-era) that
TASK-005 consumes and TASK-013 extends; define the comparison algorithm;
restate TASK-000's schema claim as the 7-method + 3-field subset it actually
verified.

**H-8. SPEC-007 cites an injected delegation rule that SPEC-011's normative
list omits.** [pre-existing] SPEC-007: "the injected instructions require
Codex to avoid overlapping write-heavy delegation …". SPEC-011's "The
instructions MUST establish these rules:" has exactly eight bullets; none
concerns delegation, and ADR-009 is silent. Since Gomchi explicitly does not
serialize native-subagent lanes inside one writer app-server, that prompt
rule is the *only* stated mitigation for intra-writer write collisions — and
a TASK-012 prompt built from SPEC-011 will not contain it. *Fix:* add the
delegation bullet to SPEC-011 (and ADR-009's consequences), or delete the
SPEC-007 claim and state the residual plainly.

**H-9. The 1 MiB representation cap is self-contradictory on measurement
point, and the measured real world already exceeds it.** [pre-existing]
SPEC-006 pairs the cap with "Detection is streaming: count and hash while
discarding" (pre-transform reading); `architecture.md:352` says "at most
1 MiB **after redaction and representation**" (post-transform — which cannot
be streamed, since redaction requires full structural parsing). The probe's
largest observed *real* frame is 1,049,281 bytes — above 1,048,576 before any
marker/escape growth — so ordinary large command output degrades to
`payload_unrepresentable` inside the very completeness claim SPEC-010 makes.
The retained "streaming SHA-256"'s operand (raw wire bytes vs post-transform
bytes) is unstated in both documents, so the digest is not independently
reproducible. Also: `architecture.md`'s "is retained only as an opaque event"
lacks specs' "when observable" hedge, and the probe record shows the unhedged
reading is false. *Fix:* measure the cap on pre-transform wire bytes with a
stated post-transform growth allowance; name the digest operand as raw wire
payload bytes; align the subagent sentence with the hedge; either raise the
cap above the measured frame maximum or state that main-turn completeness is
metadata-only beyond it.

### B. Coordination and identity (the round-3 layer — mechanisms held, edges did not)

**H-10. Group cleanup requires a live leader, making cleanup impossible in
both shapes it exists for.** [patch-surface] "Individual member signalling is
permitted only after the recorded leader was revalidated `Match`"; "If the
leader is initially absent/mismatched and possible members remain, the group
is `Unverifiable`." The app-server is *necessarily* the group leader (spawned
`SETPGROUP`, pgid == pid). So: crash shape — app-server dead, its shell/build
children alive → leader Absent + members → nothing may ever be signalled,
while SPEC-005 advertises `recover` performs "verified process-group
cleanup". Clean shape — TERM kills the leader first; every member spawned
after the snapshot is "never signalled", so a writer running `cargo build`
can fail ordinary `close`/`pause` into `Unverifiable`, and "Close cannot
finalize a run whose prior generation remains unverifiable." Step 8 ("confirm
group absence") also has no deadline. A second, independent contradiction:
`architecture.md:568-571` § Process Cleanup describes a looser leaderless
procedure ("verify every enumerated member … forced termination of remaining
matches") with no leader precondition — SPEC-007 and architecture disagree.
The fail-closed intent is sound (recycled pgids are real; round 3 measured
PID wrap), so the fix must add authority narrowly. *Fix:* grant
post-leader-loss signal authority only where group continuity was
independently observed (a member kqueue-bound before leader loss, or a
still-live revalidated snapshot member); persist the member snapshot beside
`cleanup_in_progress` so a later recoverer inherits continuity evidence;
permit re-snapshot while at least one revalidated snapshot member lives;
bound step 8 with a deadline mapping to `Unverifiable`; reconcile § Process
Cleanup with SPEC-007. State explicitly that with no continuity observation
at all (both processes dead before any snapshot) the group stays
`Unverifiable` until it empties or the host reboots — and amend SPEC-005's
"verified process-group cleanup" sentence to disclose that limit.

**H-11. The writer-activation gate has a three-way scope contradiction, and
the H-7 disposition mislabels the substitution.** [patch-surface; found by
verification] (a) SPEC-007 (`specs.md:431-440`) + `architecture.md:418-423`:
an unverifiable *foreign-run* generation gates writer **activation**
workspace-wide — `Unverifiable` "releases the newly acquired lease and
returns `RECOVERY_REQUIRED`". (b) The binding error table (`specs.md:344`;
binding per `specs.md:361-362`) **excludes "new-run `start`"** from
`RECOVERY_REQUIRED` — while SPEC-007 (`specs.md:543-546`) puts write-access
`run start` in "the complete writer-acquisition set". The SOT simultaneously
requires and forbids write-access `run start` to emit the code. (c)
ADR-004 (`architecture-decisions.md:136-138`) states the *same-run* scope:
"An unverifiable generation blocks same-thread recovery and same-run writer
acquisition, but a stale foreign-run `writer.json` does not override the
kernel lease." Round-3 H-7 asked for the same-run scoping; the disposition
says "Implemented" but only the flock step was unblocked — activation still
refuses, i.e., the workspace-wide outage persists in reboot-bounded form.
The substitution itself is defensible (admitting a writer over a possibly
live foreign app-server group would be worse — the flock win proves no live
lease *holder*, not a dead *group*), but it is undisclosed and the three
texts now disagree. *Fix:* decide the scope once and make ADR-004, the error
row, and SPEC-007 agree (recommended: keep the workspace-wide activation
gate; remove "new-run `start`" from the row's exclusions or assign write
start a distinct code); state the operator-visible consequence and the two
clearing conditions (boot-session change; group provably empty) next to the
no-force sentence; amend the H-7 disposition row to record the substitution.

**H-12. The worker inherits the CLI's startup-lock descriptor; closing it
silently drops byte 1.** [patch-surface; found independently by two lanes]
The CLI "holds byte 0 … before fork", so the forked worker inherits that
descriptor, then "opens that lock once, acquires byte 1". POSIX releases
*all* of a process's record locks on a file when *any* descriptor to it
closes (round 3 measured this). The mandated daemonize recipe puts the
standard close-inherited-fds step **after** byte-1 acquisition, and the
implementer is already doing manual fd surgery here (fd 3 must survive the
same re-exec — itself unstated: the SOT only makes fd 3 CLOEXEC "before
app-server launch", and Rust opens everything CLOEXEC by default). One fd
sweep and the run's exclusivity guard evaporates mid-service: `F_GETLK`
reports byte 1 unlocked and a contender lawfully starts a second serving
worker — two O_APPEND writers on one `audit.jsonl`. The nearest counter-text
(ADR-004: "Runtime ownership is never inferred from an inherited lock")
governs inference, not hygiene. *Fix:* make the CLI's startup-lock descriptor
`FD_CLOEXEC` before the worker re-exec; state as a *property* that the worker
holds exactly one descriptor to the lock file for its serving lifetime; state
that fd 3 must survive the `__worker` re-exec. Add the missing OS-probe case
(child closes inherited fd → third process's `F_GETLK` on byte 1 reports
unlocked) — the committed probe builds this exact shape and never closes the
inherited fd.

**H-13. No rule authorizes the next owner to unlink an existing socket
path.** [pre-existing] Shutdown "leaves the stale socket and sidecar for the
next verified owner rather than deadlocking" — but AF_UNIX `bind(2)` on an
existing path fails `EADDRINUSE`, the path is per-run-*forever* (derived from
workspace + run ID, not generation), and the stale socket is the *expected*
recovery input (only clean shutdowns unlink). All seven `unlink` mentions in
the SOT are lock-file inode checks or the dying worker's own unlink; "verified
owner" is used twice and defined never. The identity-mismatch check
(`RUNTIME_PATH_COLLISION`) cannot fire on a same-run restart — the sidecar
matches by construction. Silence permits both a fail-closed-forever
implementation and a pre-election path theft. *Fix:* one rule: only the
byte-0 election winner, after proving the recorded prior generation Absent or
cleaned, may unlink a matching-identity socket and sidecar; the unlink is
performed by the worker after byte 1 is held and before bind, never by the
byte-0 starter; any other occupied path fails closed.

**H-14. `starting -> start_failed` and its mandatory seal have no authorized
writer.** [patch-surface] "The worker is the single state-transition
authority for its run"; the CLI "never writes the audit ledger while a worker
owns the run" with a bootstrap exception scoped to "initial records"; and
`architecture.md:316` requires "Closed and start-failed runs append a final
seal event." For a starter/worker killed before runtime identity persists,
no actor may write the transition or the seal — the run sits in `starting`
forever (round-3 C-1's fresh-fork escape reads state; it never repairs it).
The `running|waiting_* -> outcome_unknown` analogue is defensible design
(under `Unverifiable` the app-server may still be running), but then the SOT
should say so, and round-3 H-8's still-missing operator guidance (the two
clearing conditions; "reboot" appears nowhere in SPEC-005/007) becomes
load-bearing.
*Fix:* an explicit narrow exception: a CLI that holds byte 0 and proves no
worker ever bound may append `start_failed` and its seal; state plainly that
no transition is written for the `Unverifiable` case; add round-3 H-8's two
clearing conditions beside the no-force sentence.

**H-15. "The newly held inode must equal the inode recorded in
`writer.json`" traps every future writer after any legitimate lock-file
recreation.** [patch-surface] The held-fd vs root-relative-pathname
comparison (the real round-3 H-3 fix) is sound; the *additional* historical
comparison against the recorded inode turns a restored/cleaned state root, a
`--state-root` migration, or external cleanup into permanent
`RUNTIME_PATH_COLLISION` for the whole workspace — on a file the SOT
pointedly declines to declare permanent (startup locks are "permanent once
created" one paragraph later; writer locks are not), with runtime records
elsewhere described as "recoverable coordination caches" and no documented
repair.
*Fix:* keep the clause's protective intent but invert the authority: declare
writer lock files permanent once created, and on absent-or-divergent recorded
pair *reconstruct* `writer.json` from the newly held pair; reserve
`RUNTIME_PATH_COLLISION` for held-fd vs pathname mismatch.

**H-16. Re-initialization is undefined and can replace the recorded lock
root under a live writer.** [patch-surface] No text defines `init` on an
already-initialized workspace (`re-init`/`already initialized`: zero hits;
the error table specifies the exact analogue for targets —
`TARGET_ALREADY_EXISTS` — and nothing for workspaces). A second
`init --state-root B` rewrites `lock-root.json` while a live writer holds
`locks/writer/<digest>` under root A; the next writer flocks a different
file — two "exclusive" writers, the same class round-3 H-3(b) closed for the
environment-variable path (an explicit flag is not an "environment change",
so the ignore-rule does not reach it). Also undefined: whether re-init
overwrites `config.toml` (resetting `default_access`), and `init --non-git`
on a Git workspace. *Fix:* make `init` on an initialized workspace
idempotent-or-rejected with a dedicated code; forbid lock-root and mode
changes unless every recorded value is byte-identical; never overwrite
existing tracked policy files.

**H-17. The provisional identity record cannot satisfy the tuple it is the
sole source for.** [patch-surface] The round-3 H-6 fix writes provisional
identity
from "`PROC_PIDTBSDINFO`, `proc_pidpath`, and a second BSD-info sample" —
which cannot supply `executable_dev`, `executable_ino`, `executable_sha256`;
those are recorded only by the post-handshake `generation_started` event. But
the identity tuple requires all ten fields, `Unverifiable` "includes
missing/unparseable required recorded generation or identity fields", and the
only carve-outs are *path*-scoped and *mismatch*-scoped ("The provisional
path is non-dispositive…"). A SIGKILL during the initialize handshake
(hundreds of ms to seconds) leaves a generation missing three required
fields → `Unverifiable` → `RECOVERY_REQUIRED` until reboot. The
runtime-record schema (`architecture.md:180-184`) itself mandates the three
fields the pre-`SIGCONT` write cannot have. *Fix:* populate the full 10-field
tuple in the provisional record — the child is stopped post-exec, so
`proc_pidpath`, `fstat`, and hashing are all available before `SIGCONT`. (Do
not instead widen the non-dispositive carve-out: absence proof would then
rest on the group predicate alone, which routes into H-10.)

**H-40. A wholly-absent generation record is classified `Unverifiable`, and
with no recorded boot UUID the reboot escape can never fire.** [patch-surface;
promoted from Medium at cold-read for consistency with H-14/H-15/H-17] The
exhaustive `Unverifiable` list "includes missing/unparseable required
recorded generation or identity fields", and every clause of the absence
procedure presupposes a record exists ("load the fsynced generation identity
…", "compare the recorded boot-session UUID …"). No clause states the safe
reading (nothing recorded ⇒ nothing to classify ⇒ `Absent`). A run whose
first worker died before persisting runtime identity therefore reads
`Unverifiable` — and uniquely *permanently*: with no recorded boot UUID,
`specs.md:460`'s escape has nothing to mismatch, making this the SOT's only
reboot-immune `Unverifiable`. It lands on a run stuck in `starting` (H-14's
no-writer gap). The general form fails loudly on the first virgin-workspace
write start; the shipping risk is the starting-with-no-generation case.
*Fix:* one clause: a run with no recorded generation has no prior generation
to classify — the absence procedure is skipped and the verdict is `Absent`;
`Unverifiable` applies only to a present-but-unreadable record.

**H-18. `proc_listpgrppids` units and kqueue ESRCH semantics: two normative
misstatements, found by running the missing probes.** [patch-surface]
Measured on the target platform: `proc_listpgrppids` takes `buffersize` in
**bytes** but returns a **PID count** (3-member group, 64-byte buffer → 3;
4-byte buffer → 1, silently). The SOT's sole truncation detector is "repeats
with doubled capacity whenever the return count equals capacity" — an
implementer passing entries as bytes gets returns ≈ capacity/4 that *never*
equal capacity, so the doubling never fires, truncation is silent, and the
group-absence proof passes while survivors exist — precisely the failure the
proof exists to prevent. The committed probe cannot catch this (1-member
group, 1-entry buffer: the readings coincide). Second: `EVFILT_PROC` bind on
a *reaped* PID fails ESRCH; the exhaustive `Unverifiable` list classifies
"failure to bind or revalidate the kqueue process instance" as
`Unverifiable`, but ESRCH-at-bind proves the process *gone* — the safest
possible evidence — and instead yields non-retryable `RECOVERY_REQUIRED`
(reachable: an orphaned worker reparents to launchd and is reaped promptly).
Third: kqueue and `fstatfs`/`MNT_LOCAL` — both failure-classifying, normative
mechanisms — have no committed probe coverage at all (the C probe has exactly
7 assertions; `kqueue|EVFILT|fstatfs|MNT_LOCAL`: zero hits under `tools/`).
*Fix:* in the SOT: define capacity in entries with
`buffersize = capacity * sizeof(pid_t)`; carve ESRCH-at-kqueue-bind out of
`Unverifiable` as proof of `Absent`. In the probe: add EVFILT_PROC cases
(live/exited/zombie/reaped/recycled), `fstatfs`/`MNT_LOCAL`, the
`F_GETLK l_pid` value for flock-origin locks, and the two-descriptor
close-drops-lock case (H-12).

### C. Durability and audit pipeline

**H-19. RFC 8785 byte-identity is a write-your-own component in Rust, and
one mandated ingest feature blinds the verifier.** [pre-existing] Four
verified pitfalls: (1) JCS sorts keys by UTF-16 code units; Rust string
order is UTF-8/code-point — they diverge exactly when U+E000–U+FFFF keys meet
astral-plane keys. (2) JCS numbers require ECMAScript `Number::toString`;
`serde_json`/`ryu` emit `1.0` and `1e21` where JCS requires `1` and `1e+21`
— the default serializer is non-conformant for every integral float, and
combined with the `$gomchi_number` rule this is format- and hash-determining.
(3) `serde_json` silently accepts duplicate members last-wins; the normative
"Inbound JSON rejects duplicate object members" needs a custom visitor an
implementer will not know to write. (4) `arbitrary_precision` — effectively
mandated by "preserves number lexemes with arbitrary precision" — makes
re-serialization echo the stored lexeme, so the byte-identity check becomes
*vacuous for numbers* and a non-conformant number formatter self-verifies
forever (this also defeats the only mechanism that pins the
`$gomchi_number` predicate's intended reading — see M-4). No named
implementation, no conformance basis, no hash-scheme-bump policy, no RFC 8785
vectors in any fixture list; the ecosystem crates are small and
single-maintainer. *Fix:* name the conformance basis in the SOT (UTF-16 key
order; ES6 number formatting); require the verification path to re-serialize
numbers through the binary64/ES6 formatter, never a preserved lexeme; pin RFC
8785's published test vectors into TASK-003; state that any canonicalizer
byte change requires a new `sha256-jcs-vN`; budget the component (pinned
audited crate or in-repo module).

### D. Implementation substrate

**H-20. The SOT mandates `posix_spawn` and a child-side signal reset that no
child can perform.** [pre-existing] `specs.md:480`/`architecture.md:112`
mandate `POSIX_SPAWN_START_SUSPENDED`; `architecture.md:72-74` says "Before
every app-server exec, **the child** restores those dispositions and the
signal mask to defaults" — `posix_spawn` runs no child code; that actor does
not exist. The reset is expressible only as `POSIX_SPAWN_SETSIGDEF` +
`POSIX_SPAWN_SETSIGMASK` (present in the SDK; unmentioned in the SOT), and it
is not optional: SIG_IGN dispositions and the signal mask *survive* `execve`,
so without `SETSIGDEF` the worker's ignored SIGINT/SIGHUP leak into Codex,
and a standard threaded signal design (block-all + `sigwait`) without
`SETSIGMASK` delivers Codex a fully blocked mask — cleanup's five-second
graceful TERM phase silently degenerates to KILL, invisible in testing. The tempting
fork+exec+`pre_exec` workaround breaks round-3 H-6's guarantee differently:
a pre-exec `SIGSTOP` suspends *before* the target image loads, so
`proc_pidpath` samples the gomchi path, not the app-server. *Fix:* restate
the mechanism as spawn attributes — `START_SUSPENDED | SETPGROUP | SETSIGDEF
(full set) | SETSIGMASK (empty)` — with the reason (dispositions and mask
survive exec), and note that `std::process::Command` cannot express this.

### E. The machine-output and workspace contract

**H-21. The stdout machine contract is a wrapper with undefined contents.**
[pre-existing] Both envelopes are literally `"data": {}` and `"details": {}`.
Exactly three payloads are described anywhere (terminal result, pending
request, identity verdict); `run list` has not one named field; `run status`,
`target list/show/doctor`, `verify`, `export`, `wait`'s two non-terminal
returns, and every lifecycle command have no defined `data`; `command`
strings and `invocation_id`'s type are shown only by example; `error.details`
keys are defined for zero codes; `--human`'s machine-parseability is
unstated. The primary consumer is an AI master that branches on these fields.
*Fix:* a SPEC-006 payload subsection: per-command `data` fields and types
(binding `status` to the `state.json` field list + identity verdict, and
non-terminal `send`/`wait` to the same shape), the `command` string per
subcommand, `invocation_id` = UUIDv7, per-code `error.details` keys, the
pending-request `kind` enum, and an explicit statement that `--human` output
carries no machine guarantee.

**H-22. `retryable` has no semantics and no per-code value.** [pre-existing]
The field appears once, hardcoded `false`. No definition of what it advises,
no per-code mapping, and the only prose is negative ("never generically
retryable") or about retrying a *different* command. An AI master will branch
on this boolean; two conforming implementations can emit opposite values for
`TRANSPORT_FAILURE`/`RUN_BUSY`/`WRITER_BUSY`. *Fix:* define it ("the
identical invocation may be safely reissued unchanged"), add a Retryable
column to the error table carrying the per-code default, and state the model:
for codes whose safety depends on the call (`TRANSPORT_FAILURE`,
`OUTCOME_UNKNOWN` on `send`/`submit`), the emitted value is `true` only when
that invocation carried an idempotency key. It is advisory and implies
nothing about side effects.

**H-23. `run events` --follow has no stream contract, and the record-kind
enum exists only by accident.** [pre-existing] The cursor *type* is
inferable (specs' "sequence/event cursor" parallels architecture's
`sequence`), but `--after`'s inclusivity, the per-line envelope shape,
mid-stream error signalling, the exit code, idle behavior, and termination
(only `closed`/`start_failed` are final — `--follow` on `idle`/`paused`/
`outcome_unknown` never ends) are all undefined, "normalized records" is
never defined, and the six record kinds appear only as scattered mentions.
*Fix:* cursor = decimal ledger sequence, `--after` exclusive, stability
across torn-tail repair and `projection_rewound`; define the per-line object,
error/exit behavior, and a heartbeat-or-termination rule; enumerate the
`kind` set as a closed list.

**H-24. Timeout sweep: three named waits have no value or derivation; six
waits are unnamed.** [pre-existing] Valued: 10s bound, 10s hello, 30s
no-progress, 5s TERM, 5s interrupt, 10s handoff, 100ms group commit.
Unvalued: "the larger operation-specific startup budget" (ready wait),
`thread/read`'s "operation deadline", the state-projection "bounded
interval" (the only unvalued constant that shapes observer latency — see
M-9). Unnamed entirely: socket connect/attach, initialize handshake, doctor
schema-generation and probe deadlines, reconcile's transient-generation
budget, `pause`/`close --interrupt` terminal wait. Observable divergence:
`TRANSPORT_FAILURE` vs success on a genesis replay; hang vs error on
`close --interrupt`. *Fix:* one normative timeout table in SPEC-006 with a
value or derivation rule per wait (ready budget = floor + per-record
allowance).

**H-25. `config.toml` and `targets.toml` have no schema, unknown-key policy,
parse-error behavior, or write protocol.** [pre-existing] The whole
config.toml contract is one sentence; `targets.toml` appears once (its
path). No value domains, no malformed-TOML error code
(`WORKSPACE_NOT_INITIALIZED` is defined as "no valid workspace", which a
present-but-broken file is not), no `schema_version` mismatch rule, no
statement whether the Git-tracked, team-shared config file is hand-editable,
and no concurrency protocol for `target add`/`remove` — in a document that
gives every other owned file an explicit temp+fsync+rename treatment. *Fix:*
schema blocks for both files (keys, types, domains), unknown-key and
parse-failure behavior with an error code, hand-editability statement, and
the standard write protocol for `targets.toml`.

**H-26. `gomchi init`'s preconditions are undefined and uncodeable.**
[pre-existing] Undefined: `.gomchi` already exists (the table proves the
authors call out "pre-existing X" when they mean it — export has its own
callout; init has none); partial/corrupt layout; `init` on a non-Git tree
without `--non-git` (no covering row); nonexistent path; nested workspaces
("nested": zero hits); linked-worktree init; `--state-root` change after
runs exist (H-16); overwrite behavior for existing tracked files. The
roadmap's "repeated initialization" test bullet has no normative backing to
test against. *Fix:* an init algorithm in SPEC-002: ordered preconditions
with per-precondition codes, idempotent re-init that never overwrites
tracked files, explicit nested-`.gomchi` rejection.

**H-27. Git/non-Git mode is unpersisted and the two canonicalization rules
can disagree.** [pre-existing] Mode is stored nowhere workspace-scoped
(config.toml "contains only `schema_version` and `default_access`"; the
manifest records mode per-run, after the fact), while Git mode canonicalizes
via `git rev-parse --show-toplevel` "even when the supplied path is a
subdirectory" and discovery uses "the nearest ancestor containing `.gomchi`"
— `init --non-git` in a repo subdirectory yields two different workspaces
depending on which rule runs first. The non-Git "start baseline" is
undefined (the baseline list is Git-only) though the manifest requires one.
The `git` executable dependency has no version floor or absence error.
*Fix:* persist `mode` (only) in the tracked `config.toml` — the absolute
canonical path stays out of tracked policy (architecture forbids
machine-specific paths there); discovery derives the path from the
authoritative `.gomchi` location, cached at most in untracked
`.gomchi/runtime/`. Reject a Git-mode `.gomchi` not at the toplevel; define
(or explicitly empty) the non-Git baseline; name the git dependency.

**H-28. `run export` is one sentence: bundle contents, ledger inclusion,
permissions, and the residual are all unstated.** [pre-existing] No
filenames, no bundle index/schema_version, no determinism statement, and —
decisive for the product's purpose — no statement whether the verbatim
`audit.jsonl` is included; without it the embedded "verification result" is
unfalsifiable. "transcript" is used seven times across the SOT and defined
zero times as an artifact. Permissions: every other artifact has 0700/0600
stated (12+ mentions); export — produced by the CLI, outside the worker's
`umask(077)` — has none, and no disclosure warns that the portable bundle
carries plaintext prompts/command output/excluded-key tokens (the redaction
residual is disclosed only as a local-permissions caveat). *Fix:* exact
filenames + `bundle.json` (schema_version, run/workspace identity), verbatim
`audit.jsonl` inclusion, transcript format, determinism guarantee, 0700/0600
modes, named writing component, an exclusion list, and one residual-risk
sentence at `run export` and in architecture's "does not provide" list.

**H-29. `workspace_changes.observed_paths` is in the machine contract with
no derivation, scope, or bound.** [pre-existing] Three prose fragments
produce it; method (git status? mtime walk?), scope (`.gomchi`? ignored
dirs?), non-Git behavior, path form, ordering, and any cap are undefined —
and an unbounded list interacts with two hard limits (1 MiB payload, 8 MiB
frame), so a big refactor turn can silently degrade its own terminal result.
*Fix:* define the observation algorithm per mode, scope and exclusions,
sorted unique workspace-relative paths, a maximum count with a `truncated`
flag, and the documented degradation.

**H-30. The transition chain omits `running -> idle`.** [patch-surface] The
literal chain ("starting -> idle -> running <-> waiting_* -> idle") has no
`running -> idle` edge, while the capability table lists it and Turn
Execution step 9 transitions straight to idle. A direct SOT-vs-SOT
contradiction under the project's own invalid-state rule. *Fix:* add the
edge (or declare the capability table the sole normative source).

**H-31. SPEC-004's recovery-trigger list names 3 commands; the emitter
reality is 9.** [patch-surface] "a later `send`, `resume`, or explicit
`recover` invocation performs on-demand recovery" reads as exhaustive (no
hedge; same closed style as the tables) while `RECOVERY_REQUIRED` emits from
9 commands — an implementer wiring discovery off SPEC-004 gives
`submit`/`promote`/`pause`/`reconcile`/`close`/`fork` no recovery path
(bare transport failures instead). *Fix:* generalize the sentence
("commands that acquire a worker/writer lease or verify prior-generation
identity"), or enumerate all nine.

### F. Roadmap verification feasibility (planning layer)

| ID | Finding | Fix direction |
| --- | --- | --- |
| H-32 | TASK-004/005 verification requires an app-server stand-in that TASK-006 builds ("fake worker tests…", "fake executable matrices for … successful 0.147.0 compatibility"); two throwaway fakes precede the real one | Move the fake app-server core to TASK-003A/TASK-004 so 004/005/006/008 target one executable, 013 extends it once |
| H-33 | Control protocol v1 (ADR-011, SPEC-007) — the sole H-4/H-5 remedy — appears in no task scope and no verification ("hello" zero hits in roadmap; no task names the control channel or protocol); an unimplemented control channel silently restores the upgrade deadlock | Add to TASK-004 scope + verification: digest-mismatched `hello`/`status`/`shutdown` accepted; mutations rejected with `GOMCHI_PROTOCOL_MISMATCH`; shutdown interrupts an active turn |
| H-34 | TASK-014's "recorded random seeds… every prescribed seed to succeed" is not a determinism mechanism — a seed controls a PRNG, not the scheduler, PID allocator, or boot UUID; the SOT's sharpest windows (kill between suspended-spawn and fsync) have no injection seam; gate condition 2 becomes unsatisfiable for 1-in-N races | Add a fault-injection deliverable (named barrier points; injectable identity/boot-UUID/proc-enumeration provider), sited once alongside H-35's injectable clock in TASK-001's core contract (TASK-003's barrier fixtures already need it); replace the seed sentence with barrier list + injection method + iteration budget + flake policy |
| H-35 | No injectable clock exists while TASK-013 requires the suite to pass "without … timing-sensitive sleeps" and its siblings verify 10s/5s budgets; ≥8 normative budgets + one unnumbered "additional takeover interval"; the 10s+30s takeover path alone is a 40-second wall-clock test | Injectable time source in TASK-001's core contract; all budgets expressed against it; number the takeover interval in SPEC-005 |
| H-36 | Three round-3 fixes landed in the SOT with no roadmap verification half: H-10 marker escaping + transform order (no TASK-003 fixture), H-14 advertised/unadvertised effort (no TASK-006 fixture — the disposition's own promised fix), H-3 unlink/recreate inode mismatch (no TASK-007 case) | Append the three fixtures to TASK-003/006/007 verification lists |
| H-37 | Lock-root establishment at `init` (`--state-root`, `lock-root.json`, fd-relative EEXIST/0700/`MNT_LOCAL`, unanimous-manifest reconstruction) is owned by no task: TASK-002 stops at "safe permission creation", TASK-007 has only "persistent local lock-root validation"; `--state-root` has zero roadmap hits | Add establishment to TASK-002 scope + verification (nonlocal-default refusal; reconstruction vs conflict) |

Reverse-traceability sample (21 load-bearing MUSTs → catching task): **7 have
none** (marker escaping/transform order; effort membership; held-fd vs
`fstatat` inode check; control protocol v1; `--state-root`/`MNT_LOCAL`
establishment; SPEC-009's no-silent-replay-after-restart half; approval-flow
reality before TASK-015) and 4 more are partial or of contested
executability. See also §6.

---

## 4. Medium and Low findings

Medium:

| ID | Finding | Fix direction |
| --- | --- | --- |
| M-1 | Byte-0→byte-1 window admits a spurious second fork (harm is bounded by byte-1 exclusivity — verified); byte-1 acquisition failure has no defined behavior; an owner slot over an unlocked range has no defined meaning | Byte-1 loser exits with a structured fd-3 failure and zero side effects (maps to `RUN_BUSY`); stale slot never blocks a claimant [patch-surface] |
| M-2 | Startup-lock owner record: no field set, fixed size, checksum algorithm, layout version, or verdicts for checksum-failure/empty/stale slots — on a file that is permanent once created while control v1 tolerates binary-digest skew (cross-version hazard) | Enumerate fields (10-tuple + Gomchi identity + generation + boot UUID), add layout version; unrecognized layout/checksum ⇒ absent slot, never match [patch-surface] |
| M-3 | Write-ahead rule scoped to "bytes to app-server stdin" + acknowledgments — covers neither ledger truncation (torn-tail repair) nor reader-path group signalling; "streaming records" (the group-commit class) defined nowhere | Restate as "fsync before the effect", enumerating effects incl. truncation and group signalling [pre-existing] |
| M-4 | "cannot round-trip exactly through IEEE-754 binary64" — the intended reading (lexeme ↔ ES6-shortest round trip) is derivable only via the byte-identity rule, is written nowhere, and is *defeated* by H-19's pitfall 4; TASK-003 fixtures name no operand | State the predicate as an algorithm; fixtures: `1.0`, `1e2`, `-0`, `0.1`, `2^53+1`, `1e400` (`1e21` ⇒ marker); require verify to re-serialize via binary64/ES6 [pre-existing] |
| M-5 | Idempotency: `IDEMPOTENCY_CONFLICT` requires comparing "byte-identical normalized input" across restart, but reservation-record contents and "normalized" are never defined; the release rule for a reserved-but-never-accepted key exists only for the first turn | Intent record carries key + normalized-input digest; define normalization; state the general release rule (the key→turn join itself is derivable from ledger order + the single-active-turn invariant — verified) [patch-surface] |
| M-6 | Redaction tokenizer: empty-token handling is undefined and changes sequence-containment verdicts (`x_api__key` redacts under drop-empties, leaks under retain-empties); "final candidate token" and the trailing-`s` rule's operand are unwritten (the "may" is prose, not RFC-2119 — verified) | State: empty tokens dropped before matching; strip applies to the candidate window's last token; golden vectors incl. `api_keys`, `api__key`, `x_api__key` [pre-existing] |
| M-7 | Torn-tail repair: no evidence-file naming (second repair collides or overwrites), no crash-ordering (truncate-then-record leaves an unaudited repair), truncation sits outside every barrier; repair *authority* is derivable (worker-only — verified), but unwritten | Deterministic naming (`recovery/tail-<seq>-<hash>.bin`), order preserve+fsync → truncate → append+fsync record (append-first is unimplementable — it creates interior corruption), no overwrite [pre-existing] |
| M-8 | `AUDIT_INTEGRITY_FAILURE` gates "any state-changing run command" (undefined term, 2 hits) — `delete` is either inside (corrupt ledger ⇒ permanently undeletable run; `rm -rf` the only escape, contradicting the start-failed deletion promise) or outside (refusal has no code); export's emitter cell vs its bundled "verification result" is unresolved; reachable without tampering (delayed writeback between barriers can hole a non-suffix region) | Define "state-changing run command" once; exempt `delete --confirm`; export embeds the failing result with a `verification_failed` flag instead of refusing [pre-existing] |
| M-9 | Observer contract: `state.json`'s head is the only workable fsync watermark but is never designated; the "bounded projection interval" is the SOT's sole unvalued constant (unbounded `--follow` latency); `run events --follow` sits in the projection-only set while architecture routes it through the worker stream — one command, two transports | Designate the watermark, value the interval, reconcile the transport (worker-stream for `--follow` is the workable reading) [patch-surface] |
| M-10 | "The worker always services a dedicated control channel" vs "the worker replays `audit.jsonl` before accepting commands" — contradiction for hello-during-replay (harm is bounded: bound/ready split + CPU-progress predicate prevent wrong takeover — verified); the "progress observed" takeover branch has no mapped outcome | Replace "before accepting commands" with "before accepting ordinary mutations; control v1 answerable from `bound` onward"; map the progress-observed branch to `RUN_BUSY` [pre-existing] |
| M-11 | Fork-to-re-exec child window: the mandated sequence is async-signal-safe (verified — argv can be pre-built), but the single-thread precondition and the AS-safe-only constraint are unstated in a document that specifies at syscall granularity | One sentence: CLI creates no thread before fork; child performs only async-signal-safe operations before exec [pre-existing] |
| M-12 | No toolchain policy: Rust named only in roadmap prose; no edition/MSRV/`rust-toolchain.toml`; the gate demands "deterministic verification" while clippy `-D warnings` drifts with every toolchain release | `rust-toolchain.toml` (pinned channel, target, components); edition/MSRV in architecture.md; qualify the clippy gate by toolchain [planning] |
| M-13 | No dependency decisions for the ~9 mechanisms std cannot express (posix_spawn attrs, libproc family, kqueue EVFILT_PROC, fcntl byte ranges, MNT_LOCAL, boot-UUID sysctl, JCS, …); raw-libc/unsafe boundary unowned | Mechanism→binding table as a TASK-001 deliverable (a verified starting inventory is in this review's §7 advisory); "no new dependency without an ADR" [planning] |
| M-14 | Probe environment unpinned: TASK-000 promises "recorded commands and versions" but only Codex is pinned; Python floor unasserted (bytecode implies 3.14), C compiler/SDK unrecorded in the evidence | Record interpreter + `cc --version` + SDK in task-000.md; add a Python floor assertion to `_probe_support.py` [planning] |
| M-15 | Version-drift gate: condition 3 has no persisted-history check (`thread/resume`/`read`) though `specs.md:838` names "lifecycle behavior" as a rejection cause; no rule compares a bound run's live app-server version against its manifest's recorded version (the data and trigger both exist — cheap fix) | Per-generation comparison vs manifest; on a changed version for a thread-bound run, fail closed with `COMPATIBILITY_REJECTED` unless the generation-starting command carries an explicit `--accept-version-change` flag, recorded as a ledger event (no interactive prompt exists); add resume+read to the gate only for existing bound runs [pre-existing] |
| M-16 | "Unknown server requests fail safely" (SPEC-012) vs "recorded and fail closed" (architecture) — no operational wire action defined; "fail closed" elsewhere means generation stop | One action per class: append evidence, reply JSON-RPC method-not-found, keep the turn; reserve generation stop for unparseable frames [pre-existing] |
| M-17 | Managed-run marker: no variable name/encoding/field list; detection defined only for matches-own-run (absent is a disclosed limitation; foreign-run/nonexistent/malformed are not) | Name the variable + fields; present-but-unparseable or foreign ⇒ treated as managed and rejected (`POLICY_REJECTED`), never as absent; note non-inheritance by non-exec'd MCP transports [pre-existing] |
| M-18 | `RUN_BUSY` emitters are a strict superset of `RECOVERY_REQUIRED`'s 9; no precedence stated for the pair (the two codified precedence rules cover other pairs), and the natural orderings genuinely differ by clause | State the actual gating order per clause — do not assume symmetry with the `WRITER_BUSY` rule [patch-surface] |
| M-19 | `worker.log` has no content contract while "no progress in … bounded worker log" is a takeover input; the file is the one non-monotonic factor in the predicate (rotation shrinks it) | Required events + rotation trigger/atomicity; progress predicate over a monotonic counter (total bytes written) or a mandatory heartbeat [pre-existing] |
| M-20 | Probe reader/correlator centralization is half-done: `JsonRpcAppServer` exists, 6/8 live probes use it; `task000_frame_probe.py` and `task000_subagent_visibility_probe.py` still hand-roll the loops round 3 diagnosed as the regression vector | Refactor both to subclass the shared class, overriding only their correlator predicates [planning] |
| M-21 | TASK-000 gate walk: condition 6's target still says "awaits independent closure … before TASK-000 completes" post-closure; round-3 §5/§6 findings have no disposition rows; condition 8's note cites only the first of the two completion commits (the note asserting completion cannot cite the commit that created it); `round-3-input.md` untracked | Refresh the two stale status paragraphs; record §5/§6 dispositions (or explicit rejections); add `266f8a2` to NOTE-001; amend gate 8 ("plus a final closing commit"); commit the archive [planning] |
| M-22 | Scope realism: TASK-003 (15 deliverables) and TASK-009 (15/17) are each several tasks; EPIC-005's verification thins to 1-2 sentences exactly where integration risk concentrates | Split TASK-003 and TASK-009; give TASK-013/014/015 per-area acceptance lists at TASK-003's granularity [planning] |
| M-23 | Round-2 bookkeeping residue L-6 flagged: round-2-disposition still self-contradicts (lines 5-6 vs 55-56); round-2-follow-up's 9 rows still have no IDs | Add F2-1..F2-9; reconcile the closing paragraph [pre-existing] |

Low:

| ID | Finding |
| --- | --- |
| L-1 | Byte-1 owner with an unreachable socket: the guard *does* apply (verified — `specs.md:504-505` binds every byte-1 owner); remaining edits are editorial — state the byte-1-failure exit and name byte 0 as the shutdown-unlink range; align SPEC-005's "unreachable" wording with SPEC-007's procedure scope |
| L-2 | Instructions-source arity (`--instructions`/file/stdin) has no "exactly one" sentence, unlike both sibling text sources; the error table's "input source"/"incompatible option combination" cells plausibly already cover the code — add the arity sentence for completeness |

---

## 5. Probe suite assessment

**The suite held up under re-audit.** The round-3 §5 repairs are real:
the crash probe's prefilter is fixed (the `turn/completed` branch is
reachable), `JsonRpcAppServer` centralizes version pinning + bounded
diagnostics + correlation for 6 of 8 live probes, the resume and interrupt
probes exist and passed, and the anti-gaming discipline (inverted effort
predicate; sandbox filesystem checks; fork-boundary variable separation)
survives inspection. Residue: the frame and subagent probes still hand-roll
their loops (M-20), and the probe environment beyond Codex is unpinned
(M-14).

**The OS-semantics probe is the suite's gap, and running the missing cases
found normative defects.** The C probe's 7 assertions cover none of: kqueue
`EVFILT_PROC` (an `Unverifiable` cause and the anti-PID-recycling anchor),
`fstatfs`/`MNT_LOCAL`, `proc_listpgrppids` truncation/doubling branches,
`F_GETLK` `l_pid` for flock-origin locks, or the inherited-descriptor
close-drops-lock hazard. Spot-running them during verification surfaced
H-18's two normative misstatements (buffer units; ESRCH-at-bind) and
confirmed `F_SETLKWTIMEOUT` gives the 10-second lock wait a direct primitive
(§7). Extend `task000_os_semantics.c` accordingly.

**Live-Codex probe campaign (blocking-ordered).** These settle the
[assumed] claims that currently block deterministic implementation:
1. **Approval/elicitation round trip** — enum from schema; writer turn under
   each non-`never` `approvalPolicy`; stdio-MCP elicitation; restart with an
   outstanding request. Settles C-2, H-1's approval half. *Blocking for
   TASK-008.*
2. **`TurnStartParams` carriage** — full property dump; behavior when
   `model`/`developerInstructions` are sent per-turn; resume with altered
   instructions. Settles H-2 (and its Critical escalation question).
3. **Fork boundary statuses** — `thread/fork` with completed /
   cleanly-interrupted / crash-interrupted / failed `lastTurnId`; record
   exact errors. Settles H-3.
4. **`thread/read` vs absent thread** — never-persisted and deleted IDs;
   record error shapes (must be `thread/read`, not `thread/resume`). Settles
   H-6.
5. **`model/list` shape** — pagination reality, identity fields, `isDefault`
   uniqueness, effort element type. Settles H-5.
6. **Response framing** — does `id` precede `result`; max observed
   `thread/read` line size. Settles H-4's ordering assumption.
7. **Cross-version history** — next Codex version resuming/reading a
   0.147.0-created rollout (copy). Informs M-15.
8. **Marker inheritance** — env marker visibility in shell and stdio-MCP
   children (names only). Informs M-17.

Still never exercised live anywhere: approval flows (all of them) and
`codexHome` identity matching (deferred to TASK-005 by the evidence's own
scope note).

---

## 6. Process and gate integrity

**H-38. Round-4 findings have no lawful landing vehicle.** [planning] The
status model allows one ACTIVE task, forbids bypassing BLOCKED, permits
task activation "only after … all SOT contradictions affecting it are
resolved", and made TASK-000 "the sole exception" for SOT-only work — and
TASK-000 is COMPLETE with no reopen rule. The model does contemplate "a
future quiescent SOT-only state" (zero active items is declared valid), but
no rule says who may change the SOT in that state, under what gate, or where
the evidence lands (the gate requires task-scoped commits and NOTE entries).
This review's own disposition work is the first test case. *Fix:* a reusable
rule in § Status Model: a TASK-000-class stabilization task (SOT, evidence,
fixtures; never production code) may be created and activated between any
two production tasks to resolve review findings, numbered TASK-000A/B/…,
using the same completion gate.

**H-39. The strengthened review method is not in the reusable gate.**
[planning] Round-3 §6 mandated, for TASK-000 "**and later task gates**",
(a) an adversarial pass with a stated attack budget against the
concurrency/recovery design and (b) empirical verification of normative
OS-semantics claims. The reusable Task Completion Gate still says only "An
independent read-only review has completed"; "adversarial" appears once in
the roadmap (TASK-000's one-shot clause). The concurrency/recovery tasks
(TASK-007/009/014) inherit the weak generic gate. *Fix:* rewrite gate
condition 4 to require, for any task touching concurrency, recovery, process
identity, or locking: an adversarial pass with a stated attack budget plus
empirical verification of every OS-semantics claim that task makes
normative.

**The closure-method finding, third iteration.** Round 3 recorded that two
consecutive "no findings" closures were followed by Critical defects. The
round-3 closure then adversarially reviewed the patched snapshot — naming
"streamed history", "lock-root/inode authority", "process-group signalling"
among its covered areas — and reported no findings. This round confirmed
defects inside exactly that text: H-4 (the H-11 patch's undelivered
classification rule), H-11 (the H-7 substitution and its three-way
contradiction), H-10/H-12/H-17 (patch-surface coordination edges), and two
Partial dispositions (§1). The pattern is now three rounds old and the
lesson is unchanged from round 3's own words — closures validate documents,
adversarial verification validates *mechanisms*. What found this round's
defects was refutation-first verification with recorded counter-searches and
platform measurements; that is the method H-39 asks the gate to require.
(Also worth stating: the same verification pass *cleared* eight candidate
findings — the method cuts both ways, and §7's do-not-churn list grew
because of it.)

Bookkeeping: M-21 (gate walk + stale status text + NOTE-001 commit + the
uncommitted archive), M-23 (round-2 residue).

---

## 7. What held up — do not churn

Round 3's list stands: forced-recovery removal + TODO-005 parking; the
transient read-only reconcile generation; the four-verdict identity model
with boot-session UUID; the oversized-stdout quarantine posture (round-3
M-11's rejection: the cited ADR text exists verbatim in ADR-007 — verified);
the
`$gomchi_*` marker design; the error-table format; the CLI grammar; the XDG
state root + `MNT_LOCAL`; the fd-3 handshake concept; SIGTERM evidence-first
shutdown; probe anti-gaming discipline; the `docs/reviews/` convention.

Round-4 additions — verified sound under adversarial attack, and in several
cases *because* of it:

- **Byte-1 exclusivity is the real single-worker guard and it works.** The
  byte-0→byte-1 window admits at most a spurious fork whose loser can take
  no observable action (M-1); the claimed double-writer harm was refuted.
  Keep the two-byte protocol exactly as designed; fix only H-12's descriptor
  hygiene and M-1's loser rule.
- **The takeover guard composes correctly.** Bound/ready split + the
  CPU-progress predicate protect a replaying worker; `specs.md:504-505`
  already binds every byte-1 owner to the full guard (the bypass claim was
  refuted). Only M-10's wording fix is needed.
- **macOS is friendlier than the round assumed.** `F_SETLKWTIMEOUT`
  (`sys/fcntl.h:244`) provides the 10-second bounded lock wait directly —
  measured returning `ETIMEDOUT` on schedule. The mandated fork-child
  sequence is achievable entirely with async-signal-safe calls. Do not add
  polling machinery the platform makes unnecessary.
- **The repair/authority derivations hold.** Torn-tail repair authority is
  derivable (worker-only), the idempotency key→turn join is derivable from
  ledger order + the single-active-turn invariant, and `state.json`'s head
  is a workable observer watermark — these designs need *statement*, not
  redesign (M-5, M-7, M-9).
- **The lazy first-turn transaction, the reconcile read-only path, and the
  ledger's group-commit model** survived crash-point walks at the mechanism
  level; the surviving issues are definitional (M-3..M-8).

Advisory (non-normative), from the substrate lane, verified against SDK
headers and crate sources: a runtime model satisfying all 18 SOT-derived
constraints exists — no async runtime; dedicated OS threads (control/
protocol server; stdout pump; stderr pump; ledger/state authority owning
group-commit and replay; kqueue/liveness on its own thread; `sigwait`
signal thread); CLI single-threaded until after fork; `EVFILT_PROC` is
unreachable through tokio/mio (verified in mio source), and every durability
primitive here is blocking-only, which is why the threaded shape fits. The
mechanism→crate inventory (libc-raw for libproc/spawn attrs; nix for
kqueue/statfs/fcntl ranges; write-your-own-or-audit for JCS) is in the
review record for TASK-001's dependency table (M-13).

---

## 8. Recommended order of work

1. **Unblock the process (hours).** H-38: add the stabilization-task rule to
   the status model; open TASK-000A for this round's disposition. H-39: fold
   the method mandate into gate condition 4. Commit the round-3 archive and
   link edits (snapshot note above; M-21).
2. **Probe campaign (days, mostly parallel).** §5's eight live probes —
   approval round trip first (it gates C-2, H-1, and TASK-008's design) —
   plus the OS-probe extensions (H-18, H-12's descriptor case). Every result
   lands in `task-000.md` under the existing pinning discipline.
3. **SOT patch pass 4 (~1-1.5 weeks, no code).** Decide C-1's normalization
   rule first — it is upstream of the lock paths, the socket digest, and the
   TASK-002/007 fixtures. Then C-1 (workspace identity),
   C-2's spec half (SPEC-009 wire pinning once probe 1 lands), then §3 in
   theme order: A (H-1..H-9, mostly one-paragraph tables and rules), B
   (H-10..H-18 — the H-11 three-way reconciliation and H-12's two sentences
   first), C/D (H-19 conformance basis + H-20 spawn attributes), E
   (H-21..H-31 — the payload subsection and the timeout table are the two
   big writes), then the Medium tables (M-1..M-19 are one-sentence-to-
   one-paragraph edits). Record the H-11 scope decision and the H-19
   conformance basis as ADR updates.
4. **Roadmap patch (half day).** §3F (H-32..H-37) + M-12/M-13/M-22: move the
   fake, own control v1, add the fault-injection and clock deliverables, add
   the three missing fixtures, split TASK-003/009, pin the toolchain, add
   the dependency table.
5. **Probe-suite patch (half day).** M-20 centralization; M-14 environment
   pinning.
6. **Independent re-review with the §6 method** (adversarial + empirical, as
   now required by the amended gate), then TASK-000A's gates, then TASK-001.

With the probe campaign and patch pass done, the remaining risk profile is
implementation execution, not design: the grade moves from **B to B+**, with
A− reachable once the conformance/fault-injection machinery (H-32/H-34/H-35)
exists and TASK-013/014 can fail for the right reasons.
