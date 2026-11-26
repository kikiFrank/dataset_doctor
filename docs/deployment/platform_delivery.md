# 跨平台交付

当前仅完成独立 Rig SDK 在三平台的编译和测试，不代表安装包已生成。目标平台 Windows、Linux、macOS。桌面端使用 Tauri 2，Rust Core 管理本地 Agent Service 和 Python Worker 生命周期，gRPC 仅绑定 loopback。打包、运行依赖、安装和卸载验证见 [L2 验收](../testing/acceptance_and_platform_validation.md) 与 [L3 实施任务](../development/implementation_spec_and_tasks.md)。真实部署、模型配置和升级回滚命令须在实现后按实际情况维护。
