# L2 数据契约与 SQLite

> DD-DESIGN-L2-001 方案Ⅰ v1.0；按主题迁移于 2026-11-26。设计契约非已实现代码。原章节编号保留，导航见 [文档索引](../README.md)。

## 三、数据契约与 SQLite B

> 状态：方案Ⅰ的配套数据设计基线（Design Default），不是已经实施的数据库。
>
> 本章落实选定方案的数据权威、审核门槛和存储布局。
>
> 已确认的前提：SQLite B 方案；工作时每图一份 JSON，导出时汇总 COCO；外部图片默认引用原目录；无数据集或标注历史版本；Core 统一提交正式修改。单图 COCO 最小结构、类别字典位置及本章具体字段作为本基线的配套设计默认维护。

### 1. 什么由谁负责

| 内容 | 权威位置 | 约束 |
|---|---|---|
| 当前框及其类别引用 | 图片对应的工作 JSON | SQLite 不保存 bbox 或第二套 annotations |
| Dataset 类别字典 | datasets.categories_json，正式建议 | 单图 categories 是解释文件所需的副本；名称不要求附加描述 |
| 资源身份、当前位置、来源 | assets | UUID 独立于文件名、路径及 COCO 整数 ID |
| 正式 Dataset 成员 | dataset_assets | 同一图片当前最多属于一个 Dataset |
| 尚未注入的数据 | staging_items | 不靠物理目录判断；审核后只改关系，不复制帧 |
| 人工确认是否有效 | review_sessions 及成员的 final_review_id | 必须与当前图片、标注及当前类别字典指纹对应 |
| Workflow 与 Task 状态 | workflow_runs、tasks | Agent、Worker 上报；Core 校验并持久化 |
| 检查报告、候选、模型输出 | 任务目录中的 Artifact | SQL 保存引用，不复制报告正文 |
| 当前帧定位、当前跟踪关联 | assets 元数据所引用的正式 Artifact | 属于当前数据依赖，不能作为临时候选清除 |

一个 Workspace 使用自己的 workspace.db。Workspace UUID 放在建议的 workspace.json 标志/配置文件中；Core 打开数据库后绑定这一身份，不能仅凭路径认定工作区。全局应用注册库仍按 L1 管理，以下八表是工作区业务库。另有一个 schema_meta 技术表保存数据库格式号及 Workspace UUID；打开时与 workspace.json 比对，不匹配就拒绝写入，不能把移错的数据库自动改成当前身份。

#### 1.1 通用类型与身份

所有业务 ID 均采用 UUID 字符串：workspace_id、workspace_session_id、dataset_id、asset_id、workflow_run_id、node_id、task_id、attempt_id、request_id、idempotency_key、artifact_id；本章的 review_id、operation_id、sampling_config_id、track_id 也采用 UUID。UUID 由指定调用方生成并经 Core 验证，不是访问授权凭据。

workspace_session_id 在每次打开工作区后重新生成，用于拒绝旧会话的迟到执行结果。request_id 标识一次调用；同一业务操作重发时保留 idempotency_key，但可以使用新的 request_id。幂等作用域为 (workspace_id, operation_kind, idempotency_key)，并绑定规范化请求的 request_fingerprint；workspace.db 内 Workspace 已隐含。Task 是逻辑可调度工作；重试保持 task_id，生成新的 attempt_id。输入、目标或计算目的变化属于新 Task，不能当成旧尝试。

时间字段为 UTC Unix 毫秒 INTEGER；界面按用户时区显示。摘要使用 64 位小写十六进制 SHA-256 字符串，与公共 DTO 一致；示例中的 SHA256_HEX_64 是形状占位符。JSON 列为 UTF-8 TEXT，由 Core 按对应契约验证；SQL NULL 与 JSON null 不混用。下方 DDL 中标注 NOT NULL 的字段必须提供，未写 DEFAULT 的字段由 Core 显式赋值；可空字段默认 SQL NULL。

row_revision 只是乐观并发控制序号，变更时递增。它不提供历史浏览、还原或数据集版本功能。

### 2. 单图工作 JSON：COCO bbox profile

结构以 [单图 COCO v1 Schema](../../contracts/json-schema/v1/coco-single-image.schema.json) 为准。它保留结构正确的几何异常供检查。Core 在读写边界校验图片和类别引用、局部 ID 唯一性、文件授权及正式 bbox 条件。

本章建议使用一张图片的 COCO 结构，而非 AnyLabeling 原生 JSON：

```json
{
  "images": [
    {"id": 1, "file_name": "frame_000012.jpg", "width": 1920, "height": 1080}
  ],
  "annotations": [
    {"id": 1, "image_id": 1, "category_id": 3,
     "bbox": [120.0, 80.0, 40.0, 60.0], "area": 2400.0, "iscrowd": 0}
  ],
  "categories": [
    {"id": 3, "name": "red_light"}
  ]
}
```

规则：

