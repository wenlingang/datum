# Datum 用 nsh 重写:设计

## 为什么

- 让 Datum 跑在 Windows 和 Linux 上(现在只发 macOS)。
- 借这个真实项目练熟 nsh(Native Shell,`~/github/nsh`)。所以界面用 **TSX** 写,数据层留在 Rust。

## 目标 / 非目标

- **目标**:外观与功能和 GPUI 版(`crates/app`,v0.0.1)保持一致,直接替换,不双轨维护。
- **非目标**:新视觉、新功能;**MongoDB**(用户 2026-11-10 决定 nsh 版只做 SQLite / MySQL / PostgreSQL / Redis。
  core 的 mongo 代码保留;已存的 Mongo 连接留在配置里,界面不显示)。
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

## 工具链与发版

- nsh 的 crate 用 GitHub 仓库的固定提交(`app/native/Cargo.toml` 里的 `rev`);nsh CLI 必须与之同源,
  CI 用 `script/install-nsh.sh` 按该 rev 安装。升级 nsh:改 rev → 重装 CLI → `nsh check`/测试。
- 版本号唯一来源:`app/nsh.json` 的 `"version"`。标签 `v<版本>` 触发 `.github/workflows/release.yml`:
  测试 → macOS(aarch64、x86_64)/ Windows / Linux 打包 → macOS 用 "Datum Release" 证书重签并出 dmg
  (`script/sign-macos.sh`)→ `nsh update-manifest` 用 `UPDATE_SIGNING_KEY` 签 `latest.json` → 建 Release。
- 自动更新:`nsh/updater`,`nsh.json` 的 `updater` 指向 GitHub 仓库并带公钥(私钥在仓库外,
  `~/datum-update-key/update.secret`)。不兼容 v0.0.1 的旧更新器(用户决定直接切换,v0.0.1 需手动下载新版)。
- 测试:`app/testing/run-tests.sh`(在临时配置目录里分块跑)。
