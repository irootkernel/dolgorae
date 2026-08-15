# Gomchi Pre-Implementation Technical Review — Round 5

Date: 2026-08-16
Snapshot note: this is the immutable round-5 input review. Owner dispositions
belong in `docs/reviews/` (per the established convention), so later fixes do
not rewrite the review that motivated them. At round start the round-4 input
review was relocated verbatim from this slot to
`docs/reviews/round-4-input.md` (SHA-256
`c97cf45908bded9b3fc741e98d68b737704bce337fad43f3285137b8a799d3c8`, verified
identical) and the two inbound references were retargeted
(`docs/reviews/README.md`, `docs/reviews/round-4-disposition.md:7`); that
archive and those link edits are uncommitted and should be committed with this
round's artifacts.
Scope: the full SOT at HEAD `a7ef311` (`docs/specs.md` 1130 lines,
`docs/architecture.md` 683, `docs/architecture-decisions.md` 476,
`docs/roadmap.md` 464, `docs/todo.md` 71, `docs/implementation-notes.md` 141,
`docs/deferred-feedback.md` 31), the checked protocol artifacts reviewed here
as first-class surfaces for the first time
(`docs/protocol/codex-0.147.0-required-subset.json` 147,
`docs/protocol/gomchi-error-contract-v1.json` 169,
`docs/protocol/gomchi-machine-v1.schema.json` 247), the toolchain pin
(`rust-toolchain.toml` 5), the probe evidence (`docs/probes/task-000.md` 340)
and probe suite (`tools/probes/`, 13 Python files + 1 C file, including the
round-4-added `task000a_*` probes), and the round-2/3/4 review record
(`docs/reviews/`). Weighted on technical and implementation feasibility, per
request. This is the first review of the post-TASK-000-A state — the first
round in which the checked schemas, the stabilized wire contract, and the
disposition of all 67 round-4 findings are themselves review surfaces.
Method: nine independent review lanes (round-4 disposition-truth audit in two
halves; Codex protocol and probe-evidence integrity; workspace/identity/
locking/OS substrate; durability/audit/serialization; lifecycle end-to-end
with three written walkthroughs — crash→recover→resume, contested takeover,
SIGTERM shutdown; machine-output and error contract; implementation readiness
and traceability, including a fresh 19-MUST reverse-traceability sample;
cross-document consistency and carry-forward). Candidate findings then passed
adversarial verification in three refutation clusters (contracts, systems,
mechanical), each verifier re-reading every load-bearing quote at source,
running fresh counter-searches with recorded terms and hit counts, arguing the
design's side against the finder's steelman, and re-deriving severity
independently; a final calibration merged duplicates across clusters and
ordered severities comparatively. Empirical checks ran on the target platform
(macOS 26.5.2 build 25F84, arm64, Apple clang, git 2.50.1; scratch-space
measurements only; repository untouched except the round bookkeeping above):
the full `task000_os_semantics` C probe (13/13 PASS, no divergence from the
recorded evidence), `POSIX_SPAWN_START_SUSPENDED` + `proc_pidpath` against
direct and wrapper targets, libc `realpath(3)` against APFS firmlinks, the
`tmp_cleaner` LaunchDaemon policy, `git rev-parse` path forms in main and
linked worktrees, `F_SETLKWTIMEOUT` timing and struct layout, `sun_path`
arithmetic, and an independent reimplementation of the SPEC-010 redaction
tokenizer. 91 adversarial scenarios were recorded across the finder lanes and
55+ fresh verification counter-searches across the clusters (the round-4 gate
requires 40). No live Codex turns were executed, per the standing reviewer
conduct. Verification cut both ways: 27 candidate claims were refuted or
materially narrowed and are excluded from the findings below — the strongest
refutations are recorded in §7 as verified-sound design. Several findings
below (H-13, H-19, and the sharpened halves of M-16 and M-21) were discovered
by the verification pass itself.

## Verdict

**The round-4 patch held where round 4 aimed it — and the closure did not.**
The disposition-truth audit of all 67 round-4 findings returns **54 Verified,
12 Partial, 1 Absent** (§1). The independent round-4 closure certified all 67
"Implemented" with "no blocking or non-blocking findings" at `ca91f7d`; twelve
of its rows conceal partially-landed fixes and one conceals a fix that exists
only as a sentence in the disposition file itself. Every Partial hides inside
a grouped disposition row — the exact pattern round 4 predicted at
`docs/reviews/round-4-input.md:136-137`.

**The unreviewed altitude did not hold.** Round 4 reviewed the patch surfaces;
this round reviewed the system end-to-end and the checked artifacts as
artifacts, and both moves found defects the prior method structurally could
not see. The one Critical (C-1) is an OS-level false premise — the mandated
pre-`SIGCONT` identity sample captures the interpreter image, not "the
suspended final executable path", for exactly the wrapper targets SPEC-003
permits and the roadmap names for alpha acceptance — measured live on this
machine, invisible to a probe suite that never calls `proc_pidpath`. The High
band clusters the same way: end-to-end walkthroughs found lifecycle edges with
no legal landing state (H-12), a takeover predicate that kills healthy workers
(H-10), a version-upgrade path that permanently strands paused runs (H-11),
and a frozen control protocol whose escape hatch is wired to no command
(H-13); contract cross-validation found five machine-output states with no
conforming envelope (H-14..H-18); and the checked compatibility manifest — the
round-4 patch's centerpiece — carries a comparison policy that is
unimplementable against its own pointers (H-8), a behavioral-observations
block no probe re-measures and no checker validates (H-7), and one recorded
value no measurement ever produced (M-1).

**The mechanical core is sound.** The 31 error codes are byte-identical across
the error table, the error contract, and the machine schema, with a total
exit-class map; all 28 commands carry exactly one data binding; the two-byte
startup-lock protocol, the takeover guard composition, the JCS output rules,
the redaction escape order, and the torn-tail repair sequence all survived
adversarial re-derivation intact. The gap is not the mechanisms — it is the
seams between documents, the seams between states, and the evidence chain
behind the external-facing claims.

Feasibility **drops to C+** this round, by the rule fixed before any finding
was collected — anchor at round 4's B; any confirmed Critical caps the grade
at C+; fifteen or more confirmed Highs independently blocks B; zero Criticals
with a clean §1 and few Highs permits B+. One Critical is confirmed, and the
22 confirmed Highs would independently block B. The distance back to B is short and
concrete — C-1 is a rename-and-reconcile fix, and most of the High band is
one-sentence completions of decisions already made; the distance from B to B+
runs through the TASK-000-B probe campaign in §5. Severity uses the round-4
rubric verbatim: **Critical** = cannot be implemented as written or defeats a
core promise; **High** = wrong-behavior path or blocks deterministic
implementation; **Medium** = rework/operational pain; **Low** = polish.
Tags: `patch-surface` (text TASK-000-A introduced or reshaped), `pre-existing`
(text predating round 4), `planning` (roadmap/probe layer).

## Findings index (Critical and High)

| ID | Sev | One line | Primary doc | Tag |
| --- | --- | --- | --- | --- |
| C-1 | Critical | Pre-`SIGCONT` identity sample records the interpreter, not the "final executable"; SOT splits on whether the drift is a Mismatch | specs.md | pre-existing |
| H-1 | High | `realpath(3)` does not collapse firmlinks: one `--non-git` directory yields two workspace digests and two writer leases | specs.md | pre-existing |
| H-2 | High | macOS tmp reaper deletes the socket sidecar and eventually the socket root; no document says who creates the root | architecture.md | pre-existing |
| H-3 | High | `writableRoots` is derived from `git rev-parse` forms the spec calls absolute and which return relative paths | specs.md | pre-existing |
| H-4 | High | The 10-second startup-lock wait names no acquisition primitive; `F_SETLKWTIMEOUT` appears nowhere in the SOT | specs.md | patch-surface |
| H-5 | High | The wire event that ends a turn is named in no SOT document and no manifest field; architecture.md claims a manifest key that does not exist | architecture.md | pre-existing |
| H-6 | High | The sole automatic replay authorization gates on generic `-32600`, measured once, on the wrong input, with no negative control | specs.md | patch-surface |
| H-7 | High | `behavioral_observations` is read at runtime, re-measured by no step-3 probe, and validated by no checker | codex-0.147.0-required-subset.json | patch-surface |
| H-8 | High | The manifest's comparison policy is unimplementable against its own pointers and contradicts its own additive-change rule | codex-0.147.0-required-subset.json | patch-surface |
| H-9 | High | The fork-boundary rule exists only for outcome-unknown sources; the ordinary fork after `run interrupt` is undefined | specs.md | patch-surface |
| H-10 | High | The takeover no-progress predicate is unevaluable as written and is satisfied by a healthy worker in a quiet turn | specs.md | patch-surface |
| H-11 | High | `--accept-version-change` exists only on `recover`, which for paused runs starts no generation: upgrades strand paused and outcome-unknown runs | specs.md | patch-surface |
| H-12 | High | `pause`/`close --interrupt` have no defined behavior on expiry of the 5-second wait, and no transition edge exists for any landing | specs.md | pre-existing |
| H-13 | High | Control-protocol v1 `shutdown` — the ADR-011 escape hatch — is wired to no CLI command; a gomchi upgrade strands live runs | specs.md | pre-existing |
| H-14 | High | An `Unreadable` lost-first-turn run must emit `OUTCOME_UNKNOWN` whose required non-nullable `turn_id` does not exist | gomchi-error-contract-v1.json | patch-surface |
| H-15 | High | Failures before a command is identified have no conforming envelope; the closed 28-command enum has no `unknown` member | gomchi-machine-v1.schema.json | pre-existing |
| H-16 | High | Non-`--follow` `run events` has no implementable output contract: one-object cap plus a schema that forbids the batch shape | specs.md | patch-surface |
| H-17 | High | `run respond` has two mutually exclusive input vocabularies, and unpinned decision variants pass validation unmapped | specs.md | patch-surface |
| H-18 | High | `workspace_changes` is scoped to terminal results by the SOT but required on every turn payload by the checked schema | gomchi-machine-v1.schema.json | patch-surface |
| H-19 | High | Omitted `--effort` has no default-derivation rule while omitted `--model` explicitly does; the chosen default feeds the idempotency digest | specs.md | patch-surface |
| H-20 | High | Zero mechanizable traceability: a fresh 19-MUST sample finds 7 uncaught (37%), statistically unchanged from round 4's 33% | roadmap.md | planning |
| H-21 | High | Two roadmap tasks each claim to build the shared fake app-server, and no architecture decision governs the fixture | roadmap.md | planning |
| H-22 | High | No JSON-parsing decision exists; the obvious crate's last-wins default silently defeats the duplicate-member anti-forgery rule | architecture-decisions.md | planning |

