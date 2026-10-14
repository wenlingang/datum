# Datum

一款简约现代的桌面数据库管理工具,支持 **PostgreSQL**、**MySQL**、**SQLite** 与 **Redis**。

这是 Datum 的 Rust 原生重写版:界面基于 [GPUI Kit](https://github.com/longbridge/gpui-kit)(GPUI + gpui-component),
数据层沿用原 Tauri 版的 Rust 后端,不再依赖 WebView 与 Node.js。

> Datum(/ˈdeɪtəm/,"data" 的单数)—— 专注于数据的工具。

## 功能

- **连接管理**:新建/编辑/删除/测试连接,连接串预览;按环境(local / staging / prod)着色;凭据按环境路由——`local` 明文存本地、`staging/prod` 存 macOS 钥匙串;侧栏切库(同实例复用或自动克隆连接)、库右键导出/删除
- **表浏览**:服务端分页与排序、列类型分级配色、结果筛选(且/或、多种操作符)、单元格全文查看(JSON 自动格式化)、暂存编辑与删除后 ⌘S 单事务提交、复制 INSERT、导出 CSV / SQL / Markdown
- **SQL 编辑器**:语法高亮、schema 感知补全(表名、`表.` / 别名列、作用域列、子句模板)、关键字自动大写、运行光标所在语句或选区、格式化、Explain、行数上限、查询历史(按库隔离)
- **破坏性语句审查**:DROP / TRUNCATE / 无 WHERE 的 DELETE、UPDATE 等执行前统一确认;Redis 的 FLUSHALL / FLUSHDB 同样拦截
- **Redis**:浏览 key、按类型展示值、执行任意命令(命令与 key 名补全)、key 详情(类型 / TTL / 大小)
- **AI 助手**:可配置多个服务商(OpenAI 兼容 / Anthropic / Gemini,内置 DeepSeek、通义、Kimi 等预设),面板内切换模型;按连接分会话;自动注入表结构与知识库文档(内置数据库开发规范,可替换);生成的 SQL 以卡片展示,确认后执行或在编辑器中运行
- **关系图**:物理外键 + 按 `xxx_id` 命名推断的逻辑外键,分层布局、拖拽、缩放、搜索高亮、表结构图 / 工程 ER 图切换、导出 PNG
- **导入**:从 .sql 文件或粘贴内容单事务导入(失败整体回滚并定位到第几条)
- **界面**:简体中文 / 繁體中文 / English(默认跟随系统,AI 提示词与内置规范随语言切换)、亮色 / 暗色 / 跟随系统、字体字号设置、可拖动分栏、多标签页、应用内更新

## 安装

从 [Releases](https://github.com/wenlingang/datum/releases) 下载 `Datum_<版本>_aarch64.dmg`,把 Datum 拖进「应用程序」。

Datum 目前没有经过苹果公证,第一次打开会被系统拦截,任选一种方式放行(只需一次,之后的应用内更新不受影响):

- 打开「系统设置 → 隐私与安全性」,在页面下方找到 Datum 的提示,点「仍要打开」;
- 或在终端执行 `xattr -dr com.apple.quarantine /Applications/Datum.app`。

之后应用会自动检查更新。更新包带有 Ed25519 签名,应用验签通过才会安装。

## 结构

```text
crates/
├── core/   datum-core:连接、凭据、历史、PG/MySQL/SQLite/Redis 驱动、AI、自更新,
│           以及 SQL 切分/风险识别/生成/格式化、筛选排序导出、ER 布局、补全等纯逻辑
└── app/    datum:GPUI Kit 界面(工作区、标签页、结果网格、对话框、AI 面板、关系图)
```

## 环境要求

- **Rust** ≥ 1.88(开发用 1.96 验证)
- **macOS**(当前打包目标;GPUI 本身支持 Linux / Windows)
- 跑数据层集成测试时需要本地可连的 PostgreSQL / Redis(或 Docker)

## 开发

```bash
cargo run                  # 启动应用
cargo test --workspace     # 单元测试(数据层集成测试默认忽略)
```

配置与旧版共用同一目录(`~/Library/Application Support/com.dbstudio.app`),从 Tauri 版升级后连接、历史与 AI 配置直接可用。
开发时可用 `DATUM_CONFIG_DIR=/某个目录 cargo run` 指向独立的配置目录。

### 无窗口截图

没有屏幕录制权限的环境(CI、远程会话)里,可以用 GPUI 的 Metal 无头渲染器跑一段脚本并截图:

```bash
DATUM_CONFIG_DIR=/tmp/datum-cfg \
DATUM_SNAPSHOT="conn:<连接 id>;wait:600;table:users;wait:500;shot:/tmp/a.png" \
cargo run --features snapshot
```

可用步骤见 `crates/app/src/workspace/mod.rs` 的 `snapshot_step`(`query`、`sql:`、`run`、`detail`、`ai`、`erd`、`settings`、`theme:light`、`key:cmd-enter` 等)。

### 集成测试(需要数据库)

```bash
cargo test -p datum-core -- --ignored --test-threads=1
```

可用环境变量覆盖测试连接:`DBSTUDIO_TEST_PG`(libpq 连接串)、`DBSTUDIO_TEST_REDIS`(redis URL)。

## 打包与发布

### 发版(GitHub Actions)

版本号改好并提交后,推送一个与之同名的附注标签即可,说明文字会作为应用内的更新说明:

```bash
# Cargo.toml 的 version 改为 0.0.2 并提交后
git tag -a v0.0.2 -m "本次更新内容"
git push origin v0.0.2
```

`.github/workflows/release.yml` 会在 macOS 上依次:校验标签与版本号一致 → 跑测试 → 构建 → 代码签名 →
(有苹果证书时)公证 → 给更新包签名 → 在本仓库建 Release(附件:dmg、自更新 zip、`latest.json`)。
也可以在 Actions 页面手动运行(选择已有标签,可勾选构建 Apple Silicon + Intel 通用包)。

仓库 Settings → Secrets and variables → Actions 需要配置:

| Secret | 必需 | 内容 |
| --- | --- | --- |
| `UPDATE_SIGNING_KEY` | 是 | 更新包签名私钥(见下方「密钥」) |
| `MACOS_CERT_P12` | 否 | 代码签名证书(含私钥)的 .p12,base64 编码;不配置则 ad-hoc 签名 |
| `MACOS_CERT_PASSWORD` | 否 | 上述 .p12 的密码 |
| `NOTARY_API_KEY` / `NOTARY_KEY_ID` / `NOTARY_ISSUER` | 否 | App Store Connect API Key(.p8 全文、Key ID、Issuer ID),仅 Developer ID 证书可公证 |

### 密钥

**更新签名密钥**(必需)。应用内置公钥(`crates/core/src/update.rs` 的 `UPDATE_PUBLIC_KEY`),只安装用对应私钥签名的更新包。
私钥丢失将无法再给已安装的用户推送更新,务必备份;泄露则需要换密钥并让用户手动重装。

```bash
cargo run -p datum-core --bin datum-sign -- keygen ~/.config/datum/update-signing.key   # 打印公钥
gh secret set UPDATE_SIGNING_KEY < ~/.config/datum/update-signing.key
```

**代码签名证书**(推荐)。没有苹果开发者账号时,用自签名的代码签名证书:签名身份固定,用户对钥匙串
(staging / prod 连接的密码)的「始终允许」在更新后仍然有效;ad-hoc 签名则每次更新都要重新授权。

```bash
openssl req -x509 -newkey rsa:3072 -nodes -days 3650 -keyout codesign.key -out codesign.crt \
  -subj "/CN=Datum Self-Signed Code Signing" -addext "basicConstraints=critical,CA:false" \
  -addext "keyUsage=critical,digitalSignature" -addext "extendedKeyUsage=critical,codeSigning"
openssl pkcs12 -export -legacy -inkey codesign.key -in codesign.crt \
  -name "Datum Self-Signed Code Signing" -out codesign.p12   # 需 OpenSSL 3;-legacy 让 macOS 能导入
base64 -i codesign.p12 | gh secret set MACOS_CERT_P12
gh secret set MACOS_CERT_PASSWORD   # 输入导出时设置的密码
```

以后有了苹果开发者账号,把 `MACOS_CERT_P12` 换成 Developer ID 证书并配置公证 Secrets 即可,用户无需重装。

### 本地打包与自测

```bash
script/bundle-macos.sh               # 未签名 Datum.app
script/bundle-macos.sh --universal   # Apple Silicon + Intel 通用包
script/release-macos.sh              # ad-hoc 签名 + 更新签名 + latest.json(读 ~/.config/datum/update-signing.key)
DATUM_SIGN_IDENTITY="…" script/release-macos.sh --notes "…" --publish   # CI 不可用时手动发版
```

产物在 `target/bundle/`。应用启动后会静默检查本仓库最新 Release 附件里的 `latest.json`
(`UPDATE_MANIFEST_URL`,见 `crates/core/src/update.rs`):

```jsonc
{
  "version": "0.0.1",
  "url": "<DMG 下载直链>",
  "zip_url": "<.app zip 下载直链>",
  "sha256": "<zip 的 SHA-256>",
  "signature": "<zip 的 Ed25519 签名>",
  "notes": "本次更新内容"
}
```

应用内更新依次校验:SHA-256 与清单一致、Ed25519 签名有效、新包与当前应用的 Bundle ID 一致、
当前应用有 Developer ID 签名时新包也须完整签名且属于同一开发者团队,全部通过才原地替换。

## 快捷键

| 快捷键 | 作用 |
| --- | --- |
| ⌘↵ | 运行光标所在语句 / 选区 |
| ⌘⇧F | 格式化 SQL |
| ⌘E | Explain |
| ⌘Y | 查询历史 |
| ⌘⇧A | 把选中 SQL 发给 AI 助手 |
| ⌘F | 结果筛选(编辑器内为查找) |
| ⌘S | 提交暂存的编辑与删除 |
| ⌘R | 刷新当前标签页与表列表 |
| ⌘T / ⌘W | 新建查询 / 关闭标签页 |
| ⌘B | 显示 / 隐藏侧栏 |
| ⌘, | 设置 |
