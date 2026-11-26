use std::fs;
use std::path::Path;
use std::time::Duration;

use dd_infra::artifact_manifest::{TaskManifestScope, verify_task_result_manifest};
use dd_infra::json::JsonContracts;
use dd_infra::task_store::format_digest;
use dd_protocol::v1::{
    TrackingFrameInput, TrackingSeedInput, VideoProbeInput, VideoProbeRequest, VideoTrackingInput,
    VideoTrackingRequest,
};
use serde_json::{Value, json};
use uuid::Uuid;

#[path = "support/worker_task.rs"]
mod worker_task;
use worker_task::{WorkerTask, digest, file_locator, run_root};
#[path = "support/tracking_video_fixture.rs"]
mod tracking_video_fixture;

const PROBE_DOMAIN: &[u8] = b"datasetdoctor.v1.PythonWorker.StartVideoProbe\0";
const TRACK_DOMAIN: &[u8] = b"datasetdoctor.v1.PythonWorker.StartTracking\0";

#[tokio::test]
async fn authorized_tracking_uses_the_frozen_display_frames_and_seals_candidates() {
    let run = run_root();
    let source = run.join("Tracking fixture.mkv");
    tracking_video_fixture::make_motion_clip(&source);
    let original = fs::read(&source).unwrap();
    let sha = digest(&original);
    let video = Uuid::new_v4();
    let segment = Uuid::new_v4();
    let profile = Uuid::new_v4();
    let track = Uuid::new_v4();
    let index = probe_index(&source, &original, video, sha).await;
    let frame_inputs: Vec<_> = [0, 2, 4, 5]
        .into_iter()
        .map(|offset| {
            let frame = index.resolve_index(sha, offset).unwrap();
            TrackingFrameInput {
                frame_index: Some(frame.frame_index),
                expected_raw_pts: frame.raw_pts,
                display_width: frame.width,
                display_height: frame.height,
            }
        })
        .collect();
    let seed = TrackingSeedInput {
        track_id: track.to_string(),
        category_id: 3,
        bbox_xywh: vec![8.0, 8.0, 5.0, 5.0],
    };
    let tail = TrackingSeedInput {
        track_id: track.to_string(),
        category_id: 3,
        bbox_xywh: vec![15.0, 11.0, 5.0, 5.0],
    };
    for case in ["success", "wrong pts", "changed config", "changed limits"] {
        let mut frames = frame_inputs.clone();
        if case == "wrong pts" {
            frames[1].expected_raw_pts = Some(1);
        }
        let parameters = json!({
            "segment_id":segment,"tracker_profile_id":profile,"stream_index":0,
            "frames":frames.iter().map(|frame| json!({
                "frame_index":frame.frame_index.unwrap().to_string(),
                "raw_pts":frame.expected_raw_pts.map(|value|value.to_string()),
                "width":frame.display_width,"height":frame.display_height,
            })).collect::<Vec<_>>(),
            "seeds":[{"track_id":track,"category_id":"3","bbox_xywh":seed.bbox_xywh}],
            "tail_anchors":[{"track_id":track,"category_id":"3","bbox_xywh":tail.bbox_xywh}],
            "policy":{"search_radius":6,"max_error":0.06,"min_margin":0.01},
            "limits":{"max_pixels":"884","max_frames":12,"timeout_ms":10_000},
        });
        let task = WorkerTask::new(
            case,
            "VIDEO_TRACK",
            "video_tracking.v1",
            video,
            &sha,
            json!({"tracking_binding":parameters, "segment_revision":"1"}),
        );
        let input = VideoTrackingInput {
            source_video: Some(file_locator(&source, &original)),
            input_manifest: Some(task.manifest_locator()),
            output_directory: task.output.to_str().unwrap().into(),
            stream_index: 0,
            frames,
            segment_id: segment.to_string(),
            tracker_profile_id: profile.to_string(),
            seeds: vec![seed.clone()],
            tail_anchors: vec![tail.clone()],
            search_radius: if case == "changed config" { 5 } else { 6 },
            max_error: 0.06,
            min_margin: 0.01,
            max_pixels: 34 * 26,
            max_frames: if case == "changed limits" { 13 } else { 12 },
            timeout_ms: 10_000,
        };
        let (context, manifest) = task.dispatch(TRACK_DOMAIN, &input);
        let mut worker = task.launch().await;
        let handle = worker
            .worker
            .start_tracking(VideoTrackingRequest {
                context: Some(context),
                input_manifest_ref: Some(manifest),
                parameters: Some(input),
            })
            .await
            .unwrap();
        let outcome = worker.wait(&handle, Duration::from_secs(20)).await;
        if case == "success" {
            assert_eq!(outcome.status, "SUCCEEDED", "{outcome:?}");
            assert_eq!(task.outcome().status, "SUCCEEDED");
            let scope = scope(&task, &handle.attempt_id);
            let seal = verify_task_result_manifest(
                &task.output,
                &JsonContracts::new().unwrap(),
                &scope,
                outcome.result_manifest_ref.as_ref().unwrap(),
            )
            .unwrap();
            let [entry] = seal.entries() else {
                panic!("expected one TrackBundle");
            };
            assert_eq!(entry.kind, "VIDEO_TRACK_BUNDLE");
            let result: Value =
                serde_json::from_slice(&fs::read(task.output.join(&entry.relative_path)).unwrap())
                    .unwrap();
            assert_eq!(result["source_video_id"], video.to_string());
            assert_eq!(result["source_digest"], format_digest(&sha));
            assert_eq!(result["segment_id"], segment.to_string());
            assert_eq!(result["tracker_profile_id"], profile.to_string());
            assert_eq!(result["processed_frame_count"], 4);
            assert_eq!(result["observations"][0]["source"], "HUMAN_SEED");
            assert_eq!(result["observations"][1]["source"], "TRACKER");
            assert_eq!(result["observations"][1]["frame_index"], "2");
            assert_eq!(
                result["observations"][1]["bbox_xywh"],
                json!([11.0, 9.0, 5.0, 5.0])
            );
            assert_eq!(result["observations"][2]["track_id"], track.to_string());
            assert_eq!(result["observations"][2]["frame_index"], "4");
            assert_eq!(result["observations"].as_array().unwrap().len(), 3);
            assert_eq!(result["uncertain_ranges"][0]["first_frame_index"], "5");
            assert_eq!(result["uncertain_ranges"][0]["reason"], "TARGET_LOST");
            assert_eq!(result["tail_checks"][0]["status"], "NO_CANDIDATE");
        } else {
            assert_eq!(outcome.status, "FAILED", "{case}: {outcome:?}");
            assert_eq!(outcome.error_code, "input_changed", "{case}");
            assert!(fs::read_dir(&task.output).unwrap().next().is_none());
        }
        worker.shutdown().await;
    }
    assert_eq!(fs::read(source).unwrap(), original);
}

