import assert from "node:assert/strict";
import test from "node:test";
import { allChecks, selectChecks } from "../../scripts/ci/select-checks.mjs";

test("documentation alone needs no product runtime jobs", () => {
  assert.deepEqual(selectChecks(["README.md", "docs/development/mvp_progress.md"]), []);
  assert.deepEqual(selectChecks(["tests/rig-poc/src/main.rs"]), []);
});
test("a frontend change selects its type/build tests and actual desktop boundary", () => {
  assert.deepEqual(selectChecks(["apps/desktop/src/review-panel.tsx"]), ["typescript", "desktop"]);
});
test("a Worker change tests its code and the real Core and desktop consumers", () => {
  assert.deepEqual(selectChecks(["services/python-worker/src/dd_worker/video_decoder.py"]), ["python", "integration", "desktop"]);
});
test("storage tests do not rerun unrelated algorithm internals", () => {
  assert.deepEqual(selectChecks(["tests/storage/workspace/open.rs"]), ["sqlite"]);
});
test("shared locks, contracts and CI bootstrap require all affected checks", () => {
  for (const path of ["Cargo.lock", "contracts/proto/datasetdoctor/v1/worker_service.proto", "scripts/ci/prepare-windows.ps1", ".github/workflows/product-ci.yml", "config/toolchains/windows-x64.json"]) {
    assert.deepEqual(selectChecks([path]), allChecks, path);
  }
});
test("combined paths are stable and deduplicated, manual full runs remain possible", () => {
  assert.deepEqual(selectChecks(["tests/storage/a.rs", "apps/desktop/src/main.tsx", "tests/storage/a.rs"]), ["typescript", "sqlite", "desktop"]);
  assert.deepEqual(selectChecks([], { all: true }), allChecks);
});
test("new source directories cannot silently miss acceptance", () => {
  assert.deepEqual(selectChecks(["crates/dd-infra/src/review_final.rs"]), ["rust", "sqlite", "integration", "desktop"]);
  assert.deepEqual(selectChecks(["apps/new-product/src/main.rs"]), allChecks);
});