Mediums M-1..M-24 and Lows L-1..L-13 are indexed in §4. Three round-4
Partials are closed with evidence rather than re-raised (§1).

## 1. Round-4 disposition verification

All 67 round-4 findings (2 C, 40 H, 23 M, 2 L) were audited against HEAD in
two independent halves, each finding decomposed out of its grouped disposition
row and checked quote-level against the round-4 demand; every
Partial/Absent verdict was then re-verified by a third agent, which also
sample-checked 16 Verified verdicts (0 flips in the first half, 1 in the
second — within tolerance).

**54 Verified, 12 Partial, 1 Absent across 67 audited items.**

| Verdict | Items |
| --- | --- |
| Verified (54) | H-1, H-2, H-4, H-5, H-8, H-10*, H-12..H-17, H-19, H-21, H-22, H-24..H-31, H-32..H-40, M-1..M-9, M-12, M-14..M-23, L-1, L-2. (*H-10 was initially audited Partial and overturned to Verified on re-verification: the zero-continuity clearing conditions are stated at specs.md:810-811 and SPEC-005's no-force disclosure at specs.md:292-296 names process groups.) |
| Partial (12) | **C-1** — the digest/canonicalization fix landed, but the demanded "state the exact root-relative lock paths once" did not: specs.md:579 resolves the lock root to `…/gomchi/locks/` while specs.md:671 and architecture.md:205 name files `locks/writer/<digest>`, composing to a doubled `…/locks/locks/writer/…` reading (re-raised as M-22). **C-2** — the five server-request methods and restart non-survival are pinned and live-probed, but the mandated method-not-found behavioral claim was never observed for any class, while SPEC-012 makes "required server-request probes" a version-acceptance gate (re-raised as M-3); the demanded stdio-MCP elicitation probe is dissolved — MCP elicitation is now out of v1 scope (specs.md:891-897). **H-3** — the forkable-status concept landed, but the scan/fallback ladder at specs.md:843 is scoped to "an outcome-unknown source" while specs.md:834 permits history-copying fork from idle/paused/closed (re-raised, merged into H-9). **H-6** — the never-persisted case is probed via the right RPC; the deleted-thread half of the demanded probe pair was never exercised (closed with evidence — see below). **H-7** — the manifest and "contains" algorithm are rigorous, but zero notification names exist anywhere in the SOT and architecture.md:668-671 falsely claims the manifest lists notifications (re-raised, merged into H-5). **H-9** — caps resolved; architecture.md:377 still lacks specs.md:1019's "when observable" hedge (re-raised as L-5). **H-11** — the scope contradiction is resolved; the demanded amendment to round-3-disposition.md:29 was never made (re-raised as L-6). **H-18** — units/ESRCH text fixes landed; `F_GETLK` was never exercised against a `flock()`-origin lock (closed with evidence). **H-20** — spawn attributes landed; the demanded `std::process::Command` inadequacy note is absent (closed with evidence). **H-23** — the `--follow` stream contract landed, but `$defs.event_data.record` is a bare unconstrained `{"type":"object"}`, "normalized record" remains undefined, and the closed record-kind enum is deferred to TASK-003-C — colliding with the closed-schema rule at specs.md:342-345 (re-raised as M-24). **M-10** — the takeover wording fix landed at specs.md:713-714, but architecture.md:386-387 still reads "replays `audit.jsonl` before accepting commands" — the exact sentence the finding demanded be replaced — and the progress-observed abort branch maps to no outcome code (re-raised as M-21). **M-13** — ADR-012's Darwin/libc boundary landed; the demanded safe-Rust mechanism→binding table and the "no new dependency without an ADR" rule exist nowhere (re-raised, merged into H-22). |
| Absent (1) | **M-11** — the demanded one-sentence normative constraint ("CLI creates no thread before fork; the child performs only async-signal-safe operations before exec") appears nowhere in specs.md, architecture.md, or the ADRs. The disposition row (round-4-disposition.md:39) asserts it as settled fact — the sole repo-wide occurrence of "async-signal" is that row. architecture.md:607-611 mandates a multi-threaded worker while architecture.md:68-70 mandates the fork; the constraint that reconciles them is stated only in a file implementers are not required to read (re-raised as M-23). |

Three Partials are **closed with evidence rather than re-raised**: round-4
H-6's deleted-thread probe (Gomchi is forbidden to delete Codex threads,
specs.md:1049-1051, and task-000.md:231-233 records the stated cache reason
for substituting `thread/list`); H-18's `flock`-origin `F_GETLK` case (every
`F_GETLK` in the SOT is scoped to the startup lock's byte ranges — the two
lock namespaces never cross); H-20's `std::process::Command` note (ADR-012
:457-459 already confines spawn to the one `libc` module, making the note
redundant). The remaining ten Partials and the Absent are re-raised at the
severities above. One process consequence is recorded in §6: the round-4
closure certified this exact state as "no blocking or non-blocking findings."

## 2. Critical findings (1)

### C-1. The pre-`SIGCONT` identity sample records the interpreter image, not the executable the SOT says it records, and the two SOT documents contradict each other on whether the resulting drift is a Mismatch.  [Critical, pre-existing]

**Where:** specs.md:663-668 (SPEC-007 suspended-spawn identity),
specs.md:614-619 (runtime identity tuple and continuity rules),
specs.md:649-651 (group verdict derivation), specs.md:159-161 (SPEC-003
wrapper targets); architecture.md:116-124 (suspended sample),
architecture.md:482-485 (identity acceptance rules); roadmap.md:449 (TASK-015
alpha acceptance targets); tools/probes/os/task000_os_semantics.c:343-388.

**Problem.**
1. specs.md:663-665 mandates: *"Before `SIGCONT`, the worker opens the
   suspended final executable path without following symlinks and derives
   device, inode, and SHA-256 from that same fd"* — and specs.md:667 requires
   *"the complete ten-field provisional identity"* with
   *"Replacement/unavailability is `Unverifiable`, not a partial record."*
   architecture.md:117-118 repeats the premise: *"While the final image is
   suspended, the worker samples `PROC_PIDTBSDINFO`, opens the `proc_pidpath`
   target."*
