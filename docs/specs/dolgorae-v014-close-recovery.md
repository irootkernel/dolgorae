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
An interrupt acknowledgement does not establish terminal history. An uncertain
effect remains fenced and is never redispatched. A coordinator that exits before
completion retains an actionable recovery state and its cause.

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
