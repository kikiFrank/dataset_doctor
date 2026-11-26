# Dataset Doctor

Local-first desktop application for computer-vision dataset quality governance. Rust Core + Rust Agent Service (Rig) + Python algorithm Worker; Tauri 2 / React / TypeScript frontend. Windows, Linux and macOS are target platforms.

当前处于工程设计与技术验证阶段，L1/L2 已形成方案Ⅰ基线，L3 实施任务与页面原型可供开发。生产应用尚未实现；Rig SDK 独立 PoC 已通过三平台测试。

- [项目文档入口](docs/README.md)
- [开发工作守则](docs/development/coding_guidelines.md)（AI Coding 自动阅读入口：[AGENTS.md](AGENTS.md)）
- [前端 HTML 原型](docs/modules/frontend/prototype/index.html)
- [实施任务与验收](docs/development/implementation_spec_and_tasks.md)
- [Rig SDK 测试报告](docs/testing/rig_sdk_validation.md)

## Windows 工具链预检（P00-01）

在仓库根目录运行以下命令。从其他目录运行时，将脚本参数换成完整路径。

```powershell
powershell.exe -NoProfile -NonInteractive -File .\scripts\check-toolchain.ps1
$LASTEXITCODE
```

预检读取当前进程 PATH，只查询可直接运行的本地工具。退出码 `0` 表示清单已核实且满足 Node 24 和 CPython 3.12 系列目标，`1` 表示有缺失、版本偏离或未核实项。完整 P00 还需验证工具链锁定与 Worker 打包。

预检跳过包含目录联接或其他重解析点的 PATH 项，并将发现的工具管理器代理标为未核实。只有能放入专用 Windows 作业对象的进程才会执行版本查询，超时时会清理该次查询的进程树。作业对象负责进程生命周期，未提供网络隔离。

## Windows Worker 打包验证（P00-02）

在仓库根目录运行。使用 Windows x64 的实际 CPython 3.12.12 可执行文件、仓库里的 `uv.lock` 和已预填的项目专用 uv 缓存。

```powershell
$py312 = '<CPython 3.12.12 x64 的 python.exe 绝对路径>'
$offlineCache = (Resolve-Path -LiteralPath '.\dist\p00-worker-smoke\uv-cache').Path
powershell.exe -NoProfile -NonInteractive -File .\scripts\packaging\windows\build-worker-smoke.ps1 -PythonExe $py312 -UvCacheDir $offlineCache
if ($LASTEXITCODE -ne 0) { throw 'Worker build failed.' }
powershell.exe -NoProfile -NonInteractive -File .\tests\packaging\windows\worker-smoke\verify-worker-smoke.ps1
$LASTEXITCODE
```

构建按锁文件从缓存离线安装 Windows x64 wheel，输出为 `dist/p00-worker-smoke/package/worker-smoke/worker-smoke.exe`。构建和验收日志分别位于 `dist/p00-worker-smoke/logs/build.log` 和 `dist/p00-worker-smoke/logs/verify.json`。

已在 Windows 11 x64 和 Windows PowerShell 5.1 实测 CPython 3.12.12、PyInstaller 6.18.0 与 Pillow 12.1.0。打包后的 EXE 在精简 PATH 下完成管道握手，完整解出合成 PNG 的红、蓝像素后受控退出 0。截断 PNG 返回 `decode_failed`，进程退出 2。独立干净系统和正式 Worker 接口留待后续验收。

