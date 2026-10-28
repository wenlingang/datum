# Datum

[English](README.md) | [简体中文](README.zh-CN.md) | 繁體中文

一款簡約現代的桌面資料庫管理工具,支援 **PostgreSQL**、**MySQL**、**SQLite**、**Redis** 與 **MongoDB**。

這是 Datum 的 Rust 原生重寫版:介面基於 [GPUI Kit](https://github.com/longbridge/gpui-kit)(GPUI + gpui-component),
資料層沿用原 Tauri 版的 Rust 後端,不再依賴 WebView 與 Node.js。

> Datum(/ˈdeɪtəm/,"data" 的單數)—— 專注於資料的工具。

<p align="center">
  <img src="docs/screenshots/table.png" alt="瀏覽資料表,右側展開列詳情" width="100%">
</p>

## 看一眼

**順手地寫 SQL。** 補全認得你的資料表、別名與欄位;⌘↵ 只執行游標所在的那一句。當然也有深色模式。

![SQL 編輯器:彙總查詢及其結果](docs/screenshots/sql-editor.png)

**資料表之間怎麼關聯,一眼看清。** 關聯圖既讀取真實外鍵,也會依 `xxx_id` 命名推斷沒建外鍵的關聯。

![範例商店資料庫的關聯圖](docs/screenshots/er-diagram.png)

**用你習慣的方式查 MongoDB。** 開啟集合就能用 Filter / Sort / Projection 翻看文件,表格與 JSON 一鍵切換,整篇編輯文件也不會悄悄改掉欄位型別。查詢分頁支援 mongosh 語法子集:`find`、`aggregate`、`updateMany` 等。

![依條件篩選、排序後的 MongoDB 集合與文件詳細資訊](docs/screenshots/mongo-collection.png)

![Mongo 查詢分頁中的聚合管線](docs/screenshots/mongo-query.png)

**不容易誤操作。** 連線依 local / staging / prod 標色,任何破壞性語句都要你明確確認後才會執行。

<table>
  <tr>
    <td width="50%"><img src="docs/screenshots/new-connection.png" alt="新增連線"></td>
    <td width="50%"><img src="docs/screenshots/destructive-review.png" alt="破壞性語句審查"></td>
  </tr>
</table>

<sub>截圖使用一個虛構的咖啡烘焙店資料庫,由<a href="#無視窗截圖">開發</a>一節介紹的無視窗截圖模式渲染。</sub>

## 功能

- **連線管理**:新增/編輯/刪除/測試連線,連線字串預覽;SSL 模式(關閉 / 優先 / 必須 / 驗證憑證,PG 與 MySQL 的「優先」在伺服器不支援時退回明文);依環境(local / staging / prod)著色;憑證依環境存放——`local` 明文存於本機、`staging/prod` 存於 macOS 鑰匙圈;側欄切換資料庫(同一執行個體共用或自動複製連線)、資料庫右鍵匯出/刪除
- **資料表瀏覽**:伺服器端分頁與排序、欄位型別分級配色、結果篩選(且/或、多種運算子)、儲存格全文檢視(JSON 自動格式化)、暫存編輯與刪除後 ⌘S 單一交易提交、複製 INSERT、匯出 CSV / SQL / Markdown
- **SQL 編輯器**:語法醒目提示、schema 感知自動完成(資料表名稱、`表.` / 別名欄位、作用域欄位、子句範本)、關鍵字自動大寫、執行游標所在陳述式或選取範圍、格式化、Explain、列數上限、查詢紀錄(依資料庫隔離)
- **破壞性陳述式審查**:DROP / TRUNCATE / 無 WHERE 的 DELETE、UPDATE 等執行前統一確認;Redis 的 FLUSHALL / FLUSHDB 同樣攔截;唯讀執行由資料庫強制(PG `default_transaction_read_only`、MySQL `SESSION TRANSACTION READ ONLY`、SQLite `query_only`)
- **MongoDB**:表單或連線字串兩種連線方式(支援 Atlas `mongodb+srv://`,連線字串中的密碼會拆出來依憑證規則儲存);集合瀏覽支援 Filter / Sort / Projection、伺服器端分頁與計數、表格 / JSON 檢視;在 mongosh 風格的編輯器中整篇編輯、新增、刪除文件,所有 BSON 型別(`NumberLong`、`NumberDecimal`、`ISODate`、`ObjectId` 等)往返不變;查詢分頁支援 mongosh 子集(`find` 及 `sort/skip/limit/projection`、`findOne`、`countDocuments`、`distinct`、`aggregate`、`getIndexes`、`insertOne/Many`、`updateOne/Many`、`replaceOne`、`deleteOne/Many`、`show dbs/collections`)與集合名稱自動完成;空條件的批次寫入、含 `$out` / `$merge` 的聚合都要經過破壞性審查;讀取操作在伺服器端限時 60 秒
- **Redis**:瀏覽 key、依型別顯示值、執行任意命令(命令與 key 名稱自動完成)、key 詳細資訊(型別 / TTL / 大小)
- **AI 助理**:可設定多個服務供應商(OpenAI 相容 / Anthropic / Gemini,內建 DeepSeek、通義、Kimi 等預設),在面板內切換模型;依連線分開對話;自動帶入資料表結構與知識庫文件(內建資料庫開發規範,可替換);產生的 SQL 以卡片呈現,確認後執行或在編輯器中執行
- **關聯圖**:實體外鍵 + 依 `xxx_id` 命名推斷的邏輯外鍵,分層配置、拖曳、縮放、搜尋醒目提示、表結構圖 / 工程 ER 圖切換、匯出 PNG
- **匯入**:從 .sql 檔案或貼上的內容以單一交易匯入(失敗整體復原並定位到第幾條)
- **介面**:English / 简体中文 / 繁體中文(預設跟隨系統,AI 提示詞與內建規範隨語言切換)、淺色 / 深色 / 跟隨系統、字型與字級設定、可拖曳分欄、多分頁、App 內更新

## 安裝

從 [Releases](https://github.com/wenlingang/datum/releases) 下載 `Datum-<版本>.dmg`(通用套件,Apple Silicon 與 Intel 皆可),將 Datum 拖進「應用程式」。

Datum 目前尚未經過 Apple 公證,第一次開啟會被系統阻擋,任選一種方式放行(只需一次,之後的 App 內更新不受影響):

- 開啟「系統設定 → 隱私權與安全性」,在頁面下方找到 Datum 的提示,點選「強制打開」;
- 或在終端機執行 `xattr -dr com.apple.quarantine /Applications/Datum.app`。

之後 App 會自動檢查更新。更新套件必須與目前的 App 以同一張發行憑證簽署,才會安裝。

## 結構

```text
crates/
├── core/   datum-core:連線、憑證、紀錄、PG/MySQL/SQLite/Redis/MongoDB 驅動程式、AI、自動更新,
│           以及 SQL 切分/風險辨識/產生/格式化、篩選排序匯出、ER 配置、自動完成等純邏輯
└── app/    datum:GPUI Kit 介面(工作區、分頁、結果表格、對話框、AI 面板、關聯圖)
```

## 環境需求

- **Rust** ≥ 1.88(開發時以 1.96 驗證)
- **macOS**(目前的封裝目標;GPUI 本身支援 Linux / Windows)
- 執行資料層整合測試時需要本機可連線的 PostgreSQL / Redis / MongoDB(或 Docker)

## 開發

```bash
cargo run                  # 啟動 App
cargo test --workspace     # 單元測試(資料層整合測試預設略過)
```

設定與舊版共用同一目錄(`~/Library/Application Support/com.dbstudio.app`),從 Tauri 版升級後連線、紀錄與 AI 設定可直接使用。
開發時可用 `DATUM_CONFIG_DIR=/某個目錄 cargo run` 指向獨立的設定目錄。

### 無視窗截圖

在沒有螢幕錄製權限的環境(CI、遠端工作階段)中,可以用 GPUI 的 Metal 無頭算繪器執行一段腳本並截圖:

```bash
DATUM_CONFIG_DIR=/tmp/datum-cfg \
DATUM_SNAPSHOT="conn:<連線 id>;wait:600;table:users;wait:500;shot:/tmp/a.png" \
cargo run --features snapshot
```

可用步驟見 `crates/app/src/workspace/mod.rs` 的 `snapshot_step`(`query`、`sql:`、`run`、`detail`、`ai`、`erd`、`settings`、`theme:light`、`key:cmd-enter` 等)。

### 整合測試(需要資料庫)

```bash
cargo test -p datum-core -- --ignored --test-threads=1
```

可用環境變數覆寫測試連線:`DBSTUDIO_TEST_PG`(libpq 連線字串)、`DBSTUDIO_TEST_MYSQL`(mysql URL)、`DBSTUDIO_TEST_REDIS`(redis URL)。

TLS 測試依連接埠連線本機資料庫,未設定的變數對應的測試直接略過:

```bash
DATUM_TLS_PG_PLAIN=5432 DATUM_TLS_PG_SSL=5444 DATUM_TLS_MYSQL=3306 DATUM_TLS_REDIS=6379 \
  cargo test -p datum-core --test tls -- --ignored --test-threads=1
```

`DATUM_TLS_PG_SSL` 指向開啟 `ssl=on`(自簽憑證即可)的 PG,`DATUM_TLS_PG_PLAIN` / `DATUM_TLS_REDIS` 指向未開啟加密的執行個體。

MongoDB 測試連線至本機指定連接埠的執行個體:

```bash
docker run -d --name datum-mongo -p 27017:27017 mongo:7
DATUM_MONGO=27017 cargo test -p datum-core --test mongo -- --ignored --test-threads=1
```

同時設定 `DATUM_MONGO_AUTH=<連接埠>` 可執行驗證相關測試,需要一個帶 root 使用者啟動的執行個體(準備方式見 `crates/core/tests/mongo.rs`)。

## 封裝與發布

### 發版(GitHub Actions)

1. 修改工作區 `Cargo.toml` 的 `version`,執行 `cargo check` 更新 `Cargo.lock`,提交。
2. 可選試跑:Actions › Release › Run workflow,下載產物 `datum-dmg` 試裝。
3. 推送同名附註標籤,標籤說明就是 App 內顯示的更新說明:

```bash
git tag -a v0.0.2 -m "本次更新內容"
git push origin v0.0.2
```

`.github/workflows/release.yml` 在 macOS 上執行:檢查標籤與版本一致 → 跑測試 → 建置兩種架構(在匯入簽署憑證之前)
→ 簽署封裝(`script/package.sh`)→ 驗證(`script/verify-package.sh`)→ 建立 Release,附件為 `Datum-<版本>.dmg`、
它的 `.sha256`、不帶版本號的 `Datum.dmg` 與 `latest.json`。版本號帶 `-`(如 `v0.1.0-rc1`)時發為預發布,已安裝的 App 不會更新到它。

### 發行憑證

發版以自簽憑證 "Datum Release" 簽署,存放在 `RELEASE_CERT_P12` / `RELEASE_CERT_PASSWORD` 兩個 secret 中。
**不要更換它**:已安裝的 App 只接受同一張憑證簽署的更新,鑰匙圈中對 staging / prod 密碼的「永遠允許」授權也綁定在它上面。
`script/release-cert.sha1` 釘住它的 SHA-1,以其他憑證簽署時封裝直接失敗。

一次性設定:

```bash
script/release-cert.sh        # 憑證寫到 ~/datum-release-cert/,指紋寫到 script/release-cert.sha1
# 備份 datum-release.p12 與 password.txt,提交 script/release-cert.sha1,然後:
gh secret set RELEASE_CERT_P12 < ~/datum-release-cert/datum-release.p12.base64
gh secret set RELEASE_CERT_PASSWORD < ~/datum-release-cert/password.txt
```

### 本機封裝與測試

```bash
script/bundle-macos.sh                          # 未簽署的 Datum.app,輸出到 target/bundle/
script/bundle-macos.sh --universal              # Apple Silicon + Intel 通用套件
script/package.sh --identity "<本機簽署身分>"     # 已簽署的 dmg,輸出到 target/release-dist/
```

### App 內更新

App 會靜默檢查 `releases/latest/download/latest.json`(見 `crates/core/src/update.rs`),由 `script/update-manifest.sh` 產生:

```json
{ "version": "0.0.2", "dmg": "Datum-0.0.2.dmg", "sha256": "<dmg 的 SHA-256>", "size": 12345678, "notes": "本次更新內容" }
```

下載網址由版本號組出,不取自清單。安裝流程:下載 dmg → 驗證 SHA-256 → 掛載並把 `Datum.app` 複製到執行中的 App 旁
→ 以執行中 App 的 designated requirement(釘住發行憑證)驗證新套件簽章 → 核對版本 → 改名換入。ad-hoc 簽署的 App(本機自行封裝)無法自動更新。

## 快速鍵

| 快速鍵 | 作用 |
| --- | --- |
| ⌘↵ | 執行游標所在陳述式 / 選取範圍 |
| ⌘⇧F | 格式化 SQL |
| ⌘E | Explain |
| ⌘Y | 查詢紀錄 |
| ⌘⇧A | 將選取的 SQL 傳給 AI 助理 |
| ⌘F | 結果篩選(編輯器內為尋找) |
| ⌘S | 提交暫存的編輯與刪除 |
| ⌘R | 重新整理目前分頁與資料表清單 |
| ⌘T / ⌘W | 新增查詢 / 關閉分頁 |
| ⌘B | 顯示 / 隱藏側欄 |
| ⌘, | 設定 |
