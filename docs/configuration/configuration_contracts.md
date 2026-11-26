# L2 配置契约与生效规则

> DD-DESIGN-L2-001 方案Ⅰ v1.0；按主题迁移于 2026-11-26。设计契约非已实现代码。原章节编号保留，导航见 [文档索引](../README.md)。

### 配置契约与生效规则

#### 配置分层

配置采用 `内置默认值 → 应用设置 → Workspace 设置 → Dataset 检查设置 → 本次任务显式参数` 的优先级。只有列入该层白名单的字段可覆盖；任务参数不能覆盖认证、授权根、进程管理和全局资源上限。

| 位置 | 保存什么 | 不保存什么 |
|---|---|---|
| 应用设置 | 最近 Workspace、界面偏好、全局资源上限、模型连接的非秘密参数 | 完整 API Key、数据集 bbox |
| Workspace 根目录 `workspace.json`（文件名 Design Default） | Workspace 身份、格式标识、Workspace 默认采样/检查设置；同时作为唯一标志文件 | 每个样本的标注副本 |
| `datasets.config_json` | 本 Dataset 允许覆盖的检查项、阈值和默认导出选项 | 全局资源授权、秘密信息 |
| 任务输入产物 | 开始执行时解析后的配置、工具/模型标识、输入摘要 | 模型 Key；之后不断变化的实时配置引用 |
| 操作系统安全存储 | 单个共享模型 Key 的秘密值 | 普通模型输出和日志 |

技术配置可有 `config_format_version`；只用于解析与迁移。`config_revision` 是当前配置的乐观并发序号：Workspace 保存在 `workspace.json`；应用设置保存在 `app.db` 对应记录；Dataset 配置复用 `datasets.row_revision`。它们不提供旧配置或数据集历史浏览。任务产物冻结实际参数，是为了说明“这次检查用了什么”，不构成 Dataset 内容版本管理。

当前 MVP 不使用环境变量或 CLI 参数覆盖用户业务设置。开发构建中的调试变量不进入发布配置契约，也不能放宽认证、文件授权或人工关卡。全局上限与下级请求同时存在时取允许的较小值；下级配置不能通过同名键扩大执行权限。

下面是一份完整、可解析的 Workspace 标志/配置文件示例。应用级模型连接和资源上限由 Core 在运行时合并，所以不在该文件重复存放；JSON 内不允许注释。

```json
{
  "config_format_version": 1,
  "config_revision": 0,
  "workspace_id": "2c823c8a-1a3a-4b0d-97b3-0982e5f2aca9",
  "name": "Industrial Vision",
  "defaults": {
    "checks": {
      "image_quality": { "enabled": true },
      "annotation_geometry": { "enabled": true },
      "semantic": { "enabled": false },
      "cleanlab": { "enabled": false }
    },
    "video": {
      "tracking": {
        "mode": "EVERY_N_FRAMES",
        "every_n": 1,
        "interval_ms": 100
      },
      "dataset": {
        "mode": "TIME_INTERVAL",
        "every_n": 30,
        "interval_ms": 1000,
        "include_keyframes": false
      }
    },
    "workflow": {
      "max_adjustments": 2,
      "max_cloud_calls": 20,
      "max_total_tokens": 100000
    },
    "review": { "autosave_ms": 1000 },
    "repair": { "feather_px": 0 },
    "export": { "unreviewed_policy": "BLOCK" }
  }
}
```

文件为 UTF-8，禁止重复键；未知字段和高于支持范围的 `config_format_version` 拒绝写入并返回明确错误。`workspace_id` 创建后不可通过普通设置补丁更换；`name` 为 1–128 个字符。Workspace 只写自身允许项；`config_format_version` 迁移由 Core 管理。读取旧配置遇到可补齐的缺省字段时使用当前明示默认值，返回其来源，不静默重写原文件。

#### 推荐参数

下表的数值是 **MVP 初始建议**，需要在目标机器上验证；不是依赖库保证，也不是已测得性能。对于显著改变任务语义的参数，任务启动后使用其冻结值。

