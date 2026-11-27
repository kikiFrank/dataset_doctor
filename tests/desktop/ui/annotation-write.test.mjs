import assert from "node:assert/strict";
import test from "node:test";
import { annotationWriteReady, annotationWriteGrantId } from "../../../apps/desktop/src/annotation-write-state.ts";

const now = Date.UTC(2026, 10, 27, 8);
const external = { authorization_required: true, read_only: false };
const grant = { grant_id: "opaque-write-grant", allowed_operations: ["REVIEW_COMMIT"], expires_at: new Date(now + 60_000).toISOString() };

test("external Final requires a live annotation-write grant", () => {
  assert.equal(annotationWriteReady(external, null, now), false);
  assert.equal(annotationWriteReady(external, grant, now), true);
  assert.equal(annotationWriteReady(external, { ...grant, allowed_operations: ["IMPORT_INSPECT"] }, now), false);
  assert.equal(annotationWriteReady(external, grant, now + 60_000), false);
  assert.throws(() => annotationWriteGrantId(external, null, now));
  assert.equal(annotationWriteGrantId(external, grant, now), grant.grant_id);
});

test("managed annotation writes use the Workspace authority", () => {
  const managed = { authorization_required: false, read_only: false };
  assert.equal(annotationWriteReady(managed, null, now), true);
  assert.equal(annotationWriteGrantId(managed, null, now), undefined);
});

test("a readonly file needs a new successful permission probe", () => {
  const readonly = { authorization_required: false, read_only: true };
  assert.equal(annotationWriteReady(readonly, null, now), false);
  assert.throws(() => annotationWriteGrantId(readonly, null, now));
  assert.equal(annotationWriteReady(readonly, grant, now), true);
  assert.equal(annotationWriteGrantId(readonly, grant, now), grant.grant_id);
});