- images 恰好一条；annotations 可以为空；所有 image_id 必须引用该条图片。
- 图片、标注、类别 ID 是各自文件内的整数；MVP 使用非负安全整数。不同图片的 JSON 可以重复使用相同 image.id、annotation.id。
- 新建文件默认 image.id 为 1，但读取已有文件时不要求其等于 1。annotation.id 在该文件内唯一。
- bbox 为像素坐标的 x、y、width、height；写入正式结果前验证数值有限、宽高为正，并执行适用的几何质量检查。读取有问题的文件时保留诊断信息，不能悄悄修正数值。
- categories 至少覆盖本文件使用的类别。新文件使用当前 Dataset 类别字典的 ID 和名称；不用缺失的类别描述补造语义。
- 本轮 MVP profile 是普通轴对齐 bbox。polygon、segmentation、旋转框、关键点等不因“COCO”这个名字就自动获得支持。
- 文件不添加 Human、AI Candidate、Final、Workflow 或 track_id 等业务字段；这些信息在审核记录、候选 Artifact 和来源元数据中表达。
- file_name 是文件解析所需的名称或相对位置。最终导出时重新生成，不能把本机绝对路径泄漏到训练包中。

“有工作 JSON”不等于“审核通过”。它可能是导入的人工标注，也可能是此前确认的当前结果：

| 情形 | 含义 |
|---|---|
| 没有 JSON | 资源已登记，尚未保存工作标注 |
| JSON 存在，无有效确认记录 | 有标注正文，但未满足当前导出确认条件 |
| JSON 的 annotations 为空，无确认记录 | 无法仅凭空数组认定为合法负样本 |
| annotations 为空，有有效确认记录 | 用户已确认没有目标，可以作为负样本 |
| JSON 有框，有有效确认记录 | 当前 Final；导出只使用这一套正文 |

候选放在任务 Artifact 中；人工尚未提交的修改放审核草稿 Artifact。确认成功后，Core 将最终选择的结果写入工作 JSON，再结束审核并清除活动候选、草稿引用。成功前不得先删除候选。保留检查报告和确认摘要不等于保留 Human、AI Candidate 的完整历史副本。

### 3. 类别、导入及外部文件

#### 3.1 类别唯一权威

datasets.categories_json 的建议结构如下：

```json
[{"id": 1, "name": "helmet"}, {"id": 3, "name": "red_light"}]
```

类别 ID 在 Dataset 内唯一、稳定；类别名称在 Dataset 内按精确字符串唯一。显示顺序由数组顺序决定，不把顺序位置或 YOLO 类别序号当作 COCO ID。删除类别后不立即复用旧 ID。新增类别直接修改当前字典；没有 Label Schema Version。

本轮推荐保守规则：任何类别字典变化，包括增加新类别，都使旧类别摘要对应的确认暂时失效。用户可以在查看变更影响后批量确认既有标注，无需重新画框。代价是增加确认操作；好处是避免软件自行认定新类别与既有图片无关。以后如需按所用类别精确失效，应另行评审，不能暗中启用。重命名保留类别 ID，由 Core 创建可恢复的批量操作，同步受影响 JSON 的 categories。重写 JSON 会改变其摘要，原 track map 的绑定需要重建并重新核验；该批量操作不能自动刷新 CONFIRMED 摘要伪装成已人工复核。操作完成前阻止相冲突的导出和标签提交；不要出现 SQL 已改名、部分 JSON 仍使用旧名称却被认定完全同步。删除仍被使用的类别需要先处理引用，不能静默删除对应框。

导入时先根据源 categories 建立“源 ID—源名称—目标 ID”映射；不同源文件中相同数字不代表同一类别。不存在的名称可以经导入预览加入当前字典。没有自动同义词合并、类别描述生成或跨数据集本体对齐。

#### 3.2 导入分为 inspect、preview、apply

默认操作是引用原图和保留原文件，而不是转换后覆盖。

1. inspect 检查格式、图片对应关系、类别与几何类型，读取文件摘要。
2. preview 展示哪些文件原样引用、哪些需要转换、输出位置、名称冲突、不能保留的字段和预计改动。
3. apply 只执行用户选择的计划，并核对预览时的输入摘要；原文件已经改变则要求刷新预览。

对已经符合单图 COCO bbox profile 的文件，可以无损注册。对 Dataset 级 COCO 拆分，需预览每图输出位置、ID 映射及原有文件冲突。

AnyLabeling 原生 JSON 不自动成为本软件的正式契约。若检测到 polygon、旋转框或当前适配器不支持的字段，默认保留原文件并报告不支持；不能把它们默默转换为 bbox 或丢弃。需要有损输出时必须明确列出损失并由用户选择，且该转换能力必须实际实现后才能开放。初版可以只开放无损 COCO bbox 导入。

用户可选择独立转换目录，或明确选择覆盖原标注。选择独立目录不自动切换当前 Dataset 的路径；转换完成后按预览约定登记或由用户重新导入。若用户选择覆盖，必须显示精确路径、字段损失和不可撤销影响；这不改变“默认引用原目录、默认保持原文件”的行为。

#### 3.3 文件对应与冲突

正常的新工作文件使用图片旁同主体名称的 JSON。若同目录同时有 foo.jpg 和 foo.png，则 foo.json 不能自动绑定给任意一张。新文件可分别使用 foo.jpg.json、foo.png.json，并在预览中展示；注册表记录真实 annotation_path，后续不靠文件名重新猜测。

预先存在的 AnyLabeling foo.json 也不能被普通“保存标注”悄悄改写成 COCO。只有显式转换操作可以改变其格式；保存接口应拒绝尚未完成格式适配的文件。

