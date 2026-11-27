# Dataset Doctor

Local-first desktop application for computer-vision dataset quality governance. Rust Core + Rust Agent Service (Rig) + Python algorithm Worker; Tauri 2 / React / TypeScript frontend. Windows, Linux and macOS are target platforms.

L1/L2 确立方案Ⅰ，L3 定义实现与验收。Windows 桌面壳已可离线构建与启动。导入、检查、Final 和 COCO 读取已通过命令宿主和 WebView2 验收。原生窗口交互和桌面安装器仍待验收。Rig SDK 独立 PoC 有三平台验证记录。

- [项目文档入口](docs/README.md)
- [开发工作守则](docs/development/coding_guidelines.md)（AI Coding 自动阅读入口：[AGENTS.md](AGENTS.md)）
- [前端 HTML 原型](docs/modules/frontend/prototype/index.html)
- [实施任务与验收](docs/development/implementation_spec_and_tasks.md)
- [MVP 开发进度与验收证据](docs/development/mvp_progress.md)
- [Rig SDK 测试报告](docs/testing/rig_sdk_validation.md)

## Windows x64 桌面壳构建与启动

使用 x64 VS Developer PowerShell 和锁定的 Node、pnpm、Rust。初次准备缓存时，设置 `$env:CARGO_HOME` 为仓库下 `target/dev/cargo-home`，执行 `cargo fetch --locked --target x86_64-pc-windows-msvc`。在 `apps/desktop` 执行 `pnpm install --frozen-lockfile --ignore-scripts --store-dir <仓库绝对路径>/target/dev/pnpm-store`。

启用桌面几何检查的构建需要将 `protoc.exe` 加入 PATH。开发版启动后使用 `target/dev/worker-venv/Scripts/python.exe` 运行仓库中的 Worker。下方跨语言 Worker 验收命令会从本地缓存准备此环境。也可在启动前同时设置 `DD_WORKER_PYTHON` 和 `DD_WORKER_ENTRYPOINT` 为可信的解释器和 `services/python-worker/src/dd_worker/entrypoint.py` 的绝对路径。

```powershell
$rustBin = '<Rust 1.97.1 MSVC 工具链的 bin 绝对路径>'
$pnpm = '<pnpm 10.18.3 的 pnpm.exe 绝对路径>'
powershell.exe -NoProfile -NonInteractive -File .\scripts\packaging\windows\build-desktop.ps1 -RustBin $rustBin -PnpmExe $pnpm
if ($LASTEXITCODE -ne 0) { throw 'Desktop build failed.' }
$exe = '.\target\dev\cargo-target\x86_64-pc-windows-msvc\debug\dd-desktop.exe'
powershell.exe -NoProfile -NonInteractive -File .\tests\desktop\windows\verify-runtime.ps1 -DesktopExe $exe
if ($LASTEXITCODE -ne 0) { throw 'Desktop launch smoke failed.' }
& $exe
```

冒烟测试在独立 WebView2 数据目录加载内嵌的 React 前端，经真实 Tauri IPC 握手后退出。普通启动后可用 Windows 文件夹选择器创建或打开工作区，查看离线资源数，再关闭工作区。传入 `-Suite Workspace -PythonExe $py312` 复跑已有工作区的打开、查询、关闭、重开和旧会话拒绝。传入 `-Suite WorkspaceCreate` 复跑空目录创建、拒绝重复创建和重开。完整业务和安装器另有验收。详见 [MVP 进度与证据](docs/development/mvp_progress.md)。

Windows 开发版的自动化业务验收使用 `tests/desktop/windows/verify-runtime.ps1 -DesktopExe $exe -Suite WorkspaceBusiness -PythonExe $readerPython`。读取器环境需安装 Pillow 和 pycocotools 2.0.11，开发版需能启动本机 Worker。隐藏的 WebView2 通过真实 IPC 创建合成工作区、导入、检查、修正 Final、导出 COCO 和重开工作区。脚本使用专用夹具目录提供选择路径，并用目标读取器复核导出文件。系统选择器与画布鼠标操作有单独的原生窗口验收。

