# Datum 用 nsh 重写:设计

## 为什么

- 让 Datum 跑在 Windows 和 Linux 上(现在只发 macOS)。
- 借这个真实项目练熟 nsh(Native Shell,`~/github/nsh`)。所以界面用 **TSX** 写,数据层留在 Rust。

## 目标 / 非目标

- **目标**:外观与功能和 GPUI 版(`crates/app`,v0.0.1)保持一致,直接替换,不双轨维护。
- **非目标**:新视觉、新功能。
- **暂缓(TODO)**:ER 图画布(GPUI 版 `crates/app/src/erd/`)。nsh 没有现成的节点/连线画布,先留占位 tab,
  后续用 nsh 的 `Canvas` 或 Rust 组件实现。

## 结构

```
crates/core/          数据层(原样复用;只允许为跨边界补 Serialize 等小改动)
app/                  nsh 应用(混合模式:"rust": "native",无 "root",入口 App.tsx)
  nsh.json  theme.json  locales/(rust-i18n 文案,原 crates/app/locales)  assets/icons/
  native/             Rust crate datum-native:#[native_module("datum/native")]
  lib/types.ts        跨边界数据形状(与 core 的 serde camelCase 输出一致)
  lib/native.ts       datum/native 的类型化封装 —— 界面只从这里调原生
  store/              全局状态(signal 私有,导出读写函数):prefs conns history toast workspace ai …
  ui/                 样式组件层(kit.tsx、icons.ts、text.ts)—— 页面只用这里的组件
  workspace/          外壳:顶栏、连接竖轨、侧栏、标签条、详情栏、状态栏
  tab/                查询/表浏览 tab:编辑器、结果网格、工具条、过滤、历史、单元格编辑…
  dialogs/            连接表单、设置、导入、风险确认、文档编辑
  chat/               AI 助手面板
```

## 约定

- **原生边界**:跨边界一律 JSON(`JsonValue`),Rust 端 `Result<JsonValue, String>`,错误文案用
  `AppError::display()`。异步导出函数用 `background(..)`(nsh 引擎自带 tokio 运行时)跑驱动。
  `#[module]` 的返回类型要写全 `Result<JsonValue, String>`,宏不认类型别名。每个领域一个文件、一个 `#[module]`,
  在 `native/src/lib.rs` 注册,并在 `lib/native.ts` 加类型化封装。
- **状态**:每份状态一个所有者。Rust 只管持久化与 IO;界面状态在 TSX 的 store/ 或页面里。
- **样式**:颜色只用 `theme.json` 里的 token(`bg-panel`、`text-fg-faint`、`border-hairline`…),
  不写裸色值;字号用 `ui/text.ts` 的 `FS.*`(跟随「正文字号」偏好)。`theme.json` 的 token 来自
  GPUI 版 `assets/themes/datum.json` + `theme.rs` 的 Palette,名字对应关系见该文件。
- **文案**:`t("…")` 来自 `nsh/i18n`,键与 GPUI 版完全相同(locales/*.yml)。
- **列表**:可增长的集合用 `List`(虚拟化);结果网格用 nsh/base 的 `DataTable`;SQL 编辑器用 `Editor`。
- **快捷键**:`keymap.bind("mod+enter", …)`(nsh),与 GPUI 版 `actions.rs` 的键位一致。
- **验证循环**:`app/` 下 `nsh check` → `nsh build`(warn 都要看)→ `nsh test` → `nsh run` 看实际效果。

## 工具链

- nsh 的 crate 用本地源码 `~/github/nsh`(路径依赖);全局 CLI 已从该源码 `cargo install` 到最新
  (`nsh 0.4.0 (88c0eee)`),与宿主同源。crates.io 上的 0.4.0 缺 `nsh-platform-tui`,不能用。
- 待办:发版前改成 git 依赖或 crates.io 版本,并补三平台的打包、签名与自动更新(`nsh/updater`)。
