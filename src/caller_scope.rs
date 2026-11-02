//! Provenance and accounting for the caller's independent assignment budget.
//!
//! The caller scope is trusted transport provenance, never model input: only a
//! sealed [`TrustedCallerPort`] can construct one, and the closed port refuses
//! rather than falling back to Controller IDs or executor session references
//! (MDC:139). Accounting lives in the generation-6
//! `caller_scope_assignments` table, one row per accepted assignment, and its
//! state vocabulary is exactly `counted_unresolved`, `counted_accepted` and
//! `proved_nondispatch`.

use crate::delegation_identity::CallerScope;
use crate::machine::MachineError;
use rusqlite::{Connection, OptionalExtension as _, params};
use uuid::Uuid;

/// One caller scope authenticated by the protected host producer.
pub struct TrustedCallerScope {
    accepted: CallerScope,
}

impl TrustedCallerScope {
    #[must_use]
    pub fn accepted(&self) -> &CallerScope {
        &self.accepted
    }
}

pub(crate) mod sealed {
    pub trait Sealed {}
}

/// The protected host producer's reader of one caller scope.
///
/// TASK-077 supplies the production implementation; a missing provenance never
/// falls back to any other identity.
pub trait TrustedCallerPort: sealed::Sealed {
    fn load(&self) -> Result<([String; 5], u64), MachineError>;
}

/// Only the protected native producer can supply this keyed provenance.
pub(crate) trait TrustedInvocationPort: sealed::Sealed {
    fn invocation(
        &self,
    ) -> Result<
        (
            crate::delegation_identity::TrustedInvocationContext,
            crate::delegation_identity::LookupNamespace,
        ),
        MachineError,
    >;
}

pub struct TrustedInvocation {
    context: crate::delegation_identity::TrustedInvocationContext,
    namespace: crate::delegation_identity::LookupNamespace,
}

impl TrustedInvocation {
    pub(crate) fn from_port(port: &impl TrustedInvocationPort) -> Result<Self, MachineError> {
        let (context, namespace) = port.invocation()?;
        context.validate()?;
        Ok(Self { context, namespace })
    }

    pub fn context(&self) -> &crate::delegation_identity::TrustedInvocationContext {
        &self.context
    }

    pub(crate) fn require_namespace(
        &self,
        namespace: &crate::delegation_identity::LookupNamespace,
    ) -> Result<(), MachineError> {
        self.context.validate()?;
        if &self.namespace != namespace {
            return Err(integrity());
        }
        Ok(())
    }
}

/// The closed port: it always refuses, so format validation alone can never
/// create provenance.
struct ClosedTrustedCallerPort;

impl sealed::Sealed for ClosedTrustedCallerPort {}

impl TrustedCallerPort for ClosedTrustedCallerPort {
    fn load(&self) -> Result<([String; 5], u64), MachineError> {
        Err(MachineError::invalid_argument(
            "caller_scope",
            "invalid caller scope",
        ))
    }
}

impl TrustedCallerScope {
    pub fn from_port(port: &impl TrustedCallerPort) -> Result<Self, MachineError> {
        let (
            [
                binding_ref,
                podway_workspace_ref,
                podway_session_ref,
                podway_attempt_ref,
                delegated_placement_ref,
            ],
            max_rounds,
        ) = port.load()?;
        let accepted = CallerScope {
            binding_ref,
            podway_workspace_ref,
            podway_session_ref,
            podway_attempt_ref,
            delegated_placement_ref,
            max_rounds,
        };
        accepted.validate()?;
        Ok(Self { accepted })
    }

    /// Require the authenticated scope of one assignment admission.
    pub fn require(scope: Option<&Self>) -> Result<&Self, MachineError> {
        match scope {
            Some(scope) => Ok(scope),
            None => {
                // Exercise the same sealed constructor as an eventual host;
                // format validation alone cannot create provenance.
                Self::from_port(&ClosedTrustedCallerPort)?;
                Err(integrity())
            }
        }
    }
}

