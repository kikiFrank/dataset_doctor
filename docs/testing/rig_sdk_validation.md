# Rig SDK 验证报告

日期：2026-11-26；状态：Validated（独立 SDK PoC）。

版本：Rig 0.47.1 / Rust 1.97.1；测试代码 [`tests/rig-poc`](../../tests/rig-poc)，固定 Cargo.lock。使用真实 Rig SDK 与确定性合成模型响应，无 API Key、不发送业务数据。

| 平台 | 通过 | 失败 | 忽略 |
|---|---:|---:|---:|
| Ubuntu x86_64 | 10 | 0 | 0 |
| Windows x86_64 | 10 | 0 | 0 |
| macOS arm64 | 10 | 0 | 0 |

覆盖：结构化计划与 schema；无效计划与调用预算；注册工具结果回传；未知工具拒绝；Hook 停止工具执行；循环预算；模型错误有限重试；待执行工具 checkpoint 序列化与恢复；不兼容 checkpoint 拒绝；图像与标注上下文传递。

复现：`cd tests/rig-poc`，`cargo +1.97.1 test --locked --all-targets -- --nocapture`。

- [首轮 CI](https://github.com/kikiFrank/dataset_doctor/actions/runs/45740148475)
- [锁定依赖复验 CI](https://github.com/kikiFrank/dataset_doctor/actions/runs/45740694862)
- 测试提交：`18698abe4ee552f6eb9e0ab509277b1cee69a14a`。

限制：没有验证真实云模型输出质量、真实 gRPC 服务和授权、Core 数据持久化、崩溃后业务幂等、Tauri 打包及真实数据集端到端效果。CI 的 checkout/upload-artifact 旧 Node 运行时存在弃用警告，未影响本轮测试通过；后续更新 CI 依赖时复验。
