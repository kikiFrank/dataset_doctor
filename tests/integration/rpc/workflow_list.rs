use std::collections::BTreeSet;
use std::sync::Arc;

use dd_infra::workflow_store::{LocalHealthcheckRequest, LocalHealthcheckRuntime, WorkflowStatus};
use uuid::Uuid;

#[path = "support/workflow_fixture.rs"]
#[allow(dead_code)] // Geometry and report tests use the fixture's execution helpers.
mod workflow_fixture;
use workflow_fixture::WorkflowFixture;

const COCO: &[u8] = br#"{"images":[{"id":1,"file_name":"signal.png","width":2,"height":1}],"categories":[{"id":7,"name":"green-light"}],"annotations":[]}"#;

#[test]
fn workflow_pages_bind_the_dataset_and_reconcile_old_active_runs_after_reopen() {
    let f = WorkflowFixture::new("Workflow History", COCO);
    let runtime =
        LocalHealthcheckRuntime::new(Arc::clone(&f.workspace), f.interpreter, f.entrypoint)
            .unwrap();
    let mut expected = BTreeSet::from([f.ready.run_id]);
    for _ in 0..2 {
        let started = runtime
            .admit(
                f.session,
                LocalHealthcheckRequest {
                    dataset_id: f.dataset,
                    sample_limit: 2,
                    max_pixels: 100,
                    request_id: Uuid::new_v4(),
                    idempotency_key: Uuid::new_v4(),
                },
            )
            .unwrap();
        assert_eq!(started.workflow.status, WorkflowStatus::Running);
        expected.insert(started.workflow.run_id);
    }
    assert!(f.store.list(f.session, f.dataset, None, 0).is_err());
    assert!(f.store.list(f.session, f.dataset, None, 201).is_err());
    assert!(
        f.store
            .list(f.session, f.dataset, Some(Uuid::new_v4()), 2)
            .is_err()
    );
    assert!(f.store.list(f.session, Uuid::new_v4(), None, 2).is_err());
    let newest = f.store.list(f.session, f.dataset, None, 1).unwrap();
    assert_eq!(newest.items.len(), 1);
    assert_eq!(newest.items[0].status, WorkflowStatus::Running);
    let cursor = newest.next_cursor.unwrap();
    assert_eq!(cursor, newest.items[0].run_id);
    let rest = f.store.list(f.session, f.dataset, Some(cursor), 2).unwrap();
    assert_eq!(rest.items.len(), 2);
    assert!(rest.next_cursor.is_none());
    assert_eq!(rest.items[1].run_id, f.ready.run_id);
    assert_eq!(
        newest
            .items
            .into_iter()
            .chain(rest.items)
            .map(|run| run.run_id)
            .collect::<BTreeSet<_>>(),
        expected
    );

    f.workspace.close(f.session).unwrap();
    let grant = f.workspace.grant_selected_directory(f.root).unwrap();
    let reopened = f
        .workspace
        .open(grant.grant_id)
        .unwrap()
        .workspace_session_id;
    assert!(f.store.list(f.session, f.dataset, None, 20).is_err());
    let recovered = f.store.list(reopened, f.dataset, None, 20).unwrap();
    for run in &recovered.items[..2] {
        assert_eq!(run.status, WorkflowStatus::Interrupted);
        assert_eq!(run.error_code.as_deref(), Some("SESSION_ENDED"));
        assert_eq!(run.profile_task.as_ref().unwrap().status, "INTERRUPTED");
    }
    assert_eq!(recovered.items[2].status, WorkflowStatus::Ready);
    f.workspace.close(reopened).unwrap();
}
