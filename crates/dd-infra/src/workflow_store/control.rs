//! Persist a Workflow pause request at a dispatch boundary.

use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use super::{WorkflowError, WorkflowStatus, WorkflowStore, database};
use crate::sqlite::row_uuid;

#[derive(Debug, Eq, PartialEq)]
pub struct WorkflowPauseReceipt {
    pub observed_status: WorkflowStatus,
    pub revision: u64,
    pub blocking_task_ids: Vec<Uuid>,
}

impl WorkflowStore {
    pub fn pause(
        &self,
        session: Uuid,
        run: Uuid,
        expected_revision: u64,
        now_ms: i64,
    ) -> Result<WorkflowPauseReceipt, WorkflowError> {
        let expected =
            i64::try_from(expected_revision).map_err(|_| WorkflowError::RevisionConflict)?;
        self.owner
            .workspace_action(session, move |owner| {
                let transaction = owner.connection.transaction().map_err(database)?;
                let run_id = run.to_string();
                let current = transaction
                    .query_row(
                        "SELECT status, row_revision, workspace_session_id
                         FROM workflow_runs WHERE workflow_run_id=?1",
                        [&run_id],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row_uuid(row, 2)?)),
                    )
                    .optional()
                    .map_err(database)?;
                let Some((status, revision, bound_session)) = current else {
                    return Ok(Err(WorkflowError::UnknownRun));
                };
                if revision != expected || bound_session != session {
                    return Ok(Err(WorkflowError::RevisionConflict));
                }
                if status != "RUNNING" {
                    return Ok(Err(WorkflowError::ControlUnavailable));
                }
                let blocking_task_ids = active_calculations(&transaction, &run_id).map_err(database)?;
                let (status, observed_status) = if blocking_task_ids.is_empty() {
                    ("PAUSED", WorkflowStatus::Paused)
                } else {
                    ("RUNNING", WorkflowStatus::Running)
                };
                let updated = transaction.execute(
                    "UPDATE workflow_runs SET status=?3,
                        node_states_json=json_set(node_states_json,'$.control.pause_requested',json('true')),
                        updated_at_ms=?4,row_revision=row_revision+1
                     WHERE workflow_run_id=?1 AND row_revision=?2 AND status='RUNNING'",
                    params![run_id, expected, status, now_ms],
                ).map_err(database)?;
                if updated != 1 {
                    return Ok(Err(WorkflowError::RevisionConflict));
                }
                transaction.commit().map_err(database)?;
                Ok(Ok(WorkflowPauseReceipt {
                    observed_status,
                    revision: expected_revision + 1,
                    blocking_task_ids,
                }))
            })
            .map_err(WorkflowError::Storage)?
    }

    pub(super) fn settle_pause(&self, session: Uuid, run: Uuid) -> Result<(), WorkflowError> {
        self.owner
            .workspace_action(session, move |owner| {
                let transaction = owner.connection.transaction().map_err(database)?;
                let workflow = run.to_string();
                if active_calculations(&transaction, &workflow)
                    .map_err(database)?
                    .is_empty()
                {
                    transaction
                        .execute(
                            "UPDATE workflow_runs SET status='PAUSED',
                            updated_at_ms=CAST(unixepoch('subsec') * 1000 AS INTEGER),
                            row_revision=row_revision+1
                         WHERE workflow_run_id=?1 AND workspace_session_id=?2 AND status='RUNNING'
                           AND json_extract(node_states_json,'$.control.pause_requested')=1",
                            params![workflow, session.to_string()],
                        )
                        .map_err(database)?;
                }
                transaction.commit().map_err(database)
            })
            .map_err(WorkflowError::Storage)
    }
}

fn active_calculations(
    connection: &rusqlite::Connection,
    run: &str,
) -> rusqlite::Result<Vec<Uuid>> {
    let mut statement = connection.prepare(
        "SELECT task_id FROM tasks WHERE workflow_run_id=?1
           AND status IN ('DISPATCHING','RUNNING','CANCELLING') ORDER BY task_id",
    )?;
    statement
        .query_map([run], |row| row_uuid(row, 0))?
        .collect()
}

pub(super) fn pause_requested(status: &str, nodes: &str) -> Result<bool, WorkflowError> {
    match status {
        "PAUSED" => Ok(true),
        "RUNNING" => {
            let nodes: serde_json::Value =
                serde_json::from_str(nodes).map_err(WorkflowError::Json)?;
            Ok(nodes["control"]["pause_requested"].as_bool() == Some(true))
        }
        _ => Ok(false),
    }
}
