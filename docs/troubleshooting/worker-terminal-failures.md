# Worker 计算异常与终态回报

| 字段 | 记录 |
| --- | --- |
| ID | DD-ISSUE-0001 |
| 环境 | Windows x64 MSVC，锁定 Worker Python 3.12.12，MVP1.1 审查基线 `205eea4`。 |
| 现象 | 算法抛出 RuntimeError 时，执行线程结束而 Core 尚无终态，任务可能持续显示 RUNNING。 |
| 根因 | `WorkerExecutions._run` 在 Started 获得接纳后直接调用 compute，统一异常边界缺失。可复现红测见 `target/dev/logs/mvp11-worker-failures-red.log`。 |
| 修复 | 输入与文件异常保留分类，未预期异常回报 INTERNAL_ERROR。安全日志仅记录错误码和三个执行标识。异常仍经同一 ReportExecution 协议进入 Core。 |
| 回报恢复 | 重试保留事件 ID、序号和内容。回包丢失后 Core 幂等接纳原终态。重试耗尽时宿主确认执行进程已退出，再通过现有中断入口持久化 INTERRUPTED。Core 已确认的终态保持其状态。 |
| 预防 | `tests/worker/execution/test_execution_failures.py` 覆盖真实 loopback RPC 边界。`tests/integration/rpc/worker_failures.rs` 使用合成库和真实 Worker/Core，核验失败、回包丢失、持续不可用、进程退出及重开后的同一任务。两个入口已纳入 Windows Worker RPC 验收脚本。 |
| 验证 | 离线运行 `scripts/contracts/windows/check-worker-rpc.ps1`，参数见项目 README。故障集成另可运行 `cargo test --locked --offline --target x86_64-pc-windows-msvc -p dd-infra --features execution-rpc --test worker_failures`，沿用该脚本设置的 Worker 与合成运行目录环境。 |
| 边界 | 新枚举保留旧编号。正式发行仍需重新打包 Worker 与执行安装验收，原生桌面操作按当前用户要求延后。 |