async fn probe_index(
    source: &Path,
    original: &[u8],
    video: Uuid,
    sha: [u8; 32],
) -> dd_domain::video::FrameIndex {
    let probe = WorkerTask::new(
        "Probe Tracking",
        "VIDEO_PROBE",
        "video_probe.v1",
        video,
        &sha,
        json!({"stream_index":0}),
    );
    let input = VideoProbeInput {
        source_video: Some(file_locator(source, original)),
        input_manifest: Some(probe.manifest_locator()),
        output_directory: probe.output.to_str().unwrap().into(),
        stream_index: Some(0),
        max_pixels: 34 * 26,
        max_frames: 12,
        timeout_ms: 10_000,
    };
    let (context, manifest) = probe.dispatch(PROBE_DOMAIN, &input);
    let mut worker = probe.launch().await;
    let handle = worker
        .worker
        .start_video_probe(VideoProbeRequest {
            context: Some(context),
            input_manifest_ref: Some(manifest),
            parameters: Some(input),
        })
        .await
        .unwrap();
    let result = worker.wait(&handle, Duration::from_secs(15)).await;
    assert_eq!(result.status, "SUCCEEDED", "{result:?}");
    let parsed = dd_infra::video_index::read_video_index(
        &probe.output,
        result.result_manifest_ref.as_ref().unwrap(),
        &scope(&probe, &handle.attempt_id),
        video,
        sha,
        Some(0),
    )
    .unwrap();
    worker.shutdown().await;
    parsed
}

fn scope(task: &WorkerTask, attempt: &str) -> TaskManifestScope {
    TaskManifestScope {
        task: task.task_id,
        attempt: Uuid::parse_str(attempt).unwrap(),
        input_fingerprint: task.fingerprint,
    }
}
