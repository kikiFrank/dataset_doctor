import assert from "node:assert/strict";
import { test } from "node:test";
import {
  makeDraft, seedBoxes, boxFromDrag,
} from "../../../apps/desktop/src/review-document.ts";

const image = { id: 1, file_name: "image.png", width: 200, height: 100 };
const categories = [{ id: 1, name: "helmet" }, { id: 2, name: "vest" }];

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
