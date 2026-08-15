# Gomchi Pre-Implementation Technical Review — Round 3

Date: 2026-08-15
Snapshot note: this is the immutable round-3 input review. Owner dispositions
belong in `docs/reviews/` (per the convention the team established), so later
fixes do not rewrite the review that motivated them.
Scope: Re-review of the round-3 SOT set (`docs/specs.md` 696 lines,
`docs/architecture.md` 536, `docs/architecture-decisions.md` 370,
`docs/roadmap.md` 343, `docs/todo.md` 71), the expanded TASK-000 probe suite
(`tools/probes/`, 7 files incl. `_probe_support.py`, frame probe, and the
workspace-write crash/fork variant), the probe evidence
(`docs/probes/task-000.md`), and the round-2 disposition set
(`docs/reviews/round-2-{disposition,follow-up,closure}.md`). Weighted on
functionality and implementability, per request.
Method: three independent verification passes — (1) disposition-truth check of
every "Implemented" claim, (2) design review of the mechanisms this round
introduced or reshaped, (3) static probe-suite re-audit — cross-checked
against the document originals by the orchestrating reviewer. OS-level claims
in this round were verified **empirically**: purpose-built C probes were run
on this machine (Darwin 25.5, arm64; macOS 26 SDK headers) to measure fcntl/
flock semantics, `proc_pidinfo`/`proc_listpgrppids`/`kqueue` behavior on live,
dead, and zombie processes, and PID-space recycling. Findings that did not
survive cross-checking were dropped.

## Verdict

**The round-2 absorption is again excellent** — the disposition table verifies
almost entirely (Section 1), the forced-recovery removal is clean and
correctly parked (TODO-005), the probe suite is substantively hardened, and
P3/P4 plus the two new charter probes (frame measurement, workspace-write
crash + live `thread/fork(lastTurnId)`) now pass predicates that genuinely
gate what the evidence claims.

**But this round's fixes reached OS-semantics territory, and there the new
machinery specifies mechanisms that do not exist or do not compose.** Measured
on this machine: POSIX record locks are **not inherited across `fork(2)`** —
so the new startup-lock design has no lawful way to hand the lock from the
CLI to the worker it forks (C-2); unlink-and-recreate yields **two live
holders of the "exclusive" writer lease** because the round-2 dev/inode
re-verification was dropped in the move off `/tmp` (H-3); the group-absence
proof's operands (member predicate, zombie branch, enumeration recipe,
boot-UUID scope) are undefined on a platform whose PID space **wrapped inside
a 60-second sample** (H-1 cluster); and at the state-machine level the sole
documented escape, `fork --fresh`, is **unreachable in the canonical crash it
was created for** (C-1).

Feasibility **holds at B** (it does not yet reach the B+ that round 2
projected). Nothing requires re-architecting — every fix below is a bounded
edit, and several (the boot-UUID group rule, the two-byte lock protocol, the
inode checks, marker escaping) are single paragraphs — but the
concurrency/recovery layer needs one more focused patch pass before TASK-001,
and the independent-review step needs a stronger method: its "no findings"
closure has now been contradicted twice in a row (Section 6).

Severity: **Critical** = cannot be implemented as written or defeats a core
promise; **High** = wrong-behavior path or blocks deterministic
implementation; **Medium** = rework/operational pain; **Low** = polish.

---

## 1. Round-2 disposition verification

| Verdict | Items |
| --- | --- |
| Fully implemented, verified against text/code | C-1, C-2 (see B-4 residual), H-E1/E2, H-L1–L4, H-R1/R2 (one residual), H-K1–K5, H-W1–W4, M-W1/W2/W3/W4, M-E1–E5, M-L1–L3, M-P1/P2, M-R1–R3, L-1 through L-12, probe-gate items (frame + workspace-write charters, version binding, `experimental_flag_passed` rename, deletion proof via `thread/list`) |
| Implemented by simplification, verified | H-Z1–Z3, M-Z1–Z3 (forced recovery + hazard markers removed; TODO-005 parks the mechanism with sound promotion criteria) |
| **Partial or regressed** | **H-F1(c)** — the proportionality recommendation was consciously not adopted (oversized/invalid stdout still quarantines the accepted turn); defensible as a stricter no-fabrication posture, now evidence-bounded (measured max real frame 1,049,281 B vs 16 MiB cap), but no ADR records the rationale, and a new circularity appears at H-14. **H-W5** — the reader posture it asked for was never stated; the simplification removed the reader/writer distinction without a reader rule (B-6). **H-W6** — the relocation off `/tmp` landed but the dev/inode re-verification was dropped entirely (H-3): grep for `fstat`/`st_dev`/`st_ino`/`inode` across the SOT returns nothing. **H-W1 residual** — `Unverifiable` causes are not exhaustively enumerated. **H-R2 residual** — `sessionKey` (named in the original list) still matches nothing; `session key` is absent from the vocabulary while `key` sits on the exclusion list. |
| Probe dispositions overclaimed | "records only bounded message shapes on failure" and "correlates nested terminal turn IDs" are claimed suite-wide but hold for only 3 of 5 live probes (Section 5) |