外部目录缺失时标记 OFFLINE，不自动删除记录。重新定位保留 Asset ID，通过内容摘要、已有绑定和用户选择核对身份。相同字节可能属于两个独立 Asset，摘要不能充当唯一主键。复制过的 sidecar 也不能使两个物理资源自动合并。

### 4. 来源、跟踪与输入指纹

#### 4.1 来源元数据

source_metadata_json 使用带 kind 的结构。video_frame 的最小形状为：

```json
{
  "kind": "video_frame",
  "frame": {
    "source_video_id": "UUID",
    "source_digest": "SHA256_HEX_64",
    "stream_index": 0,
    "frame_index": "12",
    "pts_us": "480000",
    "pts": "12",
    "time_base_num": 1,
    "time_base_den": 25,
    "timestamp_ms": 480
  },
  "sampling": {
    "tracking": {"sampling_config_id": "UUID", "mode": "EVERY_N_FRAMES", "every_n": 1},
    "dataset": {"sampling_config_id": "UUID", "mode": "TIME_INTERVAL",
                "interval_ms": 1000, "include_keyframes": false}
  },
  "extraction_method": "registered_decoder_capability",
  "extraction_task_id": "UUID",
  "extraction_attempt_id": "UUID"
}
```

frame_index、pts_us、底层 pts 用十进制整数字符串避免前端整数精度损失；time_base_den 必须为正。frame_index 是选定视频流按展示顺序排列的帧索引，不是压缩包或解码到达次序；timestamp_ms 仅用于展示，不通过秒数乘 FPS 反推正式帧号。两种 sampling 分别有 sampling_config_id，记录实际生效值。mode 只能是 EVERY_N_FRAMES 或 TIME_INTERVAL；前者记录正整数 every_n，后者记录正整数 interval_ms，实际来源记录不混入未激活模式的偏好。源视频列与来源 JSON 中的 source_video_id 必须一致。

Core 使用 `videos/<source_video_id>/<source_digest>/stream-<stream_index>/frame-<frame_index>.png` 保存抽样帧。SQLite 登记同一受管路径前核对现有 Asset 的来源和目标，重复采样返回现有 Asset。实际选中的人工关键帧保存已校验的单图 JSON，并改用受管图片的文件名。已核验的追踪结果仅在有实际追踪框的采样帧上登记来源字段 `tracking_candidate_ref`。该字段是封存 TrackBundle 的无路径引用，有人工标注的帧继续使用人工标注。无候选的帧保持无标注。新 Asset 和 Staging 行在同一事务登记，只有实际生成标注时才填写 annotation_path；源视频保持引用，不形成新 Dataset 成员或 Final。

普通导入图片使用 kind=imported_image；源视频使用 kind=source_video。未标注图片没有 JSON 时，来源仍由 assets 保存，不强迫先创建空标签文件。

#### 4.2 正式跟踪关联不是临时候选

assets.metadata_json 的 video_probe_task_id 保存最近登记的探测 Task UUID，用于定位原任务的已封存证据。旧记录继续通过原任务输入清单定位。

assets.metadata_json 可以包含 current_track_map_ref、video_frame_index_ref 和 video_annotation_manifest_ref。对外引用复用公共 ArtifactRef，owner 明确为 TASK、ASSET 或 REVIEW 及对应 UUID，不强制所有产物都依附 Task，不暴露内部路径；Core 的持久 manifest entry 保存 artifact_id、relative_path、sha256、kind、media_type 等定位信息，由 Core 校验后解析。

Core 将视频工作清单写入不可变的 artifacts/videos/<video_id>/manifests/<artifact_id>.json，人工草稿位于同一视频的 keyframes 目录。metadata_json 的 video_annotation_manifest_entry 保存 reference 与内部 relative_path，公开命令仅返回 ArtifactRef。草稿携带完整帧引用、目标 Dataset 类别摘要和已校验的 COCO。文件落盘并同步后才以 assets.row_revision 条件更新当前清单指针，失败保留旧指针；未被指针引用的文件不构成已保存状态。备份和恢复核验当前清单及其草稿的身份、归属、长度和摘要，并保留帧引用所依赖的 Task 产物。

video_annotation_manifest_ref 指向当前视频工作清单，包含已保存片段、精确端点、关键帧及人工草稿引用。video_save_segment 必须更新这一正式依赖并持久化；它需要备份，不能作为 AI 临时候选清理。每张图片当前 track map 的载荷建议为：

```json
{
  "asset_id": "UUID",
  "annotation_digest": "SHA256_HEX_64",
  "bindings": [
    {"local_annotation_id": 1, "track_id": "UUID",
     "tracking_task_id": "UUID", "tracking_attempt_id": "UUID"}
  ]
}
```

跟踪算法的局部数字 ID 可以在产物中保留，但对外绑定到 Core 分配的 track_id。人工新增且没有跨帧身份的框可以不建立绑定。正式标注 ID 改变时必须同步重建映射；外部修改导致摘要不匹配时映射失效，不能猜测对应关系。

显式采用待接收帧的追踪候选时，Core 按图像、来源帧和目标类别核验所选轨迹。审核草稿可通过 `track_bindings` 记录已采用标注 ID、轨迹 ID、追踪任务及执行尝试，随草稿摘要封存。移动同一标注框可保留关联，删除标注或更换其类别或局部 ID 会解除旧关联。该数据位于审核草稿外层，正式标注仍使用标准单图 COCO 结构。

