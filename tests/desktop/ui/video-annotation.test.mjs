import assert from "node:assert/strict";
import { test } from "node:test";
import { keyframeChoices, locateDraft, segmentInput } from "../../../apps/desktop/src/video-annotation.ts";

const frame = (index, time, digest = "source") => ({
  source_video_id: "video", source_digest: digest, stream_index: 0,
  frame_index: index, pts_us: time, width: 48, height: 32,
});
const saved = (id, f) => ({ frame_ref: f, annotation_ref: { artifact_id: id, owner: { kind: "ASSET", id: "video" } } });
const first = saved("first", frame("2", "400000"));
const old = saved("old", frame("3", "600000"));
const current = saved("current", frame("3", "600000"));
const manifest = {
  unassigned_keyframe_drafts: [{ ...current, target_dataset_id: "target" }, { ...first, target_dataset_id: "other" }],
  segments: [{ target_dataset_id: "target", keyframes: [first, old, first] }],
};

test("a resolved frame reopens the newest unassigned draft in the chosen dataset", () => {
  assert.equal(locateDraft(manifest, "target", frame("3", "600000")), current.annotation_ref);
  assert.equal(locateDraft(manifest, "target", frame("2", "400000")), first.annotation_ref);
  assert.equal(locateDraft(manifest, "target", frame("3", "600000", "changed")), null);
  assert.equal(locateDraft(manifest, "other", frame("3", "600000")), null);
  assert.deepEqual(keyframeChoices(manifest, "target").map(k => k.annotation_ref.artifact_id), ["first", "current", "old"]);
});

test("segment input uses exact integers and excludes the end boundary and duplicate frames", () => {
  assert.deepEqual(segmentInput("400000", "600000", [first]), { start_us: "400000", end_us: "600000", keyframes: [first] });
  assert.throws(() => segmentInput("400000", "600000", [current]), /区间/);
  assert.throws(() => segmentInput("600000", "600001", [current, old]), /同一帧/);
  assert.throws(() => segmentInput("00", "20", []), /微秒/);
  assert.throws(() => segmentInput("0", "9223372036854775808", []), /微秒/);
  assert.throws(() => segmentInput("20", "20", []), /结束/);
  const exact = saved("exact", frame("9", "9007199254740993"));
  assert.equal(segmentInput("9007199254740993", "9007199254740994", [exact]).keyframes[0], exact);
});
