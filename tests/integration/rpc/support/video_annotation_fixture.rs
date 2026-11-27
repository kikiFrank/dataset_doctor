use dd_domain::video::SeekPolicy;
use dd_infra::frame_runtime::{FrameResolveRuntime, FrameService, FrameSubmission, FrameTarget};
use dd_infra::video_annotation_store::VideoFrameReference;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;
#[path = "workspace_video.rs"]
pub(crate) mod workspace_video;
pub(crate) use workspace_video::Fixture;

pub async fn prepared() -> (Fixture, Uuid, VideoFrameReference) {
    prepared_from(Fixture::create().await).await
}

pub async fn prepared_from(fixture: Fixture) -> (Fixture, Uuid, VideoFrameReference) {
    let dataset = Uuid::new_v4();
    Connection::open(fixture.root.join("workspace.db"))
        .unwrap()
        .execute(
            "INSERT INTO datasets(dataset_id,name,categories_json,created_at_ms,updated_at_ms)
         VALUES (?1,'Target','[{\"id\":1,\"name\":\"signal\"}]',100,100)",
            [dataset.to_string()],
        )
        .unwrap();
    let frame = resolve_frame(&fixture, 3, 250).await;
    (fixture, dataset, frame)
}

pub async fn resolve_frame(fixture: &Fixture, index: u64, accepted_at: i64) -> VideoFrameReference {
    let frames = FrameService::new(Arc::clone(&fixture.owner)).unwrap();
    let (task, job) = frames
        .submit(
            fixture.session,
            FrameSubmission {
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                video_id: fixture.video_id,
                target: FrameTarget::Index(index),
                policy: SeekPolicy::Nearest,
            },
            accepted_at,
        )
        .unwrap();
    let mut worker = FrameResolveRuntime::launch(
        Arc::clone(&fixture.owner),
        fixture.session,
        fixture.python.clone(),
        fixture.entry.clone(),
    )
    .await
    .unwrap();
    assert_eq!(worker.run(job.unwrap()).await.unwrap().status, "SUCCEEDED");
    worker.shutdown().await.unwrap();
    let resolved = frames
        .snapshot(fixture.session, task.task_id)
        .unwrap()
        .frame
        .unwrap();
    VideoFrameReference::from_resolved(task.task_id, &resolved).unwrap()
}

pub fn document() -> Value {
    json!({"images":[{"id":1,"file_name":"frame.png","width":48,"height":32}],
        "categories":[{"id":1,"name":"signal"}],
        "annotations":[{"id":1,"image_id":1,"category_id":1,"bbox":[2,3,10,8],"area":999,"iscrowd":0}]})
}
