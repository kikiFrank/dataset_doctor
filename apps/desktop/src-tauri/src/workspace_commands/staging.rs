use std::sync::Arc;

use dd_infra::artifact_reference::{
    ArtifactOwner as StoredOwner, ArtifactReference as StoredReference,
};
use dd_infra::frame_runtime::FrameService;
use dd_infra::staging::{StagingFrameSource, StagingQuery, StagingState as StoredState};
use dd_infra::staging_commit::{
    AcknowledgedFinding, AcknowledgedIncompleteCheck, StagingCommitError,
    StagingCommitRequest as StoredCommitRequest, StagingCommitService,
};
use dd_infra::staging_report::{
    ReportCheck, StagingBatchReportRequest, StagingReportError, StagingReportService,
};
use dd_infra::task_store::{TaskStoreError, format_digest};
use dd_ui_contracts::dataset_import::{ImportOperationKind, ImportOperationRef, ImportTaskView};
use dd_ui_contracts::staging::{
    StagingBatchReportBuildRequest, StagingBatchReportView, StagingCommitRequest,
    StagingCommitView, StagingFindingAck, StagingIncompleteCheckAck, StagingItem,
    StagingListRequest, StagingPage, StagingReportCheckView, StagingReportFindingView,
    StagingReportGetRequest, StagingReportReasonView, StagingReportScopeView, StagingReportView,
    StagingState,
};
use dd_ui_contracts::video::VideoFrame;
use dd_ui_contracts::workflow::{ReportGetRequest, ReportGetView};
use dd_ui_contracts::{ArtifactOwner, ArtifactReference, DecimalU64};
use tauri::State;
use uuid::Uuid;

use super::{AppError, Commands, WorkspaceThread, WorkspaceThreadError, run};

#[tauri::command]
pub async fn report_get(
    request: ReportGetRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<ReportGetView, AppError> {
    let request_id = match &request {
        ReportGetRequest::Workflow(request) => request.context.request_id.clone(),
        ReportGetRequest::Staging(request) => request.context.request_id.clone(),
    };
    run(thread, request_id, move |commands| match request {
        ReportGetRequest::Workflow(request) => commands
            .get_workflow_report(request)
            .map(ReportGetView::Workflow),
        ReportGetRequest::Staging(request) => commands
            .get_staging_report(request)
            .map(ReportGetView::Staging),
    })
    .await
}

#[tauri::command]
pub async fn staging_batch_report_build(
    request: StagingBatchReportBuildRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<StagingBatchReportView, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.prepare_staging_batch(request),
    )
    .await
}

#[tauri::command]
pub async fn staging_commit(
    request: StagingCommitRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<ImportOperationRef, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.commit_staging(request),
    )
    .await
}

#[tauri::command]
pub async fn staging_list(
    request: StagingListRequest,
    thread: State<'_, Arc<WorkspaceThread>>,
) -> Result<StagingPage, AppError> {
    run(
        thread,
        request.context.request_id.clone(),
        move |commands| commands.list_staging(request),
    )
    .await
}

impl Commands {
    pub(super) fn prepare_staging_batch(
        &self,
        request: StagingBatchReportBuildRequest,
    ) -> Result<StagingBatchReportView, AppError> {
        let (session, request_id, idempotency_key) = self.import_identity(request.context)?;
        let dataset = self.parse_uuid(request.target_dataset_id.clone())?;
        let refs = request
            .source_report_refs
            .into_iter()
            .map(|reference| self.stored_report_reference(reference))
            .collect::<Result<Vec<_>, _>>()?;
        let service = StagingReportService::new(Arc::clone(&self.thread))
            .map_err(|_| self.error("INTERNAL", "无法初始化批次报告。"))?;
        let report = service
            .build_batch(
                session,
                StagingBatchReportRequest {
                    target_dataset_id: dataset,
                    source_report_refs: refs,
                    request_id,
                    idempotency_key,
                },
                self.now_ms()?,
            )
            .map_err(|error| match error {
                StagingReportError::Workspace(WorkspaceThreadError::Task(
                    TaskStoreError::IdempotencyConflict,
                )) => self.error("CONFLICT", "批次内容已变化，请重新选择样本。"),
                StagingReportError::Workspace(error) => self.dataset_error(error),
                _ => self.error("REPORT_STALE", "无法核验批次报告，请刷新选中的样本。"),
            })?;
        let label_schema_digest = report.label_schema_digest().to_owned();
        let unexecuted_checks = report.unexecuted_checks().iter().cloned().collect();
        Ok(StagingBatchReportView {
            target_dataset_id: request.target_dataset_id,
            label_schema_digest,
            unexecuted_checks,
            asset_ids: report
                .asset_ids
                .into_iter()
                .map(|id| self.uuid_text(id.to_string()))
                .collect::<Result<_, _>>()?,
            quality_report_ref: self.wire_artifact(report.reference)?,
        })
    }

