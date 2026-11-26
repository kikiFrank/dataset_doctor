from datasetdoctor.v1 import common_pb2 as _common_pb2
from datasetdoctor.v1 import core_execution_pb2 as _core_execution_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class HealthRequest(_message.Message):
    __slots__ = ()
    def __init__(self) -> None: ...

class HealthSnapshot(_message.Message):
    __slots__ = ("role", "executor_session_id", "protocol_major", "build_version", "loaded_capability_ids", "active_executions")
    ROLE_FIELD_NUMBER: _ClassVar[int]
    EXECUTOR_SESSION_ID_FIELD_NUMBER: _ClassVar[int]
    PROTOCOL_MAJOR_FIELD_NUMBER: _ClassVar[int]
    BUILD_VERSION_FIELD_NUMBER: _ClassVar[int]
    LOADED_CAPABILITY_IDS_FIELD_NUMBER: _ClassVar[int]
    ACTIVE_EXECUTIONS_FIELD_NUMBER: _ClassVar[int]
    role: str
    executor_session_id: str
    protocol_major: int
    build_version: str
    loaded_capability_ids: _containers.RepeatedScalarFieldContainer[str]
    active_executions: int
    def __init__(self, role: _Optional[str] = ..., executor_session_id: _Optional[str] = ..., protocol_major: _Optional[int] = ..., build_version: _Optional[str] = ..., loaded_capability_ids: _Optional[_Iterable[str]] = ..., active_executions: _Optional[int] = ...) -> None: ...

class ShutdownRequest(_message.Message):
    __slots__ = ()
    def __init__(self) -> None: ...

class ShutdownReceipt(_message.Message):
    __slots__ = ("accepted",)
    ACCEPTED_FIELD_NUMBER: _ClassVar[int]
    accepted: bool
    def __init__(self, accepted: _Optional[bool] = ...) -> None: ...

class WorkerInputFile(_message.Message):
    __slots__ = ("path", "sha256")
    PATH_FIELD_NUMBER: _ClassVar[int]
    SHA256_FIELD_NUMBER: _ClassVar[int]
    path: str
    sha256: str
    def __init__(self, path: _Optional[str] = ..., sha256: _Optional[str] = ...) -> None: ...

class GeometryAuditInput(_message.Message):
    __slots__ = ("image", "annotation", "input_manifest", "output_directory", "max_pixels")
    IMAGE_FIELD_NUMBER: _ClassVar[int]
    ANNOTATION_FIELD_NUMBER: _ClassVar[int]
    INPUT_MANIFEST_FIELD_NUMBER: _ClassVar[int]
    OUTPUT_DIRECTORY_FIELD_NUMBER: _ClassVar[int]
    MAX_PIXELS_FIELD_NUMBER: _ClassVar[int]
    image: WorkerInputFile
    annotation: WorkerInputFile
    input_manifest: WorkerInputFile
    output_directory: str
    max_pixels: int
    def __init__(self, image: _Optional[_Union[WorkerInputFile, _Mapping]] = ..., annotation: _Optional[_Union[WorkerInputFile, _Mapping]] = ..., input_manifest: _Optional[_Union[WorkerInputFile, _Mapping]] = ..., output_directory: _Optional[str] = ..., max_pixels: _Optional[int] = ...) -> None: ...

class GeometryAuditRequest(_message.Message):
    __slots__ = ("context", "input_manifest_ref", "parameters")
    CONTEXT_FIELD_NUMBER: _ClassVar[int]
    INPUT_MANIFEST_REF_FIELD_NUMBER: _ClassVar[int]
    PARAMETERS_FIELD_NUMBER: _ClassVar[int]
    context: _core_execution_pb2.TaskContext
    input_manifest_ref: _common_pb2.ArtifactRef
    parameters: GeometryAuditInput
    def __init__(self, context: _Optional[_Union[_core_execution_pb2.TaskContext, _Mapping]] = ..., input_manifest_ref: _Optional[_Union[_common_pb2.ArtifactRef, _Mapping]] = ..., parameters: _Optional[_Union[GeometryAuditInput, _Mapping]] = ...) -> None: ...

class ExecutionLookup(_message.Message):
    __slots__ = ("task_id", "attempt_id")
    TASK_ID_FIELD_NUMBER: _ClassVar[int]
    ATTEMPT_ID_FIELD_NUMBER: _ClassVar[int]
    task_id: str
    attempt_id: str
    def __init__(self, task_id: _Optional[str] = ..., attempt_id: _Optional[str] = ...) -> None: ...

class ExecutionSnapshot(_message.Message):
    __slots__ = ("handle", "status", "result_manifest_ref", "error_code")
    HANDLE_FIELD_NUMBER: _ClassVar[int]
    STATUS_FIELD_NUMBER: _ClassVar[int]
    RESULT_MANIFEST_REF_FIELD_NUMBER: _ClassVar[int]
    ERROR_CODE_FIELD_NUMBER: _ClassVar[int]
    handle: _core_execution_pb2.ExecutionHandle
    status: str
    result_manifest_ref: _common_pb2.ArtifactRef
    error_code: str
    def __init__(self, handle: _Optional[_Union[_core_execution_pb2.ExecutionHandle, _Mapping]] = ..., status: _Optional[str] = ..., result_manifest_ref: _Optional[_Union[_common_pb2.ArtifactRef, _Mapping]] = ..., error_code: _Optional[str] = ...) -> None: ...
