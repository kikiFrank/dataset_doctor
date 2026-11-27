# DD-ISSUE-0003 Windows CI 冷缓存引导

状态：Resolved。2026-11-27，cc0a961 首次远程全绿，7a9c30d 在 30 秒场景预算下再次全绿。实际运行和本机原始日志索引见进度检查点。

环境：Windows Server 2025 GitHub runner，Rust 1.97.1、uv 0.9.26、CPython 3.12.12 x64、pnpm 10.18.3。实际失败运行：[45988980817](https://github.com/kikiFrank/dataset_doctor/actions/runs/45988980817)、[45989499120](https://github.com/kikiFrank/dataset_doctor/actions/runs/45989499120)。未涉及真实媒体、密钥或本机桌面控制。

| 现象 | 已确认根因 | 修复与回归 |
| --- | --- | --- |
| setup-python 找不到 3.12.12 | 其 Windows 构建列表没有锁定版本 | 复用 uv managed CPython 同版本，不放宽版本锁。prepare-python.ps1 明确架构与实际版本，恢复禁止下载策略；verify-python-bootstrap.ps1 以真实解释器离线验收。543f9fe 后远程 Python 组 43 项通过。 |
| pnpm.exe 不在 PATH | action 暴露 node_modules/.bin 包装器，native exe 在 @pnpm/exe 包目录 | 向现有准备入口显式传 native exe 路径，继续执行严格版本检查。远程 TypeScript/desktop 待复验。 |
| Schema 拒绝缓存目录 | CI 默认缓存未放在既有验收批准的 dist/p01-01 目录内 | 默认改为 dist/p01-01/uv-cache，保留旧路径约束。本机用默认准备参数及旧脚本离线通过 10 项。 |
| P00 拒绝 Cargo.lock 摘要 | runner 的 core.autocrlf=true 也作用于 git archive；相同 blob 的归档内容变成 CRLF | 归档时明确 core.autocrlf=false，不修改 P00 锁或断言。verify-source-archive.mjs 在真实归档中核对锁文件和生成桩与 Git blob 完全相同。模拟 runner 的真实 Git 配置可复现红测，显式覆盖后绿测。 |

第三轮运行 45990742241 已验证 Python、TypeScript、Rust、contracts 通过。归档固定摘要已恢复；P00 随后出现新引导故障：托管 UTF-8 控制台的 InputEncoding 带 BOM，.NET Framework 重定向 stdin 继承其 preamble，使受控 broker 的精确 run 握手被拒绝。最小实测显示带 BOM 时读取的字符不是 114,117,110，无 BOM 时匹配。CI 用 run-sqlite-file-lock.ps1 临时设无 BOM 输入编码并恢复调用者设置，原 P00 脚本与断言不变。新增 verify-sqlite-file-lock-encoding.ps1 强制带 BOM 的调用者环境，真实执行全套 NTFS/WAL 双进程验收并确认编码恢复；本机红绿通过，日志 mvp11-ci-sqlite-encoding-{red,green}.log。远程复验待接续。

本机证据：target/dev/logs/mvp11-ci-python-bootstrap-{red,green}.log、mvp11-ci-schema-default-{prepare,green}.log、mvp11-ci-archive-{red,green}.log。归档验收从有 .git 的 checkout 执行：`node tests/ci/verify-source-archive.mjs <实际源码 tar 路径>`；不在提取后的无 Git 目录执行。

冷缓存准备通过不等于业务通过。远程测试最终结果和提交号只以 [进度检查点](../development/mvp_progress.md) 为准。保持专项脚本、固定锁、归档摘要和失败拒绝；不以更新预期摘要或跳过测试掩盖引导问题。

限时器集成还复现了 Python 桥接 PowerShell 7 到 Windows PowerShell 5 时的 Utility 模块加载失败：继承的 PSModulePath 可指向不兼容的 bundled PowerShell 7 模块，Get-FileHash 不可用。仅在启动 powershell.exe 子进程时按大小写不敏感的键排除该继承值，让其初始化系统默认路径；不修改调用者环境。Windows 的 Python 环境键会大写，普通 dict 的大小写敏感 pop 不足以排除它。真实 Get-FileHash 红绿及原 P00 通过，回归归 tests/testing/windows，日志 test-deadline-powershell-red.log、test-deadline-final-unit.log、test-deadline-p00-green.log。最终远程七组通过，所有实际测试执行均在预算内。
