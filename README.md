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

## 已有可执行验证

```sh
cd tests/rig-poc
cargo +1.97.1 test --locked --all-targets -- --nocapture
```

无需 API Key；使用合成模型响应。产品构建、运行与安装说明在正式实现后按实际情况维护。
