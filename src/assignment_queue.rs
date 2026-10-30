//! Durable assignment ordering shared by internal and external owners.
//!
//! The central scheduler consumes this ordering under retained owner authority.
//! Live entrypoints retain ready-target-only admission pending TASK-083
//! wait/result semantics and TASK-027 integration; target readiness is not an
//! access mode.

use crate::machine::MachineError;
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Owner {
    Managed(Uuid),
    External(Uuid),
}

impl Owner {
    pub(crate) fn scope(self) -> (&'static str, Uuid) {
        match self {
            Self::Managed(id) => ("orchestrated_session", id),
            Self::External(id) => ("external_engagement", id),
        }
    }
}

/// Immutable metadata supplied by the authenticated assignment owner, never by
/// a model-selected scheduling argument. Lower priority values run first.
pub(crate) struct Entry<'a> {
    pub owner: Owner,
    pub task: Uuid,
    pub target: Uuid,
    pub source: &'a str,
    pub priority: u8,
    pub deadline_ms: i64,
}

fn error(message: &str) -> MachineError {
    MachineError::new(
        "SPECIALIST_POLICY_DENIED",
        message,
        false,
        serde_json::json!({}),
    )
}

fn sql(error: rusqlite::Error) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        error.to_string(),
        false,
        serde_json::json!({}),
    )
}

fn event(
    transaction: &Transaction<'_>,
    owner: Owner,
    kind: &str,
    task: Uuid,
) -> Result<(), MachineError> {
    let payload = crate::jcs::sha256_hex(task.as_bytes());
    match owner {
        Owner::External(id) => crate::engagement::append_event(transaction, id, kind, &payload),
        Owner::Managed(id) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| error("queue clock precedes epoch"))?;
            let now = i64::try_from(now.as_millis()).map_err(|_| error("queue clock overflows"))?;
            crate::orchestration::append_event(transaction, id, kind, &payload, now)
        }
    }
}

/// Install the bounded queue extension inside the same transaction that admits
/// its first entry. A rejected admission rolls back the extension as well.
fn install(transaction: &Transaction<'_>) -> Result<(), MachineError> {
    let version: Option<String> = transaction
        .query_row(
            "SELECT value FROM metadata WHERE key='assignment_queue_schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)?;
    match version.as_deref() {
        Some("1") => return Ok(()),
        Some(_) => return Err(error("assignment queue schema is unsupported")),
        None => {}
    }
    transaction.execute_batch(
        "CREATE TABLE assignment_queue_entries(
           sequence INTEGER PRIMARY KEY AUTOINCREMENT,
           task_id TEXT NOT NULL UNIQUE,
           scope_kind TEXT NOT NULL,
           scope_ref TEXT NOT NULL,
           target_run_id TEXT NOT NULL,
           source_ref TEXT NOT NULL,
           priority INTEGER NOT NULL CHECK(priority BETWEEN 0 AND 2),
           deadline_ms INTEGER NOT NULL
         ) STRICT;
         CREATE INDEX assignment_queue_target ON assignment_queue_entries(target_run_id,priority,deadline_ms,sequence);
         CREATE TABLE assignment_member_reuses(
           scope_kind TEXT NOT NULL, scope_ref TEXT NOT NULL, request_key TEXT NOT NULL,
           request_sha256 TEXT NOT NULL, run_id TEXT NOT NULL, operation_id TEXT NOT NULL,
           PRIMARY KEY(scope_kind,scope_ref,request_key)
         ) STRICT;
         DROP INDEX IF EXISTS one_active_external_task_per_member;
         CREATE UNIQUE INDEX one_active_external_task_per_member
           ON tasks(specialist_run_id) WHERE state IN ('accepted','claimed','dispatching','running');
         INSERT INTO metadata(key,value) VALUES('assignment_queue_schema_version','1');"
    ).map_err(sql)
}

