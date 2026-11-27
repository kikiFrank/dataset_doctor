//! Rebuild the local runtime job from a validated paused Workflow.

use dd_core::workflow::EnabledStep;
use serde_json::Value;
use uuid::Uuid;

use super::super::{WorkflowError, WorkflowGeometryRequest, WorkflowResumeRequest};
use super::{
    LocalHealthcheckAcceptance, LocalHealthcheckJob, LocalHealthcheckRuntime, execution,
    local_policy,
};
use crate::dataset_profile_runtime::{DatasetProfileRequest, DatasetProfileRuntime};
use crate::input_fingerprint::manifest_u32;
use crate::worker_runtime::now_ms;

impl LocalHealthcheckRuntime {
    pub fn resume(
        &self,
        session: Uuid,
        request: WorkflowResumeRequest,
        request_id: Uuid,
    ) -> Result<LocalHealthcheckAcceptance, WorkflowError> {
        let policy = local_policy();
        let verified = self.store.verify_resume(session, &policy, &request)?;
        let max_pixels = verified.scope["resolved_config"]["max_pixels"]
            .as_number()
            .and_then(manifest_u32)
            .filter(|limit| *limit > 0)
            .ok_or(WorkflowError::InvalidRecord)?;
        if verified.scope["resolved_config"]["plan_source"] != "LOCAL_DEFAULT"
            || request_id.is_nil()
        {
            return Err(WorkflowError::InvalidInputScope);
        }
        let now = now_ms().map_err(execution)?;
        if !verified.row.geometry_tasks.is_empty() {
            let ready = self.store.resume_geometry_batch(
                session,
                verified,
                &WorkflowGeometryRequest {
                    reference: request.reference.clone(),
                    expected_row_revision: request.expected_row_revision,
                    request_id,
                    idempotency_key: Uuid::new_v4(),
                    max_pixels,
                },
                now,
            )?;
            let job = LocalHealthcheckJob {
                session,
                reference: request.reference,
                row_revision: ready.workflow.row_revision,
                request_id,
                max_pixels,
                profile: None,
                geometry_jobs: Some(ready.jobs),
            };
            return Ok(LocalHealthcheckAcceptance {
                workflow: ready.workflow,
                job: Some(job),
            });
        }
        let profile = verified
            .row
            .profile_task
            .as_ref()
            .ok_or(WorkflowError::InvalidRecord)?;
        let (workflow, profile) = match profile.status.as_str() {
            "SUCCEEDED" => (
                self.store.resume_from_profile(session, verified.row, now)?,
                None,
            ),
            "QUEUED" => {
                let limit = verified
                    .plan
                    .enabled_steps
                    .iter()
                    .find_map(|step| match step {
                        EnabledStep::DatasetProfile { sample_limit } => {
                            Some(*sample_limit as usize)
                        }
                        _ => None,
                    })
                    .ok_or(WorkflowError::InvalidInputScope)?;
                let assets: Vec<_> = verified.scope["assets"]
                    .as_array()
                    .ok_or(WorkflowError::InvalidRecord)?
                    .iter()
                    .take(limit)
                    .cloned()
                    .collect();
                let asset_ids = assets
                    .iter()
                    .map(|asset| {
                        asset["asset_id"]
                            .as_str()
                            .and_then(|id| id.parse().ok())
                            .ok_or(WorkflowError::InvalidRecord)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let prepared = DatasetProfileRuntime::prepare(
                    &self.store.owner,
                    session,
                    DatasetProfileRequest {
                        dataset_id: verified.row.dataset_id,
                        asset_ids,
                        max_pixels,
                        request_id,
                        idempotency_key: Uuid::new_v4(),
                    },
                )
                .map_err(execution)?;
                let projected: Value = serde_json::from_slice(prepared.manifest_bytes())
                    .map_err(WorkflowError::Json)?;
                if projected["assets"].as_array() != Some(&assets)
                    || projected["label_schema_digest"] != verified.scope["label_schema_digest"]
                {
                    return Err(WorkflowError::InputChanged);
                }
                let (workflow, task) = self.store.resume_queued_profile(
                    session,
                    verified.row,
                    &prepared.submission,
                    now,
                )?;
                (workflow, Some(prepared.into_job(task)))
            }
            _ => return Err(WorkflowError::ControlUnavailable),
        };
        let job = LocalHealthcheckJob {
            session,
            reference: request.reference,
            row_revision: workflow.row_revision,
            request_id,
            max_pixels,
            profile,
            geometry_jobs: None,
        };
        Ok(LocalHealthcheckAcceptance {
            workflow,
            job: Some(job),
        })
    }
}
