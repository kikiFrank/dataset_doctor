import assert from "node:assert/strict";
import { test } from "node:test";
import {
  adoptTrackingBox, makeDraft, seedBoxes, boxFromDrag, translateBoxFromDrag, trackSelections,
} from "../../../apps/desktop/src/review-document.ts";

const image = { id: 1, file_name: "image.png", width: 200, height: 100 };
const categories = [{ id: 1, name: "helmet" }, { id: 2, name: "vest" }];

test("dragging an existing box translates original pixels and clamps without resizing", () => {
  const original = [30, 20, 40, 30];
  assert.deepEqual(translateBoxFromDrag(original, [50, 35], [64, 47], image), [44, 32, 40, 30]);
  assert.deepEqual(translateBoxFromDrag(original, [50, 35], [315, -20], image), [160, 0, 40, 30]);
  assert.deepEqual(translateBoxFromDrag([190, 20, 40, 30], [195, 35], [140, 35], image), [135, 20, 40, 30]);
  assert.equal(translateBoxFromDrag(original, [50, 35], [50, 35], image), null);
  assert.equal(translateBoxFromDrag([0, 0, 201, 30], [50, 35], [60, 35], image), null);
  assert.deepEqual(original, [30, 20, 40, 30]);
});

test("explicit adoption creates one editable box and keeps provenance outside COCO", () => {
  const candidate = { track_id: "track-a", source: "TRACKER", category_id: "2", bbox_xywh: [11, 9, 5, 5] };
  const original = [{ id: 1, bbox: [0, 0, 4, 4], category_id: 1, iscrowd: 0 }];
  const edited = adoptTrackingBox(original, candidate, categories);
  assert.equal(original.length, 1);
  assert.deepEqual(edited[1], { id: 2, bbox: [11, 9, 5, 5], category_id: 2, iscrowd: 0, track_id: "track-a" });
  assert.equal(adoptTrackingBox(edited, candidate, categories), edited);
  assert.equal(adoptTrackingBox(original, { ...candidate, source: "HUMAN_SEED" }, categories), original);
  assert.equal(adoptTrackingBox(original, { ...candidate, category_id: "9007199254740993" }, categories), original);
  const bindings = trackSelections(edited);
  assert.deepEqual(bindings, [{ local_annotation_id: 2, track_id: "track-a" }]);
  const draft = makeDraft(image, categories, edited);
  assert.equal(draft.annotations[1].track_id, undefined);
  assert.deepEqual(trackSelections(seedBoxes(draft, categories, bindings)), bindings);
  assert.deepEqual(trackSelections(seedBoxes(draft, categories)), []);
  assert.deepEqual(trackSelections(edited.slice(0, 1)), []);
});

test("maps local category by exact name and preserves the correctable original box", () => {
  const original = {
    images: [{ ...image, width: 100 }],
    categories: [{ id: 2, name: "helmet" }],
    annotations: [{ id: 7, image_id: 1, category_id: 2, bbox: [190, 0, 20, 10], area: 200, iscrowd: 0 }],
  };
  const [box] = seedBoxes(original, categories);
  assert.equal(box.category_id, 1);
  assert.deepEqual(box.bbox, [190, 0, 20, 10]);
  assert.throws(() => makeDraft(image, categories, [box]), /范围/);
  const document = makeDraft(image, categories, [{ ...box, bbox: [190, 0, 10, 10] }]);
  assert.equal(document.images[0].width, 200);
  assert.deepEqual(document.annotations[0], {
    id: 7, image_id: 1, category_id: 1, bbox: [190, 0, 10, 10], area: 100, iscrowd: 0,
  });
});

test("requires explicit correction for unresolved categories and wrong image references", () => {
  const box = { id: 1, image_id: 1, category_id: 5, bbox: [0, 0, 10, 10], area: 100, iscrowd: 0 };
  const base = { images: [image], categories: [{ id: 5, name: "Helmet" }], annotations: [box] };
  assert.equal(seedBoxes(base, categories)[0].category_id, null);
  const wrongImage = {
    ...base,
    categories: [{ id: 5, name: "helmet" }],
    annotations: [{ ...box, image_id: 2 }],
  };
  assert.equal(seedBoxes(wrongImage, categories)[0].category_id, null);
  assert.throws(() => makeDraft(image, categories, seedBoxes(wrongImage, categories)), /类别/);
  assert.deepEqual(makeDraft(image, categories, []).annotations, []);
});

test("draws in image pixels on reverse drag and clamps at the image boundary", () => {
  assert.deepEqual(boxFromDrag([220, 90], [-5, 22], image), [0, 22, 200, 68]);
  assert.equal(boxFromDrag([0, 0], [0, 22], image), null);
  const [box] = seedBoxes({
    images: [image],
    categories,
    annotations: [{ id: 1, image_id: 1, category_id: 1, bbox: [0, 0, 10, 10], area: 100, iscrowd: 0 }],
  }, categories);
  assert.throws(() => makeDraft(image, categories, [box, box]), /编号/);
  assert.throws(() => makeDraft(image, categories, [{ ...box, bbox: [NaN, 0, 10, 10] }]), /范围/);
});