| 配置键 | 类型 / 建议默认值 | 范围 / 校验 | 生效时机 |
|---|---|---|---|
| `scheduler.gpu_slots` | 整数，1 | 1–可用设备策略上限；没有 GPU 时相关能力不可用 | 新任务；减额不强杀在途任务 |
| `scheduler.cpu_slots` | 整数，`min(2,可用逻辑CPU)` | ≥1 且不超过配置上限 | 新任务 |
| `scheduler.video_slots` | 整数，1 | 1–4；与总资源配额共同约束 | 新任务 |
| `model.max_parallel` | 整数，2 | 1–8；仍受供应商限流和全局配额约束 | 新请求 |
| `rpc.unary_deadline_ms` | 整数，10000 | 1000–60000；不代表长任务总超时 | 新 RPC |
| `retry.max_attempts` | 整数，3，包含首次 | 1–5；只用于允许安全重试的错误 | 新任务 |
| `workflow.max_adjustments` | 整数，2 | 0–5；所有模板均受限制 | 新 Workflow |
| `workflow.max_cloud_calls` | 整数，20 | 0–100；达到后停止云端扩展步骤并报告 | 新 Workflow |
| `workflow.max_total_tokens` | 整数，100000 | 0–1000000；作为预算，未知用量不得当作0 | 新 Workflow |
| `model.provider` / `model.name` | 字符串，未配置 | 必须属于适配器支持范围；模型具备所需图像能力 | 新任务；旧任务保留既有标识 |
| `model.endpoint` | HTTPS URL，供应商默认端点 | 按所选适配器校验；不接受模型输出修改端点 | 新连接 |
| `model.secret_ref` | 不透明引用，可空 | 指向 Core 管理的一个秘密；非秘密本身 | 更新后重建受控客户端 |
| `checks.semantic.enabled` | 布尔，false | 连接验证、能力和用户启用后生效 | 新任务 |
| `checks.image_quality.enabled` | 布尔，true | 没有可用适配器时显示未执行 | 新任务 |
| `checks.annotation_geometry.enabled` | 布尔，true | 对未标注图像给出“无标注”，不得当成正常负样本 | 新任务 |
| `checks.cleanlab.enabled` | 布尔，false | 需要兼容的标签与预测结果，能力检查通过后可启用 | 新任务 |
| `video.tracking.mode` | 枚举，`EVERY_N_FRAMES` | `EVERY_N_FRAMES` / `TIME_INTERVAL`；后者需工具声明支持 | 新追踪任务 |
| `video.tracking.every_n` | 整数，1 | 1–1000；过大可能降低追踪质量，界面显示实际采样密度 | 新追踪任务 |
| `video.tracking.interval_ms` | 整数，100 | 1–3600000；仅时间模式有效，且所选算法须支持 | 新追踪任务 |
| `video.dataset.mode` | 枚举，`TIME_INTERVAL` | `TIME_INTERVAL` / `EVERY_N_FRAMES` | 新采样任务 |
| `video.dataset.interval_ms` | 整数，1000 | 1–3600000；仅时间模式有效 | 新采样任务 |
| `video.dataset.every_n` | 整数，30 | 1–100000；仅帧模式有效，不作为时间的替代解释 | 新采样任务 |
| `video.dataset.include_keyframes` | 布尔，false | true 时额外加入区间内人工关键帧，并按源帧身份去重 | 新采样任务 |
| `review.autosave_ms` | 整数，1000 | 300–10000；只保存草稿，不自动确认 Final | 前端新编辑会话 |
| `repair.feather_px` | 整数，0 | 0–64，原图像素 | 新预览 |
| `export.unreviewed_policy` | 枚举，`BLOCK` | `BLOCK` / `EXCLUDE_WITH_MANIFEST`；不支持悄悄包含未确认样本 | 新导出 |
| `ui.overlay_visibility` | 各图层布尔值，默认显示 | 纯显示偏好，不改变提交集合 | 即时 |

未激活模式对应的参数可被保存为用户偏好，但解析后的任务配置只含实际生效字段，防止“every_n 与 interval_ms 同时生效”的歧义。置信度阈值必须按工具分别命名；不同算法分数不可共享一个无含义的总阈值。数据质量“通过”不等于某个通用数值阈值，仍保留人工质量确认关卡。

云端默认预算不是“20 次调用足以覆盖任意 Dataset”的承诺。计划预览必须列出计划检查范围、预计调用、抽查策略与可能未覆盖样本；预算不足时可缩小本次语义抽查或中止该阶段，报告明确写出覆盖率/未检查范围。不能在预算耗尽后把整个 Dataset 标成完成了全量 VLM 语义审核。

任务总超时使用工具能力描述中的 `execution_timeout_ms`，由工具类型和输入规模策略产生，必须在计划预览中可见；不能用上表 RPC deadline 替代。具体上限值与指数退避、取消等待时间统一以公共任务契约为准。

#### 设置 API 和密钥使用

`settings_get` 返回解析后的非秘密设置、来源层和能力状态。`settings_update` 接收带字段白名单的补丁、预期设置修订号及请求幂等键；返回已接受字段和何时生效。修订号用于防止两个窗口互相覆盖，不是 Dataset 版本。未知键、错误类型或越界值拒绝更新，不默默忽略。

前端可以提供用户输入的新 Key，但不能读取已保存 Key；保存后只返回 `configured=true` 与非秘密的连接标识。Core 持久保管 Key，按已确认 L1 在受控启动/授权通道中交给可信 Agent Service 的进程内统一模型客户端；不经命令行参数、普通日志、业务数据库或 Worker 配置传递。更换 Key 时建立新会话，旧在途调用按既有策略完成或被取消。

`model_connection_test` 验证连接/认证/模型能力，返回非秘密结果；若仅验证认证不需要图片，不能顺手上传用户 Dataset。真正的图像能力测试使用明确说明的测试输入。云端限流、供应商超时和无可用 Key 产生清晰的未执行/失败原因，本地检查可以继续。

任务清单上的调用预算是应用层控制。已经发给供应商的请求在本地超时后可能仍执行或计费；预算核算需保留“结果未知”项，不能宣称取消请求保证没有外部成本。
