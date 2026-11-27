from datasetdoctor.v1 import common_pb2 as _common_pb2
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class ExecutionFailureCode(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    EXECUTION_FAILURE_CODE_UNSPECIFIED: _ClassVar[ExecutionFailureCode]
    EXECUTION_FAILURE_CODE_INPUT_CHANGED: _ClassVar[ExecutionFailureCode]
    EXECUTION_FAILURE_CODE_INVALID_INPUT: _ClassVar[ExecutionFailureCode]
    EXECUTION_FAILURE_CODE_FILE_IO_FAILED: _ClassVar[ExecutionFailureCode]
    EXECUTION_FAILURE_CODE_DECODE_FAILED: _ClassVar[ExecutionFailureCode]
    EXECUTION_FAILURE_CODE_PIXEL_LIMIT_EXCEEDED: _ClassVar[ExecutionFailureCode]
    EXECUTION_FAILURE_CODE_UNSUPPORTED_FORMAT: _ClassVar[ExecutionFailureCode]
    EXECUTION_FAILURE_CODE_INTERNAL_ERROR: _ClassVar[ExecutionFailureCode]
EXECUTION_FAILURE_CODE_UNSPECIFIED: ExecutionFailureCode
EXECUTION_FAILURE_CODE_INPUT_CHANGED: ExecutionFailureCode
EXECUTION_FAILURE_CODE_INVALID_INPUT: ExecutionFailureCode
EXECUTION_FAILURE_CODE_FILE_IO_FAILED: ExecutionFailureCode
EXECUTION_FAILURE_CODE_DECODE_FAILED: ExecutionFailureCode
EXECUTION_FAILURE_CODE_PIXEL_LIMIT_EXCEEDED: ExecutionFailureCode
EXECUTION_FAILURE_CODE_UNSUPPORTED_FORMAT: ExecutionFailureCode
EXECUTION_FAILURE_CODE_INTERNAL_ERROR: ExecutionFailureCode

class TaskContext(_message.Message):
    __slots__ = ("workspace_id", "workspace_session_id", "workflow_run_id", "node_id", "task_id", "attempt_id", "executor_session_id", "input_fingerprint", "node_spec_fingerprint", "grant_id")
    WORKSPACE_ID_FIELD_NUMBER: _ClassVar[int]
    WORKSPACE_SESSION_ID_FIELD_NUMBER: _ClassVar[int]
    WORKFLOW_RUN_ID_FIELD_NUMBER: _ClassVar[int]
    NODE_ID_FIELD_NUMBER: _ClassVar[int]
    TASK_ID_FIELD_NUMBER: _ClassVar[int]
    ATTEMPT_ID_FIELD_NUMBER: _ClassVar[int]
    EXECUTOR_SESSION_ID_FIELD_NUMBER: _ClassVar[int]
    INPUT_FINGERPRINT_FIELD_NUMBER: _ClassVar[int]
    NODE_SPEC_FINGERPRINT_FIELD_NUMBER: _ClassVar[int]
    GRANT_ID_FIELD_NUMBER: _ClassVar[int]
    workspace_id: str
    workspace_session_id: str
    workflow_run_id: str
    node_id: str
    task_id: str
    attempt_id: str
    executor_session_id: str
    input_fingerprint: str
    node_spec_fingerprint: str
    grant_id: str
    def __init__(self, workspace_id: _Optional[str] = ..., workspace_session_id: _Optional[str] = ..., workflow_run_id: _Optional[str] = ..., node_id: _Optional[str] = ..., task_id: _Optional[str] = ..., attempt_id: _Optional[str] = ..., executor_session_id: _Optional[str] = ..., input_fingerprint: _Optional[str] = ..., node_spec_fingerprint: _Optional[str] = ..., grant_id: _Optional[str] = ...) -> None: ...

class ClaimGrantRequest(_message.Message):
    __slots__ = ("grant_id", "workspace_session_id", "task_id", "attempt_id", "executor_session_id", "worker_execution_id", "input_fingerprint", "operation_digest")
    GRANT_ID_FIELD_NUMBER: _ClassVar[int]
    WORKSPACE_SESSION_ID_FIELD_NUMBER: _ClassVar[int]
    TASK_ID_FIELD_NUMBER: _ClassVar[int]
    ATTEMPT_ID_FIELD_NUMBER: _ClassVar[int]
    EXECUTOR_SESSION_ID_FIELD_NUMBER: _ClassVar[int]
    WORKER_EXECUTION_ID_FIELD_NUMBER: _ClassVar[int]
    INPUT_FINGERPRINT_FIELD_NUMBER: _ClassVar[int]
    OPERATION_DIGEST_FIELD_NUMBER: _ClassVar[int]
    grant_id: str
    workspace_session_id: str
    task_id: str
    attempt_id: str
    executor_session_id: str
    worker_execution_id: str
    input_fingerprint: str
    operation_digest: str
    def __init__(self, grant_id: _Optional[str] = ..., workspace_session_id: _Optional[str] = ..., task_id: _Optional[str] = ..., attempt_id: _Optional[str] = ..., executor_session_id: _Optional[str] = ..., worker_execution_id: _Optional[str] = ..., input_fingerprint: _Optional[str] = ..., operation_digest: _Optional[str] = ...) -> None: ...

class ExecutionHandle(_message.Message):
    __slots__ = ("task_id", "attempt_id", "worker_execution_id")
    TASK_ID_FIELD_NUMBER: _ClassVar[int]
    ATTEMPT_ID_FIELD_NUMBER: _ClassVar[int]
    WORKER_EXECUTION_ID_FIELD_NUMBER: _ClassVar[int]
    task_id: str
    attempt_id: str
    worker_execution_id: str
    def __init__(self, task_id: _Optional[str] = ..., attempt_id: _Optional[str] = ..., worker_execution_id: _Optional[str] = ...) -> None: ...

class ExecutionStarted(_message.Message):
    __slots__ = ()
    def __init__(self) -> None: ...

class ExecutionCancelled(_message.Message):
    __slots__ = ()
    def __init__(self) -> None: ...

class ExecutionSucceeded(_message.Message):
    __slots__ = ("result_manifest_ref",)
    RESULT_MANIFEST_REF_FIELD_NUMBER: _ClassVar[int]
    result_manifest_ref: _common_pb2.ArtifactRef
    def __init__(self, result_manifest_ref: _Optional[_Union[_common_pb2.ArtifactRef, _Mapping]] = ...) -> None: ...

class ExecutionFailed(_message.Message):
    __slots__ = ("code",)
    CODE_FIELD_NUMBER: _ClassVar[int]
    code: ExecutionFailureCode
    def __init__(self, code: _Optional[_Union[ExecutionFailureCode, str]] = ...) -> None: ...

class ReportExecutionRequest(_message.Message):
    __slots__ = ("context", "execution_event_id", "worker_execution_id", "operation_digest", "source_event_seq", "started", "succeeded", "failed", "cancelled")
    CONTEXT_FIELD_NUMBER: _ClassVar[int]
    EXECUTION_EVENT_ID_FIELD_NUMBER: _ClassVar[int]
    WORKER_EXECUTION_ID_FIELD_NUMBER: _ClassVar[int]
    OPERATION_DIGEST_FIELD_NUMBER: _ClassVar[int]
    SOURCE_EVENT_SEQ_FIELD_NUMBER: _ClassVar[int]
    STARTED_FIELD_NUMBER: _ClassVar[int]
    SUCCEEDED_FIELD_NUMBER: _ClassVar[int]
    FAILED_FIELD_NUMBER: _ClassVar[int]
    CANCELLED_FIELD_NUMBER: _ClassVar[int]
    context: TaskContext
    execution_event_id: str
    worker_execution_id: str
    operation_digest: str
    source_event_seq: int
    started: ExecutionStarted
    succeeded: ExecutionSucceeded
    failed: ExecutionFailed
    cancelled: ExecutionCancelled
    def __init__(self, context: _Optional[_Union[TaskContext, _Mapping]] = ..., execution_event_id: _Optional[str] = ..., worker_execution_id: _Optional[str] = ..., operation_digest: _Optional[str] = ..., source_event_seq: _Optional[int] = ..., started: _Optional[_Union[ExecutionStarted, _Mapping]] = ..., succeeded: _Optional[_Union[ExecutionSucceeded, _Mapping]] = ..., failed: _Optional[_Union[ExecutionFailed, _Mapping]] = ..., cancelled: _Optional[_Union[ExecutionCancelled, _Mapping]] = ...) -> None: ...

class TerminalResultReceipt(_message.Message):
    __slots__ = ("accepted_at", "completion_fingerprint", "result_manifest_ref")
    ACCEPTED_AT_FIELD_NUMBER: _ClassVar[int]
    COMPLETION_FINGERPRINT_FIELD_NUMBER: _ClassVar[int]
    RESULT_MANIFEST_REF_FIELD_NUMBER: _ClassVar[int]
    accepted_at: str
    completion_fingerprint: str
    result_manifest_ref: _common_pb2.ArtifactRef
    def __init__(self, accepted_at: _Optional[str] = ..., completion_fingerprint: _Optional[str] = ..., result_manifest_ref: _Optional[_Union[_common_pb2.ArtifactRef, _Mapping]] = ...) -> None: ...

class ReportAck(_message.Message):
    __slots__ = ("accepted", "execution_event_id", "terminal_receipt")
    ACCEPTED_FIELD_NUMBER: _ClassVar[int]
    EXECUTION_EVENT_ID_FIELD_NUMBER: _ClassVar[int]
    TERMINAL_RECEIPT_FIELD_NUMBER: _ClassVar[int]
    accepted: bool
    execution_event_id: str
    terminal_receipt: TerminalResultReceipt
    def __init__(self, accepted: _Optional[bool] = ..., execution_event_id: _Optional[str] = ..., terminal_receipt: _Optional[_Union[TerminalResultReceipt, _Mapping]] = ...) -> None: ...
