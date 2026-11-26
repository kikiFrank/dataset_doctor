from datasetdoctor.v1 import common_pb2 as _common_pb2
from datasetdoctor.v1 import core_execution_pb2 as _core_execution_pb2
from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class ColorMethod(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    COLOR_METHOD_UNSPECIFIED: _ClassVar[ColorMethod]
    COLOR_METHOD_HUE_REMAP: _ClassVar[ColorMethod]
    COLOR_METHOD_SOLID_FILL: _ClassVar[ColorMethod]
COLOR_METHOD_UNSPECIFIED: ColorMethod
COLOR_METHOD_HUE_REMAP: ColorMethod
COLOR_METHOD_SOLID_FILL: ColorMethod

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

class ColorPoint(_message.Message):
    __slots__ = ("x", "y")
    X_FIELD_NUMBER: _ClassVar[int]
    Y_FIELD_NUMBER: _ClassVar[int]
    x: float
    y: float
    def __init__(self, x: _Optional[float] = ..., y: _Optional[float] = ...) -> None: ...

class ColorRectangle(_message.Message):
    __slots__ = ("origin", "width", "height")
    ORIGIN_FIELD_NUMBER: _ClassVar[int]
    WIDTH_FIELD_NUMBER: _ClassVar[int]
    HEIGHT_FIELD_NUMBER: _ClassVar[int]
    origin: ColorPoint
    width: float
    height: float
    def __init__(self, origin: _Optional[_Union[ColorPoint, _Mapping]] = ..., width: _Optional[float] = ..., height: _Optional[float] = ...) -> None: ...

class ColorPolygon(_message.Message):
    __slots__ = ("points",)
    POINTS_FIELD_NUMBER: _ClassVar[int]
    points: _containers.RepeatedCompositeFieldContainer[ColorPoint]
    def __init__(self, points: _Optional[_Iterable[_Union[ColorPoint, _Mapping]]] = ...) -> None: ...

class ColorHueGate(_message.Message):
    __slots__ = ("hue_min_deg", "hue_max_deg", "wraps_zero")
    HUE_MIN_DEG_FIELD_NUMBER: _ClassVar[int]
    HUE_MAX_DEG_FIELD_NUMBER: _ClassVar[int]
    WRAPS_ZERO_FIELD_NUMBER: _ClassVar[int]
    hue_min_deg: float
    hue_max_deg: float
    wraps_zero: bool
    def __init__(self, hue_min_deg: _Optional[float] = ..., hue_max_deg: _Optional[float] = ..., wraps_zero: _Optional[bool] = ...) -> None: ...

class ColorOperation(_message.Message):
    __slots__ = ("method", "target_rgb", "strength", "feather_px", "source_color_gate")
    METHOD_FIELD_NUMBER: _ClassVar[int]
    TARGET_RGB_FIELD_NUMBER: _ClassVar[int]
    STRENGTH_FIELD_NUMBER: _ClassVar[int]
    FEATHER_PX_FIELD_NUMBER: _ClassVar[int]
    SOURCE_COLOR_GATE_FIELD_NUMBER: _ClassVar[int]
    method: ColorMethod
    target_rgb: _containers.RepeatedScalarFieldContainer[int]
    strength: float
    feather_px: float
    source_color_gate: ColorHueGate
    def __init__(self, method: _Optional[_Union[ColorMethod, str]] = ..., target_rgb: _Optional[_Iterable[int]] = ..., strength: _Optional[float] = ..., feather_px: _Optional[float] = ..., source_color_gate: _Optional[_Union[ColorHueGate, _Mapping]] = ...) -> None: ...

class ColorPreviewInput(_message.Message):
    __slots__ = ("image", "input_manifest", "output_directory", "max_pixels", "rectangle", "polygon", "operation")
    IMAGE_FIELD_NUMBER: _ClassVar[int]
    INPUT_MANIFEST_FIELD_NUMBER: _ClassVar[int]
    OUTPUT_DIRECTORY_FIELD_NUMBER: _ClassVar[int]
    MAX_PIXELS_FIELD_NUMBER: _ClassVar[int]
    RECTANGLE_FIELD_NUMBER: _ClassVar[int]
    POLYGON_FIELD_NUMBER: _ClassVar[int]
    OPERATION_FIELD_NUMBER: _ClassVar[int]
    image: WorkerInputFile
    input_manifest: WorkerInputFile
    output_directory: str
    max_pixels: int
    rectangle: ColorRectangle
    polygon: ColorPolygon
    operation: ColorOperation
    def __init__(self, image: _Optional[_Union[WorkerInputFile, _Mapping]] = ..., input_manifest: _Optional[_Union[WorkerInputFile, _Mapping]] = ..., output_directory: _Optional[str] = ..., max_pixels: _Optional[int] = ..., rectangle: _Optional[_Union[ColorRectangle, _Mapping]] = ..., polygon: _Optional[_Union[ColorPolygon, _Mapping]] = ..., operation: _Optional[_Union[ColorOperation, _Mapping]] = ...) -> None: ...

class ColorPreviewRequest(_message.Message):
    __slots__ = ("context", "input_manifest_ref", "parameters")
    CONTEXT_FIELD_NUMBER: _ClassVar[int]
    INPUT_MANIFEST_REF_FIELD_NUMBER: _ClassVar[int]
    PARAMETERS_FIELD_NUMBER: _ClassVar[int]
    context: _core_execution_pb2.TaskContext
    input_manifest_ref: _common_pb2.ArtifactRef
    parameters: ColorPreviewInput
    def __init__(self, context: _Optional[_Union[_core_execution_pb2.TaskContext, _Mapping]] = ..., input_manifest_ref: _Optional[_Union[_common_pb2.ArtifactRef, _Mapping]] = ..., parameters: _Optional[_Union[ColorPreviewInput, _Mapping]] = ...) -> None: ...

class VideoProbeInput(_message.Message):
    __slots__ = ("source_video", "input_manifest", "output_directory", "stream_index", "max_pixels", "max_frames", "timeout_ms")
    SOURCE_VIDEO_FIELD_NUMBER: _ClassVar[int]
    INPUT_MANIFEST_FIELD_NUMBER: _ClassVar[int]
    OUTPUT_DIRECTORY_FIELD_NUMBER: _ClassVar[int]
    STREAM_INDEX_FIELD_NUMBER: _ClassVar[int]
    MAX_PIXELS_FIELD_NUMBER: _ClassVar[int]
    MAX_FRAMES_FIELD_NUMBER: _ClassVar[int]
    TIMEOUT_MS_FIELD_NUMBER: _ClassVar[int]
    source_video: WorkerInputFile
    input_manifest: WorkerInputFile
    output_directory: str
    stream_index: int
    max_pixels: int
    max_frames: int
    timeout_ms: int
    def __init__(self, source_video: _Optional[_Union[WorkerInputFile, _Mapping]] = ..., input_manifest: _Optional[_Union[WorkerInputFile, _Mapping]] = ..., output_directory: _Optional[str] = ..., stream_index: _Optional[int] = ..., max_pixels: _Optional[int] = ..., max_frames: _Optional[int] = ..., timeout_ms: _Optional[int] = ...) -> None: ...

class VideoProbeRequest(_message.Message):
    __slots__ = ("context", "input_manifest_ref", "parameters")
    CONTEXT_FIELD_NUMBER: _ClassVar[int]
    INPUT_MANIFEST_REF_FIELD_NUMBER: _ClassVar[int]
    PARAMETERS_FIELD_NUMBER: _ClassVar[int]
    context: _core_execution_pb2.TaskContext
    input_manifest_ref: _common_pb2.ArtifactRef
    parameters: VideoProbeInput
    def __init__(self, context: _Optional[_Union[_core_execution_pb2.TaskContext, _Mapping]] = ..., input_manifest_ref: _Optional[_Union[_common_pb2.ArtifactRef, _Mapping]] = ..., parameters: _Optional[_Union[VideoProbeInput, _Mapping]] = ...) -> None: ...

class FrameResolveInput(_message.Message):
    __slots__ = ("source_video", "input_manifest", "output_directory", "stream_index", "frame_index", "expected_raw_pts", "display_width", "display_height", "max_pixels", "max_frames", "timeout_ms")
    SOURCE_VIDEO_FIELD_NUMBER: _ClassVar[int]
    INPUT_MANIFEST_FIELD_NUMBER: _ClassVar[int]
    OUTPUT_DIRECTORY_FIELD_NUMBER: _ClassVar[int]
    STREAM_INDEX_FIELD_NUMBER: _ClassVar[int]
    FRAME_INDEX_FIELD_NUMBER: _ClassVar[int]
    EXPECTED_RAW_PTS_FIELD_NUMBER: _ClassVar[int]
    DISPLAY_WIDTH_FIELD_NUMBER: _ClassVar[int]
    DISPLAY_HEIGHT_FIELD_NUMBER: _ClassVar[int]
    MAX_PIXELS_FIELD_NUMBER: _ClassVar[int]
    MAX_FRAMES_FIELD_NUMBER: _ClassVar[int]
    TIMEOUT_MS_FIELD_NUMBER: _ClassVar[int]
    source_video: WorkerInputFile
    input_manifest: WorkerInputFile
    output_directory: str
    stream_index: int
    frame_index: int
    expected_raw_pts: int
    display_width: int
    display_height: int
    max_pixels: int
    max_frames: int
    timeout_ms: int
    def __init__(self, source_video: _Optional[_Union[WorkerInputFile, _Mapping]] = ..., input_manifest: _Optional[_Union[WorkerInputFile, _Mapping]] = ..., output_directory: _Optional[str] = ..., stream_index: _Optional[int] = ..., frame_index: _Optional[int] = ..., expected_raw_pts: _Optional[int] = ..., display_width: _Optional[int] = ..., display_height: _Optional[int] = ..., max_pixels: _Optional[int] = ..., max_frames: _Optional[int] = ..., timeout_ms: _Optional[int] = ...) -> None: ...

class FrameResolveRequest(_message.Message):
    __slots__ = ("context", "input_manifest_ref", "parameters")
    CONTEXT_FIELD_NUMBER: _ClassVar[int]
    INPUT_MANIFEST_REF_FIELD_NUMBER: _ClassVar[int]
    PARAMETERS_FIELD_NUMBER: _ClassVar[int]
    context: _core_execution_pb2.TaskContext
    input_manifest_ref: _common_pb2.ArtifactRef
    parameters: FrameResolveInput
    def __init__(self, context: _Optional[_Union[_core_execution_pb2.TaskContext, _Mapping]] = ..., input_manifest_ref: _Optional[_Union[_common_pb2.ArtifactRef, _Mapping]] = ..., parameters: _Optional[_Union[FrameResolveInput, _Mapping]] = ...) -> None: ...

class TrackingFrameInput(_message.Message):
    __slots__ = ("frame_index", "expected_raw_pts", "display_width", "display_height")
    FRAME_INDEX_FIELD_NUMBER: _ClassVar[int]
    EXPECTED_RAW_PTS_FIELD_NUMBER: _ClassVar[int]
    DISPLAY_WIDTH_FIELD_NUMBER: _ClassVar[int]
    DISPLAY_HEIGHT_FIELD_NUMBER: _ClassVar[int]
    frame_index: int
    expected_raw_pts: int
    display_width: int
    display_height: int
    def __init__(self, frame_index: _Optional[int] = ..., expected_raw_pts: _Optional[int] = ..., display_width: _Optional[int] = ..., display_height: _Optional[int] = ...) -> None: ...

class TrackingSeedInput(_message.Message):
    __slots__ = ("track_id", "category_id", "bbox_xywh")
    TRACK_ID_FIELD_NUMBER: _ClassVar[int]
    CATEGORY_ID_FIELD_NUMBER: _ClassVar[int]
    BBOX_XYWH_FIELD_NUMBER: _ClassVar[int]
    track_id: str
    category_id: int
    bbox_xywh: _containers.RepeatedScalarFieldContainer[float]
    def __init__(self, track_id: _Optional[str] = ..., category_id: _Optional[int] = ..., bbox_xywh: _Optional[_Iterable[float]] = ...) -> None: ...

class VideoTrackingInput(_message.Message):
    __slots__ = ("source_video", "input_manifest", "output_directory", "stream_index", "frames", "segment_id", "tracker_profile_id", "seeds", "tail_anchors", "search_radius", "max_error", "min_margin", "max_pixels", "max_frames", "timeout_ms")
    SOURCE_VIDEO_FIELD_NUMBER: _ClassVar[int]
    INPUT_MANIFEST_FIELD_NUMBER: _ClassVar[int]
    OUTPUT_DIRECTORY_FIELD_NUMBER: _ClassVar[int]
    STREAM_INDEX_FIELD_NUMBER: _ClassVar[int]
    FRAMES_FIELD_NUMBER: _ClassVar[int]
    SEGMENT_ID_FIELD_NUMBER: _ClassVar[int]
    TRACKER_PROFILE_ID_FIELD_NUMBER: _ClassVar[int]
    SEEDS_FIELD_NUMBER: _ClassVar[int]
    TAIL_ANCHORS_FIELD_NUMBER: _ClassVar[int]
    SEARCH_RADIUS_FIELD_NUMBER: _ClassVar[int]
    MAX_ERROR_FIELD_NUMBER: _ClassVar[int]
    MIN_MARGIN_FIELD_NUMBER: _ClassVar[int]
    MAX_PIXELS_FIELD_NUMBER: _ClassVar[int]
    MAX_FRAMES_FIELD_NUMBER: _ClassVar[int]
    TIMEOUT_MS_FIELD_NUMBER: _ClassVar[int]
    source_video: WorkerInputFile
    input_manifest: WorkerInputFile
    output_directory: str
    stream_index: int
    frames: _containers.RepeatedCompositeFieldContainer[TrackingFrameInput]
    segment_id: str
    tracker_profile_id: str
    seeds: _containers.RepeatedCompositeFieldContainer[TrackingSeedInput]
    tail_anchors: _containers.RepeatedCompositeFieldContainer[TrackingSeedInput]
    search_radius: int
    max_error: float
    min_margin: float
    max_pixels: int
    max_frames: int
    timeout_ms: int
    def __init__(self, source_video: _Optional[_Union[WorkerInputFile, _Mapping]] = ..., input_manifest: _Optional[_Union[WorkerInputFile, _Mapping]] = ..., output_directory: _Optional[str] = ..., stream_index: _Optional[int] = ..., frames: _Optional[_Iterable[_Union[TrackingFrameInput, _Mapping]]] = ..., segment_id: _Optional[str] = ..., tracker_profile_id: _Optional[str] = ..., seeds: _Optional[_Iterable[_Union[TrackingSeedInput, _Mapping]]] = ..., tail_anchors: _Optional[_Iterable[_Union[TrackingSeedInput, _Mapping]]] = ..., search_radius: _Optional[int] = ..., max_error: _Optional[float] = ..., min_margin: _Optional[float] = ..., max_pixels: _Optional[int] = ..., max_frames: _Optional[int] = ..., timeout_ms: _Optional[int] = ...) -> None: ...

class VideoTrackingRequest(_message.Message):
    __slots__ = ("context", "input_manifest_ref", "parameters")
    CONTEXT_FIELD_NUMBER: _ClassVar[int]
    INPUT_MANIFEST_REF_FIELD_NUMBER: _ClassVar[int]
    PARAMETERS_FIELD_NUMBER: _ClassVar[int]
    context: _core_execution_pb2.TaskContext
    input_manifest_ref: _common_pb2.ArtifactRef
    parameters: VideoTrackingInput
    def __init__(self, context: _Optional[_Union[_core_execution_pb2.TaskContext, _Mapping]] = ..., input_manifest_ref: _Optional[_Union[_common_pb2.ArtifactRef, _Mapping]] = ..., parameters: _Optional[_Union[VideoTrackingInput, _Mapping]] = ...) -> None: ...

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
