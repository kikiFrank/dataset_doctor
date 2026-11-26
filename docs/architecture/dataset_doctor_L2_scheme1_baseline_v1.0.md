# Dataset Doctor — L2 方案Ⅰ设计基线

**文档编号：** DD-DESIGN-L2-001  
**文档版本：** 1.0  
**方案确认日期：** 2026-11-25  
**决策状态：** Confirmed — 用户已明确选择方案Ⅰ  
**基线状态：** Baselined — 以模板工作流与专用类型接口作为唯一当前实现方向  
**实现与验证状态：** 未开始应用编码；Rig SDK 独立 PoC 已验证；其他工程 PoC、平台打包、模型效果和性能待验证  
**适用阶段：** L2 模块、数据、API、工作流、前端与配置设计

## 当前结论与使用方式

用户于 2026-11-25 明确回复：“那么我选择方案1”。本文据此将原双方案评审稿收敛为方案Ⅰ，保留已有模块、数据、审核、视频及恢复契约，采用 **模板工作流 + 专用类型接口 + Agent 有限规划与异常调整**。

本文中的状态作如下区分：

| 标记 | 含义 |
|---|---|
| Confirmed | 已有对话明确确认的决策，包括本次方案Ⅰ选择和既定 L1 边界 |
| Design Default | 选定方案的配套设计默认值与具体契约；可用于后续设计细化，不表示用户曾逐字段确认 |
| To Validate | 需要通过真实工程验证才能成立的能力与运行参数，不表示已经实现 |

具体字段、接口名、COCO 工作 profile、类别字典位置、并发数、超时和重试默认值沿用配套设计。它们作为本基线的工作约定维护；若后续实现发现冲突，应记录调整及影响。无需为普通字段重复进行架构选型。

**当前方向已确定：** Agent 从已注册模板中选择检查项、参数、允许分支与替代策略；Workflow Engine 确定性推进；Core 统一准入和正式提交；Worker 使用明确的 StartX RPC 执行计算。

方案Ⅱ未被选用，其通用能力子图和 Execute 入口不属于当前实现清单。原双方案评审内容保留于本文件的历史版本，供理解选型来源。此处的文档版本与数据库格式版本不增加 Dataset / Annotation 内容版本管理功能。

### 文档内容

| 部分 | 内容 |
|---|---|
| 一、决策边界与已选方案 | 方案Ⅰ决策记录、继承的 L1 约束与首版范围 |
| 二、模块契约 | Core、Agent、Worker、前端职责与用例 |
| 三、数据契约与 SQLite B | 单图 COCO、八张业务表、类别、来源、审核与文件一致性 |
| 四、API、RPC、工具与执行契约 | 前端命令、专用服务接口、状态机、事件与恢复 |
| 五、前端、视频、修复、配置与验收 | 页面映射、双采样、颜色处理、配置与验证标准 |
| 六、设计检查点与下一阶段 | 当前完成状态和 L3 细化入口 |

---

## 分册（原章节的权威位置）

- [二、共同模块契约](../modules/module_contracts.md)
- [三、数据契约与 SQLite](../data/data_contracts_and_sqlite.md)
- [四、API、RPC、工具与执行](../interfaces/api_rpc_workflow_contracts.md)
- [五、领域执行](../modules/domain_execution.md)、[前端契约](../modules/frontend_contracts.md)、[配置](../configuration/configuration_contracts.md)、[验收](../testing/acceptance_and_platform_validation.md)

2026-11-26 更新：采用 Rig v0.47.1（Rust 1.97.1），SDK 独立 PoC 三平台已通过（每平台 10 项），不等同于生产应用集成。Core 持久化、资源准入、授权、人工审核和 Python Worker 职责不变。真实模型、完整恢复与打包仍需验收。

## 一、决策边界与已选方案

### 1.1 有效基线与本轮调整

依据为已确认的《Dataset Doctor L1 系统架构定稿 v1.0》、用户提供的《软件项目设计与开发规范 v1.1》，以及用户在进入 L2 后的最新决定。**新决定优先于旧文档中的冲突描述。**

