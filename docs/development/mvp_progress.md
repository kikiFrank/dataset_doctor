# Dataset Doctor MVP 开发进度

语言、进程边界与验收门槛以 L3 为准。本页记录可继续开发的证据，阶段按实际验收判定。[L3 实施顺序](implementation_spec_and_tasks.md#五工程实施顺序验证门槛与-ai-coding-任务包)

## 当前工作区

- Windows x64，`H:\DEV_self\dataset_doctor`，分支 `dot/mvp1-foundation`。本轮接手基线 `6d40194`，当时工作树干净。
- P00/P01 已有部分 Windows 验收。P06 桌面业务闭环和 P07–P10 产品验收均待实现及验证。

## 已有验收

| 范围 | 证据与复跑入口 |
| --- | --- |
| P00 工具发现 | `12eab1a`。Windows PowerShell 5.1 运行 `scripts/check-toolchain.ps1`，核实工具发现与版本。 |
| P00 Windows x64 精确工具链 | `2e710d2`。`tests/toolchain/windows/verify-locked-toolchain.ps1` 验证 `scripts/check-toolchain.ps1 -Locked` 精确匹配，并确认 uv 0.11.2 在普通模式可识别、在锁定模式返回 1。实现前测试返回 1，原因是预检缺少锁定模式。 |
| P00 Windows Worker smoke | `06960a4`、`5be1f00`。锁定离线构建 `scripts/packaging/windows/build-worker-smoke.ps1`，随后 `tests/packaging/windows/worker-smoke/verify-worker-smoke.ps1`。打包程序完成握手、正常 PNG 解码与截断 PNG 拒绝。 |
| P00 SQLite | `2c8f962`。在 x64 VS Developer PowerShell 运行 `scripts/storage/windows/check-sqlite-file-lock.ps1`。Windows NTFS/WAL 双进程文件锁和 bundled 静态 SQLite 3.54.1 实测通过。 |
| P01 JSON 与 Protobuf | `163c72e`、`2f04e29`、`6d40194`。本机 `scripts/contracts/windows/check-json-schema.ps1` 运行 InputManifest 与 Workspace 配置共 8 项测试。使用 protoc 29.3 运行 `scripts/contracts/windows/check-protobuf.ps1`，验证 ArtifactRef 描述符与线格式。 |
| P01 Workspace SQLite | `870b5fb`、`e2238fe`。Windows x64 MSVC 运行 `scripts/storage/windows/check-workspace-sqlite.ps1 -RustBin <bin>`，连接 6 项与初始迁移 5 项通过。 |
| P01 全局 SQLite | `c90afcb`。增加 `-Suite AppMigration`，3 项通过。验证独立版本、设置重开保留、错库和高版本拒绝、校验失败回滚及重试。累计 SQLite 14 项通过。红测和绿测日志分别在 `target/p01-04/logs/fd41180807764a1daedb7d15e2aeb7b3.log`、`target/p01-04/logs/f4498d8472dc4a7ab9a49f2c92857e7e.log`。 |
| P01 单图 COCO bbox | `d5a00c7`。`scripts/contracts/windows/check-json-schema.ps1 -Suite CocoSingleImage` 使用共享的 20 个结构和解析向量。红测因 Schema 缺失失败，绿测 2 项通过。累计 Python Schema 10 项通过。日志 `dist/p01-01/logs/coco-single-image-red.log`、`dist/p01-01/logs/check.log`。 |
| P01 Rust JSON 校验 | `803aa94`。在 Windows x64 MSVC 执行 `cargo test --locked --offline --target x86_64-pc-windows-msvc -p dd-infra --test json_contracts`。3 项通过，覆盖 Python 同用的 75 个向量及整数和 UTF-8 边界。红测日志 `target/dev/logs/json-rust-red.log`，基础设施累计回归日志 `target/p01-04/logs/2481219801994f458cfc1c1bba8b84f9.log`。 |
| P01 Rust/Python 生成与实际 Worker RPC | `630dddc`。`scripts/contracts/windows/check-worker-rpc.ps1` 通过 Python 生成物漂移检查和 2 项集成测试。Tonic/Prost 客户端连到锁定 Python Worker 的动态 loopback 端口，确认未认证拒绝、健康和关停。共享 ArtifactRef 线格式经生成后的 Rust 和 Python 双向转换，保留 `uint64`、归属和可选字段。红测 `target/dev/logs/worker-rpc-red.log`，累计绿测 `target/contract-check/worker-rpc/80368ef992cd4463a0fd01a79e1381a3/check.log`。 |
| P01 Rust/TypeScript 表示基础 | `8cc0e5a`。`scripts/contracts/windows/check-ui-wire.ps1` 通过生成物漂移检查，Rust 2 项和 Node TypeScript 22 项通过。覆盖同一组十进制整数和归属联合。红测 `target/dev/logs/ui-wire-{rust,ts}-red.log`，绿测 `target/contract-check/ui-wire/d3c56f63aaab4644844ee94d4e5057e5/check.log`。 |
| P00 Windows Tauri/WebView2 | `9ca4c5a`、`3c4c3f9`。`scripts/packaging/windows/build-desktop.ps1` 锁定离线运行 TypeScript 编译、Vite 和 Windows MSVC 构建。用 `tests/desktop/windows/verify-runtime.ps1` 实际启动 EXE，WebView2 内的 React 完成 Tauri IPC，应用正常退出。构建日志 `target/dev/logs/desktop-build.log`，红/绿启动证据在 `target/desktop-smoke/runs/4b3cde3ff9cb48a19c1cf31e9af7bbac/verify.log` 与 `target/desktop-smoke/runs/9e6da58f3b18477bbfb62666f284dffa/verify.log`。 |
| P04 Worker 图像读取适配器 | `d24505b`。Windows 锁定的 Worker Python 执行 `python -I -B -m unittest discover -s tests/worker/image -v`，4 项通过。真实解码合成 PNG/JPEG，校验方向、源摘要、坏图、格式和像素预算。红绿日志 `target/dev/logs/image-adapter-{red,green}.log`。Worker RPC 回归 2 项通过。 |
| P02 Workspace 文件锁 | `881e39c`。`scripts/storage/windows/check-workspace-sqlite.ps1 -Suite WorkspaceLock` 在本机 NTFS 中文、空格路径中用真实子进程确认排他写锁、拒绝删除正在使用的锁文件、关闭句柄后可重获。红测 `target/dev/logs/workspace-ownership-red.log`，绿测和累计回归在 `target/p01-04/logs/61f05bc86ca149d3b469f45f35d5e1ef.log`、`target/p01-04/logs/054eb3ff516b4b9fa78aebeca3004518.log`。 |
| P02 打开已有 Workspace 的基础设施 | `2b4df12`。`scripts/storage/windows/check-workspace-sqlite.ps1 -Suite WorkspaceOpen`，3 项通过。打开标志及库身份一致的合成工作区，在连接生命周期持有所有权，拒绝坏标志、错身份、未来版本、缺库和空库。空库仍为空，缺库时不创建库。累计回归 `target/p01-04/logs/c0c2e521a08a4319b6bddd352efa0ea5.log`，红测 `target/dev/logs/workspace-open-red.log`。 |
| P04 Worker 标注几何适配器 | `0ab351a`。Windows 锁定的 Worker Python 执行 `python -I -B -m unittest discover -s tests/worker/annotation -v`，4 项通过。对照解码尺寸验证 bbox 和局部引用，区分缺标注和空标注，保留原数据。红绿日志 `target/dev/logs/geometry-{red,green}.log`。图像适配器 4 项复跑通过。 |
| P02 Core Workspace 授权与会话 | `f35192c`。`scripts/storage/windows/check-workspace-sqlite.ps1 -Suite WorkspaceAccess`，2 项通过。通过 Core 的真实存储端口打开合成库，验证授权失效无文件操作、同一时刻只打开一个工作区、旧授权不可复用、重开更换会话且旧会话不能关闭新连接。红测 `target/dev/logs/workspace-access-red.log`，累计回归 `target/p01-04/logs/78b339f0b97d48b19c33b8bbaa7bb611.log`。 |
| P02 Workspace 数据线程 | `3ee1ea8`。`scripts/storage/windows/check-workspace-sqlite.ps1 -Suite WorkspaceThread`，真实并发的两个打开请求仅一个获得连接。线程退出会关闭 SQLite 并释放锁。累计回归 `target/p01-04/logs/3f2d31da65554c3aa279aeee99a5e2fe.log`。 |
| P02/P05 Windows 桌面打开已有 Workspace | `acf0fe1`。`tests/desktop/windows/verify-runtime.ps1 -DesktopExe $exe -Suite Workspace -PythonExe $py312` 从 WebView2 经 Tauri、Core 和真实 SQLite 验证打开、查询、关闭、重开和拒绝旧会话。红测 `target/desktop-smoke/runs/85c2573a33a84c868fbfbc6faa98098a/verify.log`，绿测 `target/desktop-smoke/runs/91f29d9c19fb4c22a9bd377b1e02f102/verify.log`。也在普通应用窗口用原生文件夹选择器打开合成库，确认界面显示名称和 1 个离线资源，关闭后回到工作区入口。进程返回 0，WebView2 退出时记录 `Chrome_WidgetWin_0` 类注销警告。存储累计回归 `target/p01-04/logs/f31dd1378e3c4e98b8c09b122ce5bddb.log`，TypeScript 与 Worker RPC 回归日志见 `target/contract-check/ui-wire/9faca2bafe884cb0b40c4d127102184b/check.log`、`target/contract-check/worker-rpc/6cb85db260814cc9a3414ac5e383c0be/check.log`。 |
| P02 Workspace 创建存储 | `f806670`。`scripts/storage/windows/check-workspace-sqlite.ps1 -Suite WorkspaceCreate` 新增 5 项。验证迁移及重开、锁持有、完整性与外键检查、拒绝覆盖、待发布标志前后的重试，以及 SQLite 初始化失败时不发布部分数据库。使用同目录临时文件，SQLite checkpoint 后发布 DB，最后发布 `workspace.json`。红测 `target/p01-04/logs/5fd8aa8645f549f2a3623fd592b90040.log`，累计回归 `target/p01-04/logs/df11aa48fac6498d9377b50729ebe4cf.log`。中断状态用固定合成夹具复现，尚未实测断电恢复。 |
| P02 Core 创建与数据线程 | 本页所在提交。`scripts/storage/windows/check-workspace-sqlite.ps1 -Suite WorkspaceThread` 共 3 项通过，包括并发创建仅一个成功、授权只能消费一次、过期授权不产生任何文件、重开时保留库身份并换发会话。红测 `target/p01-04/logs/690651a5e90a452a8560331a15911935.log`，绿测及累计回归 `target/p01-04/logs/0655391088564f9abfbeb7688a624d83.log`、`target/p01-04/logs/ecde57e616114d61869c86a91b8f1e4f.log`。 |
| 独立 Rig SDK | 使用独立锁文件在 `target/dev/cargo-home` 执行 `cargo fetch --manifest-path tests/rig-poc/Cargo.toml --locked --target x86_64-pc-windows-msvc`，之后相同 target 运行 `cargo test --manifest-path tests/rig-poc/Cargo.toml --locked --offline --all-targets`。Windows 本轮 10 项通过，日志 `target/dev/logs/rig-test.log`。 |

上述验证的参数示例见 [README](../../README.md)。Rig 的其他平台历史结果见 [Rig SDK 报告](../testing/rig_sdk_validation.md)。

## 未验证与接续点

- Windows 桌面壳已本机启动。最终生产依赖、随包 Worker 和干净系统安装验收仍待 P04/P10。Windows 以外的产品还没有本轮实测。
- P01 还需首切片命令 DTO、专用 StartX 和 Core 执行契约、三语言完整 G-CONTRACT 及统一开发入口。旧发布库升级验收等待实际发布基线。
- 下一步通过 `workspace_create` 将已经验证的 Core 创建入口接到 Tauri 和桌面表单，准备 Windows 原生冒烟场景。Core 任务准入与 Worker 专用 StartX 还未串联。
- 桌面操作已结束。暂用命令行继续开发，新增 GUI 实测等用户释放桌面后补做。