    pub(super) fn commit_staging(
        &self,
        request: StagingCommitRequest,
    ) -> Result<ImportOperationRef, AppError> {
        let (session, request_id, idempotency_key) = self.import_identity(request.context)?;
        let stored = StoredCommitRequest {
            request_id,
            idempotency_key,
            target_dataset_id: self.parse_uuid(request.target_dataset_id)?,
            asset_ids: request
                .asset_ids
                .into_iter()
                .map(|id| self.parse_uuid(id))
                .collect::<Result<_, _>>()?,
            quality_report_ref: self.stored_report_reference(request.quality_report_ref)?,
            expected_schema_fingerprint: request.expected_schema_fingerprint,
            acknowledged_findings: request
                .acknowledged_findings
                .into_iter()
                .map(|ack| {
                    Ok(AcknowledgedFinding {
                        issue_id: self.parse_uuid(ack.issue_id)?,
                        reason: ack.reason,
                    })
                })
                .collect::<Result<_, AppError>>()?,
            acknowledged_incomplete_checks: request
                .acknowledged_incomplete_checks
                .unwrap_or_default()
                .into_iter()
                .map(|ack| AcknowledgedIncompleteCheck {
                    check_id: ack.check_id,
                    reason: ack.reason,
                })
                .collect(),
        };
        let receipt = self
            .staging_commits()?
            .commit(session, stored, self.now_ms()?)
            .map_err(|error| self.staging_commit_error(error))?;
        self.import_operation(
            ImportOperationKind::StagingCommit,
            receipt.task_id,
            "SUCCEEDED".into(),
            receipt.accepted_at_ms,
        )
    }

    fn stored_report_reference(
        &self,
        reference: ArtifactReference,
    ) -> Result<StoredReference, AppError> {
        let ArtifactOwner::Task(task) = reference.owner else {
            return Err(self.error("INVALID_ARGUMENT", "请选择待接收区的检查报告。"));
        };
        Ok(StoredReference {
            artifact_id: self.parse_uuid(reference.artifact_id)?,
            owner: StoredOwner::Task(self.parse_uuid(task)?),
            kind: reference.kind,
            media_type: reference.media_type,
            sha256: reference.sha256,
            size_bytes: reference.size_bytes.into(),
            schema_id: reference.schema_id,
        })
    }

    pub(super) fn staging_commit_task(
        &self,
        session: Uuid,
        task: Uuid,
    ) -> Result<ImportTaskView, AppError> {
        let receipt = self
            .staging_commits()?
            .get(session, task)
            .map_err(|error| self.staging_commit_error(error))?
            .ok_or_else(|| self.error("NOT_FOUND", "当前工作区没有此接收任务。"))?;
        let staging_commit = StagingCommitView {
            target_dataset_id: self.uuid_text(receipt.target_dataset_id.to_string())?,
            accepted_asset_ids: receipt
                .accepted_asset_ids
                .into_iter()
                .map(|id| self.uuid_text(id.to_string()))
                .collect::<Result<_, _>>()?,
            quality_report_ref: self.wire_artifact(receipt.quality_report_ref)?,
            acknowledged_findings: receipt
                .acknowledged_findings
                .into_iter()
                .map(|ack| {
                    Ok(StagingFindingAck {
                        issue_id: self.uuid_text(ack.issue_id.to_string())?,
                        reason: ack.reason,
                    })
                })
                .collect::<Result<_, AppError>>()?,
            acknowledged_incomplete_checks: receipt
                .acknowledged_incomplete_checks
                .into_iter()
                .map(|ack| StagingIncompleteCheckAck {
                    check_id: ack.check_id,
                    reason: ack.reason,
                })
                .collect(),
        };
        Ok(ImportTaskView {
            color_preview: None,
            color_apply: None,
            task_id: self.uuid_text(task.to_string())?,
            operation_kind: ImportOperationKind::StagingCommit,
            status: "SUCCEEDED".into(),
            error_code: None,
            preview: None,
            registration: None,
            geometry_report: None,
            coco_export: None,
            report_export: None,
            workspace_backup: None,
            video_frame: None,
            video_sample: None,
            staging_commit: Some(staging_commit),
            video_tracking: None,
        })
    }

    fn staging_commits(&self) -> Result<StagingCommitService, AppError> {
        StagingCommitService::new(Arc::clone(&self.thread))
            .map_err(|error| self.staging_commit_error(error))
    }