| 事项 | 当前状态与处理 |
|---|---|
| Rust 主应用、Rust Agent Service、Python Worker | Confirmed；三个后端执行边界，Managers 是逻辑模块 |
| 桌面前端 | Confirmed；Tauri 2 + React + TypeScript + Vite |
| 本地服务通信 | Confirmed；gRPC + Protobuf + TCP Loopback；UI 使用 Tauri Commands / Events |
| 调用拓扑 | Confirmed；Core 控制准入和持久化，Agent 获准后可以直接调用 Worker |
| Agent 组织 | Confirmed；治理协调与视觉审核两个逻辑 Agent，共享一个模型 Key |
| Python 集成 | Confirmed；Rust 管理本地 Worker，Python 库由 Worker 内适配器调用 |
| 标注文件组织 | Confirmed；每张图片旁边一个 JSON，导出时汇总为一个 Dataset COCO JSON |
| 单图文件具体结构 | Design Default；本文建议单图 COCO bbox 工作格式，不等同于 X-AnyLabeling 原生格式 |
| 原 L1 的 Human / Candidate / Final 放同一自定义 JSON | 被最新 COCO 方向替代；本文提出审核草稿与候选外置，当前正式标注写单图 COCO |
| 数据库 | Confirmed；SQLite 方案 B 八张核心业务表；具体字段与约束在本文提出 |
| 当前标注与版本 | Confirmed；保存当前事实，不引入 Dataset / Annotation 内容版本管理 |
| 类别 | Confirmed；一个 Dataset 一个类别集合，仅需名称，允许修改和新增 |
| 类别字典具体权威位置 | Design Default；采用 datasets.categories_json；单图 categories 是可携带的投影 |
| Staging 注入 | Confirmed；检查与用户确认后进入目标 Dataset，或新建 Dataset；不额外复制视频帧 |
| 外部 Dataset | Confirmed；原地引用，外部 JSON 与图片相邻；不在导入时复制全部媒体 |
| 视频双采样 | Confirmed；Tracking Sampling 与 Dataset Sampling 独立配置 |
| 框架、打包、性能 | Rig SDK 独立 PoC 为 Validated；业务集成、打包、性能仍为 To Validate |

文档版本、数据库 Schema 版本、协议版本和输入摘要只服务于兼容性与正确性，不是重新引入被排除的数据历史版本功能。

### 1.2 已确认方案与决策记录

| 项目 | 当前决策 |
|---|---|
| 决策编号 | DD-L2-ADR-001 |
| 决策日期与状态 | 2026-11-25；Confirmed |
| 用户确认 | “那么我选择方案1” |
| 执行组织 | 已注册工作流模板、允许的并行步骤与有限条件分支 |
| Agent 权限 | 选择模板、范围、参数和已有替代策略；异常调整有预算与校验 |
| 计划类型 | TemplateInstancePlan；plan_kind 固定为 template_instance |
| Worker 计算入口 | StartProfileDataset、StartImageQualityInspection、StartTracking 等专用类型 RPC |
| 控制与提交 | Core 统一资源准入、任务状态持久化、人工批准和正式文件提交 |
| Agent 组织 | 治理协调 Agent、视觉审核 Agent；共享一个模型客户端和 Key |
| 数据与交付 | 沿用 SQLite B、单图 JSON/导出 COCO、Tauri 桌面与 Python Worker |
| 未采用内容 | 通用能力子图、任意节点端口组合和统一 Execute 计算入口 |
| 后续改动规则 | 普通实现细节在 L3 细化；改变进程边界、数据权威或正式提交权限需记录架构变更 |

选定方案支持“检查 → 候选 → 人工选择 → Core 提交 → 复检 → 导出”的完整闭环，视频生成样本复用同一套治理逻辑。模板可以包含并行和条件分支，不要求每一步都调用模型。

### 1.3 首版格式与能力边界

本文将首版的正式标注目标限定为 **COCO bbox 检测数据**，加上本项目按图片拆分的 COCO 工作文件。COCO 还包含分割、关键点等格式；使用 COCO JSON 并不表示本项目已经支持这些任务。[S1]

| 能力 | 首版契约 |
|---|---|
| 单图 JSON | 一个 images 条目、该图 annotations、类别字典；正式保存时按本文 profile 校验 |
| Dataset 导入 | COCO bbox 聚合文件及本项目单图 COCO；预览后登记或拆分，原地转换必须显式选择 |
| Dataset 导出 | 默认输出一个合法聚合 COCO bbox JSON；图片路径与导出清单一致 |
| X-AnyLabeling / YOLO | 预留具名格式适配器；首版必选适配范围须在实现清单中声明。不能因“一图一 JSON”就宣称原生兼容 |
| 不支持的标注形状 | 明确列出并阻止有损转换；不能悄悄把 polygon、旋转框或 crowd 当普通矩形 |
| 图片质量 | 可执行的结构/图像/重复检查；各检查有独立适用条件和覆盖状态 |
| 标签质量 | 确定性几何与类别检查；需要模型预测的检查，必须先有匹配的有效预测 |
| 语义检查 | 选择疑点样本调用模型；结果是候选意见，不直接升级为 Final |
| 视频 | 时间段标注、追踪、间隔采样、来源追溯、进入 Staging；不做跨视频泄漏检查 |
| 修复 | 区域颜色调整候选；人工选择应用；不承诺恢复真实物理颜色 |
| 模型训练 | 不隐式训练模型。若缺预测或模型，报告检查未执行，而不是质量通过 |