依赖许可见 [Windows Worker PoC 依赖许可记录](docs/deployment/platform_delivery.md#p00-02-windows-worker-poc)。

## Windows bundled SQLite 文件锁验证（P00-03）

在已初始化 x64 VS Developer PowerShell 的仓库根目录运行。使用 Rust/Cargo 1.97.1 的 MSVC 工具链和预填到 `target/p00-03/cargo-home/` 的批准离线缓存。

```powershell
$rustBin = '<Rust 1.97.1 x86_64-pc-windows-msvc 工具链的 bin 绝对路径>'
$cache = (Resolve-Path -LiteralPath '.\target\p00-03\cargo-home').Path
powershell.exe -NoProfile -NonInteractive -File .\scripts\storage\windows\check-sqlite-file-lock.ps1 -CargoExe (Join-Path $rustBin 'cargo.exe') -RustcExe (Join-Path $rustBin 'rustc.exe') -OfflineCargoHome $cache
$LASTEXITCODE
```

入口以 `--locked --offline` 执行 `cargo fetch`、`cargo tree -e features` 和 `cargo test -vv -- --nocapture`。运行日志位于 `target/p00-03/logs/check.log`，每次合成数据库保留在 `target/p00-03/runs/` 下的新目录。退出码 `0` 为通过，`1` 为构建、来源核对或运行验收失败，`2` 为工具或离线输入不足。

Windows x64 固定 NTFS 实测确认逐连接启用外键、WAL、FULL 同步和 250 ms busy timeout。两个独立子进程以管道协调。持锁期间另一进程看不到未提交数据，写入返回有界的 `SQLITE_BUSY`。持锁进程提交并退出后，另一进程写入成功，重开数据库后核对三条数据。Cargo 构建输出记录静态 `sqlite3` 链接，MSVC 导入表检查排除了 SQLite DLL。

[专用 Cargo.lock](tests/storage/sqlite/file-lock/Cargo.lock) 固定 `rusqlite 0.42.0` 的 `bundled`、`backup` 和 `libsqlite3-sys 0.40.0`。包内 amalgamation 和实际运行值均为 [SQLite 3.54.1](https://sqlite.org/releaselog/3_54_1.html)，Source ID 为 `2026-10-16 14:45:20 555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76`。交接核对的 WAL 补丁依据见 [RUSTSEC-2026-0459](https://rustsec.org/advisories/RUSTSEC-2026-0459.html)。11 个本地 crate 归档的校验和与锁文件一致，许可按归档的 manifest 和许可证文件记录。

| 锁定依赖 | 测试用途 | 归档许可 |
| --- | --- | --- |
| `rusqlite 0.42.0` | SQLite Rust 连接与错误接口 | MIT |
| `libsqlite3-sys 0.40.0` | SQLite 绑定、bundled 源与本地编译 | MIT，SQLite amalgamation 为[公共领域](https://sqlite.org/copyright.html) |
| `bitflags 2.13.2`、`fallible-iterator 0.3.0`、`fallible-streaming-iterator 0.1.9`、`smallvec 1.16.6` | Rust 运行依赖 | MIT 或 Apache-2.0 |
| `cc 1.11.3`、`find-msvc-tools 0.1.15`、`pkg-config 0.3.34`、`shlex 2.0.1`、`vcpkg 0.2.15` | 构建依赖 | MIT 或 Apache-2.0 |

本切片的通过范围为 Windows x64 上的静态 SQLite 来源、连接设置和 NTFS 文件锁。SQL 迁移、备份行为、应用层 Workspace 所有权锁、崩溃恢复及跨平台发行留待后续验收。

## Windows InputManifest 结构检查（P01-01）

在仓库根目录运行。使用 Windows x64 上实际安装的 CPython 3.12.12、仓库里的验证依赖锁和已预填的 P01-01 专用 uv 缓存。

```powershell
$py312 = '<CPython 3.12.12 x64 的 python.exe 绝对路径>'
$offlineCache = (Resolve-Path -LiteralPath '.\dist\p01-01\uv-cache').Path
powershell.exe -NoProfile -NonInteractive -File .\scripts\contracts\windows\check-json-schema.ps1 -PythonExe $py312 -UvCacheDir $offlineCache
$LASTEXITCODE
```

入口锁定离线同步验证环境，执行严格 UTF-8 JSON 解析、Draft 2020-12 结构和 UUID 格式检查，并用封闭本地注册表解析引用。输出记录在 `dist/p01-01/logs/check.log`。通过表示结构和合成向量符合契约。操作输入前提、授权与摘要真实性由后续 Core 工作验证。

查看 [InputManifest v1 Schema](contracts/json-schema/v1/input-manifest.schema.json)、[共享向量](testdata/contracts/json-schema/v1/input-manifest/cases.json) 和 [验收依赖及许可](docs/deployment/platform_delivery.md#p01-01-inputmanifest)。

## Windows ArtifactRef 线格式检查（P01-02）

在仓库根目录运行。需要自报 `libprotoc 29.3` 的本机编译器、同一安装的 `google/protobuf/descriptor.proto` 和已验证的 CPython 3.12.12。测试使用 Python 标准库。

```powershell
$protoc = '<protoc.exe 的绝对路径>'
$include = '<包含 google/protobuf/descriptor.proto 的 include 目录绝对路径>'
$py312 = '<CPython 3.12.12 的 python.exe 绝对路径>'
powershell.exe -NoProfile -NonInteractive -File .\scripts\contracts\windows\check-protobuf.ps1 -ProtocExe $protoc -GoogleProtoRoot $include -PythonExe $py312
$LASTEXITCODE
```

入口离线编译并比对重复生成的描述符，用 [共享合成向量](testdata/contracts/proto/v1/artifact-ref/cases.json) 验证 [ArtifactRef](contracts/proto/datasetdoctor/v1/common.proto) 的字段编号、三种 owner、`uint64` 和 `schema_id` 存在性。输出和日志位于 `target/contract-check/p01-02/`。退出码 `0` 为通过，`1` 为编译或验收失败，`2` 为工具或 include 先决条件失败。生产代码生成、跨语言 RPC 和授权校验留待后续验证。

## Windows Workspace SQLite 写连接初始化（P01-03）

在已初始化的 x64 VS Developer PowerShell 中，从仓库根目录运行。使用 Rust/Cargo 1.97.1 的 MSVC 工具链和 P00-03 预填的 `target/p00-03/cargo-home/`。当前进程应使用默认的 Cargo/MSVC 编译选项。以下命令清除 SQLite 链接覆盖变量并使用 bundled 源。

```powershell
$rustBin = '<Rust 1.97.1 x86_64-pc-windows-msvc 工具链的 bin 绝对路径>'
$cargo = Join-Path $rustBin 'cargo.exe'
$env:RUSTC = Join-Path $rustBin 'rustc.exe'
$env:CC = (Get-Command cl.exe -CommandType Application -ErrorAction Stop).Source
$env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = (Get-Command link.exe -CommandType Application -ErrorAction Stop).Source
Get-ChildItem Env: | Where-Object Name -Match 'SQLITE|SQLCIPHER' | ForEach-Object {
    [Environment]::SetEnvironmentVariable($_.Name, $null, 'Process')
}
$env:CARGO_HOME = (Resolve-Path -LiteralPath '.\target\p00-03\cargo-home').Path
$env:CARGO_NET_OFFLINE = 'true'
$env:CARGO_REGISTRIES_CRATES_IO_PROTOCOL = 'sparse'
$out = Join-Path (Get-Location).Path 'target\p01-03'
$run = [Guid]::NewGuid().ToString('N')
$env:CARGO_TARGET_DIR = Join-Path $out 'cargo-target'
$env:DD_SQLITE_INIT_RUN_DIR = Join-Path $out "runs\$run"
$env:TEMP = Join-Path $out 'temp'
$env:TMP = $env:TEMP
$logs = Join-Path $out 'logs'
New-Item -ItemType Directory -Path $env:DD_SQLITE_INIT_RUN_DIR, $env:TEMP, $logs -Force | Out-Null
$cargoArgs = @('--manifest-path', '.\Cargo.toml', '--locked', '--offline', '--target', 'x86_64-pc-windows-msvc', '-p', 'dd-sqlite-connection-init-tests')
& $cargo tree @cargoArgs -e features
if ($LASTEXITCODE -ne 0) { throw 'Offline dependency resolution failed.' }
& $cargo test @cargoArgs --test workspace_connection -- --nocapture 2>&1 | Tee-Object -FilePath (Join-Path $logs "$run.log")
if ($LASTEXITCODE -ne 0) { throw 'Workspace SQLite connection tests failed.' }
```

Windows x64 MSVC 上 6 项合成测试全部通过。`dd_infra::sqlite::initialize_workspace_write_connection` 接收调用者已打开的文件连接，设置并读回 WAL、FULL、外键和指定的 busy timeout。重开后确认 WAL 持久化，并重新应用逐连接设置。测试还验证了孤儿行拒绝、只读和内存连接拒绝、活动事务丢弃未提交行、无效超时拒绝，以及后续设置失败时保留 SQLite 错误码和已生效的 WAL。

根锁文件的 11 个外部依赖与 P00-03 一致，使用 `rusqlite 0.42.0`（`bundled`、`backup`）和 `libsqlite3-sys 0.40.0`。已核对运行时 SQLite 3.54.1 及其 Source ID，检查了静态链接和测试程序的 DLL 导入表。验证范围限于合成 Workspace 写连接初始化。数据库迁移、`app.db`、真实 Workspace 数据和其他平台待独立验收。

## Windows Workspace SQLite 空库迁移（P01-04）

在 x64 VS Developer PowerShell 中使用 P00-03 的本机 Cargo 缓存和 Rust/Cargo 1.97.1。默认执行根 Cargo workspace 的全部测试，包括 P01-03 连接初始化和 P01-04 空库迁移。可用 `-Suite Migration` 或 `-Suite Connection` 单独复现。

```powershell
$rustBin = '<Rust 1.97.1 x86_64-pc-windows-msvc 工具链的 bin 绝对路径>'
powershell.exe -NoProfile -NonInteractive -File .\scripts\storage\windows\check-workspace-sqlite.ps1 -RustBin $rustBin
$LASTEXITCODE
```

迁移入口复用写连接初始化，并在同一事务里应用 [Workspace v1 SQL](migrations/workspace/0001_initial.sql)、写入带版本的 Workspace 身份及时间戳、执行完整性与外键检查。合成库验证 8 张业务表、12 个显式索引、重开保留数据、拒绝身份或版本不符，以及校验阶段故障时回滚到空 schema 并可重试。验收要求 `integrity_check` 返回唯一的 `ok`，`foreign_key_check` 返回零行。构建、迁移样本和本次日志位于 `target/p01-04/`，P01-03 的连接样本仍位于 `target/p01-03/runs/`。

## 已有可执行验证

```sh
cd tests/rig-poc
cargo +1.97.1 test --locked --all-targets -- --nocapture
```

无需 API Key；使用合成模型响应。产品构建、运行与安装说明在正式实现后按实际情况维护。