/// Record ordering in the task acceptance transaction. The task's existing
/// retained request, caller accounting and immutable receipt remain authoritative.
pub(crate) fn admit(transaction: &Transaction<'_>, entry: &Entry<'_>) -> Result<(), MachineError> {
    if entry.source.is_empty() || entry.source.len() > 256 || entry.priority > 2 {
        return Err(error("invalid trusted assignment scheduling context"));
    }
    install(transaction)?;
    let (kind, scope) = entry.owner.scope();
    let existing: Option<(String,String,String,String,u8,i64)> = transaction.query_row(
        "SELECT scope_kind,scope_ref,target_run_id,source_ref,priority,deadline_ms FROM assignment_queue_entries WHERE task_id=?1",
        [entry.task.to_string()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
    ).optional().map_err(sql)?;
    let expected = (
        kind.to_owned(),
        scope.to_string(),
        entry.target.to_string(),
        entry.source.to_owned(),
        entry.priority,
        entry.deadline_ms,
    );
    if let Some(existing) = existing {
        return if existing == expected {
            Ok(())
        } else {
            Err(error("accepted queue identity changed"))
        };
    }
    let (target, pair, aggregate): (u64, u64, u64) = transaction
        .query_row(
            "SELECT COALESCE(SUM(target_run_id=?3),0),
                COALESCE(SUM(target_run_id=?3 AND source_ref=?4),0),COUNT(*)
           FROM assignment_queue_entries WHERE scope_kind=?1 AND scope_ref=?2",
            params![
                kind,
                scope.to_string(),
                entry.target.to_string(),
                entry.source
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(sql)?;
    if target >= 16 || pair >= 4 || aggregate >= 128 {
        return Err(error("assignment queue limit reached"));
    }
    transaction.execute(
        "INSERT INTO assignment_queue_entries(task_id,scope_kind,scope_ref,target_run_id,source_ref,priority,deadline_ms)
         VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![entry.task.to_string(),kind,scope.to_string(),entry.target.to_string(),entry.source,entry.priority,entry.deadline_ms],
    ).map_err(sql)?;
    crate::scheduler::install(transaction)?;
    crate::scheduler::policy(transaction, entry.owner)?;
    event(transaction, entry.owner, "assignment_queued", entry.task)
}

/// Select without claiming or preempting a Turn. Dispatch must separately hold
/// the Run authority and prove that its accepted task has not crossed dispatch.
pub(crate) fn next(
    transaction: &Transaction<'_>,
    owner: Owner,
    target: Uuid,
) -> Result<Option<Uuid>, MachineError> {
    if !present(transaction)? {
        return Ok(None);
    }
    let (kind, scope) = owner.scope();
    let query = match owner {
        Owner::Managed(_) => "SELECT q.task_id FROM assignment_queue_entries q JOIN brokered_tasks t ON t.task_id=q.task_id
           WHERE q.scope_kind=?1 AND q.scope_ref=?2 AND q.target_run_id=?3 AND t.state='queued'
           ORDER BY q.priority,q.deadline_ms,q.sequence LIMIT 1",
        Owner::External(_) => "SELECT q.task_id FROM assignment_queue_entries q JOIN tasks t ON t.task_id=q.task_id
           WHERE q.scope_kind=?1 AND q.scope_ref=?2 AND q.target_run_id=?3 AND t.state='queued'
           ORDER BY q.priority,q.deadline_ms,q.sequence LIMIT 1",
    };
    let selected: Option<String> = transaction
        .query_row(
            query,
            params![kind, scope.to_string(), target.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)?;
    selected
        .map(|id| Uuid::parse_str(&id).map_err(|_| error("invalid queued task identity")))
        .transpose()
}

/// Reserve only the next queued task when no accepted, running or uncertain
/// task owns the target. This is a pre-dispatch claim, never a Turn replay.
pub(crate) fn claim(
    transaction: &Transaction<'_>,
    owner: Owner,
    target: Uuid,
) -> Result<Option<Uuid>, MachineError> {
    let Some(task) = next(transaction, owner, target)? else {
        return Ok(None);
    };
    let query = match owner {
        Owner::Managed(_) => {
            "UPDATE brokered_tasks SET state='accepted' WHERE task_id=?1 AND state='queued'
          AND NOT EXISTS(SELECT 1 FROM brokered_tasks WHERE target_run_id=?2
              AND state IN ('accepted','claimed','dispatching','running','interrupted_unknown'))"
        }
        Owner::External(_) => {
            "UPDATE tasks SET state='accepted' WHERE task_id=?1 AND state='queued'
          AND NOT EXISTS(SELECT 1 FROM tasks WHERE specialist_run_id=?2
              AND state IN ('accepted','claimed','dispatching','running','interrupted_unknown'))"
        }
    };
    let changed = transaction
        .execute(query, params![task.to_string(), target.to_string()])
        .map_err(sql)?;
    if changed == 1 {
        event(transaction, owner, "assignment_queue_claimed", task)?;
    }
    Ok((changed == 1).then_some(task))
}

/// Cancellation, expiry and a proved dispatch remove only the exact entry.
/// An uncertain dispatched Turn belongs to the existing task recovery owner.
pub(crate) fn remove(
    transaction: &Transaction<'_>,
    owner: Owner,
    task: Uuid,
) -> Result<(), MachineError> {
    if !present(transaction)? {
        return Ok(());
    }
    let (kind, scope) = owner.scope();
    let changed=transaction.execute(
        "DELETE FROM assignment_queue_entries WHERE task_id=?1 AND scope_kind=?2 AND scope_ref=?3",
        params![task.to_string(),kind,scope.to_string()],
    ).map_err(sql)?;
    if changed != 0 {
        event(transaction, owner, "assignment_queue_removed", task)?;
    }
    Ok(())
}

fn present(connection: &Connection) -> Result<bool, MachineError> {
    let value: Option<String> = connection
        .query_row(
            "SELECT value FROM metadata WHERE key='assignment_queue_schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)?;
    match value.as_deref() {
        None => Ok(false),
        Some("1") => Ok(true),
        _ => Err(error("assignment queue schema is unsupported")),
    }
}

/// A queued task must retain its exact owning acceptance record before a
/// cancellation can use the known pre-dispatch path.
pub(crate) fn contains(
    connection: &Connection,
    owner: Owner,
    task: Uuid,
    target: Uuid,
) -> Result<bool, MachineError> {
    if !present(connection)? {
        return Ok(false);
    }
    let (kind, scope) = owner.scope();
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM assignment_queue_entries
         WHERE task_id=?1 AND scope_kind=?2 AND scope_ref=?3 AND target_run_id=?4)",
            params![
                task.to_string(),
                kind,
                scope.to_string(),
                target.to_string()
            ],
            |row| row.get(0),
        )
        .map_err(sql)
}

/// Admission is read-only. Unmarked or incomplete extension objects are never
/// adopted or repaired by a writable opener.
pub(crate) fn validate_schema(connection: &Connection) -> Result<(), MachineError> {
    let version: Option<String> = connection
        .query_row(
            "SELECT value FROM metadata WHERE key='assignment_queue_schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql)?;
    let objects: u64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE
          (type='table' AND name IN ('assignment_queue_entries','assignment_member_reuses'))
          OR (type='index' AND name='assignment_queue_target')",
            [],
            |row| row.get(0),
        )
        .map_err(sql)?;
    if !matches!((version.as_deref(), objects), (None, 0) | (Some("1"), 3)) {
        return Err(MachineError::new(
            "ORCHESTRATION_SCHEMA_UNSUPPORTED",
            "assignment queue schema is not supported",
            false,
            serde_json::json!({"observed":version}),
        ));
    }
    if objects != 0 {
        connection.prepare("SELECT sequence,task_id,scope_kind,scope_ref,target_run_id,source_ref,priority,deadline_ms FROM assignment_queue_entries LIMIT 0")
            .map_err(sql)?;
        connection.prepare("SELECT scope_kind,scope_ref,request_key,request_sha256,run_id,operation_id FROM assignment_member_reuses LIMIT 0")
            .map_err(sql)?;
    }
    Ok(())
}

/// Select only accepted compatible member snapshots. No author files or live
/// Role registry enter selection. Unknown work remains a recovery obligation.
pub(crate) fn reusable_member(
    connection: &Connection,
    owner: Owner,
    role: &str,
    configuration_sha256: &str,
    access: &str,
) -> Result<Option<(Uuid, Uuid)>, MachineError> {
    let query = match owner {
        Owner::Managed(_) => "SELECT m.run_id,m.spawn_operation_id,
             (SELECT COUNT(*) FROM brokered_tasks t WHERE t.target_run_id=m.run_id
              AND t.state IN ('accepted','queued','claimed','dispatching','running','completed_not_delivered')) AS pending
           FROM brokered_members m WHERE m.session_id=?1 AND m.role_ref=?2
             AND m.agent_configuration_sha256=?3 AND m.requested_access=?4
             AND m.membership_state='active'
             AND NOT EXISTS(SELECT 1 FROM brokered_tasks t WHERE t.target_run_id=m.run_id AND t.state='interrupted_unknown')
           ORDER BY pending<>0,pending,m.run_id LIMIT 1",
        Owner::External(_) => "SELECT m.specialist_run_id,m.hire_operation_id,
             (SELECT COUNT(*) FROM tasks t WHERE t.specialist_run_id=m.specialist_run_id
              AND t.state IN ('accepted','queued','claimed','dispatching','running','completed_not_delivered')) AS pending
           FROM members m WHERE m.engagement_id=?1 AND m.role_ref=?2
             AND m.configuration_sha256=?3 AND m.requested_access=?4 AND m.state='active'
             AND NOT EXISTS(SELECT 1 FROM tasks t WHERE t.specialist_run_id=m.specialist_run_id AND t.state='interrupted_unknown')
           ORDER BY pending<>0,pending,m.specialist_run_id LIMIT 1",
    };
    let selected: Option<(String, String)> = connection
        .query_row(
            query,
            params![
                owner.scope().1.to_string(),
                role,
                configuration_sha256,
                access
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sql)?;
    selected
        .map(|(run, operation)| {
            Ok((
                Uuid::parse_str(&run).map_err(|_| error("invalid reusable member identity"))?,
                Uuid::parse_str(&operation)
                    .map_err(|_| error("invalid reusable operation identity"))?,
            ))
        })
        .transpose()
}

/// Bind a compatible selection to its owner's normalized request and key. A
/// retry returns the original member even when later queue counts change.
pub(crate) fn reuse(
    transaction: &Transaction<'_>,
    owner: Owner,
    key: &str,
    request_sha256: &str,
    role: &str,
    configuration_sha256: &str,
    access: &str,
) -> Result<Option<(Uuid, Uuid)>, MachineError> {
    install(transaction)?;
    let (kind, scope) = owner.scope();
    let previous: Option<(String, String, String)> = transaction
        .query_row(
            "SELECT request_sha256,run_id,operation_id FROM assignment_member_reuses
         WHERE scope_kind=?1 AND scope_ref=?2 AND request_key=?3",
            params![kind, scope.to_string(), key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(sql)?;
    if let Some((digest, run, operation)) = previous {
        if digest != request_sha256 {
            return Err(MachineError::new(
                "IDEMPOTENCY_CONFLICT",
                "reuse request differs",
                false,
                serde_json::json!({}),
            ));
        }
        return Ok(Some((
            Uuid::parse_str(&run).map_err(|_| error("invalid reuse receipt"))?,
            Uuid::parse_str(&operation).map_err(|_| error("invalid reuse receipt"))?,
        )));
    }
    let Some((run, operation)) =
        reusable_member(transaction, owner, role, configuration_sha256, access)?
    else {
        return Ok(None);
    };
    transaction.execute(
        "INSERT INTO assignment_member_reuses(scope_kind,scope_ref,request_key,request_sha256,run_id,operation_id)
         VALUES(?1,?2,?3,?4,?5,?6)",
        params![kind,scope.to_string(),key,request_sha256,run.to_string(),operation.to_string()],
    ).map_err(sql)?;
    event(transaction, owner, "assignment_member_reused", run)?;
    Ok(Some((run, operation)))
}
