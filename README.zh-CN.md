# Datum

[English](README.md) | 简体中文 | [繁體中文](README.zh-TW.md)

一款简约现代的桌面数据库管理工具,支持 **PostgreSQL**、**MySQL**、**SQLite**、**Redis** 与 **MongoDB**。

这是 Datum 的 Rust 原生重写版:界面基于 [GPUI Kit](https://github.com/longbridge/gpui-kit)(GPUI + gpui-component),
数据层沿用原 Tauri 版的 Rust 后端,不再依赖 WebView 与 Node.js。

> Datum(/ˈdeɪtəm/,"data" 的单数)—— 专注于数据的工具。

<p align="center">
  <img src="docs/screenshots/table.png" alt="浏览数据表,右侧展开行详情" width="100%">
</p>

## 看一眼

**顺手地写 SQL。** 补全认得你的表、别名和列;⌘↵ 只跑光标所在的那一条。当然也有深色模式。

![SQL 编辑器:聚合查询及其结果](docs/screenshots/sql-editor.png)

**表之间怎么关联,一眼看清。** 关系图既读取真实外键,也会按 `xxx_id` 命名推断出没建外键的关联。

![示例商城库的关系图](docs/screenshots/er-diagram.png)

**用你习惯的方式查 MongoDB。** 打开集合就能用 Filter / Sort / Projection 翻看文档,表格和 JSON 一键切换,整篇编辑文档也不会悄悄改掉字段类型。查询 tab 支持 mongosh 语法子集:`find`、`aggregate`、`updateMany` 等。

![按条件过滤、排序后的 MongoDB 集合与文档详情](docs/screenshots/mongo-collection.png)

![Mongo 查询 tab 中的聚合管道](docs/screenshots/mongo-query.png)

**不容易误操作。** 连接按 local / staging / prod 标色,任何破坏性语句都要你明确确认后才会执行。

<table>
  <tr>
    <td width="50%"><img src="docs/screenshots/new-connection.png" alt="新建连接"></td>
    <td width="50%"><img src="docs/screenshots/destructive-review.png" alt="破坏性语句审查"></td>
  </tr>
</table>

<sub>截图使用一个虚构的咖啡烘焙店数据库,由<a href="#无窗口截图">开发</a>一节介绍的无窗口截图模式渲染。</sub>

## 功能

- **连接管理**:新建/编辑/删除/测试连接,连接串预览;SSL 模式(关闭 / 优先 / 必须 / 校验证书,PG 与 MySQL 的「优先」在服务器不支持时退回明文);按环境(local / staging / prod)着色;凭据按环境路由——`local` 明文存本地、`staging/prod` 存 macOS 钥匙串;侧栏切库(同实例复用或自动克隆连接)、库右键导出/删除
- **表浏览**:服务端分页与排序、列类型分级配色、结果筛选(且/或、多种操作符)、单元格全文查看(JSON 自动格式化)、暂存编辑与删除后 ⌘S 单事务提交、复制 INSERT、导出 CSV / SQL / Markdown
- **SQL 编辑器**:语法高亮、schema 感知补全(表名、`表.` / 别名列、作用域列、子句模板)、关键字自动大写、运行光标所在语句或选区、格式化、Explain、行数上限、查询历史(按库隔离)
- **破坏性语句审查**:DROP / TRUNCATE / 无 WHERE 的 DELETE、UPDATE 等执行前统一确认;Redis 的 FLUSHALL / FLUSHDB 同样拦截;只读执行由数据库强制(PG `default_transaction_read_only`、MySQL `SESSION TRANSACTION READ ONLY`、SQLite `query_only`)
- **MongoDB**:表单或连接串两种连接方式(支持 Atlas `mongodb+srv://`,连接串里的密码会拆出来按凭据规则保存);集合浏览支持 Filter / Sort / Projection、服务端分页与计数、表格 / JSON 视图;在 mongosh 风格的编辑器里整篇编辑、新增、删除文档,所有 BSON 类型(`NumberLong`、`NumberDecimal`、`ISODate`、`ObjectId` 等)往返不变;查询 tab 支持 mongosh 子集(`find` 及 `sort/skip/limit/projection`、`findOne`、`countDocuments`、`distinct`、`aggregate`、`getIndexes`、`insertOne/Many`、`updateOne/Many`、`replaceOne`、`deleteOne/Many`、`show dbs/collections`)和集合名补全;空条件的批量写入、带 `$out` / `$merge` 的聚合都要经过破坏性审查;读操作在服务端限时 60 秒
- **Redis**:浏览 key、按类型展示值、执行任意命令(命令与 key 名补全)、key 详情(类型 / TTL / 大小)
- **AI 助手**:可配置多个服务商(OpenAI 兼容 / Anthropic / Gemini,内置 DeepSeek、通义、Kimi 等预设),面板内切换模型;按连接分会话;自动注入表结构与知识库文档(内置数据库开发规范,可替换);生成的 SQL 以卡片展示,确认后执行或在编辑器中运行
- **关系图**:物理外键 + 按 `xxx_id` 命名推断的逻辑外键,分层布局、拖拽、缩放、搜索高亮、表结构图 / 工程 ER 图切换、导出 PNG
- **导入**:从 .sql 文件或粘贴内容单事务导入(失败整体回滚并定位到第几条)
- **界面**:简体中文 / 繁體中文 / English(默认跟随系统,AI 提示词与内置规范随语言切换)、亮色 / 暗色 / 跟随系统、字体字号设置、可拖动分栏、多标签页、应用内更新

## 安装

从 [Releases](https://github.com/wenlingang/datum/releases) 下载 `Datum-<版本>.dmg`(通用包,Apple Silicon 与 Intel 均可),把 Datum 拖进「应用程序」。

Datum 目前没有经过苹果公证,第一次打开会被系统拦截,任选一种方式放行(只需一次,之后的应用内更新不受影响):

- 打开「系统设置 → 隐私与安全性」,在页面下方找到 Datum 的提示,点「仍要打开」;
- 或在终端执行 `xattr -dr com.apple.quarantine /Applications/Datum.app`。

之后应用会自动检查更新。更新包必须与当前应用用同一张发版证书签名,才会安装。

## 结构

```text
crates/
├── core/   datum-core:连接、凭据、历史、PG/MySQL/SQLite/Redis/MongoDB 驱动、AI、自更新,
│           以及 SQL 切分/风险识别/生成/格式化、筛选排序导出、ER 布局、补全等纯逻辑
└── app/    datum:GPUI Kit 界面(工作区、标签页、结果网格、对话框、AI 面板、关系图)
```

## 环境要求

- **Rust** ≥ 1.88(开发用 1.96 验证)
- **macOS**(当前打包目标;GPUI 本身支持 Linux / Windows)
- 跑数据层集成测试时需要本地可连的 PostgreSQL / Redis / MongoDB(或 Docker)

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

可用环境变量覆盖测试连接:`DBSTUDIO_TEST_PG`(libpq 连接串)、`DBSTUDIO_TEST_MYSQL`(mysql URL)、`DBSTUDIO_TEST_REDIS`(redis URL)。

TLS 用例按端口连本机库,未设置的变量对应用例直接跳过:

```bash
DATUM_TLS_PG_PLAIN=5432 DATUM_TLS_PG_SSL=5444 DATUM_TLS_MYSQL=3306 DATUM_TLS_REDIS=6379 \
  cargo test -p datum-core --test tls -- --ignored --test-threads=1
```

`DATUM_TLS_PG_SSL` 指向开启 `ssl=on`(自签证书即可)的 PG,`DATUM_TLS_PG_PLAIN` / `DATUM_TLS_REDIS` 指向未开加密的实例。

MongoDB 用例连本机指定端口的实例:

```bash
docker run -d --name datum-mongo -p 27017:27017 mongo:7
DATUM_MONGO=27017 cargo test -p datum-core --test mongo -- --ignored --test-threads=1
```

同时设置 `DATUM_MONGO_AUTH=<端口>` 可跑认证相关用例,需要一个带 root 用户启动的实例(准备方式见 `crates/core/tests/mongo.rs`)。

## 打包与发布

### 发版(GitHub Actions)

1. 修改工作区 `Cargo.toml` 的 `version`,运行 `cargo check` 更新 `Cargo.lock`,提交。
2. 可选试跑:Actions › Release › Run workflow,下载产物 `datum-dmg` 试装。
3. 推送同名附注标签,标签说明就是应用内显示的更新说明:

```bash
git tag -a v0.0.2 -m "本次更新内容"
git push origin v0.0.2
```

`.github/workflows/release.yml` 在 macOS 上运行:校验标签与版本一致 → 跑测试 → 构建两种架构(在导入签名证书之前)
→ 签名打包(`script/package.sh`)→ 校验(`script/verify-package.sh`)→ 建 Release,附件为 `Datum-<版本>.dmg`、
它的 `.sha256`、不带版本号的 `Datum.dmg` 和 `latest.json`。版本号带 `-`(如 `v0.1.0-rc1`)时发为预发布,已安装的应用不会更新到它。

### 发版证书

发版用自签名证书 "Datum Release" 签名,存放在 `RELEASE_CERT_P12` / `RELEASE_CERT_PASSWORD` 两个 secret 里。
**不要更换它**:已安装的应用只接受同一张证书签名的更新,钥匙串里对 staging / prod 密码的「始终允许」授权也绑定在它上面。
`script/release-cert.sha1` 钉住它的 SHA-1,用其他证书签名时打包直接失败。

一次性配置:

```bash
script/release-cert.sh        # 证书写到 ~/datum-release-cert/,指纹写到 script/release-cert.sha1
# 备份 datum-release.p12 和 password.txt,提交 script/release-cert.sha1,然后:
gh secret set RELEASE_CERT_P12 < ~/datum-release-cert/datum-release.p12.base64
gh secret set RELEASE_CERT_PASSWORD < ~/datum-release-cert/password.txt
```

### 本地打包与测试

```bash
script/bundle-macos.sh                          # 未签名的 Datum.app,输出到 target/bundle/
script/bundle-macos.sh --universal              # Apple Silicon + Intel 通用包
script/package.sh --identity "<本机签名身份>"     # 签名的 dmg,输出到 target/release-dist/
```

### 应用内更新

应用静默检查 `releases/latest/download/latest.json`(见 `crates/core/src/update.rs`),它由 `script/update-manifest.sh` 生成:

```json
{ "version": "0.0.2", "dmg": "Datum-0.0.2.dmg", "sha256": "<dmg 的 SHA-256>", "size": 12345678, "notes": "本次更新内容" }
```

下载地址由版本号拼出,不取自清单。安装流程:下载 dmg → 校验 SHA-256 → 挂载并把 `Datum.app` 拷到运行中应用旁边
→ 用运行中应用的 designated requirement(钉住发版证书)校验新包签名 → 核对版本 → 改名换入。ad-hoc 签名的应用(本机自打包)不能自更新。

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