Cleanlab 的目标检测标签质量接口同时接收 labels 和 predictions，因此本项目必须把“预测可用性”建模为前置条件；不能仅安装 Cleanlab 就认为所有标签问题都能检查。[S2]

---


## 六、设计检查点与下一阶段

### 6.1 当前状态

| 事项 | 状态 | 下一步 |
|---|---|---|
| L1 系统架构 | Confirmed | 作为模块设计约束继承 |
| L2 执行方案选择 | Confirmed | 方案Ⅰ为当前唯一实现方向 |
| L2 综合设计文档 | Baselined | 本文收敛模块、数据、API、模板、页面与配置 |
| 具体字段和参数 | Design Default | 在同一契约下细化；变化需同步相关定义 |
| 可编译 Protobuf / JSON Schema / 类型与迁移文件 | 待固化 | 按本文生成并进行契约核验，不能让各端独立猜字段 |
| Agent SDK / Python 打包、视频定位等 | Rig 独立 SDK PoC Validated；其余 To Validate | 业务集成仍按既定门槛验证 |
| 应用实现 | 未开始 | 本次只收敛设计，没有启动业务代码开发 |

L2 已形成一套方案Ⅰ设计基线。方案选择完成与工程验证通过是不同状态；本次确认不把未实现能力改写为已完成。

### 6.2 L3 的范围与交付清单

下一阶段针对既定契约形成可交给 AI Coding 的实现约束，重点为：

1. Rust workspace / crates、前端、Agent Service、Python Worker、contracts、migrations、tests 和 docs 的目录与依赖规则。
2. 每个模块的实现入口、公开接口、可调用依赖和禁止跨越的持久化边界。
3. Protobuf、Tauri DTO、Agent 工具 Schema 与文件 Schema 的生成和映射方法。
4. 任务取消、文件提交、数据库迁移、服务启动/关闭及恢复所需的实现约束。
5. 第一个纵向功能闭环、开发任务拆分、可复核验收标准和必要 PoC 顺序。

这些内容细化既定架构，不需要重新比较两套工作流方案。普通库选择和参数可采用合适默认值；只有影响业务边界、数据归属或交付能力的变化才需要单独记录决策。

### 6.3 后续实现切分

| 顺序 | 实现路径 | 可验收结果 |
|---|---|---|
| 1 | Workspace、资源登记、单图 COCO 与安全提交 | 一张图可登记、审核、保存与重开；中断不伪报成功 |
| 2 | Dataset、有效 Final 与 COCO 导出 | 局部 ID 可正确重映射，负样本和未审核样本区分 |
| 3 | Core 调度、Worker 计算、状态查询和通知 | 真实检查可从提交走到终态，并处理服务中断 |
| 4 | 工作流模板、两个 Agent 与受限工具 | 合法规划与有限调整可执行，正式数据提交仍有批准关卡 |
| 5 | 视频定位、关键帧、追踪、双采样与 Staging | 视频样本进入同一审核及导出路径 |
| 6 | 区域改色、复检、备份和平台交付 | 应用候选后正确复核，登记资源可备份恢复 |

本表是后续实施顺序，不是本次已经执行的工作。当前没有创建工程骨架、运行生产迁移或开始真实数据修改。

### 6.4 文档变更记录

| 版本 | 日期 | 变更 |
|---|---|---|
| 0.1 | 2026-11-23 | 双方案评审稿，形成共同模块与数据/API契约 |
| 1.0 | 2026-11-25 | 根据用户选择收敛方案Ⅰ；移除未采用的子图计划及 Execute 入口；同步 DDL、RPC 示例、页面和验收要求 |

原评审稿作为本文件历史版本保留。默认值与待验证项保持清楚标识；后续修改继续维护同一基线，避免多个相互冲突的执行方案并行流转。

