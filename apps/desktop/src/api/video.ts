import { invoke } from "@tauri-apps/api/core";
import type { PathGrant, PathSelectRequest, VideoImportRequest, VideoImportView, VideoGetRequest, VideoDetail, AssetListRequest, AssetPage } from "./generated/wire-primitives";
import type { DisplayLocator, FrameResolution, MediaGetLocatorRequest, VideoFrame, VideoFrameTarget, VideoResolveFrameRequest, VideoSeekPolicy } from "./generated/wire-primitives";
import type {
  ArtifactReference, CocoDocument, ImportOperationRef, KeyframeDraftReceipt, SegmentReceipt,
  VideoGetKeyframeDraftRequest, VideoSampleSubmitRequest, VideoSaveKeyframeDraftRequest, VideoSaveSegmentRequest,
  VideoTrackSubmitRequest,
} from "./generated/wire-primitives";
import { context } from "./workspace";
import type { OpenWorkspace } from "./workspace";

export function getKeyframeDraft(workspace: OpenWorkspace, video: string, annotation: ArtifactReference): Promise<CocoDocument> {
  const request: VideoGetKeyframeDraftRequest = {
    context: context(workspace, crypto.randomUUID()), source_video_id: video, annotation_ref: annotation,
  };
  return invoke("video_get_keyframe_draft", { request });
}

export function saveKeyframeDraft(workspace: OpenWorkspace, fields: Omit<VideoSaveKeyframeDraftRequest, "context">): Promise<KeyframeDraftReceipt> {
  const request: VideoSaveKeyframeDraftRequest = { ...fields, context: context(workspace, crypto.randomUUID()) };
  return invoke("video_save_keyframe_draft", { request });
}

export function saveSegment(workspace: OpenWorkspace, fields: Omit<VideoSaveSegmentRequest, "context">): Promise<SegmentReceipt> {
  const request: VideoSaveSegmentRequest = { ...fields, context: context(workspace, crypto.randomUUID()) };
  return invoke("video_save_segment", { request });
}

export function submitVideoSamples(workspace: OpenWorkspace, fields: Omit<VideoSampleSubmitRequest, "context">,
  idempotencyKey: string): Promise<ImportOperationRef> {
  const request: VideoSampleSubmitRequest = {
    ...fields, context: { ...context(workspace, crypto.randomUUID()), idempotency_key: idempotencyKey },
  };
  return invoke("video_sample_submit", { request });
}

export function submitVideoTracking(workspace: OpenWorkspace, fields: Omit<VideoTrackSubmitRequest, "context">,
  idempotencyKey: string): Promise<ImportOperationRef> {
  const request: VideoTrackSubmitRequest = {
    ...fields, context: { ...context(workspace, crypto.randomUUID()), idempotency_key: idempotencyKey },
  };
  return invoke("video_track_submit", { request });
}

export function pickVideoFile(workspace: OpenWorkspace): Promise<PathGrant | null> {
  const requestId = crypto.randomUUID();
  const request: PathSelectRequest = {
    request_id: requestId,
    context: context(workspace, requestId),
    purpose: "IMPORT",
    kind: "FILE",
    access: "READ",
  };
  return invoke("path_select", { request });
}

export function importVideo(
  workspace: OpenWorkspace, fileGrantId: string, idempotencyKey: string, streamIndex: number | null,
): Promise<VideoImportView> {
  const request: VideoImportRequest = {
    context: { ...context(workspace, crypto.randomUUID()), idempotency_key: idempotencyKey },
    file_grant_id: fileGrantId,
    display_name: null,
    stream_index: streamIndex,
  };
  return invoke("video_import", { request });
}

export function getVideo(workspace: OpenWorkspace, sourceVideoId: string): Promise<VideoDetail> {
  const request: VideoGetRequest = {
    context: context(workspace, crypto.randomUUID()), source_video_id: sourceVideoId,
  };
  return invoke("video_get", { request });
}

export function listVideos(workspace: OpenWorkspace, cursor?: string): Promise<AssetPage> {
  const request: AssetListRequest = {
    context: context(workspace, crypto.randomUUID()), kind: "VIDEO", cursor, page_size: 25,
  };
  return invoke("asset_list", { request });
}

export function resolveVideoFrame(
  workspace: OpenWorkspace, sourceVideoId: string, target: VideoFrameTarget,
  seekPolicy: VideoSeekPolicy, idempotencyKey: string,
): Promise<FrameResolution> {
  const request: VideoResolveFrameRequest = {
    context: { ...context(workspace, crypto.randomUUID()), idempotency_key: idempotencyKey },
    source_video_id: sourceVideoId, target, seek_policy: seekPolicy,
  };
  return invoke("video_resolve_frame", { request });
}

export function getFrameImageLocator(workspace: OpenWorkspace, frame: VideoFrame): Promise<DisplayLocator> {
  if (frame.image_ref.owner.kind !== "TASK") throw new Error("帧产物归属无效。");
  const request: MediaGetLocatorRequest = {
    context: context(workspace, crypto.randomUUID()), purpose: "DISPLAY",
    artifact_id: frame.image_ref.artifact_id, owner_task_id: frame.image_ref.owner.id,
  };
  return invoke("media_get_locator", { request });
}
