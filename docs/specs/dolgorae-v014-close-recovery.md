# Retained Close and build recovery

This document owns TASK-071's native control correction. The
[roadmap](../roadmap/README.md) owns delivery status. The frozen H1 and public-v1
packages retain their exact bytes; public-v2 projection belongs to TASK-078.

## One retained Close

A whole-session Close retains its operation identity, interrupt choice and last
safe failure cause across retries and gateway restart. Repeating the same Close
resumes that operation once its previous owner has stopped. A different
interrupt choice is refused. New work remains fenced throughout settlement.

The coordinator holds a private permanent per-Run lifecycle lock before
settlement or generation replacement. Its synced owner witness and held kernel
lock establish a live owner. A stored `settling` phase alone does not establish
active progress. Observation reports recovery when no live owner can be proved;
a competing caller cannot replace an active owner.

Recover/Reconcile first resolves the Primary's actual process, admission or
outcome condition, including a lost worker on a Thread-bearing idle Run. It then
settles children and closes the Primary at its current post-settlement revision.
An interrupt acknowledgement does not establish terminal history. If the Primary
still has an active Turn, pending interaction or nonterminal lifecycle after the
acknowledgement, Close retains its original operation and reports
`RECOVERY_REQUIRED` before attempting Writer release. The caller observes the
terminal outcome and explicitly recovers that same Close; the coordinator does
not wait indefinitely or create another Close. Public v2 projects this retained
recovery as an accepted effect with recovery required. A live-owner
`SESSION_CLOSE_IN_PROGRESS` instead requests a snapshot with no effect claimed
for the contending request, whose interrupt choice has not been admitted.
Changing the retained interrupt choice remains a no-effect refusal.
An uncertain effect remains fenced and is never redispatched. A coordinator
that exits before completion retains an actionable recovery state and its cause.

Caller-workspace generations 3 and 4 use the same retained session-close
coordinator, pre-close Writer release and Recover/Reconcile path. Generation 4
must retain its captured delegation instruction identity when projecting close
or recovery state; it is not a generation-3 instruction capture. The legacy
adapter remains restricted to generations 1 and 2. A retained close reports the
same operation and recovery diagnostics for either caller-workspace generation.

For caller-workspace generations 3 and 4, a terminal Run projection does not
finish Close cleanup. Before a retained Close becomes complete, its owner MUST
prove the current Run generation absent, retire its original Profile membership
and verify that retirement. Recovery of
an already-Closed Primary performs these remaining obligations under the same
Close operation and lifecycle lease; it does not replay a native Close or
create a replacement generation. An absent Worker requires complete owned-scope
absence, not a PID-only observation. A missing locator requires the existing
generation-bound absence marker, startup witnesses and empty inventory. The
highest actually evidenced generation governs this proof; a Run with no worker
attempt evidence remains generation zero and requires no invented absence
marker. Display observation opens only existing startup evidence and never
creates a lock file, prunes inventory or records fresh absence authority.
Both successful terminal Close receipts, `completed` and interrupt-driven
`aborted`, remain immutable while unfinished or unverifiable cleanup is
reported as recovery required. Legacy generation 1 and 2 continuation keeps
its existing cleanup behavior. An aborted session without a retained Close
intent does not create a new Close operation.

For caller-workspace generations 3 and 4, an explicitly repeated Close of a
durably Closed ordinary Run likewise finishes only owned-generation cleanup and
Profile retirement after current Controller and revision validation. The retiring
ordinary Close refuses Profile retirement while that Run still holds Writer
authority; the non-retiring successor Close leaves Writer settlement with its
existing release owner. This continuation does not redispatch the original
native Close. Legacy generations 1 and 2 retain their existing cleanup rules.

## Explicit build handoff

Ordinary mutation still requires the worker's exact version, mutation protocol
and executable digest. Explicit `run recover` may replace an old build only
after fresh Controller, workspace, process, ledger and writer checks establish
quiescence. Busy Turns, pending interactions or admissions, unknown effects,
unsettled writer authority and unverified process identities are typed recovery
blockers. A display observation cannot authorize handoff.

Handoff uses only frozen control-v1 status and shutdown. It requires a terminal
shutdown acknowledgement and exact absence of the recorded old generation and
its owned process scope before publishing a replacement. The workspace writer
boundary excludes new admissions while quiescence, shutdown and absence are
proved; the startup boundary remains held through replacement publication.
The architecture's bounded handoff exception permits only idle Status/Shutdown
and verified exit checks under the writer lock, with ten-second deadlines for
each control call and the exit wait. It releases that lock before replacement
startup and never waits there for a Turn or user action.
Retained history, accepted receipts, Thread identity and instructions remain
unchanged. Failure
after shutdown requires recovery; it never authorizes replay of uncertain work.

The native recovery path carries blockers as stable `RECOVERY_REQUIRED` reason
values. Public-v2 recovery projection and actual consumer runtime qualification
remain with their assigned later owners.

| Reason | Required recovery |
| --- | --- |
| `build_handoff_lifecycle_busy` | Observe or resume the retained lifecycle owner. |
| `build_handoff_run_not_quiescent`, `build_handoff_status_not_quiescent` | Settle the existing Turn and interactions using the verified original build. |
| `build_handoff_admission_unresolved`, `build_handoff_submit_unresolved`, `build_handoff_outcome_unknown` | Reconcile the original admission or effect; retain its original key. |
| `build_handoff_writer_unsettled` | Settle the Run's writer authority before replacement. |
| `build_handoff_identity_mismatch`, `build_handoff_controller_changed`, `build_handoff_process_unverifiable`, `build_handoff_workload_unverifiable` | Restore current authority and exact process proof; do not signal an unverified process. |
| `build_handoff_history_unverifiable`, `build_handoff_retained_state_changed` | Restore and revalidate the retained history and accepted identities. |
| `build_handoff_status_unverifiable`, `build_handoff_shutdown_unconfirmed`, `build_handoff_absence_unverifiable` | Prove the original generation's outcome and complete process absence before retrying. |

A later blocked request cannot downgrade retained `outcome_unknown` progress.
Only authoritative reconciliation and successful settlement may advance that
Close to completion.
