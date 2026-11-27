# 产品 Windows CI

入口为 [.github/workflows/product-ci.yml](../../.github/workflows/product-ci.yml)。当前仅设置 Windows x64，Linux 和 macOS 产品验收仍按阶段安排。本文区分 CI 配置、同一入口在本机的结果，以及实际 GitHub Actions 结果，实际记录统一在 [MVP 进度](mvp_progress.md)。

## 测试选择

Push 和 PR 使用完整 Git diff 选择检查组，手动 workflow_dispatch 运行全部组。文档不触发产品运行作业。前端选择 TypeScript 与桌面边界；Worker 选择 Python、实际 RPC 与桌面边界；存储用例选择 SQLite。共享锁、契约和 CI 准备逻辑扩大检查。Rig PoC 保留已有独立工作流。未识别的源码或配置保守选择全部组。选择逻辑和可复跑验收位于 [select-checks.mjs](../../scripts/ci/select-checks.mjs) 与 [选择器测试](../../tests/ci/select-checks.test.mjs)。

| 检查组 | 执行范围 |
| --- | --- |
| rust | 全工作区格式、除桌面外的全部目标/feature 的严格 Clippy、Core 与领域测试。桌面 Clippy 在前端产物准备后执行。 |
| typescript | 类型检查、Vite 构建、页面状态逻辑与 CI 选择器测试。 |
| python | 不产生字节码的语法检查，Worker 各场景单元测试，包括真实短视频解码、追踪和取消。 |
| contracts | JSON Schema、Protobuf 描述符/线格式、Rust/TS 生成漂移和 Python 桩漂移；StubsOnly 在检查完成后返回，不重复执行 RPC。 |
| sqlite | 连接、Workspace/app 迁移、打开/创建、Task 持久化及独立 NTFS/WAL 跨进程锁验收。 |
| integration | 当前关键 Core/Worker、Final/权限、取消/进程树及审核复检，全部使用真实本地进程、SQLite 与短合成媒体。 |
| desktop | 前端与 Tauri 构建、全 feature 编译检查、直接命令宿主，以及真正启动 WebView2 的 Runtime/WorkspaceCreate 冒烟。 |

每个组完成后输出 `PRODUCT_CI_CHECK_PASSED <组>`。本机默认不启动桌面，desktop 在构建和宿主验证后明确返回未完成；只有明确传入 `-LaunchDesktop` 才执行最后的真实冒烟。用户占用桌面期间不要传该参数，也不要将构建或宿主通过记成整个 desktop CI 通过。

## 工具与环境

版本继续来自 [Windows 版本锁](../../config/toolchains/windows-x64.json)，工具归档 URL/摘要在 [CI 工具锁](../../config/toolchains/ci-windows-x64.json)。GitHub Actions 使用固定提交 SHA。准备阶段可下载锁定工具和 `--locked` 依赖，验收阶段复用缓存并采用 `--locked --offline` 或对应离线入口。缓存不包含真实媒体和模型凭据，检查阶段不读取工厂数据或调用真实模型 API。下载的归档先核对 SHA-256 再解压。

GitHub 已公告 Windows runner 的 D 盘将于 2026-11-30 改为 ReFS，因此远程 CI 在系统 NTFS 卷 materialize 当前 Git 已跟踪源码，再执行构建与存储验收，保证路径检查与文件系统语义一致。该目录只属于新建的托管 CI 虚拟机，不用于本机开发，不复制任何 ignored 数据。本机始终使用原来的 `H:\DEV_self\dataset_doctor`。环境 JSON 保存系统版本、架构、版本锁和实际 Rust/Cargo 版本。 [GitHub Windows 2025 镜像说明](https://github.com/actions/runner-images/blob/main/images/windows/Windows2025-Readme.md)

## 本机复跑

例如仅检查前端，沿用 README 中锁定 pnpm 的绝对路径。

```powershell
& .\scripts\ci\prepare-windows.ps1 -Check typescript -PnpmExe $pnpm
& .\scripts\ci\run-windows-check.ps1 -Check typescript
```

准备其他组时显式传入本机工具 `-RustBin`、`-PythonExe`、`-UvExe`，必要时传 `-ProtocExe`/`-GoogleProtoRoot`、`-FfmpegExe`/`-FfprobeExe` 和 `-SchemaCache`。默认只使用离线缓存；缓存不足会失败并指出所缺前提。准备成功只表示依赖就绪，之后再执行 run-windows-check。第一次使用的新 CI runner 通过 `-AllowNetwork` 准备缓存，项目锁文件不会因此重新解析或改写。

准备记录与主日志位于 `target/ci/<组>/`。原有专项脚本继续保留各自日志入口。Actions 只上传明确列出的合成验收日志与环境 JSON，保留七天，不上传通用 target 树、缓存、原媒体或本地私用夹具。不得将尚未推送、未启动或失败的 Actions 描述为通过。

准备 Schema 环境时沿用已有锁中的 `--prerelease disallow`。P00 独立锁仍使用 `target/p00-03/cargo-home`，不改为工作区通用缓存。命令输出通过显式 Tee 保存为 commands.log，避免 PowerShell Transcript 丢失延迟格式化的子进程输出。以上入口与原有专项脚本共享实现，默认 CI 不跳过失败用例，未执行桌面冒烟时不会输出整组通过。

CPython 3.12.12 在 setup-python 的 Windows 构建列表中不可用。远程准备使用锁定 uv 的 `scripts/ci/prepare-python.ps1` 安装指定 managed CPython Windows x64，验证真实版本/位数，再传绝对路径；只在引导期间允许下载，之后恢复原策略。`tests/ci/verify-python-bootstrap.ps1` 可在已有 managed 缓存下离线复验，兼容 Windows PowerShell 5 与 CI PowerShell 7。此路径遵循 [uv 官方 CI 安装方法](https://docs.astral.sh/uv/guides/integration/github/#setting-up-python)。

冷缓存引导须与专项验收边界一致：Schema 缓存默认为 `dist/p01-01/uv-cache`，符合原 Schema 验收的批准目录约束；pnpm action 的 PATH 入口是包装器，准备步骤显式传入其 `node_modules/@pnpm/exe/pnpm.exe`，再由已有版本检查确认 10.18.3，避免依赖机器上的其他 pnpm。

源码归档显式使用 `git -c core.autocrlf=false archive`，因为 Git archive 也会受 runner 的换行转换设置影响。提取后、准备前，在原 checkout 用 `verify-source-archive.mjs` 比较实际归档与 Git blob 字节，保护独立 P00 锁的固定摘要与生成桩。详细复现见 [DD-ISSUE-0003](../troubleshooting/windows-ci-bootstrap.md)。

SQLite 组通过 `tests/ci/verify-sqlite-file-lock-encoding.ps1` 执行原独立 P00 验收：强制模拟托管 UTF-8 BOM 控制台，再由薄调用器临时使用无 BOM stdin 并恢复编码，避免改变 broker 的精确握手。该入口仍实际核验锁摘要、bundled SQLite、NTFS/WAL 和两进程锁，不替换或跳过 P00。
