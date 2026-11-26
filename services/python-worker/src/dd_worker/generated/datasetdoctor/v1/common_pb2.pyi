from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from typing import ClassVar as _ClassVar, Optional as _Optional

DESCRIPTOR: _descriptor.FileDescriptor

class ArtifactRef(_message.Message):
    __slots__ = ("artifact_id", "owner_task_id", "owner_asset_id", "owner_review_id", "kind", "media_type", "size_bytes", "sha256", "schema_id")
    ARTIFACT_ID_FIELD_NUMBER: _ClassVar[int]
    OWNER_TASK_ID_FIELD_NUMBER: _ClassVar[int]
    OWNER_ASSET_ID_FIELD_NUMBER: _ClassVar[int]
    OWNER_REVIEW_ID_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    MEDIA_TYPE_FIELD_NUMBER: _ClassVar[int]
    SIZE_BYTES_FIELD_NUMBER: _ClassVar[int]
    SHA256_FIELD_NUMBER: _ClassVar[int]
    SCHEMA_ID_FIELD_NUMBER: _ClassVar[int]
    artifact_id: str
    owner_task_id: str
    owner_asset_id: str
    owner_review_id: str
    kind: str
    media_type: str
    size_bytes: int
    sha256: str
    schema_id: str
    def __init__(self, artifact_id: _Optional[str] = ..., owner_task_id: _Optional[str] = ..., owner_asset_id: _Optional[str] = ..., owner_review_id: _Optional[str] = ..., kind: _Optional[str] = ..., media_type: _Optional[str] = ..., size_bytes: _Optional[int] = ..., sha256: _Optional[str] = ..., schema_id: _Optional[str] = ...) -> None: ...
