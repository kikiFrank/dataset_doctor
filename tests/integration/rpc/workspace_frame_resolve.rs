use std::fs;
use std::process::Command;
use std::sync::Arc;

use dd_domain::video::SeekPolicy;
use dd_infra::frame_runtime::{FrameResolveRuntime, FrameService, FrameSubmission, FrameTarget};
use uuid::Uuid;

#[path = "support/workspace_video.rs"]
mod workspace_video;
use workspace_video::Fixture;

#[tokio::test]
async fn workspace_frame_tasks_decode_the_selected_pixels_and_reopen_a_verified_cache() {
    let fixture = Fixture::create().await;
    let Fixture {
        root,
        source,
        original,
        owner,
        session,
        video_id,
        python,
        entry,
    } = fixture;
    let service = FrameService::new(Arc::clone(&owner)).unwrap();
    let request = FrameSubmission {
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        video_id,
        target: FrameTarget::TimestampMs(401),
        policy: SeekPolicy::AtOrAfter,
    };
    assert!(service.submit(Uuid::new_v4(), request, 210).is_err());
    assert!(
        service
            .submit(
                session,
                FrameSubmission {
                    target: FrameTarget::Index(100),
                    ..request
                },
                210
            )
            .is_err()
    );
    let (queued, job) = service.submit(session, request, 220).unwrap();
    assert_eq!(queued.status, "QUEUED");
    assert!(queued.frame.is_none());
    let (same, duplicate) = service.submit(session, request, 221).unwrap();
    assert_eq!(same.task_id, queued.task_id);
    assert_eq!(same.accepted_at_ms, queued.accepted_at_ms);
    assert!(duplicate.is_none());
    assert!(
        service
            .submit(
                session,
                FrameSubmission {
                    target: FrameTarget::Index(0),
                    ..request
                },
                222
            )
            .is_err()
    );
    let mut runtime = FrameResolveRuntime::launch(Arc::clone(&owner), session, python, entry)
        .await
        .unwrap();
    assert_eq!(runtime.run(job.unwrap()).await.unwrap().status, "SUCCEEDED");
    runtime.shutdown().await.unwrap();
    let completed = service.snapshot(session, queued.task_id).unwrap();
    let frame = completed.frame.unwrap();
    assert_eq!(frame.frame.frame_index, 3);
    assert_eq!(frame.frame.pts_us, Some(600_000));
    assert_eq!(frame.reference.media_type, "image/png");
    let png = root.join(&frame.relative_path);
    let pixels = Command::new(std::env::var_os("DD_WORKER_PYTHON").unwrap())
        .args(["-I", "-B", "-c", "import sys; from PIL import Image; im=Image.open(sys.argv[1]); im.load(); r,g,b=im.getpixel((20,12)); assert im.size==(48,32) and b>240 and max(r,g)<12"])
        .arg(&png).status().unwrap();
    assert!(pixels.success());
    owner.close(session).unwrap();
    let folder = owner.grant_selected_directory(root).unwrap();
    let reopened = owner.open(folder.grant_id).unwrap().workspace_session_id;
    let (cached, next_job) = service
        .submit(
            reopened,
            FrameSubmission {
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                target: FrameTarget::Index(3),
                ..request
            },
            300,
        )
        .unwrap();
    assert_eq!(cached.task_id, queued.task_id);
    assert!(cached.frame.is_some());
    assert!(next_job.is_none());
    assert!(service.snapshot(session, queued.task_id).is_err());
    fs::write(&png, b"altered frame bytes").unwrap();
    assert!(service.snapshot(reopened, queued.task_id).is_err());
    fs::write(&source, b"altered source video").unwrap();
    assert!(
        service
            .submit(
                reopened,
                FrameSubmission {
                    idempotency_key: Uuid::new_v4(),
                    ..request
                },
                310
            )
            .is_err()
    );
    fs::write(&source, &original).unwrap();
    assert_eq!(fs::read(source).unwrap(), original);
}

#[tokio::test]
async fn queued_frame_tasks_preserve_source_changes_and_reopen_as_interrupted() {
    let fixture = Fixture::create().await;
    let service = FrameService::new(Arc::clone(&fixture.owner)).unwrap();
    let request = FrameSubmission {
        request_id: Uuid::new_v4(),
        idempotency_key: Uuid::new_v4(),
        video_id: fixture.video_id,
        target: FrameTarget::Index(0),
        policy: SeekPolicy::Nearest,
    };
    let (pending, job) = service.submit(fixture.session, request, 200).unwrap();
    fs::write(&fixture.source, b"external synthetic video edit").unwrap();
    let mut runtime = FrameResolveRuntime::launch(
        Arc::clone(&fixture.owner),
        fixture.session,
        fixture.python.clone(),
        fixture.entry.clone(),
    )
    .await
    .unwrap();
    let failed = runtime.run(job.unwrap()).await.unwrap();
    assert_eq!(failed.status, "FAILED");
    assert_eq!(failed.error_code.as_deref(), Some("INPUT_CHANGED"));
    assert_eq!(
        fs::read(&fixture.source).unwrap(),
        b"external synthetic video edit"
    );
    assert!(
        service
            .snapshot(fixture.session, pending.task_id)
            .unwrap()
            .frame
            .is_none()
    );
    runtime.shutdown().await.unwrap();
    fs::write(&fixture.source, &fixture.original).unwrap();
    let (queued, _) = service
        .submit(
            fixture.session,
            FrameSubmission {
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                target: FrameTarget::Index(1),
                ..request
            },
            300,
        )
        .unwrap();
    fixture.owner.close(fixture.session).unwrap();
    let directory = fixture
        .owner
        .grant_selected_directory(fixture.root)
        .unwrap();
    let session = fixture
        .owner
        .open(directory.grant_id)
        .unwrap()
        .workspace_session_id;
    let interrupted = service.snapshot(session, queued.task_id).unwrap();
    assert_eq!(interrupted.status, "INTERRUPTED");
    assert_eq!(interrupted.error_code.as_deref(), Some("SESSION_ENDED"));
    assert!(interrupted.frame.is_none());
    let (_, job) = service
        .submit(
            session,
            FrameSubmission {
                request_id: Uuid::new_v4(),
                idempotency_key: Uuid::new_v4(),
                ..request
            },
            400,
        )
        .unwrap();
    let mut runtime = FrameResolveRuntime::launch(
        Arc::clone(&fixture.owner),
        session,
        fixture.python,
        fixture.entry,
    )
    .await
    .unwrap();
    fs::remove_file(&fixture.source).unwrap();
    let failed = runtime.run(job.unwrap()).await.unwrap();
    assert_eq!(failed.status, "FAILED");
    assert_eq!(failed.error_code.as_deref(), Some("FILE_IO_FAILED"));
    runtime.shutdown().await.unwrap();
    fs::write(fixture.source, fixture.original).unwrap();
}
