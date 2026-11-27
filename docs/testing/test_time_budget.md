# Windows 测试场景的 30 秒预算

用户于 2026-11-27 要求单个测试和冒烟测试不超过 30 秒。按单个可执行场景计时，构建、依赖下载和缓存准备单独记录；套件可以包含多个独立场景，不把编译耗时误记为测试运行时间。跨平台验收仍留到功能完成后。

## 统一入口

`scripts/ci/run-windows-check.ps1` 使用 `scripts/testing/bounded_test.py`。Windows Job Object 在启动握手前接管进程；执行接近预算时终止整个进程树，为停止和回收留出时间。超时返回 124，原本失败仍失败，不自动重试或跳过断言。正常退出也回收残留子进程。

- Rust 使用忽略目录下生成的 Cargo runner 数组配置，不修改仓库或既有 Cargo-home 配置。编译后列举 libtest 场景，按原过滤条件逐项 `--exact` 执行，每项独立限时并核对确实运行一次。普通 `cargo run` 契约生成工具不会被当成 libtest。
- Worker Python 场景目录批次和 Node 测试批次均设进程预算 30 秒，形成更严格的整批上限；当前批次很短。Node 另设单用例 `--test-timeout=30000`。
- WebView2 的每个 Suite 独立受 30 秒进程树预算约束，包含夹具准备、实际启动、IPC 和独立结果核验。原脚本等待应用的上限统一为 25 秒，为结果核验留时间。
- P00 独立入口和断言保持不变，在同一进程树预算内实际执行，仍验证固定锁、静态 SQLite、NTFS/WAL 和两进程锁。

输出 `TEST_DEADLINE` 和 `target/ci/<组>/test-durations.jsonl`，记录场景、真实耗时、原退出码、是否超时及残留进程数。CI 上传这些合成验收元数据。Windows PowerShell 子进程重新初始化自己的模块路径，避免 Python 桥接后误用 PowerShell 7 的 Utility 模块；不改变调用者环境。

单独复跑一个已构建测试：

```powershell
& $python -I -B scripts/testing/bounded_test.py --kind rust -- $testExecutable --nocapture
```

单独复跑一个冒烟场景（需先获得本机桌面使用授权）：

```powershell
& $python -I -B scripts/testing/bounded_test.py -- powershell.exe -NoProfile -NonInteractive -File tests/desktop/windows/verify-runtime.ps1 -DesktopExe $exe -Suite Runtime
```

`$python` 使用锁定 CPython 3.12.12 x64。低层构建/准备脚本不是独立的限时验收入口；从上述入口或产品 CI 复跑场景。历史完整业务、颜色、视频等冒烟需要在桌面可用后按此预算复验，旧通过记录不自动等同于新预算通过。

## 限时器自身验收

`$python -I -B -m unittest discover -s tests/testing/windows -v`：真实成功和失败、超时停止整棵树、正常退出后清理孤儿、拒绝无效或超过 30 秒的预算、普通 Cargo 工具边界、Windows PowerShell 实际 Get-FileHash。没有 Mock 执行器或时钟。测试使用更短预算注入超时，不实际等待 30 秒。

耗时超过预算时保留失败证据，拆分为具有独立输入和断言的场景，固定夹具与生成物分开；不放宽预算，不删除断言，不以更少检查冒充原场景通过。真实原生鼠标验收也按功能拆分，数据和窗口准备单独安排。

## 已执行证据

7a9c30d 的 Windows CI 七组通过，106 条限时记录最长 13.641 秒，Runtime/WorkspaceCreate 冒烟分别 8.953/2.078 秒。所有执行记录退出成功、无超时、无残留进程；准备和构建另计。限时器本机 7 项真实测试通过（约 3.1 秒），实际 RPC、TaskStore、P00 和契约直接回归通过。完整数字与原始日志位置以进度检查点为准。尚未按新预算实跑的复杂桌面/原生交互以及其他平台继续保留未验证状态。
