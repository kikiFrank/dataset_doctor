import assert from "node:assert/strict";
import test from "node:test";
import { cancellationState, cancellationReceiptMessage } from "../../../apps/desktop/src/task-cancellation-state.ts";

test("only active compute states offer a cancellation action", () => {
  for (const status of ["QUEUED", "DISPATCHING", "RUNNING"]) assert.equal(cancellationState(status), "AVAILABLE");
  assert.equal(cancellationState("CANCELLING"), "REQUESTED");
  assert.equal(cancellationState("CANCELLED"), "STOPPED");
  for (const status of ["SUCCEEDED", "FAILED", "INTERRUPTED", "UNKNOWN"]) assert.equal(cancellationState(status), "SETTLED");
});

test("a cancellation request is never presented as a confirmed stop", () => {
  const receipt = { requested: true, observed_status: "CANCELLING" };
  assert.match(cancellationReceiptMessage(receipt), /等待/);
  assert.doesNotMatch(cancellationReceiptMessage(receipt), /已停止/);
  assert.match(cancellationReceiptMessage({ ...receipt, observed_status: "CANCELLED" }), /已停止/);
});

test("a confirmed stop clears the waiting action even before the parent refreshes", () => {
  assert.equal(cancellationState("QUEUED", "CANCELLED"), "STOPPED");
  assert.equal(cancellationState("RUNNING", "CANCELLING"), "REQUESTED");
  assert.equal(cancellationState("SUCCEEDED", "CANCELLING"), "SETTLED");
});

test("late cancellation preserves the authoritative completed result", () => {
  for (const status of ["SUCCEEDED", "FAILED", "INTERRUPTED"]) {
    assert.match(cancellationReceiptMessage({ requested: false, observed_status: status }), new RegExp(status));
    assert.doesNotMatch(cancellationReceiptMessage({ requested: false, observed_status: status }), /已停止/);
  }
});
