use dd_domain::video::SeekPolicy;
use dd_infra::frame_runtime::{FrameResolveRuntime, FrameService, FrameSubmission, FrameTarget};
use dd_infra::video_annotation_store::VideoFrameReference;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;
#[path = "workspace_video.rs"]
mod workspace_video;
pub(crate) use workspace_video::Fixture;

pub async fn prepared() -> (Fixture, Uuid, VideoFrameReference) {
    let fixture = Fixture::create().await;
    let dataset = Uuid::new_v4();
    Connection::open(fixture.root.join("workspace.db"))
        .unwrap()
        .execute(
            "INSERT INTO datasets(dataset_id,name,categories_json,created_at_ms,updated_at_ms)
         VALUES (?1,'Target','[{\"id\":1,\"name\":\"signal\"}]',100,100)",
            [dataset.to_string()],
        )
        .unwrap();
    let frames = FrameService::new(Arc::clone(&fixture.owner)).unwrap();
    let (task, job) = frames
        .submit(
            fixture.session,
            FrameSubmission {
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                video_id: fixture.video_id,
                target: FrameTarget::Index(3),
                policy: SeekPolicy::Nearest,
            },
            250,
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
    let frame = VideoFrameReference::from_resolved(task.task_id, &resolved).unwrap();
    (fixture, dataset, frame)
}

pub fn document() -> Value {
    json!({"images":[{"id":1,"file_name":"frame.png","width":48,"height":32}],
        "categories":[{"id":1,"name":"signal"}],
        "annotations":[{"id":1,"image_id":1,"category_id":1,"bbox":[2,3,10,8],"area":999,"iscrowd":0}]})
}
