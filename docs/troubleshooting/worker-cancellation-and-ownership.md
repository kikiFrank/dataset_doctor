# DD-ISSUE-0002 取消、孤儿进程与工作区所有权

状态 Resolved，2026-11-27。验证环境为 Windows x64 MSVC、锁定 Python 3.12.12 和本机已核验 FFmpeg，使用独立合成数据。新交互与其他平台仍待验收。

红测发现三个生命周期缺口。Core 受理取消后 Worker 仍返回成功，直接终止已经退出的 Worker 会留下其子进程，运行中的工作区也可以提前释放锁。红测分别记录在 `target/dev/logs/mvp11-task-cancel-running-red.log`、`mvp11-task-cancel-process-tree-red.log`、`mvp11-task-cancel-close-red.log`。进一步模拟监督器未完成持久化时重开，遗留任务仍为 RUNNING，证据 `mvp11-task-cancel-reopen-red.log`。

修复复用当前 Core/Worker 通道和 SQLite 事务。取消请求与派发、终态接纳串行化，Worker 协作停止后持久报告 cancelled，并使用相同事件重试丢失的确认。Windows 在 bootstrap 前将执行器绑定 kill-on-close 作业对象，正常/强制停止时等待活动进程归零。工作区生命周期租约覆盖实际执行器的启动、计算和停止，关闭不能越过该租约。重开只协调遗留的执行器 attempt，排队任务、已有终态和正式文件协调日志按原规则保留。作业对象行为依据 [Microsoft Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)。

FFmpeg 的 stdout 读取原先只等待超时，协作标记无法中断它。监视线程现在观察取消和超时，杀死并 wait 回收该次真实进程。解码红绿为 `mvp11-video-cancel-{red,final}.log`。纯像素追踪还需在帧迭代和搜索行检查取消，专项红测 `mvp11-tracking-cancel-red.log`，绿测见同一 cancellation 测试目录。

直接视频回归曾发现新增 COMPLETING 状态破坏旧客户端的终态轮询。已经改为内部选择标记，公开状态继续保持 RUNNING/CANCELLING，直到 Core 接纳终态。诊断日志 `mvp11-task-cancel-boundaries.log`，修复后的对应回归 `mvp11-task-cancel-verified.log`。另一次进程夹具在 PID 文件创建但尚未写完时读取到空内容，夹具改为同目录临时文件原子发布，进程句柄始终在断言失败时回收该次合成后代。

可复跑入口见 [README 取消计算任务](../../README.md#取消计算任务)。真实集成 `task_cancellation` 与 `task_cancellation_video` 覆盖重复取消、已完成任务保护、成功/失败晚到竞争、确认丢失、Worker 崩溃、子进程停止、重开、单次 attempt、源字节、完整性和外键。最终恢复回归 `mvp11-task-cancel-recovery-regression.log`，直接关联命令宿主 `mvp11-task-cancel-{host-final,workflow-host}.log`。不以这些 CLI 结果代替原生桌面和安装后验收。