正式 track map、视频帧索引和视频工作清单是当前数据依赖，备份时包含，候选清理时排除。候选自己的 track map 可以清理。MVP 不强制另外建立 tracks 业务表，也不将 track_id 硬塞进标准 COCO annotations。

对采用轨迹的 Final，Core 将当前映射封存为 `artifacts/images/<asset_id>/track-maps/<artifact_id>.json`，绑定正式标注摘要、原视频片段与精确 FrameRef。文件操作清单记录映射的摘要和内部位置，SQLite 在确认 Final 的事务中发布当前引用。恢复重新核验同一映射，正式备份依该引用收录和校验文件。 [Track Map v1 Schema](../../contracts/json-schema/v1/track-map.schema.json)

#### 4.3 输入指纹是审核、复查与恢复的依据

任务输入 manifest 保存下述逻辑结构，实际 UUID 和摘要必须有效：

```json
{
  "dataset_id": "UUID",
  "label_schema_digest": "SHA256_HEX_64",
  "assets": [
    {"asset_id": "UUID", "media_digest": "SHA256_HEX_64",
     "annotation_digest": null, "source_metadata_digest": "SHA256_HEX_64"}
  ],
  "resolved_config": {},
  "capability_id": "registered_capability_name",
  "implementation_version": "recorded-version",
  "model_identity": null,
  "input_fingerprint": "SHA256_HEX_64"
}
```

此处是 InputManifest 唯一字段定义；公共 API 通过引用复用，不另外定义删减版。dataset_id、label_schema_digest 在 video_probe 等非 Dataset 操作中可以为 null，纯控制操作的 assets 可以为空；Dataset 检查与审核操作必须有 Dataset 和类别摘要，不得借可空规则忽略实际输入。assets 中 media_digest 绑定实际媒体字节；annotation_digest 可空，source_metadata_digest 无来源信息时可空。resolved_config 是有效配置对象；model_identity 为 null 或包含 provider、model_id、可空 revision 的对象，不含密钥。

input_fingerprint 由 Core 对规范化输入计算，排除摘要自身。规范化格式与摘要算法只由 Core 实现；Worker 接收经授权的输入 manifest 引用及其指纹，不要求 Rust、Python 各自重新序列化并算出同一个值。annotation_digest 为 null 表示该输入明确没有工作 JSON，而不是任意忽略未知文件。调用模型时记录实际可获得的模型标识；未知版本不能伪造。该记录能判断输入是否改变，不承诺云模型输出完全可复现。

### 5. 工作区八业务表与技术元数据 DDL

可执行结构以 [Workspace v1 迁移](../../migrations/workspace/0001_initial.sql) 为权威。下面保留设计基线便于评审。写连接在迁移事务前启用外键，迁移器在同一事务中创建表和索引、写入技术元数据并执行完整性和外键检查。时间、已验证的 Workspace UUID 及跨行业务规则由 Core 提供或验证。

