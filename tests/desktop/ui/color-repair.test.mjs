import assert from "node:assert/strict";
import { test } from "node:test";
import {
  colorIntent, canApproveColor,
} from "../../../apps/desktop/src/color-repair.ts";

const rectangle = {
  shape: "RECT", bbox: ["1", "0", "3", "2"], points: "",
  method: "SOLID_FILL", target: "#111e5a", strength: "0.5", feather: "1.5",
  hueGate: false, hueMin: "350", hueMax: "10",
};

test("color requests use original pixel coordinates and the selected sRGB procedure", () => {
  assert.deepEqual(colorIntent(rectangle, { width: 8, height: 5 }), {
    region: { kind: "RECT", bbox_xywh: [1, 0, 3, 2] },
    operation: { method: "SOLID_FILL", target_rgb: [17, 30, 90], strength: 0.5, feather_px: 1.5 },
  });
  assert.deepEqual(colorIntent({
    ...rectangle, shape: "POLYGON", points: "0,0\n5,0\n5,3\n0,3",
    method: "HUE_REMAP", strength: "0", hueGate: true,
  }, { width: 8, height: 5 }), {
    region: { kind: "POLYGON", points_xy: [[0, 0], [5, 0], [5, 3], [0, 3]] },
    operation: { method: "HUE_REMAP", target_rgb: [17, 30, 90], strength: 0,
      feather_px: 1.5, source_color_gate: { hue_min_deg: 350, hue_max_deg: 10, wraps_zero: true } },
  });
  for (const change of [
    { bbox: ["6", "0", "3", "2"] }, { bbox: ["1", "0", "0", "2"] },
    { shape: "POLYGON", points: "0,0\n1,1\n0,0" },
    { target: "#543" }, { strength: "NaN" }, { feather: "65" },
    { hueGate: true, hueMin: "360" },
  ]) {
    assert.throws(() => colorIntent({ ...rectangle, ...change }, { width: 8, height: 5 }));
  }
});

test("approval requires the displayed candidate for this asset and exact recipe", () => {
  const scope = { asset_id: "image", dataset_id: "set", base_image_digest: "source" };
  const receipt = {
    ...scope, changed_pixels: "3",
    candidate_ref: { artifact_id: "candidate", sha256: "candidate-proof", kind: "REPAIR_CANDIDATE",
      owner: { kind: "TASK", id: "task" } },
  };
  const approved = { preview: receipt, intent: "recipe", renderedDigest: "candidate-proof" };
  assert.equal(canApproveColor(approved, scope, "recipe"), true);
  assert.equal(canApproveColor(approved, scope, "other recipe"), false);
  assert.equal(canApproveColor(approved, { ...scope, asset_id: "wrong image" }, "recipe"), false);
  assert.equal(canApproveColor(approved, { ...scope, base_image_digest: "changed" }, "recipe"), false);
  assert.equal(canApproveColor({ ...approved, renderedDigest: null }, scope, "recipe"), false);
  assert.equal(canApproveColor({ ...approved, renderedDigest: "different pixels" }, scope, "recipe"), false);
  assert.equal(canApproveColor({ ...approved, preview: { ...receipt, changed_pixels: "0" } }, scope, "recipe"), false);
});