---


## 七、设计依据与官方技术资料

### 7.1 项目依据

- [Dataset Doctor L1 系统架构定稿 v1.0](https://chatgpt.com/library?file_id=libfile_fe732826028481919dab17195345d30c&account_id=personal)。
- [软件项目设计与开发规范 v1.1](https://chatgpt.com/library?file_id=libfile_f60c6275c940819194dfb9eb04b37608&account_id=personal)。
- 此前对话中用户对每图 JSON、导出汇总 COCO、SQLite B、无内容版本及 L1 其他事项的确认。
- 2026-11-25 用户对方案Ⅰ的明确选择：“那么我选择方案1”。
- 本文历史版本 v0.1 双方案评审稿，作为本次收敛来源。

L1 文件中的旧自定义标注字段与本轮 COCO 方向有差异，本稿已在第 1.1 节明确列出修订；没有声称旧文件已经被自动改写。

### 7.2 官方技术资料

核对日期：2026-11-23。引用支持协议和库的既有机制；本文的模块拆分、默认值和产品规则是本项目的设计建议。

| 编号 | 来源 | 本文使用范围 |
|---|---|---|
| S1 | [COCO 官方数据格式](https://raw.githubusercontent.com/cocodataset/cocodataset.github.io/master/dataset/format-data.htm) | 图像/标注/类别结构、bbox 坐标；单图工作 profile 是本项目约定 |
| S2 | [Cleanlab Object Detection Rank](https://docs.cleanlab.ai/stable/cleanlab/object_detection/rank.html) | 标签质量检查对 labels 与 predictions 的输入要求 |
| S3 | [SQLite Online Backup API](https://www.sqlite.org/backup.html) | 备份数据库需要正确快照机制；媒体备份由本项目协调 |
| S4 | [SQLite Foreign Key Support](https://www.sqlite.org/foreignkeys.html) | 每连接外键启用与约束机制 |
| S5 | [SQLite Transactions](https://www.sqlite.org/lang_transaction.html) | 读写事务边界与单写者行为 |
| S6 | [SQLite WAL](https://www.sqlite.org/wal.html) | WAL 文件、checkpoint、同机文件系统和正确备份约束 |
| S7 | [Tauri Calling Rust from the Frontend](https://v2.tauri.app/develop/calling-rust/) | Tauri Command、参数和返回机制 |
| S8 | [gRPC Core Concepts](https://grpc.io/docs/what-is-grpc/core-concepts/) | Unary、Streaming、调用终态与取消边界 |
| S9 | [gRPC Deadlines](https://grpc.io/docs/guides/deadlines/) | 客户端显式 deadline；与异步任务超时区分 |
| S10 | [gRPC Status Codes](https://grpc.io/docs/guides/status-codes/) | 本文业务错误向传输层状态的映射基础 |
| S11 | [Protocol Buffers proto3 Guide](https://protobuf.dev/programming-guides/proto3/) | oneof、字段兼容、字段编号和存在性语义 |
| S12 | [JSON Schema 2020-12](https://json-schema.org/draft/2020-12) | 文件及工具参数的结构校验；业务条件仍需 Core 验证 |
| U1 | [Tauri Calling the Frontend from Rust](https://v2.tauri.app/develop/calling-frontend/) | 事件载荷与用途，轻量通知和媒体传输分开 |
| U2 | [Tauri Capabilities](https://v2.tauri.app/security/capabilities/) | 窗口能力和前端调用边界 |
| U3 | [Tauri WebView Versions](https://v2.tauri.app/reference/webview-versions/) | 平台 WebView 差异，需要分别验证媒体行为 |
| U4 | [Tauri Embedding External Binaries](https://v2.tauri.app/develop/sidecar/) | 外部进程交付机制；不等同于 Python 依赖已经全部解决 |
| V1 | [FFmpeg fps Filter](https://ffmpeg.org/ffmpeg-filters.html#fps-1) | 恒定帧率输出可能丢弃/重复帧，与源帧身份区分 |
| V2 | [ffprobe Documentation](https://ffmpeg.org/ffprobe.html) | 流与帧信息读取能力；本项目另定义 FrameRef 和采样规则 |

**Checkpoint：** 2026-11-25 已确认选择方案Ⅰ，并形成单一 L2 设计基线。具体契约与参数作为配套设计默认维护；下一阶段为契约固化与 L3 实现约束设计。技术验证和应用编码尚未开始。
