"""Exercise the execution boundary over an authenticated, real loopback RPC."""

from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
import sys
import unittest
from uuid import uuid4

import grpc

ROOT = Path(__file__).resolve().parents[3] / "services/python-worker/src/dd_worker"
sys.path[:0] = [str(ROOT), str(ROOT / "generated")]
from datasetdoctor.v1 import common_pb2, core_execution_pb2 as wire
from datasetdoctor.v1 import core_execution_pb2_grpc as rpc
from datasetdoctor.v1 import worker_service_pb2 as protocol
from worker_execution import Execution, InputChanged, WorkerExecutions


class RecordingCore(rpc.CoreExecutionControlServicer):
    def __init__(self):
        self.events = []
        self.outage = 0
        self.started_outage = 0
        self.lose_terminal_ack = False

    def ReportExecution(self, event, context):
        if dict(context.invocation_metadata()).get("authorization") != "Bearer synthetic-token":
            context.abort(grpc.StatusCode.UNAUTHENTICATED, "missing credential")
        self.events.append(event)
        if event.source_event_seq == 1 and self.started_outage:
            self.started_outage -= 1
            context.abort(grpc.StatusCode.UNAVAILABLE, "temporary start outage")
        if event.source_event_seq == 2 and self.outage:
            self.outage -= 1
            context.abort(grpc.StatusCode.UNAVAILABLE, "temporary outage")
        if event.source_event_seq == 2 and self.lose_terminal_ack:
            self.lose_terminal_ack = False
            context.abort(grpc.StatusCode.UNAVAILABLE, "lost acknowledgment")
        return wire.ReportAck(accepted=True, execution_event_id=event.execution_event_id)


class ExecutionFailureTests(unittest.TestCase):
    def setUp(self):
        self.pool = ThreadPoolExecutor(max_workers=2)
        self.server = grpc.server(self.pool)
        self.core = RecordingCore()
        rpc.add_CoreExecutionControlServicer_to_server(self.core, self.server)
        port = self.server.add_insecure_port("127.0.0.1:0")
        self.server.start()
        self.channel = grpc.insecure_channel(f"127.0.0.1:{port}")
        self.worker = WorkerExecutions(self.channel, "synthetic-token", str(uuid4()), self.pool)
        self.request = protocol.GeometryAuditRequest(context=wire.TaskContext(
            task_id=str(uuid4()), attempt_id=str(uuid4()),
        ))
        self.item = Execution(b"digest", wire.ExecutionHandle(
            task_id=self.request.context.task_id, attempt_id=self.request.context.attempt_id,
            worker_execution_id=str(uuid4()),
        ))

    def tearDown(self):
        self.channel.close()
        self.server.stop(0).wait()
        self.pool.shutdown(wait=True)

    def run_failure(self, error):
        def compute(_request):
            raise error
        self.worker._run(self.request, self.item, "synthetic-operation", compute)

    def assert_failure(self, name):
        self.assertEqual(self.item.status, "FAILED")
        event = self.core.events[-1]
        self.assertEqual(event.WhichOneof("event"), "failed")
        self.assertEqual(wire.ExecutionFailureCode.Name(event.failed.code), name)
        self.assertEqual(event.source_event_seq, 2)
        self.assertEqual(event.context.task_id, self.request.context.task_id)
        self.assertEqual(event.context.attempt_id, self.request.context.attempt_id)
        self.assertEqual(event.worker_execution_id, self.item.handle.worker_execution_id)

    def test_unexpected_exception_reports_a_safe_correlated_failure(self):
        with self.assertLogs("worker_execution", level="ERROR") as logs:
            self.run_failure(RuntimeError("secret-token C:/private/sample.png"))
        self.assert_failure("EXECUTION_FAILURE_CODE_INTERNAL_ERROR")
        self.assertEqual(self.item.error_code, "internal_error")
        message = "\n".join(logs.output)
        for identifier in (self.item.handle.task_id, self.item.handle.attempt_id,
                           self.item.handle.worker_execution_id):
            self.assertIn(identifier, message)
        self.assertNotIn("secret-token", message)
        self.assertNotIn("private", message)

    def test_common_input_and_output_failures_remain_distinct(self):
        for error, code in ((InputChanged("changed"), "INPUT_CHANGED"),
                            (ValueError("corrupt input"), "INVALID_INPUT"),
                            (PermissionError("cannot publish result"), "FILE_IO_FAILED")):
            with self.subTest(code=code):
                self.run_failure(error)
                self.assert_failure("EXECUTION_FAILURE_CODE_" + code)

    def test_unavailable_and_lost_ack_retry_the_identical_terminal_event(self):
        self.core.outage = 1
        self.core.lose_terminal_ack = True
        self.run_failure(ValueError("corrupt input"))
        self.assert_failure("EXECUTION_FAILURE_CODE_INVALID_INPUT")
        terminals = [event for event in self.core.events if event.source_event_seq == 2]
        self.assertEqual(len(terminals), 3)
        self.assertEqual(len({event.SerializeToString() for event in terminals}), 1)
        self.assertNotEqual(self.core.events[0].execution_event_id, terminals[0].execution_event_id)

    def test_exhausted_reporting_is_visible_to_the_core_supervisor(self):
        self.core.outage = 3
        self.run_failure(PermissionError("write failed"))
        self.assertEqual(self.item.status, "FAILED")
        self.assertEqual(self.item.error_code, "core_report_failed")
        terminals = [event for event in self.core.events if event.source_event_seq == 2]
        self.assertEqual(len(terminals), 3)
        self.assertEqual(len({event.SerializeToString() for event in terminals}), 1)

    def test_unconfirmed_start_never_computes(self):
        self.core.started_outage = 3
        def compute(_request):
            self.fail("compute ran before Core accepted Started")
        self.worker._run(self.request, self.item, "synthetic-operation", compute)
        self.assertEqual(self.item.status, "FAILED")
        self.assertEqual(self.item.error_code, "core_report_failed")
        self.assertEqual(len(self.core.events), 3)
        self.assertTrue(all(event.source_event_seq == 1 for event in self.core.events))
        self.assertEqual(len({event.SerializeToString() for event in self.core.events}), 1)

    def test_invalid_adapter_outcome_cannot_leave_the_execution_running(self):
        for result in ((None, "unknown_failure"), (None, None), (None, [])):
            with self.subTest(result=result):
                self.worker._run(self.request, self.item, "synthetic-operation", lambda _: result)
                self.assert_failure("EXECUTION_FAILURE_CODE_INTERNAL_ERROR")


if __name__ == "__main__":
    unittest.main()