将上述命令的 `-Suite` 改为 `WorkspaceColor` 可继续验收颜色修复。测试操作真实 WebView2 面板和 Worker，比较原图、候选和掩膜，核验明确批准后才替换图片，旧审核随之过期，复检读取新图片。同一套读取器复核原 COCO 包。

使用 `-Suite WorkspaceWorkflow` 可以验收本地体检。测试点击数据集面板、运行真实 Worker，检查几何问题、实际覆盖和未执行原因。关闭并重开 Workspace 后读取同一份报告，保持人工审核关卡和源文件摘要。

使用 `-Suite WorkspaceReportExport` 可以验收报告导出。测试从同一份质量报告导出 JSON 和 Markdown，用独立脚本核验文件、报告摘要和持久化回执。

在命令宿主验收中传入 `-VideoFfmpegExe $ffmpeg -VideoFfprobeExe $ffprobe`，从输出的 `Real Staging fixture` 行取得
`staging-batch-ui.json` 路径。使用
`tests/desktop/windows/verify-runtime.ps1 -DesktopExe $exe -Suite WorkspaceStagingBatch -PythonExe $reader -StagingFixturePlan $plan`
可在真实 WebView2 批选两个已审核合成帧，登记检查缺项理由并接收。每次运行复制一份夹具，独立核对批次回执、成员关系和原始帧字节。

使用 `tests/desktop/windows/verify-runtime.ps1 -DesktopExe $exe -Suite WorkspaceVideo -PythonExe $reader -PackagedWorkerExe $packed -VideoToolsEvidence $tools`
可验收 Windows 搬移布局。这里 `$packed` 指向已打包的 `dd-worker.exe`，`$tools` 指向已核验的 FFmpeg/ffprobe 工具链证据 JSON。
测试复制 Worker 及视频工具，关闭开发环境路径，登记合成视频、读取指定帧并重开 Workspace，独立核对两个任务的封存产物、像素及源摘要。
将该测试运行目录中的 `Packaged desktop` 作为 `$layout`，执行
`tests/desktop/windows/check-command-handlers.ps1 -RustBin $rustBin -VideoFfmpegExe $ffmpeg -VideoFfprobeExe $ffprobe -PackagedVideoResources $layout`
可进一步复跑同一搬移布局的追踪、训练采样与待接收审核。

打开已有 Workspace 后可分页浏览 Dataset 和样本，查看正式标注与受限的本地图像。命令宿主验收使用 `tests/desktop/windows/check-command-handlers.ps1 -RustBin $rustBin`，图像授权和标注读取可用 `scripts/storage/windows/check-workspace-sqlite.ps1 -RustBin $rustBin -Suite MediaDisplay` 或 `-Suite AnnotationView` 单独复跑。这些命令均可在命令行执行。样本页面的新交互与自定义图像协议仍需实际 WebView2 验收。

在宿主验收附加 `-CocoReaderPythonExe $reader` 可用装有 `pycocotools 2.0.11` 和 Pillow 的本机 Python 重新读取测试生成的 COCO 包。该用例通过宿主完成合成样本导入、检查、审核、Final 确认和导出，产物保存在 `target/desktop-commands/runs/`。

## Windows 工具链预检（P00-01）

在仓库根目录运行以下命令。从其他目录运行时，将脚本参数换成完整路径。

```powershell
powershell.exe -NoProfile -NonInteractive -File .\scripts\check-toolchain.ps1
$LASTEXITCODE
```

预检读取当前进程 PATH，只查询可直接运行的本地工具。退出码 `0` 表示清单已核实且满足 Node 24 和 CPython 3.12 系列目标，`1` 表示有缺失、版本偏离或未核实项。完整 P00 还需验证工具链锁定与 Worker 打包。

Windows x64 构建在同一命令末尾加 `-Locked`，按项目版本锁核对七个工具的精确版本。PATH 需优先包含安装好的真实可执行文件路径。已用本机 uv 0.9.26 和 0.11.2 验证接受锁定版本和拒绝偏离。[Windows x64 版本锁](config/toolchains/windows-x64.json)

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

