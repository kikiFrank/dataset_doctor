-- Workspace v1 schema. The Rust caller owns the transaction and metadata insert.

CREATE TABLE schema_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    workspace_id TEXT NOT NULL UNIQUE,
    schema_version INTEGER NOT NULL CHECK (schema_version >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE datasets (
    dataset_id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    categories_json TEXT NOT NULL DEFAULT '[]',
    config_json TEXT NOT NULL DEFAULT '{}',
    row_revision INTEGER NOT NULL DEFAULT 0 CHECK (row_revision >= 0),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE assets (
    asset_id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('IMAGE', 'VIDEO')),
    location_kind TEXT NOT NULL
        CHECK (location_kind IN ('WORKSPACE', 'EXTERNAL')),
    path TEXT NOT NULL,
    original_filename TEXT NOT NULL,
    file_origin TEXT NOT NULL CHECK (file_origin IN ('IMPORTED', 'GENERATED')),
    availability TEXT NOT NULL DEFAULT 'ONLINE'
        CHECK (availability IN ('ONLINE', 'OFFLINE')),
    annotation_path TEXT,
    annotation_format TEXT,
    annotation_origin TEXT NOT NULL DEFAULT 'NONE'
        CHECK (annotation_origin IN ('NONE', 'PREEXISTING', 'APP_CREATED')),
    annotation_local_image_id INTEGER CHECK (annotation_local_image_id >= 0),
    media_digest TEXT,
    annotation_digest TEXT,
    source_video_id TEXT REFERENCES assets(asset_id) ON DELETE RESTRICT,
    source_metadata_json TEXT NOT NULL DEFAULT '{}',
    metadata_json TEXT NOT NULL DEFAULT '{}',
    row_revision INTEGER NOT NULL DEFAULT 0 CHECK (row_revision >= 0),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE (location_kind, path),
    CHECK (kind = 'IMAGE' OR annotation_path IS NULL),
    CHECK (
        (annotation_path IS NULL AND annotation_origin = 'NONE')
        OR (annotation_path IS NOT NULL AND annotation_origin <> 'NONE')
    )
);
CREATE UNIQUE INDEX uq_assets_work_annotation
    ON assets(location_kind, annotation_path)
    WHERE annotation_path IS NOT NULL AND annotation_format = 'COCO_IMAGE_BBOX';
CREATE INDEX idx_assets_source_video ON assets(source_video_id);
CREATE INDEX idx_assets_availability ON assets(availability);

CREATE TABLE workflow_runs (
    workflow_run_id TEXT PRIMARY KEY NOT NULL,
    dataset_id TEXT REFERENCES datasets(dataset_id) ON DELETE SET NULL,
    workspace_session_id TEXT NOT NULL,
    template_key TEXT NOT NULL,
    plan_kind TEXT NOT NULL
        CHECK (plan_kind = 'template_instance'),
    plan_json TEXT NOT NULL,
    node_states_json TEXT NOT NULL DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'DRAFT',
    request_id TEXT NOT NULL,
    operation_kind TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    input_manifest_path TEXT,
    checkpoint_manifest_path TEXT,
    result_manifest_path TEXT,
    error_code TEXT,
    error_message TEXT,
    row_revision INTEGER NOT NULL DEFAULT 0 CHECK (row_revision >= 0),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    started_at_ms INTEGER,
    finished_at_ms INTEGER,
    UNIQUE (operation_kind, idempotency_key)
);
CREATE INDEX idx_workflows_dataset_status ON workflow_runs(dataset_id, status);

CREATE TABLE tasks (
    task_id TEXT PRIMARY KEY NOT NULL,
    workflow_run_id TEXT REFERENCES workflow_runs(workflow_run_id)
        ON DELETE RESTRICT,
    node_id TEXT,
    dataset_id TEXT REFERENCES datasets(dataset_id) ON DELETE SET NULL,
    workspace_session_id TEXT NOT NULL,
    capability_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'QUEUED',
    current_attempt_id TEXT,
    attempt_no INTEGER NOT NULL DEFAULT 0 CHECK (attempt_no >= 0),
    request_id TEXT NOT NULL,
    operation_kind TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    input_manifest_path TEXT NOT NULL,
    input_fingerprint TEXT NOT NULL,
    resource_profile_json TEXT NOT NULL DEFAULT '{}',
    execution_metadata_json TEXT NOT NULL DEFAULT '{}',
    output_manifest_path TEXT,
    progress REAL DEFAULT NULL
        CHECK (progress IS NULL OR (progress >= 0 AND progress <= 1)),
    error_code TEXT,
    error_message TEXT,
    row_revision INTEGER NOT NULL DEFAULT 0 CHECK (row_revision >= 0),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    started_at_ms INTEGER,
    finished_at_ms INTEGER,
    UNIQUE (operation_kind, idempotency_key),
    CHECK (
        (workflow_run_id IS NULL AND node_id IS NULL)
        OR (workflow_run_id IS NOT NULL AND node_id IS NOT NULL)
    )
);
CREATE INDEX idx_tasks_workflow_node ON tasks(workflow_run_id, node_id);
CREATE INDEX idx_tasks_status_created ON tasks(status, created_at_ms);
CREATE UNIQUE INDEX uq_tasks_current_attempt
    ON tasks(current_attempt_id) WHERE current_attempt_id IS NOT NULL;

CREATE TABLE review_sessions (
    review_id TEXT PRIMARY KEY NOT NULL,
    asset_id TEXT NOT NULL REFERENCES assets(asset_id) ON DELETE CASCADE,
    dataset_id TEXT NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    source_task_id TEXT REFERENCES tasks(task_id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'OPEN',
    baseline_media_digest TEXT NOT NULL,
    baseline_annotation_digest TEXT,
    baseline_label_schema_digest TEXT NOT NULL,
    review_manifest_path TEXT,
    confirmed_media_digest TEXT,
    confirmed_annotation_digest TEXT,
    confirmed_label_schema_digest TEXT,
    row_revision INTEGER NOT NULL DEFAULT 0 CHECK (row_revision >= 0),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    confirmed_at_ms INTEGER,
    UNIQUE (review_id, asset_id, dataset_id)
);
CREATE UNIQUE INDEX uq_review_active_asset ON review_sessions(asset_id)
    WHERE status IN ('OPEN', 'COMMITTING', 'CONFLICT');
CREATE INDEX idx_reviews_dataset_status ON review_sessions(dataset_id, status);

CREATE TABLE dataset_assets (
    dataset_id TEXT NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    asset_id TEXT NOT NULL REFERENCES assets(asset_id) ON DELETE CASCADE,
    final_review_id TEXT,
    added_at_ms INTEGER NOT NULL,
    PRIMARY KEY (dataset_id, asset_id),
    UNIQUE (asset_id),
    FOREIGN KEY (final_review_id, asset_id, dataset_id)
        REFERENCES review_sessions(review_id, asset_id, dataset_id)
        ON DELETE RESTRICT
);

CREATE TABLE staging_items (
    asset_id TEXT PRIMARY KEY NOT NULL
        REFERENCES assets(asset_id) ON DELETE CASCADE,
    target_dataset_id TEXT REFERENCES datasets(dataset_id) ON DELETE SET NULL,
    review_id TEXT REFERENCES review_sessions(review_id) ON DELETE SET NULL,
    check_task_id TEXT REFERENCES tasks(task_id) ON DELETE SET NULL,
    check_acceptance_digest TEXT,
    check_accepted_at_ms INTEGER,
    created_at_ms INTEGER NOT NULL,
    CHECK (
        (check_acceptance_digest IS NULL AND check_accepted_at_ms IS NULL)
        OR (check_acceptance_digest IS NOT NULL AND check_accepted_at_ms IS NOT NULL)
    )
);
CREATE INDEX idx_staging_target ON staging_items(target_dataset_id);

CREATE TABLE file_operations (
    operation_id TEXT PRIMARY KEY NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_id TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    workspace_session_id TEXT NOT NULL,
    operation_kind TEXT NOT NULL,
    dataset_id TEXT REFERENCES datasets(dataset_id) ON DELETE SET NULL,
    asset_id TEXT REFERENCES assets(asset_id) ON DELETE SET NULL,
    review_id TEXT REFERENCES review_sessions(review_id) ON DELETE SET NULL,
    task_id TEXT REFERENCES tasks(task_id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'PREPARED',
    manifest_path TEXT NOT NULL,
    manifest_digest TEXT NOT NULL,
    result_json TEXT,
    error_code TEXT,
    error_message TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    completed_at_ms INTEGER,
    UNIQUE (operation_kind, idempotency_key)
);
CREATE INDEX idx_file_operations_status ON file_operations(status);
CREATE INDEX idx_file_operations_asset ON file_operations(asset_id);