---

## 2. Critical findings (2)

### C-1. The sole documented escape is unreachable in the exact failure it was created for  [Critical]

**Where:** `specs.md` SPEC-005 (recover ¶), SPEC-008 (forking ¶ + recovery ¶);
`architecture.md` "Recovery and Reconciliation", "CLI Front End", "State
Machine".

**Problem.** Chain, each link quoted from the SOT: (1) a writer crashes
mid-turn and its process group is ambiguous → `Unverifiable`; (2) the run's
projected state is still `running` — the `running|waiting_* → outcome_unknown`
transition is made during recovery by a **new worker**, and "an unverifiable
prior worker, app-server, or process group returns non-retryable
`RECOVERY_REQUIRED` **without signalling or starting a replacement**"
(SPEC-005) — so no process is ever authorized to append the transition record
("the worker is the single state-transition authority"; the CLI "never writes
the audit ledger while a worker owns the run"); (3) therefore the run stays
`running` forever; (4) "Forking is allowed from idle, paused, closed, and
outcome-unknown runs, **but not from running or waiting runs**" (SPEC-008) —
so `fork --fresh`, "the only explicit immediate escape" (SPEC-005), returns
`RUN_STATE_CONFLICT`. Every command on the run yields `RECOVERY_REQUIRED` or
`RUN_STATE_CONFLICT`; the escape that justified deleting forced recovery does
not exist for the canonical crash. This is the largest regression introduced
by the round-3 revision.

**Fix.** Either (a) define a normative ledger-only quarantine transition: when
the recorded worker is provably not serving (socket dead + identity Absent/
Mismatch/Unverifiable) and the ledger shows an accepted turn with no terminal
record, a recovering invocation MAY append `running → outcome_unknown` as an
explicitly permitted bootstrap write — this asserts only uncertainty, not an
outcome, so it does not violate ADR-008; or (b) permit `fork --fresh` from
`running`/`waiting_*` when the source worker is unreachable, and say so in
SPEC-008 and the state table. Option (a) is cleaner: it also restores
`status/events/verify` usability on the stuck run.

### C-2. The startup lock cannot be handed from CLI to worker — POSIX record locks are not inherited across fork()  [Critical]

**Where:** `specs.md` SPEC-007 ¶4; `architecture.md` "Per-Run Worker",
"Process and Transport Topology" (startup-lock ¶), ADR-004.

**Problem.** The SOT never states which process holds the startup lock, but
implies both can ("an exact worker **or transient starter** wedged while
holding the startup lock"). Measured on this machine: `fcntl` record locks are
per-process and **do not survive `fork(2)`** (child's `F_SETLK` → `EAGAIN`
against its own parent). The worker is created by the CLI "after a single fork
and `setsid()`". Therefore either the CLI holds the lock and the worker holds
nothing, or the CLI must release before the worker can acquire — and between
release and acquisition there is an unserialized window. Concrete failure:
CLI-A locks → forks worker-W → W starts up (validating target, binding
socket) → A gets fd-3 success, exits, lock releases → CLI-B acquires the free
lock, finds the socket absent, reads a runtime record still describing the
**previous** generation, proves it Absent, and per architecture "permits stale
socket/sidecar unlink and bind" — unlinking the socket W is about to bind and
starting a **second worker and app-server for the same run**.

**Fix.** A two-byte protocol on the run-keyed lock file (byte-range locking
makes this natural, and it also gives the "worker or transient starter"
wording a real mechanism): byte 0 = starter claim, held by the CLI from before
fork until fd-3 completion; byte 1 = owner claim, taken by the worker as its
**first** post-`setsid()` action, before spawning anything, and reported on
fd 3 only once held; the CLI releases byte 0 only after reading fd-3 success,
so the interval is gap-free. A contender waits on both bytes; `F_GETLK` per
byte distinguishes starter from owner for the takeover verdict. State
explicitly whether the worker holds byte 1 for its serving life or only during
startup/shutdown (see H-8 — attachment must not require the lock).

---

## 3. High findings

### Identity and group-absence proof (the weakest layer this round)

**H-1. The group-absence proof is a one-clause policy over undefined
operands.** Four gaps, all load-bearing, first two measured:
- **(a) Boot-UUID shortcut does not extend to the group proof** — step 2
  ("a difference proves `Absent`") covers the leader; step 6 has no exemption,
  so after a reboot a recycled PGID makes the group "nonempty and ambiguous"
  ⇒ `Unverifiable` — even though no process survives a reboot. Reboot
  therefore does **not** bound the fail-closed state. *Single highest-value
  fix in this review:* "a recorded boot-session UUID differing from the
  current one proves the **entire** recorded generation absent, leader and
  group alike; steps 3–6 are skipped." This converts every `Unverifiable`
  into "unverifiable until next reboot" — a bounded posture instead of an
  absorbing state.
- **(b) The member survivor predicate is missing** — step 6 says only
  "sampling each member"; round-2's rule (member start-time ≥ recorded leader
  start time, same uid, same pgid) did not survive into the text. Without it,
  every nonempty enumeration is ambiguous by construction. Restore it, and
  state that a member started strictly before the recorded generation is
  dismissed.
- **(c) Zombies are enumerable but unsampleable** — measured: an unreaped
  zombie still appears in `proc_listpgrppids` while `PROC_PIDTBSDINFO`,
  `proc_pidpath`, and `EVFILT_PROC` all fail `ESRCH`. Under "an unavailable
  required field is `Unverifiable`", one zombie forces permanent
  `RECOVERY_REQUIRED` and cannot even be signalled to clear. Add the branch:
  ESRCH on a member = `Absent` (a terminated process cannot mutate anything,
  reaped or not).
- **(d) The enumeration itself can fabricate an absence proof** — measured:
  `proc_listpgrppids(pgid, buf, 0)` returns 0 with **no error** (a
  zero-capacity buffer "proves" an empty group); truncation is silent; the
  `NULL,0` size query returns a system-wide count, not the group size. Since
  this call *is* the safety proof, make the recipe normative: capacity > 0,
  retry with doubled capacity while `returned == capacity`, `-1` ⇒
  `Unverifiable`, `0` proves absence only with positive capacity and pgid > 1.

**H-2. Step 5 signals the *group* but identity-verifies only the *leader*.**
`killpg` carries no identity check; the leader is most likely already gone by
the SIGKILL phase (TERM was meant to kill it), and this machine's PID/PGID
space recycles in minutes (measured: full wrap inside one 60-second sample on
a busy machine). Never signal by negative PID: enumerate the group, verify
each member against the survivor predicate (H-1b), signal members
individually, re-enumerate and re-verify before each escalation.

**H-3. Two independent paths yield two simultaneous "exclusive" writer
leases (round-2 H-W6 regression + a new one).**
- (a) The dev/inode re-verification is absent from the revised SOT (grep:
  nothing). Reproduced: holder A flocks the lease file → the file is unlinked
  (user cleanup, errant script — the SOT itself contemplates "manual deletion
  … outside the guarantee") → holder B creates a new inode at the same path
  and flocks it successfully: two writers, no error anywhere. Fix: after
  acquisition, `fstat` the held fd + `fstatat` the path from the validated
  root fd, require identical `(st_dev, st_ino)`, re-verify at writer barriers,
  and record `(dev, ino)` in `writer.json`; treat mismatch as
  `RUNTIME_PATH_COLLISION`. Downgrade "outside the guarantee" to "detected and
  failed closed".
- (b) The lock root is derived from the inheritable `$XDG_STATE_HOME` and is
  never recorded or cross-checked — the exact dependency class the socket
  design deliberately refuses ("discovery never recomputes a path from
  `$TMPDIR`"). Two invocations with different values resolve two lock roots ⇒
  two workers each hold "the" exclusive lease. Wrapper argv (explicitly
  permitted by SPEC-003; only `GOMCHI_*` is stripped) can cause this. Fix:
  resolve once, record the canonical root + `(dev, ino)` in `writer.json`/
  manifest, fail `RUNTIME_PATH_COLLISION` on later divergence — or derive
  from validated `$HOME` only.

**H-4. In-place binary upgrade makes live runs permanently unrecoverable.**
Measured: `proc_pidpath` **fails** for a live process whose executable was
unlinked (and returns the same path string after an unrelated binary replaces
it). `cargo install`/`brew upgrade` while runs are live ⇒ every identity
sample of the live worker hits "unavailable required field" ⇒ `Unverifiable`
⇒ non-retryable `RECOVERY_REQUIRED`, no force override. Compounded by
**H-5**: after the upgrade every command against the old worker returns
`GOMCHI_PROTOCOL_MISMATCH` — including `pause` and `close`, the natural
remedies — and whether a protocol-mismatched (reachable, promptly-answering)
worker satisfies `recover`'s "fails a ten-second socket handshake"
precondition is genuinely ambiguous. The operator's only remedy is restoring
the exact prior binary at the exact prior path. Fixes: extend recorded
identity with `(exe_dev, exe_ino)` + the binary SHA-256 already in the
manifest; define "path unavailable on a live process" as non-dispositive
(fall back to tuple + group proof); and carve out a version-frozen minimal
control sub-protocol (`hello`, bounded `shutdown`) exempt from the digest
equality check, used by `recover`/`pause --interrupt`/`close --interrupt`.

**H-5. `GOMCHI_PROTOCOL_MISMATCH` has no stated recovery path** — see H-4;
also the code sits in exit class 5 ("Codex compatibility or target
validation") though it is a Gomchi-self condition, and the table cell omits
the workspace/run-identity and generation mismatches that SPEC-004 says
return the same code.

**H-6. The pre-persist spawn window can orphan an unrecorded app-server.**
Between `posix_spawn` returning and the provisional identity persist, a live
app-server exists with no durable record. Worker SIGKILLed in that window ⇒
recovery finds nothing, classifies `Absent`, starts a second writer
app-server beside a live orphan holding workspace-write sandbox. Fix (clean,
verified available): spawn with `POSIX_SPAWN_START_SUSPENDED` +
`POSIX_SPAWN_SETPGROUP`, fsync the provisional identity, then `SIGCONT` — the
child cannot execute an instruction before its identity is durable. Also
state the provisional-only classification rule: with no fsynced
`generation_started`, a provisional-record executable-path difference is
non-dispositive (the recorded path may be the pre-exec wrapper stage) and
MUST NOT yield `Mismatch`; absence rests on the group proof alone.

### Workspace blast radius and the quarantine trap

**H-7. The unverifiable zombie blocks every future writer in the worktree.**
(Convergent finding from two independent passes.) The zombie run holds no
lease and no worker — but it holds the **`writer.json` cleanup pointer**, and
step 1 of *every* writer acquisition ("load the fsynced generation identity
and current writer cleanup pointer") classifies it. Permanently
`Unverifiable` ⇒ every future `run start --access write` / `promote` /
`resume --access write` / `fork --access write` in that canonical worktree
returns `RECOVERY_REQUIRED`, forever — a workspace-scoped outage caused by
one run, stated nowhere. Fix: scope the pointer — a new writer that acquires
the flock (proving no live holder) and whose own run has no unverifiable
generation is admitted, recording the unresolved foreign generation as an
audited observation on its own ledger; the cleanup pointer gates *same-run
same-thread* resumption, not lease acquisition by unrelated runs. (With H-1a,
the whole class also becomes reboot-bounded.)

**H-8. The zombie run itself can never be closed or deleted, and `status`
can fail on it.** `close` requires proven generation cleanup and emits
`RECOVERY_REQUIRED`; `delete` requires `closed`/`start_failed`; `run status`
is itself a `RECOVERY_REQUIRED` emitter, so even the one command SPEC-008
promises in every state can fail. Escapes that preserve no-fabrication:
(a) make `status` projection-only (report the identity verdict in `data`,
never fail on it); (b) allow `run delete --confirm` once a boot-session
change proves the recorded generation absent (asserts nothing about turn
outcomes; SPEC-010 already never touches the Codex thread); or (c)
`run close --record-unverifiable` appending a distinct
`closed_with_unverified_generation` seal — materially different from
ADR-008's rejected "user-declared completed/failed" because it records
*uncertainty*, not an outcome. State the two clearing conditions (boot-UUID
difference; group provably empty) as operator guidance next to the no-force
sentence in SPEC-005.

**H-9. State-machine leftovers re-opened by the revision.**
(a) SPEC-008 contradicts itself on ordinary fork from `outcome_unknown`: the
forking paragraph and architecture permit it (through the last confirmed
boundary, after proven absence) while the quarantine allowed-list names only
`fork --fresh`. The probe evidence exercised exactly the permissive path
(`thread/fork(lastTurnId)` through a completed boundary), so add ordinary
fork-with-proven-absence to the allowed list. (b) The state table's
`outcome_unknown` Worker column stayed a flat "No" while the App-server
column got its reconcile exception — the transient reconcile **worker**
generation the same document defines is forbidden by its own table (round-2
C-2 partially re-opened). (c) Writer-lease acquisition is listed differently
in three places (error table, SPEC-007, architecture; roadmap TASK-007 omits
`recover`), and none covers plain `run start` under a workspace whose
`default_access` is `write`. Define the list once in SPEC-007 and reference
it. (d) The wedged-worker recovery paragraph is writer-scoped ("the run's
recorded writer still holds the writer lease"), but SPEC-005's precondition
has no access qualifier — a wedged *reader* worker satisfies the
precondition and then has no defined mechanism. Restate over "the run's
recorded worker" with the lease-release step as a writer-only addendum.

### Ledger and protocol handling

**H-10. `$gomchi_number` / `$gomchi_redacted` markers are forgeable.** No
escaping rule exists for inbound keys with the `$gomchi_` prefix (grep: the
three defining occurrences only). A wire payload containing a literal
`$gomchi_redacted` object — echoed via prompt/tool output, or coincidental —
is stored verbatim; a ledger reader cannot distinguish a Gomchi-applied
redaction from a payload-supplied forgery. Hash chaining does not help: the
record is authentically what was observed; the ambiguity is semantic, in the
artifact the product designates as audit authority. Fix (one paragraph, in
the same recursive pass that already visits every key): escape inbound
`^\$+gomchi_` keys by prefixing one `$` (losslessly reversible; run before
marker insertion), and fix the pass order (escape → redact → number-adapt).

**H-11. A too-large solicited response can permanently trap quarantine.**
The 16 MiB stdout cap treats every line alike, but
`thread/read(includeTurns: true)` returns **one response line carrying the
entire thread** — and that is exactly what `reconcile` needs to leave
`outcome_unknown`. A long-lived run's full history crossing 16 MiB is
ordinary; the oversize rule then "stops the app-server generation", so
reconcile can never succeed and the run can never reach `paused`/`closed` —
circular. Fix: distinguish Gomchi-**solicited** responses (correlated JSON-RPC
`id`) from unsolicited stream traffic: for solicited oversize, fail the
*command* with `PROTOCOL_FRAME_TOO_LARGE` and leave run state untouched (a
too-large history response says nothing about turn outcomes), or parse
incrementally. Related refinement (Medium): for *complete but invalid* lines
(duplicate members), permit envelope-only extraction (id/method/threadId/
turnId, first-occurrence, bounded) for the **scoping decision only** — a line
provably about another thread should not quarantine the active turn; keep
quarantine for truncated lines and for lines that correlate to the active
turn.

**H-12. The 10-second wedged-worker budget kills healthy-but-busy workers.**
A worker legitimately exceeds 10 s while replaying `audit.jsonl` from genesis
(state.json loss), serving `run verify` (genesis scan), or in a barrier
storm; recovery then terminates it **mid-turn** and manufactures the
`outcome_unknown` it exists to repair. Fix: split liveness from work — a
dedicated always-responsive `hello` path defines the 10 s budget; escalation
additionally requires a false progress predicate over a longer window (ledger
head static, no `worker.log` growth, no CPU-time delta), with the observed
evidence recorded in the cleanup audit record. Also state the concurrent-
contender abort conditions (NOTE_EXIT fired; runtime-record generation
advanced; lock body owner changed ⇒ restart election).

**H-13. SIGTERM during an active turn never attempts interruption — every
logout/reboot during a writer turn deterministically quarantines the run.**
The shutdown sequence goes straight to child-group cleanup; it never issues
`turn/interrupt` nor waits for terminal evidence — yet TASK-000's own probe
proved the pinned binary persists a correlatable `interrupted` status for
exactly this case. Fix: insert a bounded interrupt phase ("if a turn is
active, issue `turn/interrupt`, wait up to the same five-second grace for the
terminal notification, append and fsync it, shut down from idle; only on
expiry proceed to unconditional cleanup"). This converts the most common
shutdown path from quarantine-by-default to clean-idle-by-default.

**H-14. Effort validation depends on an app-server field nothing has
verified.** `model/list.supportedReasoningEfforts` appears exactly once in
the SOT and in no probe, no evidence line, no roadmap fixture. If pinned
0.147.0's stable `model/list` does not expose it, every `--effort` returns
`COMPATIBILITY_REJECTED` and SPEC-008's rule is unimplementable — discovered
at TASK-006 instead of TASK-000, whose purpose is retiring exactly this class
of uncertainty. Fix: add the field to the schema probe's required set and to
the evidence, and an advertised/unadvertised effort fixture to TASK-006.

**H-15. Startup-lock file hygiene (three sub-items).** (a) Any re-open/close
of the held lock file by the owner silently drops the lock (measured, even
via a hardlink) — and the SIGTERM path's "unlinks its socket and sidecar
under the startup lock" implies exactly such a re-open. Rule: the lock file
is opened exactly once per process; identity re-verification uses `fstat` on
the held fd, never a second `open`. (b) The `F_GETLK` → identity chain has no
`l_pid ≤ 0` branch (measured: `l_pid = -1` for any flock-origin lock — which
also means the writer lease and startup lock MUST be provably distinct files;
mandate `locks/writer/<ws>` vs `locks/startup/<run>`), and PID reuse between
`F_GETLK` and verification is real at measured recycling rates — make the
lock file self-describing (owner writes its identity tuple into the body
under the lock) and treat `l_pid` as a hint. (c) Attachment must be defined
to not require the startup lock (otherwise every `status`/`send` against a
busy worker enters the 10 s takeover path); state that a live, answering
socket ⇒ attach, never take over, regardless of lock state.

---

## 4. Medium and Low findings

Medium:

| ID | Finding | Fix direction |
| --- | --- | --- |
| M-1 | `fork --fresh --access write` from an unverifiable **reader** source: SPEC-008 permits (writer-only wording), error table rejects (excludes only read-only fresh). Also ambiguous whether a forbidden `--access write` is rejected or silently downgraded | Permissive rule + explicit rejection: source-generation `RECOVERY_REQUIRED` applies to fresh forks only when the unverifiable generation held write access; never silently downgrade a requested access mode |
| M-2 | `fork --fresh` does not stop a possibly-live suspected writer; the fresh run (read-only, no snapshot isolation) may observe its mutations — coherent but unstated; "escape" reads as "resolved" | One sentence in SPEC-005/ADR-004 stating the residual |
| M-3 | `RECOVERY_REQUIRED` vs `WRITER_BUSY` precedence unstated for `fork --access write` (both list it) | Identity-proof failures precede lease contention |
| M-4 | Non-local `$HOME` (MNT_LOCAL unset): failure code reachable from far more commands than the `RUNTIME_PATH_INVALID` emitter list admits (startup lock also serializes attachment; readers need it too — a network home disables Gomchi entirely); no remedy documented | State the full emitter set + a validated, recorded local-root override remedy |
| M-5 | Lock-root creation race (`EEXIST` → validate) and fd-relative validation (`fstat`/`fstatfs` on the opened directory fd, `openat` for everything beneath) are hinted but not required — the socket root has the explicit `*at()` language, the lock root does not | Copy the socket root's language |
| M-6 | `run pause` performs verified group cleanup but is not a `RECOVERY_REQUIRED` emitter — an unverifiable group during pause has no assigned code | Add `pause` to the emitters (parallel to `close`) |
| M-7 | fd-3 contract: `FD_CLOEXEC` unstated (app-server inheriting fd 3 turns every startup failure into a 10 s hang), readiness point undefined (socket-bound vs fully-validated — the latter cannot fit 10 s over a big ledger replay), timeout behavior undefined (CLI must not signal the worker) | Two-object handshake (`bound` short bound, `ready` longer/derived bound); CLOEXEC on fd 3 and the lock fd; timeout ⇒ `TRANSPORT_FAILURE` without signalling |
| M-8 | Pass ordering of escape/redact/number-adapt is observable in the hash but unspecified | Fix the order (escape → redact → number-adapt); state that the redaction tokenizer never matches Gomchi marker keys |
| M-9 | Provisional runtime record's durability class unstated for the window where it is the sole identity source | write-temp + fsync + rename + dir-fsync, same as the class already defined |
| M-10 | Shutdown's socket unlink "under the startup lock" needs a bounded-acquisition fallback (skip unlink and exit; sidecar check already tolerates stale sockets) | One sentence |
| M-11 | Oversized/invalid stdout: no tolerance counter — one malformed uncorrelated line from a buggy build ends a healthy session | Bounded audited tolerance before generation stop (composes with H-11's envelope scoping) |
| M-12 | `Unverifiable` causes still not exhaustively enumerated (H-W1 residual); "two-sample"/"three-sample" naming inconsistency for the same procedure | Enumerate; name it once |

Low:

| ID | Finding |
| --- | --- |
| L-1 | `sessionKey`/`session_key` still unredacted (`session key` missing from the vocabulary; `key` is excluded) — add the sequence |
| L-2 | `RUN_BUSY` lists `run start` (a fresh UUIDv7 cannot lose a per-run race — its real contention is `WRITER_BUSY`); `PROTOCOL_FRAME_TOO_LARGE` claims app-server-frame emitters but no command receives app-server frames (that path is `payload_unrepresentable` + generation stop). Trim both cells — TASK-013 will otherwise attempt unreachable fixtures |
| L-3 | Exit-class-4 description and `INVALID_ARGUMENT` condition cell not widened for their new members (serialization contention, recovery precondition; export-collision rule) |
| L-4 | SPEC-012's managed-context enumeration says "target mutation" while the `POLICY_REJECTED` cell rejects every managed-context command except own-run `status/events/verify` — the enumeration should read "target commands" |
| L-5 | Observer barrier: SPEC-006 "committed" vs architecture "fsynced" — pick "fsynced" (only it survives the crash the design is built around) |
| L-6 | Review-ledger bookkeeping: follow-up prose says "six blockers and two low" but its table has 9 unlabeled rows; disposition header says review "closed" while its closing ¶ says "remain active until independent follow-up review completes"; `deferred-feedback.md` still claims "No independent implementation review has occurred" — stale vs `round-2-closure.md`, and it is the target of roadmap gate 6 |
| L-7 | `tools/probes/__pycache__/*.pyc` exists and `.gitignore` has no Python rules — the first TASK-000 commit (`git add -A`) will sweep compiled artifacts in |

---

## 5. Probe suite assessment

**Substantively hardened, and the recorded PASS results are genuine.**
Verified in code: version pinning now hard-fails without `--allow-unpinned`;
subprocess timeouts everywhere (including `codex --version` and schema
generation); the sandbox probe's anti-gaming design is real and layered (the
`ok` gate never reads the model's text — it requires wire-level
`item/started|completed` telemetry **and** an independent filesystem check
performed by the probe itself); the fork lesson is structurally encoded
(`fork_boundary_turn_id`, the completed setup turn, is a distinct variable
from the crashed `turn_id`, and only the former is ever passed as
`lastTurnId`); deletion proof uses a third fresh app-server's
workspace-filtered `thread/list`, never a same-session `thread/read`; the
frame probe gates ≥ 2 MiB requested / ≥ 1 MiB observed / correlated exit-0
completion and retains only counts and sizes.

**But two suite-wide disposition claims do not hold, and round-2's diagnosed
bug pattern recurs:**

- `task000_crash_history_probe.py::wait_for_command_start` (L110–114) still
  contains the exact blanket flat-`turnId` prefilter round 2 diagnosed: the
  filter runs before the method check, so its own
  `method == "turn/completed": return False` fast-fail branch is unreachable
  dead code (the same file's `wait_for_terminal` correlates `turn/completed`
  via nested `turn.id`, confirming the shape). Dormant on every recorded PASS
  (the prompts force a shell item, which always wins the race), and bounded by
  the outer deadline — but it silently degrades the one failure it exists to
  catch into a generic diagnostics-free timeout.
- Failure diagnostics ("records only bounded message shapes on failure") are
  present in the sandbox and subagent probes, partial in the frame probe
  (counts only), and **absent** in the crash and handshake probes (bare
  `RuntimeError` strings).
- Root cause is structural: `_probe_support.py` centralizes version pinning,
  bounded messages, and the exception envelope, but **not** the JSON-RPC
  read/correlate/teardown harness — so each fix had to be hand-copied into
  five independent implementations and landed in 3–4 of 5. Centralizing the
  reader/correlator would have made both regressions impossible.

**Coverage:** both new charter items are covered (frame measurement;
workspace-write crash + live `thread/read` + `thread/fork(lastTurnId)`).
Still never exercised live anywhere: **`turn/interrupt`** and
**`thread/resume`** — and the evidence's Scope Limitations section, whose job
is exactly this disclosure, does not mention either (it does correctly
disclose the approval-flow and `codexHome`-identity boundaries).
`thread/resume` is the RPC every ordinary recovery depends on; one small live
probe (start → idle → kill → resume → verify history) would close it. Add
`model/list.supportedReasoningEfforts` presence to the schema probe (H-14).

---

## 6. Process finding: the independent-review method is not catching what matters

For the second consecutive round, an independent read-only review closed with
"no blocking or non-blocking findings" (`round-2-closure.md`; previously
review.md §9), and for the second consecutive round the next verification
pass found Critical-severity defects in the same snapshot — including a
recurrence of a bug pattern the reviewed documents themselves describe. The
closure reviews validate links, parseability, and prose consistency; the
defects live in OS semantics (lock inheritance, zombie process visibility,
PID recycling) and in cross-document behavioral chains (C-1's five-quote
chain). Recommendation: the independent gate for TASK-000 (and later task
gates) should require (a) at least one adversarial pass with a stated attack
budget against the concurrency/recovery design specifically, and (b)
empirical verification of any OS-semantics claim the SOT makes normative —
the C-probe method used this round is cheap (hours) and found five measured
divergences between the spec's assumptions and the platform.

---

## 7. What this round got right — do not churn

- **Removing forced recovery + hazard markers was the right call**, and
  TODO-005's promotion criteria are exactly the right bar. The fixes in
  Sections 2–3 make the remaining fail-closed posture *bounded*; they do not
  reintroduce force.
- The **transient read-only reconcile generation** (never `thread/resume`,
  only `thread/read`) is a clean resolution of round-2 C-2 — stronger than
  what was asked; keep it (fix only the Worker-column footnote, H-9b).
- The **four-verdict identity model with boot-session UUID** is the right
  frame; this round's findings fill its operand definitions rather than
  replace it.
- The **oversized-stdout → quarantine posture** is defensible and superior to
  the round-2 suggestion it declined (failing the turn would assert an
  outcome from an unparseable stream); H-11/M-11 refine its scope, not its
  principle. Record the rationale in an ADR.
- The `$gomchi_number` / `$gomchi_redacted` design is sound apart from the
  escaping rule (H-10); the JCS byte-identity verification rule survives all
  marker interactions (verified: marker objects re-serialize byte-identically).
- The **error-table format**, the CLI grammar completion, the XDG-state-root
  *intent* (off `/tmp`), the `MNT_LOCAL` requirement, the fd-3 handshake
  concept, the SIGTERM evidence-first shutdown concept, and the probe suite's
  anti-gaming/anti-fabrication discipline are all keepers.
- The `docs/reviews/` disposition convention (immutable input review, separate
  owner disposition, separate closure) is good process and should continue.

---

## 8. Recommended order of work

1. **SOT patch pass 3 (~1 week, no code).** C-1 (quarantine bootstrap write
   or fresh-fork-from-running), C-2 (two-byte startup-lock protocol), the
   H-1 cluster (boot-UUID group rule first — it bounds everything else),
   H-2–H-9, H-10 (marker escaping + pass order), H-11/M-11 (solicited-response
   carve-out + tolerance), H-12/H-13 (liveness split; interrupt-on-SIGTERM),
   H-15, then the Medium/Low tables. Record the oversized-stdout rationale as
   an ADR while touching it.
2. **Probe patch (~half day).** Fix the recurring prefilter in
   `wait_for_command_start`; add diagnostics to the crash and handshake
   probes; centralize the reader/correlator in `_probe_support.py`; add
   `supportedReasoningEfforts` to the schema probe; add a small live
   `thread/resume` probe and disclose `turn/interrupt` in Scope Limitations;
   add `__pycache__/` to `.gitignore` before the first commit.
3. **Independent re-review with the strengthened method** (Section 6):
   adversarial pass on concurrency/recovery + empirical OS verification.
4. **Then TASK-000's commit/implementation-note gates, then TASK-001.** With
   the patch pass and re-review done, the plan is implementable as specified;
   the feasibility grade moves from **B to B+**, with the remaining distance
   to A− being execution risk, not design risk.
