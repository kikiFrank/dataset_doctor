# 跨平台交付

独立 Rig SDK 已在三平台完成编译和测试。Windows Worker 的内部打包验证记录见本页 P00-02。目标平台 Windows、Linux、macOS。桌面端使用 Tauri 2，Rust Core 管理本地 Agent Service 和 Python Worker 生命周期，gRPC 仅绑定 loopback。打包、运行依赖、安装和卸载验证见 [L2 验收](../testing/acceptance_and_platform_validation.md) 与 [L3 实施任务](../development/implementation_spec_and_tasks.md)。真实部署、模型配置和升级回滚命令须在实现后按实际情况维护。

## P00-02 Windows Worker PoC

本记录覆盖 Windows x64、CPython 3.12.12 的内部打包验证。锁定版本来自 [Worker PoC 锁文件](../../packaging/windows/worker-smoke/uv.lock)，许可依据来自本机实际安装的 wheel。下表证据文件相对于各轮子安装后的 `.dist-info/` 目录。建包和真实 onedir EXE 的正常与截断 PNG 用例均已在 Windows PowerShell 5.1 通过。

| 依赖 | 锁定版本 | PoC 中的用途 | 许可 | wheel 证据 |
| --- | --- | --- | --- | --- |
| Pillow | 12.1.0 | Worker 完整解码合成 PNG | `MIT-CMU` | `METADATA`、`licenses/LICENSE` |
| PyInstaller | 6.18.0 | 构建 Windows onedir 包 | `GPL-2.0-or-later WITH Bootloader-exception`，运行时 hooks 为 `Apache-2.0`，`PyInstaller.isolated` 另有 MIT 条款 | `METADATA`、`licenses/COPYING.txt` |
| altgraph | 0.17.5 | 构建时的模块依赖图 | `MIT` | `METADATA`、`LICENSE` |
| packaging | 26.4 | 版本和依赖标记处理 | `Apache-2.0 OR BSD-2-Clause` | `METADATA`、`licenses/LICENSE`、`licenses/LICENSE.APACHE`、`licenses/LICENSE.BSD` |
| pefile | 2024.8.26 | 构建时处理 Windows PE 文件 | `MIT` | `METADATA`、`LICENSE` |
| pyinstaller-hooks-contrib | 2026.10 | 为打包分析提供社区 hooks | 标准 hooks 及其他文件为 `GPL-2.0-or-later`，运行时 hooks 为 `Apache-2.0` | `METADATA`、`licenses/LICENSE` |
| pywin32-ctypes | 0.2.3 | 为构建提供 Windows API 绑定 | `BSD-3-Clause` | `METADATA`、`LICENSE.txt` |
| setuptools | 84.2.1 | PyInstaller 的构建环境依赖 | `MIT` | `METADATA`、`licenses/LICENSE` |

PyInstaller 的 `COPYING.txt` 将 Bootloader Exception 用于编译后的 bootloader 和一同嵌入组合产物的相关 loader 文件，涵盖链接、嵌入和分发该组合的情形。文件修改和单独分发仍由该文件列出的 GPL 条款覆盖。运行时 hooks 的 Apache-2.0 来自各自许可条款。

未来对外发布时，依据实际交付目录核对 bootloader、运行时 hooks、CPython、Pillow 携带的原生库和其他随附组件，并记录相应许可证和版权声明。随附原生库和其他嵌入组件的逐项许可及需随包提供的材料仍待发布审核确认。

## P01-01 InputManifest

以下依赖用于 Windows x64、CPython 3.12.12 上的 InputManifest v1 离线验收环境。Windows PowerShell 5.1 使用 uv 0.9.26 从本地缓存锁定同步后，通过严格 JSON 解析、Draft 2020-12 和本地引用检查。生产发行包的随附组件按实际交付内容单独审核。

版本和发行文件哈希见 [验收依赖锁](../../tests/contracts/json-schema/uv.lock)。许可取自本次实际安装的 wheel，表中证据文件相对于该包的 `.dist-info/` 目录。

| 依赖 | 锁定版本 | 验收用途 | 许可 | wheel 证据 |
| --- | --- | --- | --- | --- |
| jsonschema | 4.26.0 | 验证 Draft 2020-12 Schema 和实例 | `MIT` | `METADATA`、`licenses/COPYING` |
| attrs | 26.1.0 | 验证依赖使用的数据类工具 | `MIT` | `METADATA`、`licenses/LICENSE` |
| jsonschema-specifications | 2025.9.1 | 为验证器提供本地规范资源 | `MIT` | `METADATA`、`licenses/COPYING` |
| referencing | 0.37.0 | 提供封闭的本地引用注册表 | `MIT` | `METADATA`、`licenses/COPYING` |
| rpds-py | 2026.9.1 | 支撑引用注册表的持久数据结构 | `MIT` | `METADATA`、`licenses/LICENSE` |
| typing-extensions | 4.16.0 | 为验证依赖提供 Python 类型兼容支持 | `PSF-2.0` | `METADATA`、`licenses/LICENSE` |