    fn staging_commit_error(&self, error: StagingCommitError) -> AppError {
        match error {
            StagingCommitError::Workspace(WorkspaceThreadError::Task(
                TaskStoreError::IdempotencyConflict,
            )) => self.error("CONFLICT", "接收请求内容已变化，请重新确认。"),
            StagingCommitError::Workspace(error) => self.dataset_error(error),
            StagingCommitError::NotReady | StagingCommitError::Report(_) => {
                self.error("REPORT_STALE", "Final 或报告已变化，请刷新并重新检查。")
            }
            StagingCommitError::AcknowledgementRequired => self.error(
                "INVALID_ARGUMENT",
                "请逐项确认报告中的发现和未执行检查，并填写缺少检查的接收理由。",
            ),
            StagingCommitError::InvalidRequest | StagingCommitError::Input(_) => {
                self.error("INVALID_ARGUMENT", "接收样本、报告或数据集类别不匹配。")
            }
            StagingCommitError::InvalidReceipt => {
                self.error("DATA_CORRUPTION", "无法核验已保存的接收回执。")
            }
            _ => self.error("INTERNAL", "无法提交待接收样本。"),
        }
    }

    pub(super) fn list_staging(
        &self,
        request: StagingListRequest,
    ) -> Result<StagingPage, AppError> {
        let session = self.session(request.context)?;
        let page = self
            .thread
            .list_staging(
                session,
                StagingQuery {
                    target_dataset_id: request
                        .target_dataset_id
                        .map(|id| self.parse_uuid(id))
                        .transpose()?,
                    after: request.cursor.map(|id| self.parse_uuid(id)).transpose()?,
                    status: request.status.map(stored_state),
                    page_size: request.page_size,
                },
            )
            .map_err(|error| self.dataset_error(error))?;
        let frames = if page.items.iter().any(|item| item.source_frame.is_some()) {
            Some(
                FrameService::new(Arc::clone(&self.thread))
                    .map_err(|_| self.error("FAILED_PRECONDITION", "无法初始化帧证据读取。"))?,
            )
        } else {
            None
        };
        let items = page
            .items
            .into_iter()
            .map(|item| {
                let source_frame_ref = match (&frames, item.source_frame) {
                    (Some(frames), Some(source)) => self.staged_frame(frames, session, source)?,
                    _ => None,
                };
                Ok(StagingItem {
                    asset_id: self.uuid_text(item.asset_id.to_string())?,
                    target_dataset_id: item
                        .target_dataset_id
                        .map(|id| self.uuid_text(id.to_string()))
                        .transpose()?,
                    display_name: item.display_name,
                    state: wire_state(item.state),
                    review_state: item.review_state,
                    source_frame_ref,
                    current_report_ref: item
                        .current_report_ref
                        .map(|reference| self.wire_artifact(reference))
                        .transpose()?,
                    blocking_reasons: item
                        .blocking_reasons
                        .into_iter()
                        .map(String::from)
                        .collect(),
                })
            })
            .collect::<Result<_, AppError>>()?;
        Ok(StagingPage {
            items,
            next_cursor: page
                .next_cursor
                .map(|id| self.uuid_text(id.to_string()))
                .transpose()?,
        })
    }

    pub(super) fn get_staging_report(
        &self,
        request: StagingReportGetRequest,
    ) -> Result<StagingReportView, AppError> {
        let session = self.session(request.context)?;
        let task = match (request.report_id, request.task_id) {
            (Some(id), None) | (None, Some(id)) => self.parse_uuid(id)?,
            _ => return Err(self.error("INVALID_ARGUMENT", "请提供一个报告 ID 或任务 ID。")),
        };
        let service = StagingReportService::new(Arc::clone(&self.thread))
            .map_err(|_| self.error("INTERNAL", "无法初始化报告读取。"))?;
        let report = service.get(session, task).map_err(|error| match error {
            StagingReportError::Workspace(WorkspaceThreadError::Task(
                TaskStoreError::UnknownTask,
            )) => self.error("NOT_FOUND", "当前工作区没有此检查报告。"),
            StagingReportError::Workspace(error) => self.dataset_error(error),
            _ => self.error("REPORT_STALE", "报告无法核验，请重新运行检查。"),
        })?;
        let data = report.data;
        let scope = data.scope;
        Ok(StagingReportView {
            report_id: self.uuid_text(data.report_id.to_string())?,
            scope: StagingReportScopeView {
                dataset_id: self.uuid_text(scope.dataset_id.to_string())?,
                asset_id: self.uuid_text(scope.asset_id.to_string())?,
                media_digest: scope.media_digest,
                annotation_digest: scope.annotation_digest,
                label_schema_digest: scope.label_schema_digest,
            },
            input_fingerprint: data.input_fingerprint,
            execution_coverage: data.execution_coverage,
            findings_by_severity: data.findings_by_severity,
            human_ack_status: data.human_ack_status,
            unexecuted_checks: data.unexecuted_checks,
            repair_outcomes: data.repair_outcomes,
            checks: self.report_checks_view(data.checks)?,
            report_artifact_ref: self.wire_artifact(report.reference)?,
        })
    }