```sql
PRAGMA foreign_keys = ON;
-- 部署初始化时在实际本地工作区数据库启用并核验 WAL。
-- journal_mode=WAL 和连接配置见本章存储策略，不代表在内存示例中运行 WAL。

CREATE TABLE schema_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    workspace_id TEXT NOT NULL UNIQUE,
    schema_version INTEGER NOT NULL CHECK (schema_version >= 1),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE datasets (
    dataset_id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    categories_json TEXT NOT NULL DEFAULT '[]',
    config_json TEXT NOT NULL DEFAULT '{}',
    row_revision INTEGER NOT NULL DEFAULT 0 CHECK (row_revision >= 0),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE assets (
    asset_id TEXT PRIMARY KEY NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('IMAGE', 'VIDEO')),
    location_kind TEXT NOT NULL
        CHECK (location_kind IN ('WORKSPACE', 'EXTERNAL')),
    path TEXT NOT NULL,
    original_filename TEXT NOT NULL,
    file_origin TEXT NOT NULL CHECK (file_origin IN ('IMPORTED', 'GENERATED')),
    availability TEXT NOT NULL DEFAULT 'ONLINE'
        CHECK (availability IN ('ONLINE', 'OFFLINE')),
    annotation_path TEXT,
    annotation_format TEXT,
    annotation_origin TEXT NOT NULL DEFAULT 'NONE'
        CHECK (annotation_origin IN ('NONE', 'PREEXISTING', 'APP_CREATED')),
    annotation_local_image_id INTEGER CHECK (annotation_local_image_id >= 0),
    media_digest TEXT,
    annotation_digest TEXT,
    source_video_id TEXT REFERENCES assets(asset_id) ON DELETE RESTRICT,
    source_metadata_json TEXT NOT NULL DEFAULT '{}',
    metadata_json TEXT NOT NULL DEFAULT '{}',
    row_revision INTEGER NOT NULL DEFAULT 0 CHECK (row_revision >= 0),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE (location_kind, path),
    CHECK (kind = 'IMAGE' OR annotation_path IS NULL),
    CHECK (
        (annotation_path IS NULL AND annotation_origin = 'NONE')
        OR (annotation_path IS NOT NULL AND annotation_origin <> 'NONE')
    )
);
CREATE UNIQUE INDEX uq_assets_work_annotation
    ON assets(location_kind, annotation_path)
    WHERE annotation_path IS NOT NULL AND annotation_format = 'COCO_IMAGE_BBOX';
CREATE INDEX idx_assets_source_video ON assets(source_video_id);
CREATE INDEX idx_assets_availability ON assets(availability);

CREATE TABLE workflow_runs (
    workflow_run_id TEXT PRIMARY KEY NOT NULL,
    dataset_id TEXT REFERENCES datasets(dataset_id) ON DELETE SET NULL,
    workspace_session_id TEXT NOT NULL,
    template_key TEXT NOT NULL,
    plan_kind TEXT NOT NULL
        CHECK (plan_kind = 'template_instance'),
    plan_json TEXT NOT NULL,
    node_states_json TEXT NOT NULL DEFAULT '{}',
    status TEXT NOT NULL DEFAULT 'DRAFT',
    request_id TEXT NOT NULL,
    operation_kind TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    input_manifest_path TEXT,
    checkpoint_manifest_path TEXT,
    result_manifest_path TEXT,
    error_code TEXT,
    error_message TEXT,
    row_revision INTEGER NOT NULL DEFAULT 0 CHECK (row_revision >= 0),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    started_at_ms INTEGER,
    finished_at_ms INTEGER,
    UNIQUE (operation_kind, idempotency_key)
);
CREATE INDEX idx_workflows_dataset_status ON workflow_runs(dataset_id, status);

CREATE TABLE tasks (
    task_id TEXT PRIMARY KEY NOT NULL,
    workflow_run_id TEXT REFERENCES workflow_runs(workflow_run_id)
        ON DELETE RESTRICT,
    node_id TEXT,
    dataset_id TEXT REFERENCES datasets(dataset_id) ON DELETE SET NULL,
    workspace_session_id TEXT NOT NULL,
    capability_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'QUEUED',
    current_attempt_id TEXT,
    attempt_no INTEGER NOT NULL DEFAULT 0 CHECK (attempt_no >= 0),
    request_id TEXT NOT NULL,
    operation_kind TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    input_manifest_path TEXT NOT NULL,
    input_fingerprint TEXT NOT NULL,
    resource_profile_json TEXT NOT NULL DEFAULT '{}',
    execution_metadata_json TEXT NOT NULL DEFAULT '{}',
    output_manifest_path TEXT,
    progress REAL DEFAULT NULL
        CHECK (progress IS NULL OR (progress >= 0 AND progress <= 1)),
    error_code TEXT,
    error_message TEXT,
    row_revision INTEGER NOT NULL DEFAULT 0 CHECK (row_revision >= 0),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    started_at_ms INTEGER,
    finished_at_ms INTEGER,
    UNIQUE (operation_kind, idempotency_key),
    CHECK (
        (workflow_run_id IS NULL AND node_id IS NULL)
        OR (workflow_run_id IS NOT NULL AND node_id IS NOT NULL)
    )
);
CREATE INDEX idx_tasks_workflow_node ON tasks(workflow_run_id, node_id);
CREATE INDEX idx_tasks_status_created ON tasks(status, created_at_ms);
CREATE UNIQUE INDEX uq_tasks_current_attempt
    ON tasks(current_attempt_id) WHERE current_attempt_id IS NOT NULL;

CREATE TABLE review_sessions (
    review_id TEXT PRIMARY KEY NOT NULL,
    asset_id TEXT NOT NULL REFERENCES assets(asset_id) ON DELETE CASCADE,
    dataset_id TEXT NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    source_task_id TEXT REFERENCES tasks(task_id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'OPEN',
    baseline_media_digest TEXT NOT NULL,
    baseline_annotation_digest TEXT,
    baseline_label_schema_digest TEXT NOT NULL,
    review_manifest_path TEXT,
    confirmed_media_digest TEXT,
    confirmed_annotation_digest TEXT,
    confirmed_label_schema_digest TEXT,
    row_revision INTEGER NOT NULL DEFAULT 0 CHECK (row_revision >= 0),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    confirmed_at_ms INTEGER,
    UNIQUE (review_id, asset_id, dataset_id)
);
CREATE UNIQUE INDEX uq_review_active_asset ON review_sessions(asset_id)
    WHERE status IN ('OPEN', 'COMMITTING', 'CONFLICT');
CREATE INDEX idx_reviews_dataset_status ON review_sessions(dataset_id, status);

CREATE TABLE dataset_assets (
    dataset_id TEXT NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    asset_id TEXT NOT NULL REFERENCES assets(asset_id) ON DELETE CASCADE,
    final_review_id TEXT,
    added_at_ms INTEGER NOT NULL,
    PRIMARY KEY (dataset_id, asset_id),
    UNIQUE (asset_id),
    FOREIGN KEY (final_review_id, asset_id, dataset_id)
        REFERENCES review_sessions(review_id, asset_id, dataset_id)
        ON DELETE RESTRICT
);

CREATE TABLE staging_items (
    asset_id TEXT PRIMARY KEY NOT NULL
        REFERENCES assets(asset_id) ON DELETE CASCADE,
    target_dataset_id TEXT REFERENCES datasets(dataset_id) ON DELETE SET NULL,
    review_id TEXT REFERENCES review_sessions(review_id) ON DELETE SET NULL,
    check_task_id TEXT REFERENCES tasks(task_id) ON DELETE SET NULL,
    check_acceptance_digest TEXT,
    check_accepted_at_ms INTEGER,
    created_at_ms INTEGER NOT NULL,
    CHECK (
        (check_acceptance_digest IS NULL AND check_accepted_at_ms IS NULL)
        OR (check_acceptance_digest IS NOT NULL AND check_accepted_at_ms IS NOT NULL)
    )
);
CREATE INDEX idx_staging_target ON staging_items(target_dataset_id);

CREATE TABLE file_operations (
    operation_id TEXT PRIMARY KEY NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_id TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    workspace_session_id TEXT NOT NULL,
    operation_kind TEXT NOT NULL,
    dataset_id TEXT REFERENCES datasets(dataset_id) ON DELETE SET NULL,
    asset_id TEXT REFERENCES assets(asset_id) ON DELETE SET NULL,
    review_id TEXT REFERENCES review_sessions(review_id) ON DELETE SET NULL,
    task_id TEXT REFERENCES tasks(task_id) ON DELETE SET NULL,
    status TEXT NOT NULL DEFAULT 'PREPARED',
    manifest_path TEXT NOT NULL,
    manifest_digest TEXT NOT NULL,
    result_json TEXT,
    error_code TEXT,
    error_message TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    completed_at_ms INTEGER,
    UNIQUE (operation_kind, idempotency_key)
);
CREATE INDEX idx_file_operations_status ON file_operations(status);
CREATE INDEX idx_file_operations_asset ON file_operations(asset_id);
```

