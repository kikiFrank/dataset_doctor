//! Page the durable history of one Dataset in creation order.

use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use super::{OPERATION, WorkflowError, WorkflowSnapshot, WorkflowStore, database};
use crate::sqlite::row_uuid;
use crate::workspace_thread::WorkspaceThreadError;

const MAX_PAGE_SIZE: u16 = 200;

pub struct WorkflowPage {
    pub items: Vec<WorkflowSnapshot>,
    pub next_cursor: Option<Uuid>,
}

impl WorkflowStore {
    pub fn list(
        &self,
        session: Uuid,
        dataset: Uuid,
        cursor: Option<Uuid>,
        page_size: u16,
    ) -> Result<WorkflowPage, WorkflowError> {
        if !(1..=MAX_PAGE_SIZE).contains(&page_size) {
            return Err(WorkflowError::InvalidInputScope);
        }
        let ids = self.owner.workspace_action(session, move |owner| {
            let dataset_id = dataset.to_string();
            owner.dataset_detail(dataset).map_err(WorkspaceThreadError::Dataset)?;
            let after = if let Some(cursor) = cursor {
                let created = owner.connection.query_row(
                    "SELECT created_at_ms FROM workflow_runs
                     WHERE workflow_run_id=?1 AND dataset_id=?2 AND operation_kind=?3 AND plan_kind='template_instance'",
                    params![cursor.to_string(),&dataset_id,OPERATION], |row| row.get::<_, i64>(0),
                ).optional().map_err(database)?;
                let Some(created) = created else { return Ok(Err(WorkflowError::InvalidInputScope)); };
                Some(created)
            } else {
                None
            };
            let mut statement = owner.connection.prepare(
                "SELECT workflow_run_id FROM workflow_runs
                 WHERE dataset_id=?1 AND operation_kind=?2 AND plan_kind='template_instance'
                   AND (?3 IS NULL OR created_at_ms<?3 OR (created_at_ms=?3 AND workflow_run_id<?4))
                 ORDER BY created_at_ms DESC,workflow_run_id DESC LIMIT ?5",
            ).map_err(database)?;
            let ids = statement.query_map(
                params![dataset_id,OPERATION,after,cursor.map(|id| id.to_string()),i64::from(page_size)+1],
                |row| row_uuid(row, 0),
            ).map_err(database)?.collect::<rusqlite::Result<Vec<_>>>().map_err(database)?;
            Ok(Ok(ids))
        }).map_err(WorkflowError::Storage)??;
        let size = usize::from(page_size);
        let next_cursor = if ids.len() > size {
            Some(ids[size - 1])
        } else {
            None
        };
        let items = ids
            .into_iter()
            .take(size)
            .map(|id| self.get(session, id))
            .collect::<Result<_, _>>()?;
        Ok(WorkflowPage { items, next_cursor })
    }
}
