import assert from "node:assert/strict";
import test from "node:test";
import { reviewCompletionPayload, workflowNeedsPolling } from "../../../apps/desktop/src/workflow-review-state.ts";

const workflow = {
  workflow_run_id: "run", status: "WAITING_REVIEW", row_revision: "9007199254740993",
  review_recheck_tasks: [], review_recheck_pending: false, error_code: null,
};
const report = {
  scope: { workflow_run_id: "run", dataset_id: "dataset" }, human_ack_status: "PENDING",
  report_artifact_ref: { artifact_id: "original-report" }, unexecuted_checks: ["semantic_review"],
};

test("review completion requires explicit confirmation, matching report and reasons", () => {
  assert.throws(() => reviewCompletionPayload(workflow, report, {}, false));
  assert.throws(() => reviewCompletionPayload(workflow, report, {}, true));
  assert.throws(() => reviewCompletionPayload(workflow, report, { semantic_review: " " }, true));
  assert.throws(() => reviewCompletionPayload(workflow, { ...report, scope: { workflow_run_id: "another" } }, { semantic_review: "reason" }, true));
  const request = reviewCompletionPayload(workflow, report, { semantic_review: "  Local coverage accepted  " }, true);
  assert.equal(request.expected_revision, "9007199254740993");
  assert.deepEqual(request.expected_report, report.report_artifact_ref);
  assert.deepEqual(request.acknowledged_incomplete_checks, [{ check_id: "semantic_review", reason: "Local coverage accepted" }]);
});

test("pending rechecks poll through geometry success until Core completes or rejects", () => {
  assert.equal(workflowNeedsPolling(workflow), false);
  assert.equal(workflowNeedsPolling({ ...workflow, review_recheck_pending: true }), true);
  assert.equal(workflowNeedsPolling({ ...workflow, review_recheck_pending: true, review_recheck_tasks: [{ status: "SUCCEEDED" }] }), true);
  assert.equal(workflowNeedsPolling({ ...workflow, review_recheck_pending: true, error_code: "INPUT_CHANGED" }), false);
  assert.equal(workflowNeedsPolling({ ...workflow, review_recheck_pending: true, review_recheck_tasks: [{ status: "FAILED" }] }), false);
  assert.equal(workflowNeedsPolling({ ...workflow, status: "COMPLETED", review_recheck_pending: true }), false);
});

test("a running or already acknowledged review cannot submit another confirmation", () => {
  const reasons = { semantic_review: "Local coverage accepted" };
  assert.throws(() => reviewCompletionPayload({ ...workflow, review_recheck_pending: true }, report, reasons, true));
  assert.throws(() => reviewCompletionPayload({ ...workflow, status: "COMPLETED" }, report, reasons, true));
  assert.throws(() => reviewCompletionPayload(workflow, { ...report, human_ack_status: "ACKNOWLEDGED" }, reasons, true));
});
