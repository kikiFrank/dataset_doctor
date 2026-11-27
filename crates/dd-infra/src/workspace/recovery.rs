use rusqlite::Connection;

/// Called only after acquiring the exclusive Workspace writer lock. Live
/// supervised executors retain that ownership until their process tree stops.
pub(super) fn settle_abandoned_executions(connection: &mut Connection) -> rusqlite::Result<()> {
    let tx = connection.transaction()?;
    tx.execute_batch(
        "UPDATE workflow_runs SET
            status=CASE WHEN status='RUNNING' THEN 'INTERRUPTED' ELSE status END,
            error_code=CASE WHEN status='WAITING_REVIEW' THEN 'REVIEW_RECHECK_INTERRUPTED' ELSE 'SESSION_ENDED' END,
            updated_at_ms=CAST(unixepoch('subsec') * 1000 AS INTEGER),row_revision=row_revision+1
         WHERE (status='RUNNING' OR status='WAITING_REVIEW') AND EXISTS (
            SELECT 1 FROM tasks WHERE tasks.workflow_run_id=workflow_runs.workflow_run_id
                AND tasks.status IN ('DISPATCHING','RUNNING','CANCELLING')
                AND json_extract(tasks.execution_metadata_json,'$.executor_session_id') IS NOT NULL
                AND (workflow_runs.status='RUNNING' OR tasks.node_id GLOB 'final_annotation/recheck/*'));
         UPDATE tasks SET status='INTERRUPTED',error_code='SESSION_ENDED',
            finished_at_ms=CAST(unixepoch('subsec') * 1000 AS INTEGER),
            updated_at_ms=CAST(unixepoch('subsec') * 1000 AS INTEGER),row_revision=row_revision+1,
            execution_metadata_json=json_set(execution_metadata_json,'$.grant_state','REVOKED')
         WHERE status IN ('DISPATCHING','RUNNING','CANCELLING')
            AND json_extract(execution_metadata_json,'$.executor_session_id') IS NOT NULL;",
    )?;
    tx.commit()
}