2. `POSIX_SPAWN_START_SUSPENDED` suspends the child after the kernel loads
   the **first** image. For a shebang or exec-chain target that image is the
   interpreter — the "final executable" does not exist yet, by construction.
   SPEC-003 explicitly blesses such targets (specs.md:159-160: *"A target MAY
   use wrapper argv for additional environment preparation."*).
3. This is not hypothetical: one of the two targets roadmap.md:449 names for
   TASK-015 alpha acceptance is, verbatim on this machine, a five-line
   `#!/usr/bin/env bash` wrapper ending in `exec codex "$@"`. Measured with a
   structural replica under the SOT's own spawn attributes: pre-`SIGCONT`
   `proc_pidpath` = `/usr/bin/env` (a **third** image — neither the wrapper
   the user named nor the executable that serves the protocol);
   post-`SIGCONT` = the final binary; PID, PGID, UID, and BSD start time
   identical across both execs. A direct-binary control shows no drift. Four
   of the ten identity fields therefore record `/usr/bin/env`.
4. The recovery consequence is a live SOT-vs-SOT contradiction. A recoverer
   arriving in the `[SIGCONT, generation_started)` window compares the live
   app-server against the provisional record. specs.md:617-619 grants only a
   path-*unavailable* exception and adds *"replacement at the same path is
   not identity continuity"* → **Mismatch**; architecture.md:485 states the
   opposite — *"path replacement alone is not identity mismatch"* → **Match**.
   Under the Mismatch reading, specs.md:649-651 yields group `Unverifiable`
   → non-retryable `RECOVERY_REQUIRED` with no force override: the run is
   stranded, escapable only by `fork --fresh` at the cost of run identity.
   specs.md:8-9 declares an unreconciled cross-document contradiction an
   invalid state.
5. Nothing in the checked evidence base could have caught this: the C probe's
   `test_suspended_spawn` (task000_os_semantics.c:343-388) spawns `/bin/sleep`
   directly and never calls `proc_pidpath` — counter-searches: `proc_pidpath`
   in the C probe → 0 hits in 477 lines; `wrapper|shebang|interpreter` across
   specs.md/architecture.md → 1 hit, the defective claim itself.

**Fix.** Rename the provisional executable fields to **spawn-image** fields in
specs.md:663-668 and architecture.md:117-120, stating plainly that for
wrapper argv the spawn image is the interpreter. Declare an entry-image →
final-image transition for the same PID + PGID + UID + start time **not** a
`Mismatch`, and reconcile specs.md:617-619 with architecture.md:485 in one
direction. Keep the post-handshake `generation_started` sample as the sole
authority for final-executable identity (architecture.md:192 already says
this — make specs.md agree). Add the TASK-000-B wrapper probe (§5, P-1).

## 3. High findings

### A. Workspace identity, locking, and the OS substrate

**H-1. libc `realpath(3)` does not collapse macOS firmlinks, so one `--non-git` directory yields two workspace digests and two independently acquirable writer leases.** [pre-existing]
SPEC-002 makes `realpath(3)` the sole canonicalizer and asserts that
"filesystem alias resolution, including symlink and case-insensitive lookup,
belongs to `realpath(3)`" (specs.md:65-66) — empirically false for firmlinks.
Measured: `/Users/…/gomchi` and `/System/Volumes/Data/Users/…/gomchi` return
two distinct canonical strings for the same `(dev, ino)`; `/usr/share/firmlinks`
lists 19 such roots including `/Users`, `/private`, `/opt`, `/Applications`,
`/Volumes`, `/usr/local`. Computing the SPEC-002 preimage over both paths
yields two digests, hence two `locks/writer/<digest>` files, and both workers
acquire "the" lease — specs.md:574's one-writer promise fails silently. Git
mode is rescued (measured: `git rev-parse --show-toplevel` normalizes the
prefix), so the exposure is `--non-git` mode; re-`init` conflicts
(specs.md:126-128), but an ordinary `run start --workspace <Data-path>` on an
initialized directory is compared against nothing, and in the concurrent case
both runs see `Absent` (specs.md:623) with the split flock as the only guard.
Counter-searches: `firmlink|Volumes/Data|volume group` in SOT → 0 hits.
*Fix:* strip a leading `/System/Volumes/Data` after `realpath`, or add the
workspace `(st_dev, st_ino)` to the digest preimage and recorded identity and
reject a canonical-path change whose inode matches; TASK-000-B probe P-2.

**H-2. The macOS tmp reaper deletes the socket identity sidecar and eventually the socket root itself, and no document says who creates that root.** [pre-existing]
architecture.md:159-162 only *validates* `/tmp/gomchi-<uid>/s/` ("opened once
without following symlinks and accepted only when it is owned by the current
uid with mode 0700"), while the lock root receives full creation treatment
(specs.md:584-586: create-exclusive, `EEXIST`, `fstat` 0700, `fstatfs`
`MNT_LOCAL`). Verified live: `com.apple.tmp_cleaner.plist` is a default
LaunchDaemon (`Hour=0`) whose script deletes `-type f` files older than 3 days
and `-empty` directories — a bound AF_UNIX socket is `-type s` and survives;
the sidecar is a regular file and is deleted. specs.md:203-205 guarantees idle
workers live indefinitely, so a 3-day-idle **live** run reliably reaches
socket-present/sidecar-absent — a state the sidecar-based collision rule
(architecture.md:168-171) does not classify and specs.md:744's "Any other
occupied path fails closed" wedges; and once the sockets are gone the empty
`s/` and `gomchi-<uid>/` are reaped, after which worker start has no specified
creation path on a measured mode-1777 `/private/tmp` where the name can be
squatted. Counter-search: `socket root` creation rule → 0 hits.
*Fix:* add a socket-root creation rule mirroring specs.md:584-586; classify
socket-present/sidecar-absent as cleanable by the byte-0 election winner (or
move the sidecar under `.gomchi/runtime/`, already the discovery authority);
state that the root is volatile by OS policy. TASK-000-B probe P-3.

**H-3. `writableRoots` — the Codex sandbox boundary — is derived from two `git rev-parse` results the spec calls absolute and which are measured relative.** [pre-existing]
specs.md:570-572 defines `writableRoots` as "the sorted unique set of the
canonical workspace plus, in Git mode, absolute `git rev-parse
--git-common-dir` and `git rev-parse --git-path .` results." Measured with
git 2.50.1 (inside the stated ≥2.39 floor): from the main worktree these
return `.git` and `.git/.` — relative, and two distinct strings for one
directory, so the "sorted unique set" carries a redundant entry; from a
subdirectory they return `../.git`, and no absolutization base is named — a
`../.git` resolved against the wrong base lands a writable root outside the
workspace. A **linked** worktree returns absolute paths for both, so an
implementer exercising roadmap.md:286-287's linked-worktree test never sees
the defect. `--path-format=absolute` (git ≥2.31) yields correct absolute
normalized results for both forms. Counter-search: `path-format|absolutiz` →
0 hits.
*Fix:* pin `git -C <canonical-workspace> rev-parse --path-format=absolute
--git-common-dir` / `--git-path .`, apply `realpath(3)`, and require every
`writableRoots` entry to be absolute and deduplicated by resolved path, with
a main-worktree fixture beside the linked-worktree one.

**H-4. The mandatory 10-second bounded startup-lock wait names no acquisition primitive, and both mechanisms an implementer would infer are wrong.** [patch-surface]
specs.md:689-690 ("A contender waits at most ten seconds for ownership
handoff") and the timeout table (specs.md:512-513) make the bound normative,
while the SOT names every other OS primitive it relies on (`flock(2)`,
`F_GETLK`, `fstatfs`/`MNT_LOCAL`, `EVFILT_PROC`/`NOTE_EXIT`,
`proc_listpgrppids`, the `POSIX_SPAWN_*` attributes). `F_SETLKWTIMEOUT` — the
primitive round 4 measured, selected, and protected with "Do not add polling
machinery the platform makes unnecessary. Extend `task000_os_semantics.c`
accordingly" — appears zero times in specs/architecture/ADRs/roadmap/protocol
(all 8 repo hits are the round-4 archive), and the instructed probe extension
was never made (`F_SETLKW` in the C probe → 0 hits). The inferable
alternatives are both wrong: `F_SETLKW` is unbounded; an `F_SETLK` poll loop
is the rejected design. Re-measured this round: contended byte 0 with a 2 s
budget returns `ETIMEDOUT` at 2.001 s — and the argument struct is
`struct flocktimeout {struct flock fl; struct timespec timeout;}`
(`sys/fcntl.h:406`); the opposite field order fails `EINVAL` instantly, a
footgun an implementer would misread as "unsupported."
*Fix:* one sentence in SPEC-007 naming `F_SETLKWTIMEOUT` with the ten-second
budget and the `flocktimeout` field order; mirror in architecture.md:208-221;
add the probe case (P-4).

### B. The Codex wire contract and its evidence base

**H-5. The wire event that ends a turn is named in no SOT document and no manifest field, and architecture.md asserts a manifest key that does not exist.** [pre-existing]
A turn ends "only when Codex confirms completed, interrupted, or failed
status" (specs.md:19-21), and three behaviors rest on detecting that event:
`send` blocks on it with no default timeout (specs.md:516), the SIGTERM path
waits 5 s for it (specs.md:738-739), and recovery branches idle-vs-
`outcome_unknown` on it (specs.md:821-824). No notification method is named
anywhere normative — `turn/completed` appears 7 times in the repo, all in
`tools/probes/*.py`, a corpus whose own evidence doc disclaims authority
(task-000.md:15-17). architecture.md:668-671 asserts the checked manifest
lists "resolved JSON Pointers, methods, responses, **notifications**" — the
manifest contains **zero** notification entries (0 hits for "notification" in
the file), exactly one app-server response schema (`ModelListResponse`; the
other five `*Response` pins are gomchi's outbound replies), and 11 pointers
that terminate at a `$ref` key rather than a resolved node. There is also no
Codex-status → Gomchi-status mapping: the Gomchi-side vocabulary *is* closed
in both checked schemas, but `crash_interrupted` (manifest:143) sits outside
it with no stated mapping, and "unusable status" (specs.md:261, 410) is used
twice and defined nowhere. SPEC-012's version gate covers this only
incidentally — condition 2 compares manifest pointers (none of which pin a
status or notification), and condition 3's coverage exists only because two
probes happen to hard-code `turn/completed` internally, which is probe
plumbing, not a contract. An implementer cannot write the turn engine without
inventing the central wire event's name.
*Fix:* add a `notifications` object (terminal notification method, turn-status
JSON Pointer) and a Codex→Gomchi status mapping with three closed lists
(terminal / nonterminal / unlisted ⇒ `Unreadable`) to the manifest; one
SPEC-008 sentence mapping the lists to the idle/`outcome_unknown` branch;
correct architecture.md:668-671 to describe what the manifest actually
contains; add the condition-3 turn-completion probe (P-5).

**H-6. The sole automatic replay authorization gates on a generic JSON-RPC code that was measured once, on the wrong input, with no negative control.** [patch-surface]
specs.md:258-267 authorizes replacing the provisional thread and re-executing
the reserved first-turn intent when `thread/read` returns "the pinned
manifest's exact absent-thread error" — `-32600` (manifest:141), JSON-RPC's
reserved *generic* "Invalid Request" code. The sole measurement is
`thread/read` on a random UUIDv4 (task000a_contract_probe.py:130-131). The
gate's real production input — a real, previously-created, turnless
provisional thread after app-server restart — was never measured through
`thread/read`: the resume probe measured that condition through a different
method (`thread/resume`), matched it by message substring ("no rollout
found"), and never recorded the code. Both unmeasured directions are harmful:
if the real input returns anything ≠ `-32600` it routes to `Unreadable` and
the ADR-003 retry path is dead in every real crash; if any *other*
`thread/read` failure also returns `-32600`, gomchi promotes it to `Absent`
and performs exactly the automatic replay of a side-effecting intent that
ADR-008 forbids. Round-4 H-6's own fix demanded probes against "a
never-persisted **and a deleted** thread ID"; only the first was delivered.
Counter-searches: "invalid request" in SOT → 0 hits; `-32600` repo-wide → 3
hits (one measurement, one assertion, one constant), no negative control.
*Fix:* require the manifest code **and** a manifest-pinned message
discriminator (or a second independent absence proof) for `Absent`; record
the turnless-provisional case in `behavioral_observations`; TASK-000-B probe
P-6 (turnless `thread/read` after restart + a negative control proving a
distinct failure returns a distinct code).

**H-7. The manifest's `behavioral_observations` block is read at runtime, re-measured by no compatibility probe, and validated by no checker — and already contains one value no measurement produced.** [patch-surface]
SPEC-008:844 reads `forkable_turn_statuses` from the checked manifest at fork
time. SPEC-012 step 3's probe list (specs.md:1113-1115) covers three of the
block's six facts and omits `forkable_turn_statuses`,
`pending_requests_survive_app_server_restart`, and
`unanswered_command_approval_status_after_restart` — an `unverified` newer
Codex reaches production without re-measuring the fork boundary, restart
behavior, or effort leniency. Independently, `behavioral_observations` has
exactly one occurrence repo-wide (its own definition), and the single artifact
that opens the manifest (`task000_schema_probe.py:264`) validates only the
bundle SHA, `schema_constraints`, `client_methods`, and `server_requests`;
every probe's expected values are local constants, so manifest and evidence
can drift silently. The drift is not hypothetical: `rejected_fork_boundary_
statuses: ["crash_interrupted"]` (manifest:143) records a status string no
probe produced and Codex never emitted — every recorded measurement says
`interrupted` (M-1).
*Fix:* extend SPEC-012 step 3 to re-measure every `behavioral_observations`
entry or reject the version; have each probe read its expected values from
the manifest and assert equality; add the block to the schema probe's
validated set.

**H-8. The manifest's comparison policy is unimplementable against its own pointers, and its positional pins contradict the additive-change rule it declares.** [patch-surface]
manifest:5-10 sets `resolve_refs: true` and `additive_fields_and_enum_values_
allowed: true`; specs.md:1110-1111 orders "after resolving `$ref`, every JSON
Pointer … has the same type/const/requiredness." Executed both ways against
the repo's own resolver (`task000_schema_probe.resolve_pointer`, which has no
`$ref` branch): under the SOT-mandated resolved reading, the **11 pointers
that terminate at a `/$ref` key fail on every version including the pinned
one**; under the unresolved reading that actually passes, `resolve_refs:
true` is a false statement in a checked artifact. Twenty-two pointers carry
array indices (`anyOf/0`, `oneOf/4`, …): an additively inserted variant
shifts them and produces `COMPATIBILITY_REJECTED` for exactly the class of
change the policy admits — and `oneOf` position is already unstable *within*
0.147.0, where `decline` sits at index 2 in `FileChangeApprovalDecision` and
index 4 in `CommandExecutionApprovalDecision`. Finally, the `$ref` targets
`ClientInfo` and `ReasoningEffort` have zero constraining pointers, so
`ReasoningEffort` could stop being a string and all 69 constraints still pass.
*Fix:* constrain the resolved definitions (`/definitions/SandboxMode/enum`,
`…/ReasoningEffort`, `…/ClientInfo`) instead of `$ref` keys; replace
index-bearing enum pins with a value-set rule ("some `oneOf` branch's `enum`
contains X"); state the resolution semantics in SPEC-012; implement the
comparison block by reading the manifest's own flags.

### C. Lifecycle, recovery, and takeover

**H-9. The `lastTurnId` boundary rule exists only for outcome-unknown sources, leaving the ordinary history-copying fork — the common case — undefined.** [patch-surface]
specs.md:843-847 scopes the newest-first forkable scan to "For an
outcome-unknown source," yet specs.md:834-835 allows history-copying fork
from idle, paused, and closed runs, and three routine paths produce an idle
run whose newest turn is `interrupted`: crash reconciliation (measured:
persisted `interrupted`, task-000.md:207), `run interrupt`, and
`pause --interrupt`. `interrupted` is not forkable — the manifest lists only
`["completed"]`, and task-000.md:228 measured that `thread/fork(lastTurnId)`
**rejects** an interrupted boundary. `lastTurnId` appears in SOT prose
exactly once (a definition), so an implementer chooses among omitting it
(whether that succeeds on an interrupted-head thread was never probed),
reusing the outcome-unknown scan, or passing the newest turn (proven to
fail) — three divergent behaviors — and no error code covers a refused
boundary. Round-4 H-3's patch closed the rare branch and left the usual one
open.
*Fix:* generalize the scan to every history-copying fork regardless of source
state; add an error-table row (or explicit `COMPATIBILITY_REJECTED`
condition) for a boundary the target refuses; TASK-000-B probe P-7.

**H-10. The takeover no-progress predicate is unevaluable as written and cannot distinguish a healthy worker from a wedged one, so a bystander command can kill a live writer mid-turn.** [patch-surface]
specs.md:721-731 permits TERM/KILL of a revalidated `Match` after `hello`
exceeds ten seconds plus "an additional 30 seconds with no progress in ledger
head, runtime generation, the monotonic `worker_log_bytes_written` counter,
or process CPU." `worker_log_bytes_written` occurs exactly once in the
repository (its definition) and is absent from `state.json`
(architecture.md:384-386), the runtime record (architecture.md:183-187), and
the manifest — an external recoverer cannot read it, and the file-size proxy
is broken by design (rename rotation under a 1 MiB cap while "rotation …
never resets that counter"). The other three signals are structurally static
for a *healthy* worker in a quiet turn: `worker.log`'s content is a closed
six-kind list with nothing turn-related (specs.md:726-728), runtime
generation is constant within a generation, ledger head advances only on
Codex output, and CPU is ~0 for a worker blocked in `read`/`kevent`. None of
the three abort conditions fires. The guard's only real trigger is a worker
that is alive but unreachable for transport reasons — a saturated accept
backlog, fd exhaustion, or the reaped socket path of H-2 — which it then
kills; and specs.md:206-208 applies the procedure to *every* attaching
command, so a read-only `run pending` can drive a live writer's turn to
`outcome_unknown`.
*Fix:* replace the derived counter with a durable beacon — a monotonic
progress counter plus coarse timestamp updated in `.gomchi/runtime/runs/
<run-id>.json` at a fixed interval below 30 s (round-4 M-19 offered exactly
this alternative) — and define whose CPU is measured and how it is sampled;
TASK-000-B probe P-8.

**H-11. `--accept-version-change` exists only on `run recover`, which for a paused run starts no generation, so a routine Codex upgrade permanently strands every paused and every outcome-unknown run.** [patch-surface]
specs.md:298-300 makes `run recover --accept-version-change` the sole path to
accept a changed app-server version, bound to "before starting the
replacement generation" and "after the full compatibility gate passes"
(specs.md:301-302) — a gate requiring live `initialize` (specs.md:1113). But
"Recovery of `paused` performs only coordination cleanup and record repair;
`resume` starts its next generation" (specs.md:819-820): recover-of-paused
never reaches the gate, and `resume` — the generation-starting command — has
no flag in its grammar (specs.md:237) and returns `COMPATIBILITY_REJECTED`
(specs.md:463). The same shape closes the second escape: `reconcile` starts a
transient generation, is in the same emitter list, and has no flag, so an
`outcome_unknown` run can never be reconciled to `paused` after an upgrade
either. Only `fork` escapes (a new run records the new version), at the cost
of run identity and audit continuity.
*Fix:* add `[--accept-version-change]` to `run resume` and `run reconcile`
in the SPEC-005 grammar, plus one SPEC-008 sentence stating any
generation-starting command may carry it under the same recorded-ledger-event
conditions.

**H-12. `pause --interrupt` and `close --interrupt` have no defined behavior when the 5-second interrupt terminal wait expires, and the state machine has no legal edge for any landing.** [pre-existing]
specs.md:813-815 says only "Gomchi requests interruption, waits for a
terminal result, then stops or closes the run"; specs.md:512 caps the wait at
five seconds. Every landing is blocked: recording `idle` without evidence is
forbidden by ADR-008; the transition chain (specs.md:790-801) contains no
`running -> paused`, `running -> closed`, `waiting_approval -> paused`, or
`waiting_approval -> closed` edge; `close` can reach `closed` only via
`outcome_unknown -> closed` (:800) while `pause` can reach `paused` only via
`outcome_unknown -> paused` (:799), which specs.md:828-830 gates on reconcile
producing terminal evidence that by hypothesis does not exist — so `close`
has an exit and `pause` does not; and leaving the run `running` means a
wedged turn can never be stopped (`send` has no default timeout; neither
command accepts `--timeout`). The `waiting_approval` sub-case adds a second
gap: nothing states whether a pending approval receives the defined `cancel`
decision before `turn/interrupt` or is left unanswered.
*Fix:* map expiry to `outcome_unknown`, add the transition edges and the
`OUTCOME_UNKNOWN` result for both commands (see M-7 for the emitter-list
half), state the cancel-decision ordering, and add TASK-000-B probe P-9
(interrupt with a pending approval, answered and unanswered).

**H-13. Control-protocol v1's `shutdown` is wired to no CLI command, so the upgrade deadlock ADR-011 exists to prevent is unreachable and a routine gomchi upgrade strands every live run.** [pre-existing]
ADR-011 freezes `hello`/`status`/`shutdown` precisely because "Requiring an
exact Gomchi binary digest for every worker request prevents a new binary
from identifying or cleanly stopping a live worker created by the old binary"
(architecture-decisions.md:416-419), and specs.md:717-718 keeps ordinary
mutations digest-locked. But across all 19 `shutdown` occurrences in the SOT,
no command in the SPEC-005 grammar (specs.md:215-244) is mapped to control-v1
`shutdown`: `run recover`'s enumerated actions (specs.md:285-286) do not
include it, and the takeover path uses `hello` then signals. After a binary
upgrade, every ordinary command against a live old worker returns
`GOMCHI_PROTOCOL_MISMATCH` (specs.md:464) — including `pause` and `close`,
which are mutations — while the error's own remediation text ("retry `hello`,
bounded `status`, or `shutdown` through control protocol v1") names
operations no documented command performs. The plausible intent — the CLI
silently escalates to control-v1 `shutdown` on digest mismatch — is stated
nowhere.
*Fix:* name the commands that issue control-v1 `shutdown` on detected version
skew (naturally `run pause`/`run close`/`run recover`), and add an
upgrade-path fixture beside roadmap.md:222's digest-skew control fixtures.

### D. The machine-output contract

**H-14. An `Unreadable` lost-first-turn run must emit `OUTCOME_UNKNOWN` envelopes whose required non-nullable `turn_id` does not exist.** [patch-surface]
specs.md:261-265 routes an `Unreadable` `thread/read` to `outcome_unknown`
while the permanent binding "begins when accepted history or the response
supplies a turn ID" — by construction neither did. Every subsequent
`send/submit/respond/interrupt/set-effort/promote/demote/pause/resume` must
then emit `OUTCOME_UNKNOWN` (specs.md:460), whose details schema
`d_run_id_turn_id` (gomchi-error-contract-v1.json:149) requires a
non-nullable `turn_id` — and none of those commands takes one as an argument.
The contract has the vocabulary (`nullable_string`/`nullable_uuid` are used
five times; `run.active_turn_id` is nullable in the machine schema) but the
shape is shared with `TURN_NOT_FOUND`, where the caller supplies the id. The
only conforming escape is `turn_id: ""` (validates — `$defs/string` has no
`minLength`), which the SOT nowhere sanctions, so implementations will
diverge across `""`, the provisional thread id, and a synthesized UUID, and a
master treating the field as assertable poisons `run wait`.
*Fix:* split a `d_outcome_unknown` with nullable `turn_id` plus required
nullable `thread_id`; one sentence at specs.md:264 stating a quarantine with
no bound turn reports a null turn id. (Blocked by M-9's missing version
procedure — sequence that first.)

**H-15. Failures that occur before a command is identified have no conforming envelope, and the "stable" exit-class set is not exhaustive.** [pre-existing]
The machine schema requires `command` (gomchi-machine-v1.schema.json:215)
drawn from a closed 28-value enum with no member for "unidentified", while
`INVALID_ARGUMENT` is chartered for "CLI syntax" from "any command"
(specs.md:443). Bare `gomchi`, a typo'd subcommand, and an unknown top-level
flag therefore have no legal envelope; `--help`/`--version` appear zero times
in the SOT; exit 1 is never reserved; and the exit table declared "stable"
(specs.md:425-436) already omits exit 130, which specs.md:504 documents — so
a machine master cannot distinguish a structured failure from a parser exit,
a panic, or a signal.
*Fix:* add an `"unknown"` member to the `command` enum usable only with
`ok:false`; one sentence stating statuses outside {0, 2-8, 130} carry no
envelope and MUST NOT be interpreted as an error code; define `--help` and
`--version`.

**H-16. Non-`--follow` `run events` has no implementable output contract.** [patch-surface]
specs.md:308 caps a finite command at "exactly one newline-terminated JSON
object"; specs.md:493 reserves multi-object output to `--follow` (the two are
consistent — `--follow` is not finite — which is exactly the problem); and
the checked schema binds `run.events` data to `$defs/event_data`
(gomchi-machine-v1.schema.json:234), all three variants single records with
`additionalProperties: false`, so the `{"items":[…]}` batch shape available
to every other collection command is structurally forbidden here. A plain
`run events <id> --after 5` on a 100-record ledger therefore cannot return
more than one record — making the `--after` cursor pointless — and
`--after`'s default when omitted (genesis vs tail) is never stated. The
entire :493-505 paragraph specifies the follow stream; the non-follow mode of
a public command is specified only by the one-object cap that makes it
useless. Round-4 H-23 addressed `--follow` only.
*Fix:* rewrite specs.md:493 — `run events` may emit multiple objects in both
modes; non-follow emits records to the current head, then one `end` frame,
exit 0; `--after` defaults to 0; except the events stream from :308.

**H-17. `run respond` has two mutually exclusive input vocabularies, and under the literal reading it forwards approval decisions gomchi never enumerated.** [patch-surface]
specs.md:900-903 has `run respond` validate the master's JSON "against that
schema" — the manifest's Codex response schema, whose decision enum is
`accept`/`acceptForSession`/`decline`/`cancel` — while specs.md:913-921
exposes `accept_once`/`accept_for_generation`/`decline`/`cancel` and maps
them to those wire values. `{"decision":"accept_once"}` fails the mandated
validation (the Gomchi tokens are unreachable through the only input path),
or the raw wire vocabulary is the real input and the mapping table is dead.
Worse: the manifest pins `CommandExecutionApprovalDecision` at `oneOf`
indices 0, 1, 4, 5 — indices 2 and 3 provably exist (all 69 constraints
matched live 0.147.0) and are unconstrained, so under the literal reading a
master-supplied index-2/3 decision passes validation and is forwarded
verbatim to app-server: an approval decision gomchi never enumerated, mapped,
or audited as one of its four. `request.response_schema` is also a bare
string identifier dereferenceable nowhere.
*Fix:* make the input a closed gomchi-decision object; gomchi translates and
validates the **translated** object; constrain `response_schema` to the
manifest identifiers; add a decision `$def` to the machine schema.

**H-18. `workspace_changes` is scoped to terminal results by the SOT but required on every turn payload by the checked schema.** [patch-surface]
specs.md:534-546 introduces the member with "**A terminal result** includes …
`workspace_changes`", and the derivation (specs.md:548-553) is a
turn-interval measurement (git status, or pre/post inode snapshots — a
*post* snapshot that does not exist mid-turn). The checked schema makes it a
required member of the shared `turn` object (gomchi-machine-v1.schema.json:
132), which `run.submit` (returns an *accepted* turn, specs.md:371-372) and
timed-out `run.send`/`run.wait` (return nonterminal turns, specs.md:516-518)
also use. The implementer must emit `observed_paths: []` on a turn that has
not ended — `attribution:"unverified"` disclaims causation, not observation
(specs.md:557), so a master reads `[]` as "no changes" — or run the full
derivation on every submit and every poll, unbudgeted in the timeout table
and contradicting submit's specified fast return.
*Fix:* one sentence scoping population to terminal turns plus a
`measured: false` member (or make the member optional) — a v1 schema change,
blocked by M-9; bundle with H-14's amendment.

**H-19. Omitted `--effort` at `run start` has no default-derivation rule, while omitted `--model` explicitly does — and the chosen default feeds the idempotency digest.** [patch-surface]
specs.md:873-874 gives the model rule ("omitted `--model` selects exactly one
`isDefault`"); no parallel sentence exists for effort anywhere (24 `effort`
hits in specs.md, 5 in architecture.md — resolution covers only syntactic
validity, `set-effort` validation, and unadvertised-value rejection). Yet
`run.effort` and `turn.effort` are required non-nullable strings in the
checked schema, the manifest records "initial/default reasoning effort" as a
fixed fact, every `turn/start` sends "selected reasoning effort"
(architecture.md:537), and nothing in the manifest's `required_shapes` marks
a default. Two implementations pick different reasoning depths for the same
command; and because "requested/default effort" is folded into the JCS
idempotency digest (specs.md:527-528), a change in the chosen default turns
a legitimate key replay into `IDEMPOTENCY_CONFLICT` against a digest fsynced
under the old rule. Whether `send`/`submit --effort` is one-shot or updates
the run default is equally undefined.
*Fix:* one SPEC-008 sentence defining the omitted-`--effort` default (e.g.
the resolved model's first advertised `supportedReasoningEfforts[]` entry,
recorded in the manifest at run creation) and one stating whether per-turn
`--effort` is one-shot or sticky.

### E. Implementation readiness (planning layer)

**H-20. The roadmap has no mechanizable traceability, and a fresh sample proves the miss rate is structural rather than residual.** [planning]
roadmap.md contains zero `SPEC-` and zero `ADR-` references across 464 lines.
Round 4 sampled 21 load-bearing MUSTs and found 7 uncaught; the disposition
patched exactly those 7 (all independently re-verified as now caught). This
round's fresh sample of 19 *different* MUSTs found 7 newly uncaught — 37%
against round 4's 33%: SPEC-001's daemon/launchd MUST NOTs (`daemon|launchd`
→ 0 roadmap hits), the digest-preimage cross-consistency rule
(specs.md:79-80), the `.gitignore` content rule (specs.md:114-116),
no-retarget-after-registry-edit (specs.md:170), thread laziness
(specs.md:247-248), image-byte non-copy (specs.md:275), and `--human`
semantic neutrality (specs.md:311). The design's best defense — TASK-015's
"every SOT contract has deterministic evidence" (roadmap.md:459-460) — is the
defect in one sentence: a terminal acceptance clause asserting total coverage
with no mechanism able to evaluate it. The sample-driven patch pattern
guarantees the next round finds another third.
*Fix:* annotate each task's verification list with the SPEC IDs it catches
(`Catches: SPEC-002 §init, …`), and make the reverse index — every normative
MUST maps to ≥1 owning verification — a checked artifact rather than a
review activity.

**H-21. Two roadmap tasks each claim to build the shared fake app-server, and no architecture decision governs the fixture.** [planning]
roadmap.md:215 puts "the **one shared** controllable fake app-server/worker
fixture used by later Tasks" in TASK-004's scope; roadmap.md:250-251 puts
"the **minimal controllable fake app-server core** required for deterministic
verification" in TASK-006's; TASK-013 (roadmap.md:421) extends only
TASK-004's; TASK-005/006/008 all verify against "the fake" without saying
which. This contradicts the round-4 disposition's own H-32 claim ("One shared
fake begins before its consumers"). Beneath the duplication, the fixture has
no architecture decision of any kind — process model (in-process vs
subprocess speaking real stdio JSONL), language, repo location, and drift
ownership against the checked manifest are all unowned (`in-process|
subprocess|test double|harness` → 0 SOT hits) — while it is the sole
verification substrate for four tasks and the seed of TASK-013's conformance
executable. The serial rule makes a wrong call expensive (discovered at
TASK-008, unwound backward), and roadmap.md:22-26's stabilization-task rule
("never production code") leaves it unclear whether a `TASK-004-A` may even
lawfully repair a test fixture.
*Fix:* delete the duplicate core from TASK-006's scope so TASK-004 is the
single owner; add one architecture paragraph fixing the fixture's process
model, location, and the artifact it must not drift from (the checked
manifest); clarify the stabilization rule's application to test fixtures.

**H-22. No JSON-parsing or dependency-governance decision exists, and the obvious crate silently defeats a normative anti-forgery rule on TASK-001 day one.** [planning]
specs.md:947-948 requires inbound JSON to "reject duplicate object members
and preserve number lexemes until numeric adaptation," and specs.md:422 makes
duplicate members "protocol violations rather than last-wins input" — the
control that keeps round-3 H-10's `^\$+gomchi_` marker-escape rule
(specs.md:953-955) unwalkable. `serde_json`'s default violates both
(last-wins duplicates; lexemes lost). The only crate named anywhere in the
SOT is `libc` (ADR-012:458); ADR-012:472 rejects `serde_json` for
*serialization* only, which an implementer will read as leaving parsing
untouched; and ADR-007:265-266 plus roadmap.md:161 assign "duplicate
rejection" to the **canonicalizer** — a component architecture.md:617-618
scopes to canonicalization and which structurally receives an already-parsed
value. The same vacuum covers CLI parsing, UUIDv7, base32/base64, SHA-256,
and TOML — the safe-Rust half of round-4 M-13's demanded mechanism→binding
table, which never landed, along with its companion "no new dependency
without an ADR" rule (0 hits). ADR-012's own context states the stake:
"Leaving their shapes to prose or whichever crate is selected makes
compatibility and audit identity drift with implementation." (The roadmap's
duplicate-key and number fixtures at roadmap.md:166 will eventually force a
conforming parser — the finding is that the mechanism decision is deferred to
a fixture failure instead of being made.)
*Fix:* complete M-13's table as a TASK-001 deliverable covering the safe-Rust
mechanisms — naming the ingest strategy for duplicate-rejecting,
lexeme-preserving JSON explicitly — correct ADR-007/roadmap.md:161 to
"ingest and canonicalization," and add the no-new-dependency-without-an-ADR
rule to ADR-012's decision.

## 4. Medium and Low findings

Mediums. Every row was adversarially verified; where a verifier narrowed a
claim, the narrowed form appears here.

| ID | Finding | Fix direction |
| --- | --- | --- |
| M-1 | `codex-0.147.0-required-subset.json:143` records `rejected_fork_boundary_statuses: ["crash_interrupted"]` — a status string no probe produced (every recorded measurement says `interrupted`: crash probe :318, task-000.md:207/227/228) and the sole snake-case occurrence repo-wide; neither `interrupted` nor `failed`, the statuses SPEC-008:845 names in prose, appears in either manifest status list. Decorative today (the fork rule is allow-list-driven) but it is the empirical proof of H-7's drift. | Set it to `["interrupted","failed"]` or delete the key; record the observed wire string in task-000.md [patch-surface] |
| M-2 | SPEC-006's frame classifier (specs.md:401-403) is stated in two-way terms ("a top-level `method` proves an unsolicited notification") over a three-way wire in which both supported server requests carry `id` **and** `method`; no precedence rule orders the id-match predicate against the method predicate, and nothing requires the two JSON-RPC id namespaces to be disjoint. The one probe that measured the server-request `id` type emits it (`task000a_approval_probe.py:67`) but task-000.md and the manifest drop it. | State method-evaluated-first; a frame with both `id` and `method` is a server request, never the solicited response; record `server_request_id_type` in evidence + manifest [patch-surface] |
| M-3 | The method-not-found behavioral claims — "lets Codex determine the turn result" (specs.md:892-894, recognized-unsupported) and "do not stop the generation" (specs.md:1119-1120, unknown requests) — have zero live evidence: `-32601` has 0 hits repo-wide, the probe harness can only send `result` replies (`_probe_support.py:293`), and roadmap TASK-008 verification is fake-only, while SPEC-012:1113-1115 makes "required server-request probes" a version-acceptance gate. A refused blocking `requestUserInput` leaves the run `running` with an empty pending list, no timeout, and no documented escape but `run interrupt`. (This is round-4 C-2's surviving residue.) | Downgrade to a stated assumption or measure (P-10); one SPEC-009 sentence naming the operator-visible consequence and the escape [patch-surface] |
| M-4 | "Zero or multiple defaults is `COMPATIBILITY_REJECTED`" (specs.md:875-876) is grammatically unscoped, and the reference probe applies it globally (`default_count == 1` in the PASS tuple, `task000a_contract_probe.py:147`) while the emitter list (specs.md:463) includes `recover`/`reconcile`/`fork`/`resume`, whose runs have pinned models that never consult the default — a benign upstream deprecation window with two defaults bricks recovery of runs that structurally cannot use one. | Scope the sentence to omitted `--model`; make the probe record an observation, not a PASS condition [patch-surface] |
| M-5 | The stored default effort is never revalidated at the one moment it can go stale: later generations append a new capability snapshot (specs.md:876-877) and `recover --accept-version-change` makes drift real, but the enumerated checkpoints are only `set-effort`-while-no-generation-live and turn-time `--effort` — and the probe proves app-server silently accepts unadvertised efforts, so gomchi's check is the only guard and it is not applied there. Secondarily, `task000_effort_probe.py:107` puts `not rejected` in the PASS tuple, so the suite would fail a future Codex that *tightened* validation. | One generation-start revalidation sentence in SPEC-008; probe assertion becomes an observation [patch-surface] |
| M-6 | `target doctor` has two both-legal result encodings — `ok:false` + `COMPATIBILITY_REJECTED` (discarding the diagnostics array that is doctor's entire product) vs `ok:true` + `compatibility:"rejected"` + fail diagnostics — with no selection rule anywhere (`doctor` has 5 SOT hits, none descriptive); machines branch on `ok` first, so two conforming implementations invert each other. `target add/list/show` share the `target` shape whose validation-derived fields have no defined source for commands that never execute the target (every field is nullable/emptyable, so this is a determinism gap, not an impossibility). | Doctor always `ok:true` with the verdict in `compatibility`, `COMPATIBILITY_REJECTED` reserved for a check that could not run; non-executing commands emit `unknown`/null/empty [pre-existing] |
| M-7 | No entry in the timeout table (specs.md:507-514) has a defined expiry error code, and one case has no reachable code at all: `close --interrupt` on a non-terminating turn cannot emit `OUTCOME_UNKNOWN` (`close` is absent from the emitter list at specs.md:460 while `pause` is present), cannot emit `RUN_STATE_CONFLICT` (`--interrupt` legalizes close-from-running), and `RECOVERY_REQUIRED`'s charter is unproved identity. Expiry of the 30-second `initialize`/`model/list` waits is ambiguous between `COMPATIBILITY_REJECTED` (exit 5, `retryable:false`) and `TRANSPORT_FAILURE` (exit 6, `retryable:true`) — maximally divergent master instructions. Group absence (specs.md:657-658 → `RECOVERY_REQUIRED`) proves the pattern was expressible. | Per-budget expiry codes; validation-wait expiry = `TRANSPORT_FAILURE`/`not_accepted`; add `close` to the `OUTCOME_UNKNOWN` emitters (pairs with H-12) [patch-surface] |
| M-8 | `d_idempotency` (gomchi-error-contract-v1.json:156) is the only comparison error omitting both compared values — a single `key_digest` whose referent is undefined (specs.md:530 fsyncs *two* digests: the opaque key and the normalized-bytes SHA-256) while `d_target_comparison`/`d_compatibility`/`d_stale_request`/`d_path_collision` all require expected **and** actual. Secondarily, "ordered `(detail,lossless-canonical-path)` image tuples" (specs.md:527-528) is bistable between as-supplied and sorted order, though house style (explicit "sorted" at :549/:571) favors as-supplied. | Require `recorded_input_digest` + `observed_input_digest`; state the image-array ordering [pre-existing] |
| M-9 | Machine-output v1 is closed in both directions (specs.md:342-345) with zero mechanics for introducing a successor: no v2 introduction procedure, no dual-emission rule, no consumer unsupported-version rule, no `--schema-version` (`evolution`/`deprecat`/`negotiat`/`migrat` → 0 hits across the SOT and both protocol JSONs). The archival case is decisive — `run export` bundles carry `bundle_schema_version: const 1`, are permanent (specs.md:1046) and content-deterministic, with no rule for a later gomchi reading an earlier bundle — and **this round's own fixes (H-14, H-15, H-18) are v1 additions the missing procedure blocks**; the canonicalizer has an evolution handle (`sha256-jcs-vN`), machine-output and bundle have none. The accepted iteration cost is recorded nowhere (deferred ledger reads "None"). | One SPEC-006 paragraph: new `$id` per version, consumers read `schema_version` first, define the unsupported-version condition, bundles of any prior version remain readable; add the DF-NNN entry. Sequence this fix **before** H-14/H-15/H-18 [planning] |
| M-10 | The two 4096-byte startup-lock owner records have no stated file offsets (`4096` occurs exactly twice, `offset` zero times), yet ADR-011's cross-version tolerance makes the lock body a frozen on-disk format a *different binary* must parse, and specs.md:680's "unknown-layout slots" presupposes a layout never defined. (Byte-range/record overlap was analyzed and is harmless once offsets are stated.) | Pin `[0,4096)` / `[4096,8192)`, create the file as 8192 zero bytes, short file = `Unverifiable`, add a golden vector [pre-existing] |
| M-11 | No minimum macOS version or SDK floor exists anywhere normative (`minimum`/`deployment`/`macOS <version>` → 0 hits) while the SOT normatively names eight version-sensitive Darwin surfaces (kqueue ×13, `proc_pidpath` ×5, `proc_listpgrppids` ×4, `MNT_LOCAL` ×4, `fstatfs` ×3, `EVFILT_PROC` ×2, `START_SUSPENDED` ×2, boot-UUID sysctl) plus non-POSIX `F_SETLKWTIMEOUT` (H-4); the only version records live in probe evidence that self-disclaims authority (task-000.md:15-17), and no task promotes a floor. Probe conclusions therefore have an unstated validity range that cannot be re-audited after an OS upgrade. | One sentence in SPEC-001 (e.g. "macOS 26.x or later; earlier releases unverified") + a validity-range line in task-000.md + name it in TASK-015 [planning] |
| M-12 | The replay `ready` budget "30 s + 1 ms per ledger record capped at 5 min" (specs.md:508-509) names no source for N — readable only from `state.json`, whose invalidity is the very case that triggers full replay — and no outcome for exhaustion (no timeout code exists in the 31-code contract; architecture.md:83-85 maps only EOF); and the SOT does not settle whether every worker start replays fully (specs.md:686, architecture.md:554) or only an invalid-projection start (architecture.md:386-387). (The candidate "self-defeating at 270k records" claim was refuted: at realistic per-record cost the cap binds around ~6M records.) | State scope + input source; map exhaustion to a code; measure the constant (P-11) [pre-existing] |
| M-13 | `run export` has no snapshot boundary: it copies `audit.jsonl` "verbatim" (specs.md:1033) with no end bound at the `state.json.ledger_head` watermark (specs.md:976-978) and copies `state.json` at a different, unordered instant — so a bundle taken during a live turn can be internally inconsistent and can carry a torn tail no observer may repair — and export is omitted from the observer read-discipline enumeration all three times (specs.md:461, :866-869, architecture.md:588-591) despite being named an observer at specs.md:482. | Bound the copy at the copied `state.json`'s `ledger_head`; record the bounding sequence in `bundle.json`; add export to the observer enumerations [pre-existing] |
| M-14 | The audit ledger has no filesystem precondition although the less critical, *recoverable* lock root requires `MNT_LOCAL` (all four `MNT_LOCAL` hits are lock-root-scoped) and non-git init accepts arbitrary directories: on SMB/NFS/exFAT the `O_APPEND` atomicity, fsync semantics, and clean-prefix crash-truncation shape lose their basis — any other tail shape is unconditional `AUDIT_INTEGRITY_FAILURE` with confirmed delete the only exit — and because the lease lives on a *local* root keyed by the path digest, two hosts sharing one network workspace each hold their own lock. | `init` requires `MNT_LOCAL` for the workspace (explicit override flag mirroring `--state-root`), record `st_dev`; TASK-000-B probe P-12 [pre-existing] |
| M-15 | The 100 ms projection publication sits inside "Normative internal timeouts are:" (specs.md:507-513), where every other entry has a hard expiry consequence, but its own miss has none — implementers may fail runs on slow disks or never measure it — and `events --follow` is a projection reader with no stated wake-up mechanism (`poll`/`notify`/`wake` → 0 hits), leaving end-to-end observer latency formally unbounded. (The fsync-treadmill cost sub-claim was refuted: publication is event-driven.) | Move to SPEC-010 as a target ("diagnostic, never an error"); specify the consumer wake-up rule [pre-existing] |
| M-16 | The redaction digit rule "digits attach to the token on their left" (specs.md:984-987) defeats key matching for digit-suffixed secrets — independently reimplemented twice from the spec text: `password2`, `apiKey2`, `oauth2_token`, `accessToken2`, `clientSecret2` all leak while `password_hash` redacts — and the rule is **ambiguous at separator boundaries**: `password_2`/`api_key_2` flip between REDACT and LEAK depending on whether the digit attaches across `_`, and because redaction precedes JCS hashing (specs.md:955), the choice changes the `sha256-jcs-v1` chain invisibly to verification. specs.md:1006 documents over-redaction only; 6 of the 27 canonical sequences are dead (subsumed by shorter listed sequences). roadmap.md:167 has empty-token and plural vectors but no digit vector. | Disambiguate the separator-digit case; additionally match with trailing digit runs stripped per token; state the residual; add digit vectors [pre-existing] |
| M-17 | Promote/demote is unimplementable under the definitions as written: "clean generation shutdown" then "Promotion acquires the writer lease before replacement activation" (specs.md:751-755), where a generation is worker **plus** app-server (specs.md:24), only a worker may hold the lease (specs.md:588-589), and byte 1 is held for the worker's entire serving lifetime (specs.md:675) — every assignment of the acquirer breaks a rule. ADR-009:362 states the evident intent ("Access changes replace the **app-server** generation") but "app-server generation" is not a defined term. | State in-place semantics (same worker, same byte 1, new app-server child, incremented generation) and amend the specs.md:24 definition to match ADR-009:362 [pre-existing] |
| M-18 | SPEC-011's `.gomchi` reservation (specs.md:1062) is neither enforceable — `writableRoots` includes the workspace and the pinned `SandboxPolicy` has no deny-list field (manifest `workspace_write_fields`) — nor observable: specs.md:551 excludes `.gomchi/` wholesale from observed paths *including the tracked `config.toml`*, so an agent flipping `default_access` silently converts every future `run start` into a writer (specs.md:457, :563) and the turn report says nothing. SPEC-011 never says its rules are advisory; that statement lives only in ADR-009:364. | State prompt-enforced-only in SPEC-011; narrow the exclusion to `runs/`/`runtime/`/`cache/` so `config.toml` changes are reported; optionally digest `config.toml` in the manifest [pre-existing] |
| M-19 | No repeatable check entry point exists for a 15-task serial gate demanding "designated deterministic verification passes" and clippy "with warnings denied" — no `.github`, no runner, no script (`CI`/`nextest`/`test-runner` → 0 relevant hits); the per-probe command blocks in task-000.md are a real manual runbook, so the gap is a single entry point and a recorded commit-or-decline decision, not repeatability itself. | One local script/`just` recipe covering schema validation + probe suite + fmt/clippy; a one-paragraph CI decision in the roadmap [planning] |
| M-20 | The two throughput-shaped constants — 1 ms/record replay allowance and 100 ms projection publication — are verified only against the injectable clock through TASK-015 (`performance`/`latency`/`benchmark` → 0 hits repo-wide), and the allowance is below the real cost of its own worst case: one ledger record may carry a 2 MiB payload (specs.md:387) whose parse+JCS+SHA-256 replay cost is tens of milliseconds, so a payload-heavy ledger can exhaust the `ready` budget with no distinguishing diagnostic. | One real-hardware measurement line in TASK-014/015 for replay rate and publication latency; re-derive the constants from it [planning] |
| M-21 | architecture.md:386-387 still reads "the worker replays `audit.jsonl` **before accepting commands**" — the exact sentence round-4 M-10 demanded be replaced — while specs.md:713-714 now says the control channel is serviced from `bound` *during* replay: a live SOT-vs-SOT contradiction of the kind specs.md:8-9 declares invalid. The progress-observed takeover-abort branch (specs.md:728-730) still maps to no outcome code. | Replace with "before accepting ordinary mutations"; map the progress-observed abort to `RUN_BUSY` [patch-surface] |
| M-22 | The default lock root resolves to `…/gomchi/locks/` (specs.md:579) while the lock files are named `locks/writer/<digest>` and `locks/startup/<digest>` "below the workspace-recorded lock root" (specs.md:671, architecture.md:205) — composing to a doubled `…/locks/locks/writer/…` under the default and a single `…/locks/…` under `--state-root`, mutually exclusive readings of the same rule. This is the surviving half of round-4 C-1, whose fix demanded "state the exact root-relative lock paths once." | State once whether the recorded root already terminates in `locks/`; align specs.md:579/:671 and architecture.md:205 [pre-existing] |
| M-23 | The async-signal-safety constraint on the fork→re-exec path exists only as a disposition-row assertion (round-4-disposition.md:39 — the sole repo-wide occurrence of "async-signal"): architecture.md:607-611 mandates a multi-threaded worker and architecture.md:68-70 mandates the single fork + `setsid()` + re-exec, and no SOT sentence states the discipline that reconciles them, so a future implementer adding a pre-fork thread (logging, async runtime) violates an unwritten rule. This is round-4 M-11, still Absent. | Add the one demanded sentence to architecture.md's Per-Run Worker section: "The CLI creates no thread before fork; between fork and re-exec the child performs only async-signal-safe operations." [pre-existing] |
| M-24 | The `events` stream's `record` member is a bare unconstrained `{"type":"object"}` in the checked schema, "normalized record" is never defined, and the closed record-kind enum is deferred to TASK-003-C (roadmap.md:186) — colliding with specs.md:342-345's "consumers MUST reject unknown fields," which is unenforceable for `data.record`. This is round-4 H-23's surviving residue (the `--follow` transport half landed; the payload half did not). | Define the record envelope (kind enum + per-kind required members) in the machine schema or a referenced ledger-record schema when TASK-003-C lands; until then, scope the closed-schema rule to exclude `record` explicitly [patch-surface] |

Lows.

| ID | Finding |
| --- | --- |
| L-1 | "Reader auto-decline" is a roadmap deliverable twice (roadmap.md:294, :302) but no SOT text assigns the mechanism to gomchi code — specs.md:770's rule is realized by `approvalPolicy:"never"` (evidence: the never-policy probe turn emitted no approval request), so the deliverable is either vacuous or an undocumented second mechanism; no probe ever ran an approval scenario under `never`. One SPEC-009 sentence resolves it. |
| L-2 | A `send`/`wait` whose turn ends `failed`/`interrupted` returns exit 7 with a closed `failure` envelope carrying only `d_turn_status` — the master loses response, usage, cursor, and `workspace_changes` on exactly the outcomes it most needs them for; recoverable via `run status.last_terminal`, which one sentence should direct masters to. |
| L-3 | `targets.toml` — contents executed as argv — has no specified file/directory mode while every comparable artifact does; measured umask 022 yields 0644 (group-readable disclosure of argv and `CODEX_HOME` paths; not writable, so no injection surface). Specify 0700/0600 in SPEC-003. |
| L-4 | Idempotency normalization hashes image `(detail, path)` but not image bytes (specs.md:527-529): replacing the file at a recorded path and reusing the key returns the original turn instead of `IDEMPOTENCY_CONFLICT`; specs.md:274-276 acknowledges the hazard only for replay. Hash the bytes or state the exclusion. |
| L-5 | architecture.md:377 asserts subagent-activity opacity unconditionally while specs.md:1019 hedges "when observable" — and the probe record (task-000.md:179) falsifies the unhedged form. Add the hedge (round-4 H-9's surviving half). |
| L-6 | round-3-disposition.md:29's H-7 row was never amended to record the same-run→workspace-wide substitution, as round-4 H-11's fix demanded. One disclosure sentence. |
| L-7 | No stated policy for a newer binary opening an older `.gomchi` tree: formats are versioned and the manifest records the hash scheme (the hook exists, architecture.md:327), but nothing obliges honoring an older `sha256-jcs-vN` and no version-mismatch behavior is defined for `manifest.json`/`state.json`/`audit.jsonl` — while specs.md:1046 guarantees indefinite persistence. One policy sentence, even "fails closed; stay on the matching binary." |
| L-8 | `implementation-notes.md` and `docs/reviews/README.md` have zero inbound links — the entire review record is unreachable from any linked document — and gate item 6 hyperlinks `deferred-feedback.md` while parallel item 7 links nothing. Two links. |
| L-9 | deferred-feedback.md:29 names only Round-2 and Round-3 as closing clean; Round 4 closed clean on 2026-08-16 and is missing. One sentence. |
| L-10 | Mixed RFC-2119 register: lowercase `must` states binding rules (specs.md:656, 809) and lowercase `may` grants authorizations (specs.md:414, 733, 742, 807, 1083, 1107); the clearest exhibit is specs.md:758, which uses `MAY` and `may` in one sentence; roadmap.md:17 uses `MUST NOT` with no declared convention. (specs.md:11 does not say *only* uppercase is normative, so this is register consistency, not a normativity gap.) Capitalize ~8 instances or add a one-line note. |
| L-11 | The four SOT documents have essentially no ID-level cross-reference fabric — 0 `ADR-` refs in specs.md/architecture.md, 0 `SPEC-` refs in the ADRs, architecture.md cites 2 of 12 SPEC IDs — which is how M-21's live contradiction and L-13's transport residue survived a clean closure; the duplicated 4096-byte owner-record definition even names its fourth field differently ("full Gomchi process identity" vs "Gomchi process tuple"). Standardize and cite the owning ID at each duplication point. |
| L-12 | "operator" is used three times (specs.md:810, architecture-decisions.md:314, todo.md:71) for the actor Definitions names **Master**, and is never defined. Replace or define. |
| L-13 | architecture.md:44 still says the CLI "exchanges one request/response or one event stream **with the worker**," contradicting specs.md:866-869 and architecture.md:588-591, which make `events` projection-only (round-4 M-9's surviving residue). Qualify step 5. |

## 5. Probe suite assessment

**The harness held.** `_probe_support.py` was audited as an artifact and
enforces what the anti-gaming discipline claims: bounded frame reads with
explicit over-limit rejection, duplicate-member rejection, the pinned-version
gate, and a real process-group teardown with membership proof. The C probe
was recompiled and re-run this round: 13/13 PASS, zero warnings, no
divergence from the recorded evidence. The mechanical protocol artifacts
cross-validate cleanly (31 codes byte-identical across three files; 28/28
command bindings; UUIDv7 regex correct to the variant bits).

**The suite's blind spots are the round's findings.** The C probe never calls
`proc_pidpath` (C-1) and never exercises `F_SETLKWTIMEOUT` despite round 4's
explicit instruction (H-4). Every Python probe asserts against local
constants rather than reading the manifest it certifies (H-7), and two probes
encode pinned-target *leniency* as PASS conditions (`not rejected` for
unadvertised efforts; `default_count == 1` globally), so the suite would fail
a future Codex that tightened validation (M-4, M-5). There is still no
run-all entry point (M-19).

**Proposed TASK-000-B probe campaign.** No live Codex turns were executed by
this review, per standing conduct; the probes below are for the owner to run
under a TASK-000-B stabilization task. P-1..P-4, P-8, P-11, P-12 need no
Codex account; P-5, P-6, P-7, P-9, P-10 are live-Codex probes against the
pinned 0.147.0.

1. **P-1** — `START_SUSPENDED` + `proc_pidpath` before/after `SIGCONT` for a
   direct binary, a `#!/bin/sh` wrapper, and a `#!/usr/bin/env` chain; extend
   `task000_os_semantics.c`. Settles C-1. Blocking for TASK-009-B.
2. **P-2** — `realpath(3)` firmlink non-collapse + equal `(dev,ino)` +
   digest divergence. Settles H-1. Blocking for TASK-002.
3. **P-3** — AF_UNIX socket is `-type s` / sidecar is `-type f` under the
   `tmp_cleaner` predicate; socket-root recreation race. Settles H-2.
   Blocking for TASK-004.
4. **P-4** — `F_SETLKWTIMEOUT` bounded-wait case with the `flocktimeout`
   layout, in the C probe. Settles H-4. Blocking for TASK-007.
5. **P-5** — one completed, one interrupted, one approval-declined, one
   SIGKILL-crashed turn; record the exact terminal notification method and
   every distinct status string from `thread/read(includeTurns:true)`; pin
   them in the manifest. Settles H-5/M-1. Blocking for TASK-006.
6. **P-6** — `thread/read` on a real turnless thread after app-server
   restart; `thread/read` negative control proving a distinct failure returns
   a distinct code/message. Settles H-6. Blocking for TASK-006.
7. **P-7** — `thread/fork` with `lastTurnId` omitted on a thread whose newest
   turn is interrupted; record accept/reject. Settles H-9. Blocking for
   TASK-009-C.
8. **P-8** — takeover-predicate discrimination: healthy worker mid-quiet-turn
   vs `SIGSTOP`-ped worker under the four signals; AF_UNIX `connect`
   behavior against a live-but-backlogged listener. Settles H-10. Blocking
   for TASK-009-B.
9. **P-9** — `turn/interrupt` with a command approval outstanding, unanswered
   and after `cancel`; record terminal status and latency. Settles H-12/M-7.
   Blocking for TASK-008 and TASK-009-A.
10. **P-10** — reply `{"error":{"code":-32601}}` to a recognized-unsupported
    server request on a live turn; record whether the generation survives and
    the turn's terminal status. Settles M-3. Blocking for TASK-008.
11. **P-11** — measured replay cost per record on synthetic ledgers at
    10⁴/10⁵/10⁶ records including 2 MiB-payload records; re-derive the
    `ready` constant. Settles M-12/M-20. Blocking for TASK-003-B.
12. **P-12** — one append/fsync/SIGKILL/torn-tail cycle on an SMB mount and
    an exFAT volume; record whether the post-crash tail is a clean prefix.
    Settles M-14. Blocking for TASK-002/TASK-003-B.

Still never exercised live anywhere: the method-not-found error reply (P-10),
the turnless-provisional `thread/read` (P-6), and any approval scenario under
`approvalPolicy:"never"` (L-1).

## 6. Process and gate integrity

**The closure-method finding, fourth iteration.** Round 2's closure reported
no findings; round 3 found 2 Critical. Round 3's closure reported no
findings; round 4 found 2 Critical. Round 4's closure reported "No blocking
or non-blocking findings" after 48+ adversarial scenarios at `ca91f7d` — and
§1 of this round finds 12 Partial and 1 Absent dispositions in that same
state, plus one Critical and 22 Highs, of which at least four are one-command
mechanically checkable facts (the false manifest claim at
architecture.md:668-671; the live replay contradiction, M-21; the duplicated
fixture ownership, H-21; the unconstrained `record` member, M-24). The
pattern is stable across four rounds: closures that validate *documents*
report clean; reviews that adversarially verify *mechanisms, artifacts, and
seams* find Criticals. What found this round's defects was the same method
round 3 mandated and the gate now requires — refutation-first verification
with recorded counter-searches, empirical OS measurement, plus two additions:
end-to-end walkthroughs across spec seams, and treating checked artifacts
(schemas, manifest, probes) as review surfaces. The same pass cleared 27
candidate findings — the method cuts both ways.

**Grouped disposition rows concealed every Partial.** All 12 Partials and the
1 Absent hide inside multi-finding rows of round-4-disposition.md (e.g. the
M-11..M-14 row covers one Absent, one Partial, and two Verified with one
sentence). Round 5's disposition should carry **one row per finding**, each
citing the implementing file:line — the audit protocol in §1 is reusable as
its checklist.

**The disposition-only-assertion class.** M-11's fix exists as a sentence in
the disposition file and nowhere in the SOT (M-23). A claim that is true only
in `docs/reviews/` is not landed: implementers are bound to the SOT alone.
Gate condition 5 should say so explicitly — "fixed" means fixed in SOT text
or a checked artifact, never in disposition prose.

**Reviewer independence.** The round-2/3/4 closures were all authored by the
same in-project reviewer lane that returned three consecutive no-findings
results now contradicted three times. This round used a differently
structured review (nine independent lanes, three adversarial verification
clusters, separate calibration); its closure — when the owner's Round-5 patch
lands — should be performed with the §6 method and by a lane that did not
author the disposition, and its report should cite the mechanical checks it
ran, not only the documents it read.

**The stabilization-task rule needs one clarification.** roadmap.md:22-26
permits `TASK-NNN-A` tasks to create "probe fixtures" but "never production
code." H-21 shows the shared fake app-server sits ambiguously between those
categories; state explicitly whether test fixtures and the fake are lawful
stabilization-task deliverables (they should be).

**Round bookkeeping.** At round start the round-4 input was relocated to
docs/reviews/round-4-input.md (SHA-256 `c97cf459…` verified identical) and
two inbound references were retargeted (docs/reviews/README.md,
round-4-disposition.md:7). These edits and this review are uncommitted and
should be committed together as this round's artifacts. This review's
findings land via a `TASK-000-B` stabilization task per roadmap.md:22-26.

## 7. What held up — do not churn

Carried forward from rounds 2–4, re-verified present at HEAD this round:
forced-recovery removal with TODO-005 parked (no `--force` anywhere in the
grammar); the transient read-only reconcile generation; the four-verdict
identity model with boot-session UUID; the oversized-stdout quarantine
posture (round-3 M-11 remains rejected-by-design; do not re-raise tolerance
counters); the `$gomchi_*` marker and escaping design (injectivity
re-verified); the error-table format; the CLI grammar; the XDG state root and
`MNT_LOCAL` lock-root choice; the fd-3 `bound`/`ready` handshake; the SIGTERM
evidence-first shutdown design; the probe anti-gaming discipline; the
`docs/reviews/` immutability convention; the two-byte startup-lock protocol
exactly as designed; the takeover guard *composition* (bounded hello +
abort conditions + byte-0 election — H-10 asks for a readable beacon, not a
redesign); `F_SETLKWTIMEOUT` as the lock-wait primitive (H-4 asks for its
statement, never for polling machinery); and the repair/authority derivations
(torn-tail sequence, idempotency-key join, `state.json.ledger_head`
watermark).

New this round — candidate findings refuted by verification and recorded here
as verified-sound design (27 total; the load-bearing ones):

- **`fsync(2)`-not-`F_FULLFSYNC` is a deliberate, correctly-disclaimed
  boundary** (architecture.md:349 names the syscall; :355-356 draws exactly
  its boundary). The candidate "F_FULLFSYNC unnamed" finding died here — do
  not re-raise.
- **The 16 MiB early-ID classification** survives extrapolation attack: the
  response `id` lives in the envelope, its offset is payload-independent
  within a version, and step 3 re-probes it per version.
- **`sun_path` overflow is arithmetically impossible** for the documented
  naming rule (measured 65 bytes worst case vs 104).
- **`realpath(3)` case/NFD canonicalization on APFS is correct as claimed**
  (measured; the firmlink case, H-1, is the one genuine exception).
- **SPEC-002's digest domain separation is sound** (three distinct
  NUL-terminated prefixes over fixed-length operands).
- **The `proc_listpgrppids` entry-capacity fix is implementable and probed.**
- **The probe harness itself is sound** (bounded reads, duplicate rejection,
  pinned-version gate, group teardown with proof).
- **The mechanical error-contract core is clean**: 31 codes byte-identical in
  three artifacts, total exit map, exclusive `oneOf`, 28/28 bindings.
- **The torn-tail repair and observer-watermark semantics survived
  re-derivation** (round-4 M-6/M-7/M-8/M-9 confirmed sound; export's missing
  bound, M-13, is the one residue).
- **architecture.md's preamble correctly disclaims present-tense readings**
  ("Cargo.lock is committed" is a target-state statement, not a falsehood).
- **The security-posture section functions as the intended informal threat
  model** at this product scale (the candidate "no threat model" finding was
  refuted; the residues are the narrow L-3 and M-18).
- **TASK-014's 100/100 zero-tolerance stress rule is a stated flake policy**
  (candidate refuted).
- **The closed-schema philosophy itself is defensible** — M-9 asks for an
  evolution *procedure*, not an open schema.

Advisory (non-normative): the composed worst-case `run recover` wall time is
roughly 250–520 s from the individually-normative budgets; nothing requires
changing it, but stating the composed bound near the timeout table would
spare the first operator surprise.

## 8. Recommended order of work

1. **Reconcile C-1** — the spawn-image rename, the entry→final continuity
   rule, and the specs.md:617-619 vs architecture.md:485 reconciliation — and
   run P-1. This unblocks the identity layer everything else rests on.
   (Half a day of text; hours of probe.)
2. **Land M-9 (schema-evolution procedure) first, then bundle the v1 schema
   amendments** — H-14 (`d_outcome_unknown`), H-15 (`unknown` command,
   exit-status sentence), H-18 (`measured` scoping), M-24 (record-envelope
   scoping) — as one deliberate version step. (One day.)
3. **The one-sentence High completions**: H-4 (`F_SETLKWTIMEOUT` + struct
   order), H-9 (generalize the fork scan + error row), H-11 (flag on
   `resume`/`reconcile`), H-12 + M-7 (interrupt-expiry → `outcome_unknown`,
   emitter row, cancel ordering), H-13 (wire control-v1 `shutdown` to
   pause/close/recover on skew), H-10 (the durable takeover progress beacon
   in the runtime record + CPU attribution), H-16 (non-follow events
   contract), H-17 (respond vocabulary + translation order), H-19 (effort
   default + per-turn scope), H-6 (message discriminator sentence). (Two to
   three days, mostly parallel prose.)
4. **Manifest repairs**: H-8 (resolved-definition constraints, value-set enum
   rule, stated resolution semantics), H-7 (step-3 re-measurement rule +
   probes read the manifest), H-5 (notifications object + status mapping),
   M-1 (the value fix). (One day, plus probe reruns.)
5. **OS-substrate statements**: H-1, H-2, H-3, M-10 (offsets), M-11 (floor),
   M-22 (lock-path composition), M-23 (async-signal sentence), M-21 (replay
   contradiction + `RUN_BUSY` mapping). (One day.)
6. **Planning layer**: H-20 (traceability annotations + checked reverse
   index — the largest single item, one to two days), H-21 (fixture
   ownership + architecture paragraph + stabilization-rule clarification),
   H-22 (binding table + dependency rule), M-19 (run-all entry point +
   CI decision), M-20 (measurement line). (Two to three days.)
7. **Medium/Low sweep**: the remaining M rows (M-2..M-6, M-8, M-12..M-18)
   and L-1..L-13. (One to two days.)
8. **Run the TASK-000-B probe campaign** (§5): the seven no-Codex probes are
   hours; the five live probes are a day against the pinned target. Fold the
   results into the manifest and evidence doc.
9. **Independent re-review with the §6 method** — adversarial, empirical,
   walkthrough-inclusive, by a lane that did not author the disposition —
   then TASK-000-B's gates, then TASK-001.

Total: roughly 1.5–2 weeks of documentation and probe work, no production
code. Grade trajectory: completing steps 1–5 returns feasibility to **B**
(the Critical and the wrong-behavior Highs are gone); steps 6–8 — the
evidence chain and the planning substrate — carry it to **B+**. A− requires
the mechanized traceability index and one closure that finds its own findings
before the next round does.
