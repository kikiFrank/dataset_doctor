import { invoke } from "@tauri-apps/api/core";
import type { PathGrant, PathSelectRequest, VideoImportRequest, VideoImportView, VideoGetRequest, VideoDetail } from "./generated/wire-primitives";
import { context } from "./workspace";
import type { OpenWorkspace } from "./workspace";

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
