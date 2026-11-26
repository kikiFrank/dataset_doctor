use dd_core::execution_grant::ClaimReceipt;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

use crate::artifact_manifest::{MANIFEST_FILE, VerifiedTaskManifest};
use crate::workspace::OpenedWorkspace;

use super::{TaskStoreError, format_digest};

#[derive(Clone, Debug)]
pub struct ResultSubmission {
    pub workspace_session: Uuid,
    pub grant: Uuid,
    pub execution: ClaimReceipt,
    pub node_spec_fingerprint: [u8; 32],
    pub execution_event_id: Uuid,
    pub source_event_seq: u64,
    pub verified: VerifiedTaskManifest,
}

pub(crate) struct ReportScope {
    pub workspace: Uuid,
    pub workspace_session: Uuid,
    pub grant: Uuid,
    pub execution: ClaimReceipt,
    pub input_fingerprint: [u8; 32],
    pub node_spec_fingerprint: [u8; 32],
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TerminalReceipt {
    pub accepted_at: String,
    pub completion_fingerprint: String,
    pub result_manifest_ref: Value,
}

impl OpenedWorkspace {
    pub(crate) fn report_directory(&self, scope: &ReportScope) -> Result<PathBuf, TaskStoreError> {
        if self.identity().workspace_id != scope.workspace.to_string() {
            return Err(TaskStoreError::AttemptConflict);
        }
        let execution = scope.execution;
        let current: bool = self.connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM tasks WHERE task_id=?1 AND workspace_session_id=?2
                AND current_attempt_id=?3 AND input_fingerprint=?4
                AND json_extract(execution_metadata_json,'$.grant_id')=?5
                AND json_extract(execution_metadata_json,'$.grant_state')='CLAIMED'
                AND json_extract(execution_metadata_json,'$.claimed_execution_id')=?6
                AND json_extract(execution_metadata_json,'$.node_spec_fingerprint')=?7
                AND status IN ('DISPATCHING','RUNNING','CANCELLING','SUCCEEDED'))",
            params![
                execution.task.to_string(),
                scope.workspace_session.to_string(),
                execution.attempt.to_string(),
                format_digest(&scope.input_fingerprint),
                scope.grant.to_string(),
                execution.execution.to_string(),
                format_digest(&scope.node_spec_fingerprint)
            ],
            |row| row.get(0),
        )?;
        if !current {
            return Err(TaskStoreError::AttemptConflict);
        }
        Ok(self.root().join(format!(
            "artifacts/tasks/{}/{}",
            execution.task, execution.attempt
        )))
    }

    /// Accept only a verified artifact from the current execution. Identical
    /// completion events replay the original durable receipt.
    pub fn record_success(
        &mut self,
        result: &ResultSubmission,
        now_ms: i64,
    ) -> Result<TerminalReceipt, TaskStoreError> {
        let scope = result.verified.scope;
        let execution = result.execution;
        let relative_manifest = format!(
            "artifacts/tasks/{}/{}/{MANIFEST_FILE}",
            execution.task, execution.attempt
        );
        if result.source_event_seq == 0
            || scope.task != execution.task
            || scope.attempt != execution.attempt
            || result.verified.path != self.root().join(&relative_manifest)
        {
            return Err(TaskStoreError::AttemptConflict);
        }
        let reference = result.verified.reference();
        let manifest_ref = json!({
            "artifact_id": reference.artifact_id,
            "owner": {"kind": "TASK", "id": execution.task.to_string()},
            "kind": reference.kind,
            "media_type": reference.media_type,
            "size_bytes": reference.size_bytes.to_string(),
            "sha256": reference.sha256,
            "schema_id": reference.schema_id,
        });
        let mut digest = Sha256::new();
        digest.update(b"datasetdoctor.task.succeeded.v1\0");
        digest.update(manifest_ref.to_string());
        let fingerprint = format!("{:x}", digest.finalize());
        let event_id = result.execution_event_id.to_string();
        let sequence = result.source_event_seq.to_string();
        let task_id = execution.task.to_string();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = transaction.query_row(
            "SELECT status, json_extract(execution_metadata_json,'$.last_execution_event_id'),
                            json_extract(execution_metadata_json,'$.terminal_receipt'),
                            json_extract(execution_metadata_json,'$.last_source_event_seq')
               FROM tasks
              WHERE task_id=?1 AND workspace_session_id=?2 AND current_attempt_id=?3 AND input_fingerprint=?4
                AND json_extract(execution_metadata_json,'$.grant_id')=?5
                AND json_extract(execution_metadata_json,'$.grant_state')='CLAIMED'
                AND json_extract(execution_metadata_json,'$.claimed_execution_id')=?6
                AND json_extract(execution_metadata_json,'$.node_spec_fingerprint')=?7",
            params![task_id, result.workspace_session.to_string(), execution.attempt.to_string(),
                format_digest(&scope.input_fingerprint), result.grant.to_string(), execution.execution.to_string(),
                format_digest(&result.node_spec_fingerprint)],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, Option<String>>(3)?)),
        ).optional()?.ok_or(TaskStoreError::AttemptConflict)?;

        let receipt = match row {
            (status, Some(original_event), Some(saved), Some(last_sequence))
                if status == "SUCCEEDED"
                    && original_event == event_id
                    && last_sequence == sequence =>
            {
                let receipt: TerminalReceipt =
                    serde_json::from_str(&saved).map_err(TaskStoreError::Receipt)?;
                if receipt.completion_fingerprint != fingerprint {
                    return Err(TaskStoreError::AttemptConflict);
                }
                receipt
            }
            (status, _, None, last_sequence)
                if matches!(status.as_str(), "RUNNING" | "CANCELLING") =>
            {
                if last_sequence.is_some_and(|previous| {
                    !previous
                        .parse::<u64>()
                        .is_ok_and(|previous| result.source_event_seq > previous)
                }) {
                    return Err(TaskStoreError::AttemptConflict);
                }
                let accepted_at =
                    OffsetDateTime::from_unix_timestamp_nanos(i128::from(now_ms) * 1_000_000)
                        .map_err(|_| TaskStoreError::InvalidTimestamp)?
                        .format(&Rfc3339)
                        .map_err(|_| TaskStoreError::InvalidTimestamp)?;
                let receipt = TerminalReceipt {
                    accepted_at,
                    completion_fingerprint: fingerprint,
                    result_manifest_ref: manifest_ref,
                };
                let encoded = serde_json::to_string(&receipt).map_err(TaskStoreError::Receipt)?;
                transaction.execute(
                    "UPDATE tasks SET status='SUCCEEDED', output_manifest_path=?2, progress=1,
                            finished_at_ms=?3, updated_at_ms=?3, row_revision=row_revision+1,
                            execution_metadata_json=json_set(execution_metadata_json,
                                '$.last_execution_event_id', ?4, '$.terminal_receipt', json(?5), '$.last_source_event_seq', ?6)
                      WHERE task_id=?1",
                    params![task_id, relative_manifest, now_ms, event_id, encoded, sequence],
                )?;
                receipt
            }
            _ => return Err(TaskStoreError::AttemptConflict),
        };
        transaction.commit()?;
        Ok(receipt)
    }
}
