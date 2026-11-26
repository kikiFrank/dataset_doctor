# Dataset Doctor MVP 开发进度

语言、进程边界与验收门槛以 L3 为准。本页记录可继续开发的证据，阶段按实际验收判定。[L3 实施顺序](implementation_spec_and_tasks.md#五工程实施顺序验证门槛与-ai-coding-任务包)

## 当前工作区

- Windows x64，`H:\DEV_self\dataset_doctor`，分支 `dot/mvp1-foundation`。本轮接手基线 `6d40194`，当时工作树干净。
- P00/P01 已有部分 Windows 验收。P06 桌面业务闭环和 P07–P10 产品验收均待实现及验证。

## 已有验收

| 范围 | 证据与复跑入口 |
| --- | --- |
| P00 工具发现 | `12eab1a`。Windows PowerShell 5.1 运行 `scripts/check-toolchain.ps1`，核实工具发现与版本。 |
| P00 Windows x64 精确工具链 | 本页所在提交新增 `scripts/check-toolchain.ps1 -Locked`。`tests/toolchain/windows/verify-locked-toolchain.ps1` 用真实工具验证精确匹配，并确认 uv 0.11.2 在普通模式可识别、在锁定模式返回 1。实现前测试返回 1，原因是预检缺少锁定模式。 |
| P00 Windows Worker smoke | `06960a4`、`5be1f00`。锁定离线构建 `scripts/packaging/windows/build-worker-smoke.ps1`，随后 `tests/packaging/windows/worker-smoke/verify-worker-smoke.ps1`。打包程序完成握手、正常 PNG 解码与截断 PNG 拒绝。 |
| P00 SQLite | `2c8f962`。在 x64 VS Developer PowerShell 运行 `scripts/storage/windows/check-sqlite-file-lock.ps1`。Windows NTFS/WAL 双进程文件锁和 bundled 静态 SQLite 3.54.1 实测通过。 |
| P01 JSON 与 Protobuf | `163c72e`、`2f04e29`、`6d40194`。本机 `scripts/contracts/windows/check-json-schema.ps1` 运行 InputManifest 与 Workspace 配置共 8 项测试。使用 protoc 29.3 运行 `scripts/contracts/windows/check-protobuf.ps1`，验证 ArtifactRef 描述符与线格式。 |
| P01 Workspace SQLite | `870b5fb`、`e2238fe`。Windows x64 MSVC 运行 `scripts/storage/windows/check-workspace-sqlite.ps1 -RustBin <bin>`，连接 6 项与初始迁移 5 项通过。 |

上述已有验证的参数示例见 [README](../../README.md)。项目设计记录的 Rig 三平台历史结果见 [Rig SDK 报告](../testing/rig_sdk_validation.md)。本轮 Windows 复跑需从当前锁文件准备本机缓存。

## 未验证与接续点

- P00 还需真实 WebView2 启动、生产依赖锁及实际生产 Worker 依赖和打包链验证。Windows 以外的产品还没有本轮实测。
- P01 还需全局库迁移、单图 COCO 契约、生成后的 Rust/Python/TypeScript 互通与漂移检查。此前版本从未发布，因此旧发布库升级验收等待实际发布基线。
- 下一条可执行切片是 `app.db` v1 单独迁移。运行新测试 `cargo test -p dd-infra --test app_migration --locked --offline`，观察全局设置与 Workspace 注册两表、独立版本、重开保留及失败回滚。随后接单图 COCO 与真实 gRPC 契约。
