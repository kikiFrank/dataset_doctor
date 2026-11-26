from google.protobuf.internal import containers as _containers
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable
from typing import ClassVar as _ClassVar, Optional as _Optional

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
