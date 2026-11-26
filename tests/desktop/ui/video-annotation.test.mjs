import assert from "node:assert/strict";
import { test } from "node:test";
import {
  datasetSamplingConfig, keyframeChoices, locateDraft, segmentInput,
  matchingTrackingReceipt, trackingObservationsAtFrame, trackingSamplingConfig,
} from "../../../apps/desktop/src/video-annotation.ts";

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

test("training sampling submits only the active mode and an optional positive limit", () => {
  assert.deepEqual(datasetSamplingConfig("TIME_INTERVAL", "1000", true, "20"),
    { mode: "TIME_INTERVAL", interval_ms: 1000, include_keyframes: true, max_frames: 20 });
  assert.deepEqual(datasetSamplingConfig("EVERY_N_FRAMES", "3", false, ""),
    { mode: "EVERY_N_FRAMES", every_n: 3, include_keyframes: false });
  assert.throws(() => datasetSamplingConfig("TIME_INTERVAL", "3600001", false, ""), /间隔/);
  assert.throws(() => datasetSamplingConfig("EVERY_N_FRAMES", "01", false, ""), /间隔/);
  assert.throws(() => datasetSamplingConfig("TIME_INTERVAL", "400", true, "0"), /上限/);
});

test("tracking has its own sampling limits and sends only the selected interval", () => {
  assert.deepEqual(trackingSamplingConfig("EVERY_N_FRAMES", "1000"),
    { mode: "EVERY_N_FRAMES", every_n: 1000 });
  assert.deepEqual(trackingSamplingConfig("TIME_INTERVAL", "600"),
    { mode: "TIME_INTERVAL", interval_ms: 600 });
  assert.throws(() => trackingSamplingConfig("EVERY_N_FRAMES", "1001"), /间隔/);
  assert.throws(() => trackingSamplingConfig("TIME_INTERVAL", "0"), /间隔/);
});

test("tracking candidates bind to the exact video and display frame identity", () => {
  const target = frame("9007199254740993", "9007199254740994");
  const observed = { frame_index: target.frame_index, source: "TRACKER" };
  const tracks = {
    source_video_id: "video", source_digest: "source", stream_index: 0,
    observations: [observed, { frame_index: "9007199254740992", source: "HUMAN_SEED" }],
  };
  assert.deepEqual(trackingObservationsAtFrame(tracks, target), [observed]);
  assert.deepEqual(trackingObservationsAtFrame(tracks, { ...target, source_digest: "changed" }), []);
  assert.deepEqual(trackingObservationsAtFrame(tracks, { ...target, stream_index: 1 }), []);
  assert.deepEqual(trackingObservationsAtFrame(tracks, null), []);
});

test("sampling can offer only a tracking receipt from the exact saved segment and category scope", () => {
  const tracks = {
    segment_id: "segment", segment_revision: "9007199254740993", stream_index: 4,
    source_video_id: "video", source_digest: "source", target_dataset_id: "training",
    label_schema_digest: "labels",
  };
  const segment = {
    segment_id: "segment", revision: "9007199254740993",
    source_video_id: "video", source_digest: "source", target_dataset_id: "training",
    keyframes: [{ frame_ref: { stream_index: 4 } }],
  };
  assert.equal(matchingTrackingReceipt(tracks, segment, "labels"), tracks);
  for (const change of [
    { segment_id: "other" }, { revision: "9007199254740992" }, { keyframes: [{ frame_ref: { stream_index: 5 } }] },
    { source_video_id: "other" }, { source_digest: "changed" }, { target_dataset_id: "other" },
  ]) {
    assert.equal(matchingTrackingReceipt(tracks, { ...segment, ...change }, "labels"), null);
  }
  assert.equal(matchingTrackingReceipt(tracks, segment, "new labels"), null);
  assert.equal(matchingTrackingReceipt(tracks, undefined, "labels"), null);
});