/// The five caller references and the aggregate scope of one assignment.
pub(crate) struct CallerAssignment<'a> {
    /// The accepted `task_ref` of this assignment.
    pub assignment_id: Uuid,
    pub workspace_ref: &'a str,
    pub aggregate_scope_kind: &'a str,
    pub aggregate_scope_ref: &'a str,
    pub member_ref: Uuid,
}

fn integrity() -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "caller assignment accounting is inconsistent",
        false,
        serde_json::json!({"invariant":"caller_scope_accounting"}),
    )
}

fn rounds_exhausted() -> MachineError {
    MachineError::new(
        "ROUNDS_EXHAUSTED",
        "the caller scope assignment budget is exhausted",
        false,
        serde_json::json!({"required_action":"fix_request","effect":"none"}),
    )
}

fn transaction(connection: &Connection) -> Result<(), MachineError> {
    if connection.is_autocommit() {
        Err(integrity())
    } else {
        Ok(())
    }
}

/// `rounds_used = COUNT(counted_unresolved) + COUNT(counted_accepted)` over the
/// whole five-reference caller scope.
pub(crate) fn rounds_used(
    connection: &Connection,
    scope: &TrustedCallerScope,
) -> Result<u64, MachineError> {
    let used: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM caller_scope_assignments
              WHERE binding_ref=?1 AND podway_workspace_ref=?2 AND podway_session_ref=?3
                AND podway_attempt_ref=?4 AND delegated_placement_ref=?5
                AND accounting_state IN ('counted_unresolved','counted_accepted')",
            params![
                scope.accepted.binding_ref,
                scope.accepted.podway_workspace_ref,
                scope.accepted.podway_session_ref,
                scope.accepted.podway_attempt_ref,
                scope.accepted.delegated_placement_ref
            ],
            |row| row.get(0),
        )
        .map_err(|_| integrity())?;
    u64::try_from(used).map_err(|_| integrity())
}

