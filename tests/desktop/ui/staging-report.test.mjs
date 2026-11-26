import assert from "node:assert/strict";
import { test } from "node:test";
import { matchesCurrentReport, reportCheckLabel, stagingCommitDetails } from "../../../apps/desktop/src/staging-report-view.ts";

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

test("acceptance requires the exact current report and explicit coverage acknowledgements", () => {
  const ready = { ...item, state: "READY_FOR_ACCEPTANCE" };
  const checked = {
    ...report,
    scope: { ...report.scope, label_schema_digest: "categories" },
    unexecuted_checks: ["semantic_review"],
    checks: [{ findings: [{ severity: "WARNING", issue_id: "issue" }, { severity: "INFO", issue_id: "note" }] }],
  };
  assert.equal(stagingCommitDetails({ ...ready, state: "NEEDS_CHECK" }, checked, {}, new Set()), null);
  assert.equal(stagingCommitDetails(ready, checked, { semantic_review: " " }, new Set(["issue"])), null);
  assert.equal(stagingCommitDetails(ready, checked, { semantic_review: "Verified locally" }, new Set()), null);
  assert.deepEqual(
    stagingCommitDetails(ready, checked, { semantic_review: "  Verified locally  " }, new Set(["issue"])),
    {
      asset_ids: ["frame"], target_dataset_id: "dataset", quality_report_ref: artifact,
      expected_schema_fingerprint: "categories", acknowledged_findings: [{ issue_id: "issue" }],
      acknowledged_incomplete_checks: [{ check_id: "semantic_review", reason: "Verified locally" }],
    },
  );
});
