# Dataset Doctor 设计审核包 v0.1

日期：2026-11-26

1. 解压后在浏览器打开 index.html。无需安装 Node、Python 或运行服务。
2. 内含14个业务页面，每页HTML独立内嵌样式、脚本和合成示意图，页面间可跳转。
3. 优先审核08-review.html、09-video.html、11-color-repair.html，再按导入→体检→审核→导出检查主流程。
4. docs目录包含前端页面与交互设计、L3实现规范与工程任务清单。
5. 原型使用示例数据，不读取你的图片，不调用模型，不修改真实文件。不要填写真实API Key。
6. 已做文件/链接/JS语法检查；浏览器自动渲染与实际点击验收未完成，请在你的浏览器审核布局与交互。
7. 反馈可用“页面ID + 位置 + 修改意见”。

正式桌面应用仍按React+TypeScript+Vite+Tauri2、Rust Core/Agent与Python Worker实现，本包不包含已实现的产品后端。