/// Verify the assignment belongs to the trusted caller scope of this call.
pub(crate) fn verify_replay(
    connection: &Connection,
    task: Uuid,
    scope: &TrustedCallerScope,
) -> Result<(), MachineError> {
    let retained: Option<(String, String, String, String, String, u64)> = connection
        .query_row(
            "SELECT binding_ref,podway_workspace_ref,podway_session_ref,podway_attempt_ref,
                    delegated_placement_ref,max_rounds
               FROM caller_scope_assignments WHERE assignment_id=?1",
            [task.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()
        .map_err(|_| integrity())?;
    let Some((
        binding_ref,
        podway_workspace_ref,
        podway_session_ref,
        podway_attempt_ref,
        delegated_placement_ref,
        max_rounds,
    )) = retained
    else {
        return Err(integrity());
    };
    let recorded = CallerScope {
        binding_ref,
        podway_workspace_ref,
        podway_session_ref,
        podway_attempt_ref,
        delegated_placement_ref,
        max_rounds,
    };
    recorded.validate().map_err(|_| integrity())?;
    if recorded != scope.accepted {
        return Err(MachineError::new(
            "SPECIALIST_POLICY_DENIED",
            "assignment belongs to a different retained caller scope",
            false,
            serde_json::json!({"phase":"admission","outcome":"not_dispatched"}),
        ));
    }
    Ok(())
}

/// Record one accepted assignment as `counted_unresolved` in the caller's
/// transaction. The budget is immutable per scope and exhausted budgets refuse
/// before any acceptance row exists.
pub(crate) fn admit(
    connection: &Connection,
    assignment: &CallerAssignment<'_>,
    scope: &TrustedCallerScope,
    now_ms: i64,
) -> Result<(), MachineError> {
    transaction(connection)?;
    if assignment.assignment_id.get_version_num() != 7
        || assignment.assignment_id.get_variant() != uuid::Variant::RFC4122
    {
        return Err(integrity());
    }
    let s = &scope.accepted;
    // The budget is immutable over every retained row of this caller scope,
    // including assignments that were proved non-dispatched.
    let (minimum, maximum): (Option<u64>, Option<u64>) = connection
        .query_row(
            "SELECT MIN(max_rounds),MAX(max_rounds)
               FROM caller_scope_assignments
              WHERE binding_ref=?1 AND podway_workspace_ref=?2 AND podway_session_ref=?3
                AND podway_attempt_ref=?4 AND delegated_placement_ref=?5",
            params![
                s.binding_ref,
                s.podway_workspace_ref,
                s.podway_session_ref,
                s.podway_attempt_ref,
                s.delegated_placement_ref
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| integrity())?;
    if minimum != maximum {
        return Err(integrity());
    }
    if minimum.is_some_and(|max| max != s.max_rounds) {
        return Err(MachineError::new(
            "SPECIALIST_POLICY_DENIED",
            "the caller scope budget is immutable",
            false,
            serde_json::json!({"phase":"admission","outcome":"not_dispatched"}),
        ));
    }
    if rounds_used(connection, scope)? >= s.max_rounds {
        return Err(rounds_exhausted());
    }
    connection
        .execute(
            "INSERT INTO caller_scope_assignments(
               assignment_id,binding_ref,podway_workspace_ref,podway_session_ref,
               podway_attempt_ref,delegated_placement_ref,workspace_ref,
               aggregate_scope_kind,aggregate_scope_ref,member_ref,max_rounds,
               accounting_state,created_at_ms,updated_at_ms
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'counted_unresolved',?12,?12)",
            params![
                assignment.assignment_id.to_string(),
                s.binding_ref,
                s.podway_workspace_ref,
                s.podway_session_ref,
                s.podway_attempt_ref,
                s.delegated_placement_ref,
                assignment.workspace_ref,
                assignment.aggregate_scope_kind,
                assignment.aggregate_scope_ref,
                assignment.member_ref.to_string(),
                s.max_rounds,
                now_ms
            ],
        )
        .map_err(|_| integrity())?;
    Ok(())
}

/// Release the attempt's own accounting row when one already exists.
///
/// Case A refuses before acceptance, so the row is normally absent; a row that
/// does exist for this attempt is proven non-dispatched in the same
/// transaction.
pub(crate) fn release_attempt(connection: &Connection, task: Uuid) -> Result<(), MachineError> {
    transaction(connection)?;
    connection
        .execute(
            "UPDATE caller_scope_assignments SET accounting_state='proved_nondispatch'
              WHERE assignment_id=?1 AND accounting_state='counted_unresolved'",
            [task.to_string()],
        )
        .map_err(|_| integrity())?;
    Ok(())
}

/// The executor accepted this assignment: final for counting.
// The reporting caller is the settlement port, which the successor adapters
// construct at run time; the unit tests already exercise this transition.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn accepted(connection: &Connection, task: Uuid) -> Result<(), MachineError> {
    transition(connection, task, "counted_accepted")
}

/// The dispatcher definitively did not dispatch this assignment.
pub(crate) fn proved_nondispatch(connection: &Connection, task: Uuid) -> Result<(), MachineError> {
    transition(connection, task, "proved_nondispatch")
}

fn transition(connection: &Connection, task: Uuid, state: &str) -> Result<(), MachineError> {
    transaction(connection)?;
    let changed = connection
        .execute(
            "UPDATE caller_scope_assignments SET accounting_state=?2
              WHERE assignment_id=?1 AND accounting_state IN ('counted_unresolved',?2)",
            params![task.to_string(), state],
        )
        .map_err(|_| integrity())?;
    if changed == 0
        && crate::delegation_service::research_group::verify_accounted_task(connection, task)?
    {
        // Standalone Controller groups have their own immutable accepted origin,
        // not a Podway binding or a fictitious 1..32-round Action. The owning
        // task record supplies current execution/disposition truth.
        return Ok(());
    }
    if changed != 1 {
        return Err(integrity());
    }
    Ok(())
}

/// A modern admission cannot overtake uncertain work on the same member.
///
/// The owner's own accepted-task store answers the question: external members
/// retain theirs in `tasks`, managed members in `brokered_tasks`. This is the
/// quiescence helper, reserved for successor disposition and S release; the
/// single-assign wall is the task store's own unique index.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn require_member_settled(
    connection: &Connection,
    owner: &str,
    scope: &str,
    member: Uuid,
) -> Result<(), MachineError> {
    transaction(connection)?;
    let unknown: bool = match owner {
        "external" => connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks
              WHERE engagement_id=?1 AND specialist_run_id=?2
                AND state='interrupted_unknown')",
            params![scope, member.to_string()],
            |row| row.get(0),
        ),
        "managed" => connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM brokered_tasks
              WHERE session_id=?1 AND target_run_id=?2 AND state='interrupted_unknown')",
            params![scope, member.to_string()],
            |row| row.get(0),
        ),
        _ => return Err(integrity()),
    }
    .map_err(|_| integrity())?;
    let unresolved: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM caller_scope_assignments
              WHERE aggregate_scope_kind=?1 AND aggregate_scope_ref=?2 AND member_ref=?3
                AND accounting_state='counted_unresolved')",
            params![owner_scope_kind(owner)?, scope, member.to_string()],
            |row| row.get(0),
        )
        .map_err(|_| integrity())?;
    if unknown || unresolved {
        return Err(MachineError::new(
            "INTERRUPTED_UNKNOWN",
            "member has unresolved accepted work",
            false,
            serde_json::json!({"run_id":member,"required_action":"reconcile_run"}),
        ));
    }
    Ok(())
}

