# v0.1.4 Profile Journal Maintenance and Close Cleanup

This document owns TASK-073's native storage and cleanup behavior under the
successor requirements. Public-v2/facade activation remains TASK-078 work.
Actual consumer qualification, the complete Phase 3/H3 prerequisites and Gul's
E16 entry retain their assigned owners. EPIC-019 alone does not satisfy them.

## Capacity and retained history

The selected membership append journal is bounded by 8 MiB. Binding history
retains its 1 MiB selected-state bound. Valid storage that exhausts a bound is
`STORAGE_CAPACITY_EXCEEDED`; malformed state, broken hash continuity, unsafe
metadata and inconsistent projections remain integrity or recovery failures.
The existing frozen transport reporting boundary applies to capacity errors.
Capacity admission precedes effects and never permits corrupt state to be used.
A bounded incomplete prefix must still reject already known schema, type,
duplicate-key and chain errors before reporting capacity. Membership maintenance
and cleanup validate the home and `profiles` parents as well as the server leaf;
an unsafe parent cannot supply authority through a secure-looking child.

Membership maintenance records the complete approved source generation and
history digest before switching generations. It retains every original record
and accepted identity, including released members. Its checkpoint retains the
latest facts for each member and the absolute revision and chain head; new
records continue that head rather than restarting at revision one. Active and
unknown members remain protected. Compaction performs no content retirement.
Checkpoint files have a separate 64 MiB bound so distinct retained members do
not exhaust compaction at the append journal's 8 MiB boundary. Operator
maintenance can validate a legacy source journal up to 64 MiB and retain its
complete original bytes; ordinary append admission keeps its 8 MiB ceiling.
Binding maintenance preserves the immutable name, definition, server and launch
snapshot identities and retains the original bytes needed for audit and recovery.
Ordinary binding updates use the same compact representation as maintenance,
so a subsequent binding does not consume recovered capacity on formatting.

## Locking and crash recovery

Registry and binding operations retain the home-root lock. Membership selection
and appends retain the Profile Server lock, after the home-root lock where both
are required. Maintenance excludes competing journal writers without treating
an active or unknown member as released. Readers pin one checked generation and
never combine checkpoint and append files from different selections.

Compaction does not require Run quiescence or an exclusive workspace ledger
lease. The home-root and Profile Server locks stabilize membership and binding
sources, including active and unknown members. Run ledger heads recorded by a
plan are observation provenance; unrelated Turn progress does not invalidate a
membership-preserving plan. Run bindings remain planning provenance. Apply
revalidates the generation, journal digest and protected references of each
Profile in the retained plan, plus the complete binding-history digest. An
unrelated Run publication or removal does not change that operation's scope.

Profile operation sidecars occupy the workspace's `profile-maintenance`
namespace; ledger operation sidecars retain their separate `maintenance`
namespace and generation. The [native Profile sidecar schema](../protocol/dolgorae-native-profile-maintenance-v1.schema.json)
checks their persisted shapes. An operator-approved retained dry-run plan binds
the complete source, target, operator and original operation identity.
Preparation writes and syncs retained history, checkpoint, append and manifest
files and their directories before
atomic selection. Every selection and completion boundary can resume that
original operation. An unavailable or damaged namespace cannot establish
absence. Old reader and recovery files remain; no automatic deletion or pruning
is enabled. A repeated apply cannot allocate a replacement operation or change
an accepted identity. Protected-reference retirement remains explicit later work.

Once an immutable owner is retained, Get reports the obligation to resume even
before progress starts. Completion retains that obligation until the operation's
global admission marker is durably released. Completed membership remains
subject to current selection, append, derived index and server consistency
checks while that admission is retained. After
release, legitimate subsequent generations do not invalidate the retained
completion receipt. Credential rotation preserves the same operator identity's
ability to resume an owned operation. Get and Apply never recreate a missing
operation namespace; a healthy independent global owner
preserves the original operation reference when workspace sidecars are damaged.
While admission remains owned, binding completion verifies the selected compact
bytes against the retained original source. After admission is released, later
ordinary binding changes do not invalidate the completed receipt.

Persistence changes may omit a write only when the existing durable state
already represents the same facts. Failure or a crash cannot publish a newer
index or server revision without its accepted journal record.

## Successful Close with pending cleanup

The durable closed Run and its original terminal receipt remain authoritative
when Profile membership release fails. Transports that can represent cleanup
retain the successful Close result, closed state, original history and cursor.
Existing Run recovery fields expose the cleanup obligation. Frozen caller
Machine-v4 cannot represent the cleanup projection and reports its
registered `RECOVERY_REQUIRED` envelope with an explicit completed-Close reason.
Public-v1 retains the successful closed projection and reports required operator
repair through its existing recovery fields. Neither boundary retracts the
completed mutation or authorizes replay.

A closed Run whose membership is not durably released, or whose membership
cannot be proven, remains a cleanup obligation. Status reports it without
starting a worker. An authenticated repeated Close reuses the closed Run and
attempts only its original membership cleanup; it never redispatches work or
appends another close transition. Successful durable release clears that
obligation. Uncertain or corrupt membership continues to block destructive
Profile lifecycle operations until their existing authority can prove safety.

Display sampling never creates a Profile directory or lock and never waits on
the home-root or Profile Server lock. It compares the derived member projection
with the checked selected checkpoint and append, retaining its current head and
source identities. Unchanged captures may be shared between closed Runs on that
Profile. A busy or changing source retains the cleanup indication. This display
capture does not verify historical segment bytes or authorize release; repeated
Close and destructive lifecycle operations retain their fresh complete proof.

## Required verification

The native cases cover 10,000 synthetic membership attach/release cycles with
selected journal growth recorded, each maintenance crash boundary, concurrent
readers/writers, active and unknown members, malformed and over-cap input, and
failed membership release after a successful Close. The cleanup case preserves terminal bytes and
receipt/cursor identity across observation and retry. These isolated tests and
the complete repository gate establish native component behavior; they do not
qualify an actual Gul consumer or an account-backed Codex runtime.
