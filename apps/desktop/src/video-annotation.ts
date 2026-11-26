import type { VideoAnnotationManifest, VideoFrame, VideoKeyframe } from "./api/generated/wire-primitives";

function sameFrame(left: VideoFrame, right: VideoFrame): boolean {
  return left.source_video_id === right.source_video_id && left.source_digest === right.source_digest
    && left.stream_index === right.stream_index && left.frame_index === right.frame_index;
}

export function keyframeChoices(manifest: VideoAnnotationManifest | null, dataset: string): VideoKeyframe[] {
  if (!manifest) return [];
  const byArtifact = new Map<string, VideoKeyframe>();
  for (const { target_dataset_id, frame_ref, annotation_ref } of manifest.unassigned_keyframe_drafts) {
    if (target_dataset_id === dataset) byArtifact.set(annotation_ref.artifact_id, { frame_ref, annotation_ref });
  }
  for (const segment of manifest.segments) {
    if (segment.target_dataset_id !== dataset) continue;
    for (const keyframe of segment.keyframes) byArtifact.set(keyframe.annotation_ref.artifact_id, keyframe);
  }
  return [...byArtifact.values()].sort((left, right) => {
    const a = BigInt(left.frame_ref.frame_index);
    const b = BigInt(right.frame_ref.frame_index);
    return a < b ? -1 : a > b ? 1 : 0;
  });
}

export function locateDraft(manifest: VideoAnnotationManifest | null, dataset: string, frame: VideoFrame) {
  return keyframeChoices(manifest, dataset).find(keyframe => sameFrame(keyframe.frame_ref, frame))?.annotation_ref ?? null;
}

function nonnegativeMicros(value: string): bigint {
  if (value.length > 19 || !/^(0|[1-9][0-9]*)$/.test(value) || BigInt(value) > 9_223_372_036_854_775_807n) {
    throw new Error("请输入有效的非负整数微秒。");
  }
  return BigInt(value);
}

export function segmentInput(start_us: string, end_us: string, keyframes: VideoKeyframe[]) {
  const start = nonnegativeMicros(start_us);
  const end = nonnegativeMicros(end_us);
  if (end <= start) throw new Error("结束时间必须晚于开始时间。");
  const frames: VideoFrame[] = [];
  for (const { frame_ref } of keyframes) {
    if (frame_ref.pts_us === null || BigInt(frame_ref.pts_us) < start || BigInt(frame_ref.pts_us) >= end) {
      throw new Error("关键帧必须落在片段区间内，结束时刻不计入。");
    }
    if (frames.some(frame => sameFrame(frame, frame_ref))) throw new Error("同一帧只能选一个草稿。");
    frames.push(frame_ref);
  }
  return { start_us, end_us, keyframes };
}
