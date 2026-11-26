from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from typing import ClassVar as _ClassVar, Optional as _Optional

DESCRIPTOR: _descriptor.FileDescriptor

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