#### 5.1 不能只靠上面 DDL 表达的规则

- Dataset 名称可以重复，ID 不可重复；类别名称唯一性在 JSON 字典校验中实现。review 和 Task 的 label_schema_digest 均涵盖本次使用的完整 Dataset 类别字典，避免两套失效规则。
- 路径先按所属文件系统解析与规范化，再比较；不能为跨平台方便就把所有路径强制转小写。
- 源视频必须引用 VIDEO 资源；Dataset 成员只能是 IMAGE。普通原视频不成为训练图片成员。
- 同一 Asset 不能同时出现在 dataset_assets 与 staging_items；Core 的注入事务同时删除后者并新增前者。
- 活动审核只有一个；候选可以多份，存 review manifest，不新增审核会话来表示每一个候选。
- CONFIRMED 审核必须有三个 confirmed 摘要及 confirmed_at_ms。final_review_id 必须指向同图片、同 Dataset 的有效确认。
- 导入的现有图片可以先登记为 Dataset 成员，但 final_review_id 为空时不满足最终导出条件。批量确认现有标注可以生成确认记录，不要求逐张重新画框。
- staging_items 的 review_id、check_task_id 必须对应当前 Asset 和目标 Dataset；没有目标 Dataset 时不能确认并注入。
- check_acceptance_digest 绑定用户确认的检查输入。检查报告不因完成就自动“通过”；输入变化后此前确认失效。
- 删除 Dataset 时先结束活动任务，并在事务内解除成员和 Staging 的审核引用，再清理 Dataset；不要直接执行裸 DELETE 期待外键代替业务删除流程。

progress 为 NULL 表示未知进度，0 表示已经知道进度且尚未推进，两者不能混用。

plan_json 记录 TemplateInstancePlan 模板实例及参数。node_states_json 保存节点绑定与控制进度，不能成为 tasks.status 的另一权威副本。运行中调整必须通过计划校验和 row_revision 检查。

一行 Task 记录当前尝试。旧 attempt 的执行信息写入任务日志/manifest，不保留第二张 attempts 业务表；日志应记录任务 ID、尝试 ID、输入指纹和产物。它们不自动构成跨崩溃 exactly-once 保证。Core 只接受当前会话、当前 attempt 和合法状态迁移的结果。新的检查输入应创建新 Task 并记录前因，节点到 Task 的历史绑定可放运行记录；不能覆盖旧 Task 的输入。

#### 5.2 执行授权、游标与终态如何持久化

tasks.execution_metadata_json 保存当前 attempt 的对账信息，字段复用公共执行契约：

```text
ExecutionMetadata {
  executor_session_id: UUID,
  workspace_session_id: UUID,
  grant_id: UUID,
  resource_reservation_id: UUID,
  node_spec_fingerprint: SHA256_HEX_64,
  operation_digest: SHA256_HEX_64,
  input_fingerprint: SHA256_HEX_64,
  grant_state: ISSUED | CLAIMED | REVOKED,
  claimed_execution_id?: UUID,
  last_source_event_seq?: decimal_string,
  last_execution_event_id?: UUID,
  terminal_receipt?: {
    completion_fingerprint: SHA256_HEX_64,
    accepted_at: RFC3339_UTC,
    result_manifest_ref?: ArtifactRef
  },
  checkpoint_ref?: ArtifactRef
}
```

尚未分配尝试时该对象为 {}。执行结果被接受时，当前 attempt 的元数据与任务状态在同一 SQL 事务提交。旧 attempt 的明细保存在执行记录中；新 attempt 不复用旧授权或终态收据。

Core 重启后旧凭据与 Grant 全部失效。持久字段用于判断发生过什么，不恢复旧信号量和授权。执行方失联时不能凭“没收到心跳”立即把同一 GPU 额度发给另一个任务；先确认执行已停止或相关进程已经退出。

