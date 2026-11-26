# Dataset Doctor

Local-first desktop application for computer-vision dataset quality governance. Rust Core + Rust Agent Service (Rig) + Python algorithm Worker; Tauri 2 / React / TypeScript frontend. Windows, Linux and macOS are target platforms.

当前处于工程设计与技术验证阶段，L1/L2 已形成方案Ⅰ基线，L3 实施任务与页面原型可供开发。生产应用尚未实现；Rig SDK 独立 PoC 已通过三平台测试。

- [项目文档入口](docs/README.md)
- [前端 HTML 原型](docs/modules/frontend/prototype/index.html)
- [实施任务与验收](docs/development/implementation_spec_and_tasks.md)
- [Rig SDK 测试报告](docs/testing/rig_sdk_validation.md)

## 已有可执行验证

```sh
cd tests/rig-poc
cargo +1.97.1 test --locked --all-targets -- --nocapture
```

无需 API Key；使用合成模型响应。产品构建、运行与安装说明在正式实现后按实际情况维护。