fn owner_scope_kind(owner: &str) -> Result<&'static str, MachineError> {
    match owner {
        "external" => Ok("external_engagement"),
        "managed" => Ok("orchestrated_session"),
        _ => Err(integrity()),
    }
}

/// Fixtures supply explicit literals, never authenticated host references.
#[cfg(test)]
pub(crate) fn fixture(max_rounds: u64) -> TrustedCallerScope {
    struct Fixture(u64);
    impl sealed::Sealed for Fixture {}
    impl TrustedCallerPort for Fixture {
        fn load(&self) -> Result<([String; 5], u64), MachineError> {
            Ok((
                [
                    "fixture:binding",
                    "fixture:workspace",
                    "fixture:session",
                    "fixture:attempt",
                    "fixture:placement",
                ]
                .map(str::to_owned),
                self.0,
            ))
        }
    }
    TrustedCallerScope::from_port(&Fixture(max_rounds)).unwrap()
}

/// A second valid fixture scope, for cases that relabel another scope's task.
#[cfg(test)]
pub(crate) fn fixture_alt(max_rounds: u64) -> TrustedCallerScope {
    struct Fixture(u64);
    impl sealed::Sealed for Fixture {}
    impl TrustedCallerPort for Fixture {
        fn load(&self) -> Result<([String; 5], u64), MachineError> {
            Ok((
                [
                    "fixture:binding-alt",
                    "fixture:workspace-alt",
                    "fixture:session-alt",
                    "fixture:attempt-alt",
                    "fixture:placement-alt",
                ]
                .map(str::to_owned),
                self.0,
            ))
        }
    }
    TrustedCallerScope::from_port(&Fixture(max_rounds)).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn store() -> (crate::engagement::EngagementStore, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("dolgorae-rounds-{}", Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let store =
            crate::engagement::EngagementStore::open(&root.join("orchestration.sqlite3")).unwrap();
        (store, root)
    }

    fn assignment(task: Uuid, member: Uuid) -> CallerAssignment<'static> {
        CallerAssignment {
            assignment_id: task,
            workspace_ref: "workspace",
            aggregate_scope_kind: "external_engagement",
            aggregate_scope_ref: "engagement",
            member_ref: member,
        }
    }

    #[test]
    fn nondispatch_releases_only_its_own_round_and_the_budget_is_immutable() {
        let (mut store, root) = store();
        assert_eq!(
            TrustedCallerScope::require(None).err().unwrap().code,
            "INVALID_ARGUMENT"
        );
        let scope = fixture(1);
        let first = Uuid::now_v7();
        store
            .transact(|transaction| {
                admit(transaction, &assignment(first, Uuid::now_v7()), &scope, 1)
            })
            .unwrap();
        assert_eq!(rounds_used(store.connection(), &scope).unwrap(), 1);
        // A proved non-dispatch releases its own round for a later admission.
        store
            .transact(|transaction| proved_nondispatch(transaction, first))
            .unwrap();
        assert_eq!(rounds_used(store.connection(), &scope).unwrap(), 0);
        let second = Uuid::now_v7();
        store
            .transact(|transaction| {
                admit(transaction, &assignment(second, Uuid::now_v7()), &scope, 2)
            })
            .unwrap();
        store
            .transact(|transaction| accepted(transaction, second))
            .unwrap();
        // An accepted assignment is final for counting.
        assert_eq!(
            store
                .transact(|transaction| proved_nondispatch(transaction, second))
                .unwrap_err()
                .code,
            "INTERNAL_ERROR"
        );
        assert_eq!(
            store
                .transact(|transaction| admit(
                    transaction,
                    &assignment(Uuid::now_v7(), Uuid::now_v7()),
                    &scope,
                    3
                ))
                .unwrap_err()
                .code,
            "ROUNDS_EXHAUSTED"
        );
        // The budget is fixed for one caller scope.
        assert_eq!(
            store
                .transact(|transaction| admit(
                    transaction,
                    &assignment(Uuid::now_v7(), Uuid::now_v7()),
                    &fixture(2),
                    4
                ))
                .unwrap_err()
                .code,
            "SPECIALIST_POLICY_DENIED"
        );
        // A retained assignment belongs to exactly one caller scope.
        assert_eq!(
            verify_replay(store.connection(), second, &fixture(2))
                .unwrap_err()
                .code,
            "SPECIALIST_POLICY_DENIED"
        );
        verify_replay(store.connection(), second, &scope).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rounds_count_across_every_aggregate_under_one_caller_binding() {
        let (mut store, root) = store();
        let scope = fixture(4);
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        for (task, member) in [(first, Uuid::now_v7()), (second, Uuid::now_v7())] {
            store
                .transact(|transaction| admit(transaction, &assignment(task, member), &scope, 1))
                .unwrap();
        }
        store
            .transact(|transaction| accepted(transaction, first))
            .unwrap();
        // Both counted states participate in the total.
        assert_eq!(rounds_used(store.connection(), &scope).unwrap(), 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_unsettled_member_is_refused_before_a_fresh_admission() {
        let (mut store, root) = store();
        let scope = fixture(2);
        let member = Uuid::now_v7();
        store
            .transact(|transaction| {
                require_member_settled(transaction, "external", "engagement", member)
            })
            .unwrap();
        // An unresolved assignment on this member keeps it unsettled.
        let task = Uuid::now_v7();
        store
            .transact(|transaction| admit(transaction, &assignment(task, member), &scope, 1))
            .unwrap();
        assert_eq!(
            store
                .transact(|transaction| {
                    require_member_settled(transaction, "external", "engagement", member)
                })
                .unwrap_err()
                .code,
            "INTERRUPTED_UNKNOWN"
        );
        assert_eq!(
            store
                .transact(|transaction| {
                    require_member_settled(transaction, "managed", "engagement", member)
                })
                .unwrap_err()
                .code,
            "INTERNAL_ERROR"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