每个进程的 stream_epoch、event_seq 以及有界事件重放缓冲保存在内存，重启后换 epoch。last_source_event_seq 只用于当前执行来源的去重对账；不等于持久完整事件流。前端断线超过窗口或 Core 重启后重新获取快照，不承诺永久回放所有进度事件。

TASK 归属的 Artifact 由 tasks.output_manifest_path 中已接受的产物清单解析；ASSET、REVIEW 归属的当前工作 Artifact 分别由资源元数据、审核工作 manifest 定位。tasks.output_manifest_path 指向已接受的任务产物清单。Core 读取并校验清单条目后才对外解析 artifact_id；Worker 报出一个 ID 不等于该产物已正式注册。正式视频工作清单、track map 等另由 assets.metadata_json 保持当前引用，禁止随旧候选任务清理。

#### 5.3 app.db 的既有全局职责

以下两张表位于独立 app.db，不加入 Workspace 八业务表，也不存图片、标注或任务正文。它们落实 L1 已确认的全局注册与设置职责。

可执行全局结构以 [app.db v1 迁移](../../migrations/app/0001_initial.sql) 为权威。全局库使用 `main.user_version`，在同一事务内创建两表、写入初始设置、推进版本并执行完整性及外键检查。

```sql
CREATE TABLE workspace_registry (
    workspace_id TEXT PRIMARY KEY NOT NULL,
    root_path TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    last_opened_at_ms INTEGER,
    availability TEXT NOT NULL DEFAULT 'ONLINE'
        CHECK (availability IN ('ONLINE', 'OFFLINE'))
);

CREATE TABLE app_settings (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    config_json TEXT NOT NULL DEFAULT '{}',
    config_revision INTEGER NOT NULL DEFAULT 0 CHECK (config_revision >= 0),
    updated_at_ms INTEGER NOT NULL
);
```

workspace_registry 是入口索引；真正打开时仍验证目标 Workspace 标志和 schema_meta，不能仅按同名目录复用 ID。app_settings.config_json 保存全局资源上限、界面偏好和非秘密连接设置，API Key 仍由安全凭据存储管理。全局修改以 config_revision 校验；Dataset 覆盖保存在 datasets.config_json，实际任务配置冻结在输入 Artifact。

### 6. 导出时重新分配 COCO ID

导出以用户选定 Dataset 的正式成员为范围，逐图检查当前内容与确认摘要，然后读取工作 JSON。建立三组映射：

1. asset_id → export image.id；
2. (asset_id, local annotation.id) → export annotation.id；
3. 单图类别引用 → 当前 Dataset 的类别 ID。

所以不能建立 UNIQUE(dataset_id, local_image_id)；每图 image.id=1 是允许的。类别 ID 不因文件读取顺序变化；图片和标注 ID 可按稳定排序连续分配。file_name 以实际导出的文件布局产生，名称冲突在生成包前处理。

结果写入独立导出目录，包含汇总 COCO、所需图片以及本次导出的映射/来源清单；不修改原 Dataset 的 ID。该导出产物不建立 Dataset Version 业务。正式 track map 可用于生成来源清单，但不默认向标准 COCO 增加扩展字段。

导出开始和完成时核对受读资源摘要；MVP 可在 Core 内阻止所选数据的并发写入。其他软件仍可能修改外部文件，因此读取前后发现变化必须失败或明确要求重试，不能承诺对任意外部程序的瞬时快照。

### 7. 文件提交、幂等与故障恢复

单图 JSON 修改由 Annotation Manager 校验，文件提交模块执行；Data Manager 负责 SQL。SQLite 事务与文件替换不是一个原子事务。

#### 7.1 正常提交顺序

1. 验证 request、session、目标 Asset、当前 row_revision、基线图片/标注/类别摘要、授权路径及审核状态。
2. 以 operation_kind 和 idempotency_key 查询 file_operations，并比对 request_fingerprint。已完成返回原结果；同 key 不同意图返回冲突；未完成恢复原操作，不另起第二次覆盖。
3. 在受控临时位置准备新 JSON、必要的正式 track map 和操作 manifest。manifest 记录每个目标的旧摘要、新摘要、临时文件、预期 SQL 变更和产物身份。新的外部标注操作先消费独立写授权并实际探测权限与文件占用，失败不登记操作意图；其 manifest v1 可选字段 authorized_write_root 将授权根目录冻结在摘要中，应用时重验规范路径，拒绝目录联接重定向。旧的已受理操作允许缺省该字段，继续按原受保护清单恢复。恢复后文件位于当前 Workspace 受管目录时使用当前工作区写权限。只读/拒绝写入映射 PERMISSION_DENIED，Windows 共享锁映射 FILE_BUSY。
4. 临时内容验证通过后，持久化 PREPARED 意图；Core 获取相关资源的提交锁并再次检查基线。
5. 逐个替换目标文件。单图文件用完整文件替换避免留下半截正文；多文件操作逐项记录进度，不宣称整体文件系统事务。
6. 校验新摘要，执行一个 SQL 事务：更新登记摘要和正式依赖、确认审核、更新成员关系，并将操作记为 COMMITTED。
7. 成功后清除活动候选/草稿引用，按依赖关系清理临时文件；返回保存过的结果。

