# ADR-001：Rust Agent SDK 选型

- 日期：2026-11-26
- 决策：采用 Rig v0.47.1，Rust 1.97.1；独立 SDK PoC 已验证。
- 理由：Rust 原生接口、结构化输出、工具调用及可恢复 checkpoint；三平台编译与确定性测试通过。
- 边界：Rig 在 Agent Service 内部作为 SDK，不取代 Core 的持久化、任务准入、人工确认、权限、资源控制或确定性工作流，不直接打开 SQLite 或提交 Final。通过项目自有适配器隔离框架类型；Python Worker 继续执行视觉算法。
- 不启用非必需向量库、额外服务或任意工具；具体模型调用、重试和数据外发须遵循既定项目契约。
- 证据：[PoC 报告](../testing/rig_sdk_validation.md)。真实云模型、多模态效果、业务状态恢复及桌面发行仍待验证。
