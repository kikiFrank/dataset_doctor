use std::fs;
use std::path::{Path, PathBuf};

use dd_infra::backup_artifacts::{BackupArtifactError, stage_formal_backup_artifacts};
use dd_infra::json::JsonContracts;
use dd_infra::workspace::create_workspace;
use rusqlite::{Connection, params};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

struct Fixture {
    _run: tempfile::TempDir,
    root: PathBuf,
    stage: PathBuf,
    index: String,
    report: String,
    input: String,
}

impl Fixture {
    fn new() -> Self {
        let run = tempfile::tempdir().unwrap();
        let parent = run.path().canonicalize().unwrap();
        let root = parent.join("workspace");
        let stage = parent.join("staging");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&stage).unwrap();
        let workspace = create_workspace(
            &root,
            &JsonContracts::new().unwrap(),
            250,
            "Artifact backup",
            &Uuid::new_v4().to_string(),
            100,
        )
        .unwrap();
        drop(workspace);
        let task = Uuid::new_v4();
        let attempt = Uuid::new_v4();
        let attempt_directory = format!("artifacts/tasks/{task}/{attempt}");
        let input = "artifacts/task-inputs/input.json".to_owned();
        let index = format!("{attempt_directory}/result-manifest.json");
        let report = format!("{attempt_directory}/report.json");
        for (relative, content) in [
            (input.as_str(), b"{\"test\":true}".as_slice()),
            (report.as_str(), b"{\"findings\":[]}".as_slice()),
            (
                "artifacts/candidates/not-recorded.json",
                b"{\"candidate\":true}".as_slice(),
            ),
        ] {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        let bytes = fs::read(root.join(&report)).unwrap();
        let graph = json!({
            "manifest_contract_version": 1, "task_id": task, "attempt_id": attempt,
            "input_fingerprint": "2d".repeat(32), "sealed_at": "2026-11-26T12:00:00Z",
            "entries": [{"artifact_id": Uuid::new_v4(), "relative_path": "report.json",
                "sha256": sha(&bytes), "size_bytes": bytes.len(), "kind": "CHECK_RESULT",
                "media_type": "application/json"}],
        });
        let manifest = serde_json::to_vec(&graph).unwrap();
        fs::write(root.join(&index), &manifest).unwrap();
        let execution = json!({"terminal_receipt": {"result_manifest_ref": {
            "artifact_id": Uuid::new_v4(), "owner": {"kind": "TASK", "id": task},
            "kind": "TASK_RESULT_MANIFEST", "media_type": "application/json",
            "sha256": sha(&manifest), "size_bytes": manifest.len().to_string(),
        }}})
        .to_string();
        let database = db(&root);
        database.execute(
            "INSERT INTO tasks(
                task_id, workspace_session_id, capability_id, status, current_attempt_id, attempt_no,
                request_id, operation_kind, idempotency_key, request_fingerprint, input_manifest_path,
                input_fingerprint, output_manifest_path, execution_metadata_json, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, 'annotation_geometry.v1', 'SUCCEEDED', ?3, 1,
                ?4, 'ANNOTATION_GEOMETRY', ?5, ?6, ?7, ?8, ?9, ?10, 200, 200)",
            params![task.to_string(), Uuid::new_v4().to_string(), attempt.to_string(),
                Uuid::new_v4().to_string(), Uuid::new_v4().to_string(), "1d".repeat(32),
                input, "2d".repeat(32), index, execution],
        ).unwrap();
        Self {
            _run: run,
            root,
            stage,
            index,
            report,
            input,
        }
    }
}

#[test]
fn snapshot_references_copy_real_artifact_files_and_exclude_unregistered_neighbors() {
    let fixture = Fixture::new();
    let records =
        stage_formal_backup_artifacts(&db(&fixture.root), &fixture.root, &fixture.stage).unwrap();
    assert_eq!(records.len(), 3);
    for relative in [&fixture.input, &fixture.index, &fixture.report] {
        let record = records
            .iter()
            .find(|entry| &entry.workspace_path == relative)
            .unwrap();
        let original = fs::read(fixture.root.join(relative)).unwrap();
        assert_eq!(record.sha256, sha(&original));
        assert_eq!(record.size_bytes, original.len() as u64);
        assert_eq!(
            fs::read(fixture.stage.join(&record.package_path)).unwrap(),
            original
        );
    }
    assert!(
        !fixture
            .stage
            .join("artifacts/candidates/not-recorded.json")
            .exists()
    );
}

#[test]
fn a_changed_sealed_artifact_rolls_back_the_staged_collection() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join(&fixture.report), b"{\"findings\":[42]}").unwrap();
    let error = stage_formal_backup_artifacts(&db(&fixture.root), &fixture.root, &fixture.stage)
        .unwrap_err();
    assert!(matches!(error, BackupArtifactError::DigestMismatch(_)));
    assert_eq!(fs::read_dir(&fixture.stage).unwrap().count(), 0);
    assert_eq!(
        fs::read(fixture.root.join(&fixture.report)).unwrap(),
        b"{\"findings\":[42]}"
    );
}

#[test]
fn a_changed_result_manifest_cannot_replace_the_original_receipt() {
    let fixture = Fixture::new();
    let result = fixture.root.join(&fixture.index);
    let mut bytes = fs::read(&result).unwrap();
    bytes.push(b'\n');
    fs::write(result, bytes).unwrap();
    let error = stage_formal_backup_artifacts(&db(&fixture.root), &fixture.root, &fixture.stage)
        .unwrap_err();
    assert!(matches!(error, BackupArtifactError::DigestMismatch(_)));
    assert_eq!(fs::read_dir(fixture.stage).unwrap().count(), 0);
}

#[test]
fn a_recorded_path_cannot_collect_an_external_or_unmanaged_file() {
    let fixture = Fixture::new();
    let extra = fixture.root.join("not-for-backup.env");
    fs::write(&extra, b"unregistered synthetic content").unwrap();
    let snapshot = db(&fixture.root);
    snapshot
        .execute(
            "UPDATE tasks SET input_manifest_path = 'not-for-backup.env'",
            [],
        )
        .unwrap();
    let error =
        stage_formal_backup_artifacts(&snapshot, &fixture.root, &fixture.stage).unwrap_err();
    assert!(matches!(error, BackupArtifactError::InvalidPath(_)));
    assert_eq!(fs::read_dir(&fixture.stage).unwrap().count(), 0);
    assert_eq!(fs::read(extra).unwrap(), b"unregistered synthetic content");
}

fn db(root: &Path) -> Connection {
    Connection::open(root.join("workspace.db")).unwrap()
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