审核待人确认期间不长期占用 GPU 或文件提交锁。类别重命名之类多文件操作使用 Dataset 级变更门槛，普通单图提交按 Asset 串行；计算任务仍可并行，只是旧输入的结果不能直接提交。

#### 7.2 恢复判定

| 故障窗口 | 恢复动作 |
|---|---|
| 临时文件写完，但没有 PREPARED | 原工作文件未变；识别并清理无引用临时文件 |
| PREPARED 已写，目标仍是旧摘要 | 检查用户授权仍有效与临时文件完好，再安全继续，或记为中止 |
| 部分目标为新摘要、部分仍旧 | 按 manifest 逐项核对后完成剩余步骤；发现第三种内容则进入 CONFLICT |
| 文件已更新，SQL 尚未提交 | 以新摘要验证实际文件，幂等补齐同一操作的 SQL 更新 |
| SQL 已提交，客户端没收到结果 | 根据同一 key 返回已保存的结果，不重复修改文件或注入成员 |
| 旧、新摘要都不匹配 | 保留冲突证据，不自动覆盖外部变更，等待明确处理 |

文件操作状态建议为 PREPARED、FILES_APPLIED、COMMITTED、CONFLICT、ABORTED、FAILED；即使状态写入滞后，也必须检查实际文件，不能仅凭状态推进。操作完成后可压缩临时记录，但在允许的重发窗口内保留幂等键及结果；不能刚成功就删除去重依据。

对应用内部采用提交锁与摘要校验；对未配合的外部软件，“检查摘要后到替换文件前”仍存在竞争窗口。MVP 不宣称严格跨进程 CAS。检测到变化要停写或进入冲突；外部软件对受管理文件的并行编辑不属于无损合并承诺。

图片修复同样先产生候选文件，用户接受后才替换目标图片；替换会使旧检查和旧标签确认失效，必须复查或重新确认。file_operations 是恢复日志，不是原图和旧标签的长期版本库。

颜色应用受理时核对候选归属、基础图像摘要、标注摘要、类别摘要与 Asset 修订。Core 封存用户选择的候选字节与操作清单，恢复时按目标指纹区分尚待替换、替换已发生和外部变化冲突。SQLite 事务登记新图像摘要、过期审核与清除已失效待接收检查引用。标注内容随资产保留，复检使用新图像摘要。

### 8. 删除与备份边界

#### 8.1 本地 SQLite 运行约束

MVP 的 workspace.db 放在本地文件系统；不把网络共享目录或多机器同时同步修改的数据库作为运行方式。初始化真实数据库时设置并核验 journal_mode=WAL；每次新建连接在事务前启用 foreign_keys=ON。写连接采用 synchronous=FULL，并配置有限 busy timeout；这些是建议默认，不能通过无限等待掩盖死锁或重复提交。Core 仍是业务写入入口，连接池不改变这一点。

WAL 模式下备份使用 SQLite 备份接口生成一致的数据库副本，或在干净关闭连接、完成检查点后复制关闭的数据库；不能在应用运行时只复制 workspace.db 并忽略未检查点的内容。不要把 sqlite 主文件、wal、shm 随意按不同时间复制后当作一致备份。备份目标可以位于其他位置，但不能让两个 Core 同时打开同一个活动工作区数据库。

#### 8.2 删除与备份范围

Dataset 删除和单张资源删除不是同一动作。默认保护外部原图及导入前已经存在的 JSON；只按用户已选择的规则清理软件创建的标签/生成产物。annotation_origin 不能因为软件编辑过一个旧 JSON 就从 PREEXISTING 改为 APP_CREATED。外部路径范围必须在删除计划中明确列出。

单张图片可解除关系或显式物理删除；物理删除无回收站。物理删除前先记录可恢复的操作意图，完成后删除相应引用；不把恢复日志当作能恢复已删除原文件的备份。源视频仍被帧引用时保留来源登记，不能级联清除帧；若源视频物理文件被用户删除，则标记来源离线。

备份包含 SQLite 的一致副本、所有注册的外部图片/JSON/源视频、内部帧、正式来源、track map、视频工作清单及当前人工草稿、必要任务报告和恢复记录。先暂停提交并明确备份边界，备份期间检测外部文件变化；必要时标记不完整并重试。不会因为登记一个目录就把其中所有无关文件自动复制。模型缓存和临时候选可按保留规则排除，API Key 不作为普通明文配置打包。

备份 manifest 记录原路径、包内相对路径及摘要，恢复到用户选择的新根目录后重建定位。备份恢复是一项显式操作，不构成日常 Dataset Version 功能。

### 9. 本章采用的配套设计约定

1. 单图工作 JSON 使用 COCO bbox profile；AnyLabeling 作为可选格式适配，不默认为同一格式。
2. Dataset 类别字典由 datasets.categories_json 管理，工作 JSON 保留必要类别副本。
3. B 八表的职责及上述约束；详细状态值复用统一状态机。
4. 源帧和当前 track map 作为正式元数据依赖，避免标准 COCO 承担业务扩展。
5. 对外部修改采用检测与冲突处理；文件和数据库使用可恢复提交，明确不承诺外部并发严格 CAS。

上述约定为本方案的数据契约；不再维护另一套并行 JSON、数据库或审核实现。


---
