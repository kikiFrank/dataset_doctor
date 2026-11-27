# Dataset Doctor 文档入口

当前设计基线：L1 v1.0、L2 方案Ⅰ v1.0、L3 实现规范 v0.1；已确认前端风格；Rig SDK 独立 PoC 已通过。Windows 桌面应用已实现部分业务闭环，完整 MVP 与跨平台发行待验收。当前状态见开发进度与验收证据。

| 分类 | 权威入口 |
|---|---|
| requirements | [项目背景与愿景](requirements/dataset_doctor_project_context_and_vision_v0.1.md)、[延后需求](requirements/dataset_doctor_deferred_feature_backlog_v0.1.md) |
| architecture | [L1](architecture/dataset_doctor_L1_architecture_final_v1.0.md)、[L2 基线与分册索引](architecture/dataset_doctor_L2_scheme1_baseline_v1.0.md) |
| modules | [模块职责](modules/module_contracts.md)、[领域执行](modules/domain_execution.md)、[前端契约](modules/frontend_contracts.md)、[页面与交互](modules/frontend/pages_and_interactions.md)、[HTML 原型](modules/frontend/prototype/index.html) |
| interfaces | [API、RPC、Workflow 和 Tool](interfaces/api_rpc_workflow_contracts.md) |
| data | [数据契约与 SQLite](data/data_contracts_and_sqlite.md) |
| configuration | [配置契约](configuration/configuration_contracts.md) |
| development | [开发工作守则](development/coding_guidelines.md)、[L3 实现规范与任务](development/implementation_spec_and_tasks.md)、[MVP 开发进度](development/mvp_progress.md)、[产品 CI 与专项复跑](development/product_ci.md) |
| testing | [验收与平台验证](testing/acceptance_and_platform_validation.md)、[Rig PoC 报告](testing/rig_sdk_validation.md) |
| deployment | [跨平台交付](deployment/platform_delivery.md) |
| troubleshooting | [长期问题记录](troubleshooting/README.md) |
| decisions | [ADR-001 Rig](decisions/adr-001-rig-agent-sdk.md) |

## 阅读与维护

先读 [根 README](../README.md)、本索引及 [开发工作守则](development/coding_guidelines.md)，按任务选择相关模块、接口、数据或配置；跨模块变更再读 L1/L2。L2 原章节已按主题拆分，编号保留。实际 schema、迁移、配置及 tests 形成后以代码和机器可读契约为权威；同步维护对应设计与测试。Confirmed/Design Default/Proposed/Validated/Implemented 不得混用，未验收事项明确保留。项目文档不收录两份调研报告全文或 Python Agent 框架比较。
 

AI Coding 的轻量阅读入口为 [AGENTS.md](../AGENTS.md)；工作守则只在 development 中维护。长期经验归 troubleshooting，自动回归归 tests，诊断工具按需归 scripts；当前不建立重复的项目级 Skills。
