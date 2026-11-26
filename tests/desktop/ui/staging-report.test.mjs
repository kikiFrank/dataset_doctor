import assert from "node:assert/strict";
import { test } from "node:test";
import { matchesCurrentReport, reportCheckLabel } from "../../../apps/desktop/src/staging-report-view.ts";

const artifact = {
  artifact_id: "report-artifact",
  owner: { kind: "TASK", id: "report-task" },
  kind: "STAGING_CHECK_REPORT",
  media_type: "application/json",
  sha256: "sealed-report",
  size_bytes: "80",
  schema_id: null,
};
const item = { asset_id: "frame", target_dataset_id: "dataset", current_report_ref: artifact };
const report = {
  report_id: "report-task",
  report_artifact_ref: structuredClone(artifact),
  scope: { asset_id: "frame", dataset_id: "dataset" },
};

test("a report panel only displays the exact current reference for the selected staged frame", () => {
  assert.equal(matchesCurrentReport(item, report), true);
  assert.equal(matchesCurrentReport(item, { ...report, scope: { ...report.scope, asset_id: "other" } }), false);
  assert.equal(matchesCurrentReport(item, { ...report, report_id: "other" }), false);
  assert.equal(matchesCurrentReport(item, { ...report, report_artifact_ref: { ...artifact, sha256: "changed" } }), false);
  assert.equal(matchesCurrentReport({ ...item, current_report_ref: undefined }, report), false);
});

test("omitted or incomplete check execution is always distinguished from PASS", () => {
  assert.equal(reportCheckLabel({ execution_status: "COMPLETED", assessment: "PASS" }), "通过");
  assert.equal(reportCheckLabel({ execution_status: "SKIPPED", assessment: "NOT_EVALUATED" }), "未执行 · 未评估");
  assert.equal(reportCheckLabel({ execution_status: "SKIPPED", assessment: "PASS" }), "未执行 · 未评估");
  assert.equal(reportCheckLabel({ execution_status: "COMPLETED", assessment: "FINDINGS" }), "发现问题");
});