    pub(super) fn report_checks_view(
        &self,
        checks: Vec<ReportCheck>,
    ) -> Result<Vec<StagingReportCheckView>, AppError> {
        checks
            .into_iter()
            .map(|check| {
                let findings = check
                    .findings
                    .into_iter()
                    .map(|finding| {
                        Ok(StagingReportFindingView {
                            issue_id: self.uuid_text(finding.issue_id.to_string())?,
                            asset_id: self.uuid_text(finding.asset_id.to_string())?,
                            check_id: finding.check_id,
                            finding_code: finding.finding_code,
                            severity: finding.severity,
                            input_fingerprint: finding.input_fingerprint,
                            explanation: finding.explanation,
                            annotation_id: finding.annotation_id.map(Into::into),
                        })
                    })
                    .collect::<Result<_, AppError>>()?;
                let evidence_refs = check
                    .evidence_refs
                    .into_iter()
                    .map(|reference| self.wire_artifact(reference))
                    .collect::<Result<_, AppError>>()?;
                Ok(StagingReportCheckView {
                    check_id: check.check_id,
                    check_contract_version: check.check_contract_version,
                    execution_status: check.execution_status,
                    assessment: check.assessment,
                    skip_or_failure_reason: check.skip_or_failure_reason.map(|reason| {
                        StagingReportReasonView {
                            code: reason.code,
                            message: reason.message,
                        }
                    }),
                    input_fingerprint: check.input_fingerprint,
                    processed_assets: check.processed_assets,
                    total_assets: check.total_assets,
                    finding_count: check.finding_count,
                    findings,
                    evidence_refs,
                    started_at: check.started_at,
                    finished_at: check.finished_at,
                })
            })
            .collect()
    }

    pub(super) fn wire_artifact(
        &self,
        reference: StoredReference,
    ) -> Result<ArtifactReference, AppError> {
        let owner = match reference.owner {
            StoredOwner::Task(id) => ArtifactOwner::Task(self.uuid_text(id.to_string())?),
            StoredOwner::Asset(id) => ArtifactOwner::Asset(self.uuid_text(id.to_string())?),
            StoredOwner::Review(id) => ArtifactOwner::Review(self.uuid_text(id.to_string())?),
        };
        Ok(ArtifactReference {
            artifact_id: self.uuid_text(reference.artifact_id.to_string())?,
            owner,
            kind: reference.kind,
            media_type: reference.media_type,
            sha256: reference.sha256,
            size_bytes: DecimalU64::try_from(reference.size_bytes)
                .map_err(|_| self.error("DATA_CORRUPTION", "产物记录的大小无效。"))?,
            schema_id: reference.schema_id,
        })
    }

    fn staged_frame(
        &self,
        frames: &FrameService,
        session: Uuid,
        source: StagingFrameSource,
    ) -> Result<Option<VideoFrame>, AppError> {
        let Some(task) = source.extraction_task_id else {
            return Ok(None);
        };
        let Ok(snapshot) = frames.snapshot(session, task) else {
            return Ok(None);
        };
        let Some(resolved) = snapshot.frame else {
            return Ok(None);
        };
        let frame = resolved.frame;
        if frame.source_video_id != source.source_video_id
            || format_digest(&frame.source_digest) != source.source_digest
            || frame.stream_index != source.stream_index
            || frame.frame_index != source.frame_index
            || frame.pts_us != source.pts_us
        {
            return Ok(None);
        }
        self.frame_view(task, resolved).map(Some)
    }
}

fn wire_state(state: StoredState) -> StagingState {
    match state {
        StoredState::Offline => StagingState::Offline,
        StoredState::Conflict => StagingState::Conflict,
        StoredState::Accepting => StagingState::Accepting,
        StoredState::Checking => StagingState::Checking,
        StoredState::NeedsAnnotation => StagingState::NeedsAnnotation,
        StoredState::NeedsReview => StagingState::NeedsReview,
        StoredState::NeedsCheck => StagingState::NeedsCheck,
        StoredState::ReadyForAcceptance => StagingState::ReadyForAcceptance,
    }
}

fn stored_state(state: StagingState) -> StoredState {
    match state {
        StagingState::Offline => StoredState::Offline,
        StagingState::Conflict => StoredState::Conflict,
        StagingState::Accepting => StoredState::Accepting,
        StagingState::Checking => StoredState::Checking,
        StagingState::NeedsAnnotation => StoredState::NeedsAnnotation,
        StagingState::NeedsReview => StoredState::NeedsReview,
        StagingState::NeedsCheck => StoredState::NeedsCheck,
        StagingState::ReadyForAcceptance => StoredState::ReadyForAcceptance,
    }
}