## Windows JSON Schema 结构检查

在仓库根目录运行。使用 Windows x64 上实际安装的 CPython 3.12.12、仓库里的验证依赖锁和已预填的 P01-01 专用 uv 缓存。

```powershell
$py312 = '<CPython 3.12.12 x64 的 python.exe 绝对路径>'
$offlineCache = (Resolve-Path -LiteralPath '.\dist\p01-01\uv-cache').Path
powershell.exe -NoProfile -NonInteractive -File .\scripts\contracts\windows\check-json-schema.ps1 -PythonExe $py312 -UvCacheDir $offlineCache
$LASTEXITCODE
```

入口锁定离线同步验证环境，执行严格 UTF-8 JSON 解析、Draft 2020-12 结构和 UUID 格式检查，并用封闭本地注册表解析引用。输出记录在 `dist/p01-01/logs/check.log`。通过表示结构和合成向量符合契约。操作输入前提、授权与摘要真实性由后续 Core 工作验证。

查看 [InputManifest v1 Schema](contracts/json-schema/v1/input-manifest.schema.json)、[共享向量](testdata/contracts/json-schema/v1/input-manifest/cases.json) 和 [验收依赖及许可](docs/deployment/platform_delivery.md#p01-01-inputmanifest)。

默认验证 InputManifest、Workspace 配置和单图 COCO bbox。可用 `-Suite InputManifest`、`-Suite WorkspaceConfig` 或 `-Suite CocoSingleImage` 单独运行。Workspace 配置覆盖必填身份和版本、部分默认设置、未知字段、参数边界、重复键和 Unicode 名称长度。[Workspace 配置 v1 Schema](contracts/json-schema/v1/workspace-config.schema.json) · [共享向量](testdata/contracts/json-schema/v1/workspace-config/cases.json)

单图 COCO 验证单条图片、局部安全整数、空标注、四元组 bbox 及不支持字段的拒绝。几何与图像、类别引用需要另行检查。[单图 COCO v1 Schema](contracts/json-schema/v1/coco-single-image.schema.json) · [共享向量](testdata/contracts/json-schema/v1/coco-single-image/cases.json)

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

## Windows Rust 与 Python Worker 进程契约

在 x64 VS Developer PowerShell 中使用锁定的 Rust、protoc、CPython 和 uv。先按 Worker 锁文件在 `target/dev/uv-cache` 准备依赖，再运行：

```powershell
$env:UV_PROJECT_ENVIRONMENT = Join-Path (Get-Location).Path 'target\dev\worker-venv'
& $uv sync --locked --no-build --no-python-downloads --no-config --project .\services\python-worker --cache-dir .\target\dev\uv-cache --python $py312
powershell.exe -NoProfile -NonInteractive -File .\scripts\contracts\windows\check-worker-rpc.ps1 -RustBin $rustBin -ProtocExe $protoc -PythonExe $py312 -UvExe $uv
```

入口在离线缓存中同步 Python 和构建 Rust。临时重生成 Python 桩并检查受版本控制的文件无漂移。集成测试验证本机 loopback 鉴权、正常退出、ArtifactRef 跨语言往返和 `ClaimGrant` 持久化重放。另一个测试启动真实 Worker，通过专用 `StartAnnotationGeometryAudit` 检查合成 PNG 和逐图 COCO，核对发现、产物摘要和原文件。配置 Core loopback 连接后，Worker 报告 `annotation_geometry.v1` 能力。工具输出和用例日志位于 `target/contract-check/worker-rpc/`。

已同步 Worker 环境后，`target/dev/worker-venv/Scripts/python.exe -I -B -m unittest discover -s tests/worker/image -v` 可验收实际 PNG/JPEG 解码、EXIF 方向、源字节摘要、损坏输入和像素限制。

Rust/TypeScript 公共整数和归属类型的验收使用 `scripts/contracts/windows/check-ui-wire.ps1 -RustBin $rustBin -NodeExe (Get-Command node.exe).Source`。脚本重新生成 TypeScript 声明，检查文件漂移，再用同一组向量运行 Serde 和 Node TypeScript 解析。显式传入 `-Generate` 更新受版本控制的声明。

### 备份和恢复工作区

在打开的工作区展开「备份工作区」，选择工作区之外的本机目录并开始备份。成功回执会显示备份包子目录，重新打开工作区可查看最近一条备份结果。备份包包含 SQLite 快照、已登记的外部与工作区资源、标注及正式产物。

恢复时先关闭当前工作区，填写新目录名。依次选择回执显示的备份包目录和恢复位置的上级目录。验证通过后应用打开恢复出的工作区。恢复要求新目录，已有文件会保留。

### Windows 生产 Worker 与视频适配验收

使用现有的 `target/dev/uv-cache` 和 `dist/p00-worker-smoke/uv-cache`，从锁文件离线构建实际 Worker 入口。构建输出会打印 `$workerExe` 对应的绝对路径，所在运行目录包含两份锁文件和 EXE 的摘要记录。

```powershell
powershell.exe -NoProfile -NonInteractive -File .\scripts\packaging\windows\build-worker-product.ps1 -PythonExe $py312 -UvExe $uv
powershell.exe -NoProfile -NonInteractive -File .\tests\packaging\windows\worker-product\verify-worker-product.ps1 -WorkerExe $workerExe -RustBin $rustBin -ProtocExe $protoc -PythonExe $py312 -UvExe $uv
```

为 `$ffmpeg` 和 `$ffprobe` 提供已核验的 Windows x64 本机工具绝对路径。通过下列命令生成合成视频并分别验收像素解码及 Core 授权的帧索引。运行包内 Worker 的验证命令也接受同一对视频工具参数。正式桌面安装验收仍待完成。 [FFmpeg Windows 验收包](https://github.com/FFmpeg-Builds/FFmpeg-Builds/releases/tag/autobuild-2026-11-24-12-41)

```powershell
powershell.exe -NoProfile -NonInteractive -File .\scripts\contracts\windows\check-video-decoder.ps1 -FfmpegExe $ffmpeg -FfprobeExe $ffprobe -PythonExe .\target\dev\worker-venv\Scripts\python.exe
powershell.exe -NoProfile -NonInteractive -File .\scripts\contracts\windows\check-worker-rpc.ps1 -RustBin $rustBin -ProtocExe $protoc -PythonExe $py312 -UvExe $uv -VideoFfmpegExe $ffmpeg -VideoFfprobeExe $ffprobe
```

视频详情支持按时间（毫秒）或从 0 开始的显示帧序号请求精确帧。时间定位可选最近帧或该时间及之后的帧，显示结果保留实际帧序号和规范化微秒时间。后台解析成功后取得会话显示授权，缓存命中时复用已核验的帧。

视频片段持久化使用相同离线环境执行 `cargo test --locked --offline --target x86_64-pc-windows-msvc -p dd-infra --features execution-rpc --test video_segments`。用例覆盖半开时间区间、已保存草稿引用、类别上下文、修订冲突、移出关键帧的保留和备份恢复，累计 Worker RPC 视频验收也包含这一组。

人工关键帧持久化可用 `cargo test --locked --offline --target x86_64-pc-windows-msvc -p dd-infra --features execution-rpc --test video_keyframes` 复跑，环境变量与 Worker RPC 验收一致。它使用真实合成视频与专用 Worker，验证草稿的帧、类别、几何、修订冲突和备份恢复。传入视频工具的 `check-worker-rpc.ps1` 也包含这些用例。桌面关键帧编辑入口仍待接入。

视频登记后可查询时长、视频流、旋转、帧率节奏和来源状态。刷新信息会重新核对视频摘要，离线时保留已经封存的元数据。视频片段与抽帧工作台仍在开发。

桌面命令宿主的完整视频验收需要同时传入已核验的 FFmpeg、FFprobe 路径，脚本会启用 video-acceptance 测试特性并运行真实帧解析与图像协议用例。

Windows 开发版的视频入口从启动进程继承已核验的视频工具路径。使用 `$env:DD_WORKER_VIDEO_FFMPEG = $ffmpeg` 和 `$env:DD_WORKER_VIDEO_FFPROBE = $ffprobe` 设置工具路径后启动桌面程序。`tests/desktop/windows/check-command-handlers.ps1` 接受 `-VideoFfmpegExe` 和 `-VideoFfprobeExe` 参数，可在命令行复跑 Windows 桌面命令宿主的真实视频探测。

在视频工作台保存人工关键帧和片段后，从「视频追踪」选择片段和取帧方式。本机平移追踪使用首个有标注的关键帧。在时间轴选择实际显示帧时，可查看对应的候选框、来源及需审核的轨迹区间。

在「训练采样」中选中同一已保存片段，并勾选「随样本保留当前追踪候选」。实际生成候选框的样本会在待接收区保留对应追踪来源。已审核的正式标注随原样本保留。

对有候选的待接收帧运行标注检查并开始人工审核后，可通过「采用此框」加入可编辑标注，再保存审核草稿。继续审核会恢复已保存的轨迹关联。

对登记图片运行标注检查取得原图尺寸后，在「颜色修复」选择矩形或输入多边形顶点，设置目标颜色并生成预览。候选及掩膜以实际图像显示。核对候选后依次点击「应用此候选」和「确认替换图片」。应用会使已有审核过期，随后可用新图像运行标注检查。

在目标数据集的待接收区选择视频样本，运行标注检查后刷新待接收区并点击「查看报告」。报告显示实际几何检查、发现项和未评估的语义检查。确认 Final 后重新运行标注检查，Core 会按当前图像、标注和类别生成报告。

样本进入「可接收」后，在当前报告中逐项确认相关发现，为未执行的检查填写接收理由，再点击「确认接收」。接收成功后，待接收区和数据集成员列表更新，并保留可查询的任务回执。

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

在 x64 VS Developer PowerShell 中使用 Rust/Cargo 1.97.1。入口默认从 `target/dev/cargo-home` 离线运行基础设施测试，也可指定 `-OfflineCargoHome`。首次准备该缓存时，先在同一个 Cargo home 执行 `cargo fetch --locked --target x86_64-pc-windows-msvc`。`-Suite Migration` 和 `-Suite Connection` 可单独复现。

```powershell
$rustBin = '<Rust 1.97.1 x86_64-pc-windows-msvc 工具链的 bin 绝对路径>'
powershell.exe -NoProfile -NonInteractive -File .\scripts\storage\windows\check-workspace-sqlite.ps1 -RustBin $rustBin
$LASTEXITCODE
```

迁移入口复用写连接初始化，并在同一事务里应用 [Workspace v1 SQL](migrations/workspace/0001_initial.sql)、写入带版本的 Workspace 身份及时间戳、执行完整性与外键检查。合成库验证 8 张业务表、12 个显式索引、重开保留数据、拒绝身份或版本不符，以及校验阶段故障时回滚到空 schema 并可重试。验收要求 `integrity_check` 返回唯一的 `ok`，`foreign_key_check` 返回零行。构建、迁移样本和本次日志位于 `target/p01-04/`，P01-03 的连接样本仍位于 `target/p01-03/runs/`。

同一入口还运行 `app.db` 的 3 项迁移用例。用 `-Suite AppMigration` 单独运行。全局库使用独立的 `main.user_version`，初始化 Workspace 注册表和非秘密设置，故障回滚后可重新迁移。[全局库 v1 SQL](migrations/app/0001_initial.sql)

Rust 的 JSON 验收可单独运行 `cargo test --locked --offline --target x86_64-pc-windows-msvc -p dd-infra --test json_contracts`。它与 Python 读取相同的 75 个合成向量，并额外检查无效 UTF-8、过长整数和原样保留的扩展配置。Schema 随程序嵌入，解析引用时使用本地注册表。

## 已有可执行验证

```sh
cd tests/rig-poc
cargo +1.97.1 test --locked --all-targets -- --nocapture
```

无需 API Key；使用合成模型响应。产品构建、运行与安装说明在正式实现后按实际情况维护。
