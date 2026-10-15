# Datum

[English](README.md) | [简体中文](README.zh-CN.md) | 繁體中文

一款簡約現代的桌面資料庫管理工具,支援 **PostgreSQL**、**MySQL**、**SQLite** 與 **Redis**。

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
- **Redis**:瀏覽 key、依型別顯示值、執行任意命令(命令與 key 名稱自動完成)、key 詳細資訊(型別 / TTL / 大小)
- **AI 助理**:可設定多個服務供應商(OpenAI 相容 / Anthropic / Gemini,內建 DeepSeek、通義、Kimi 等預設),在面板內切換模型;依連線分開對話;自動帶入資料表結構與知識庫文件(內建資料庫開發規範,可替換);產生的 SQL 以卡片呈現,確認後執行或在編輯器中執行
- **關聯圖**:實體外鍵 + 依 `xxx_id` 命名推斷的邏輯外鍵,分層配置、拖曳、縮放、搜尋醒目提示、表結構圖 / 工程 ER 圖切換、匯出 PNG
- **匯入**:從 .sql 檔案或貼上的內容以單一交易匯入(失敗整體復原並定位到第幾條)
- **介面**:English / 简体中文 / 繁體中文(預設跟隨系統,AI 提示詞與內建規範隨語言切換)、淺色 / 深色 / 跟隨系統、字型與字級設定、可拖曳分欄、多分頁、App 內更新

## 安裝

從 [Releases](https://github.com/wenlingang/datum/releases) 下載 `Datum_<版本>_aarch64.dmg`,將 Datum 拖進「應用程式」。

Datum 目前尚未經過 Apple 公證,第一次開啟會被系統阻擋,任選一種方式放行(只需一次,之後的 App 內更新不受影響):

- 開啟「系統設定 → 隱私權與安全性」,在頁面下方找到 Datum 的提示,點選「強制打開」;
- 或在終端機執行 `xattr -dr com.apple.quarantine /Applications/Datum.app`。

之後 App 會自動檢查更新。更新套件帶有 Ed25519 簽章,通過驗證才會安裝。

## 結構

```text
crates/
├── core/   datum-core:連線、憑證、紀錄、PG/MySQL/SQLite/Redis 驅動程式、AI、自動更新,
│           以及 SQL 切分/風險辨識/產生/格式化、篩選排序匯出、ER 配置、自動完成等純邏輯
└── app/    datum:GPUI Kit 介面(工作區、分頁、結果表格、對話框、AI 面板、關聯圖)
```

## 環境需求

- **Rust** ≥ 1.88(開發時以 1.96 驗證)
- **macOS**(目前的封裝目標;GPUI 本身支援 Linux / Windows)
- 執行資料層整合測試時需要本機可連線的 PostgreSQL / Redis(或 Docker)

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

## 封裝與發布

### 發版(GitHub Actions)

修改版本號並提交後,推送一個同名的附註標籤即可,說明文字會作為 App 內的更新說明:

```bash
# 將 Cargo.toml 的 version 改為 0.0.2 並提交後
git tag -a v0.0.2 -m "本次更新內容"
git push origin v0.0.2
```

`.github/workflows/release.yml` 會在 macOS 上依序:檢查標籤與版本號一致 → 執行測試 → 建置 → 程式碼簽署 →
(有 Apple 憑證時)公證 → 簽署更新套件 → 在本儲存庫建立 Release(附件:dmg、自動更新 zip、`latest.json`)。
也可以在 Actions 頁面手動執行(選擇既有標籤,可勾選建置 Apple Silicon + Intel 通用套件)。

儲存庫 Settings → Secrets and variables → Actions 需要設定:

| Secret | 必要 | 內容 |
| --- | --- | --- |
| `UPDATE_SIGNING_KEY` | 是 | 更新套件簽章私鑰(見下方「金鑰」) |
| `MACOS_CERT_P12` | 否 | 程式碼簽署憑證(含私鑰)的 .p12,base64 編碼;未設定則使用 ad-hoc 簽署 |
| `MACOS_CERT_PASSWORD` | 否 | 上述 .p12 的密碼 |
| `NOTARY_API_KEY` / `NOTARY_KEY_ID` / `NOTARY_ISSUER` | 否 | App Store Connect API Key(.p8 全文、Key ID、Issuer ID),僅 Developer ID 憑證可公證 |

### 金鑰

**更新簽章金鑰**(必要)。App 內建公鑰(`crates/core/src/update.rs` 的 `UPDATE_PUBLIC_KEY`),只安裝以對應私鑰簽章的更新套件。
私鑰遺失將無法再推送更新給已安裝的使用者,務必備份;外洩則需要更換金鑰並請使用者手動重新安裝。

```bash
cargo run -p datum-core --bin datum-sign -- keygen ~/.config/datum/update-signing.key   # 輸出公鑰
gh secret set UPDATE_SIGNING_KEY < ~/.config/datum/update-signing.key
```

**程式碼簽署憑證**(建議)。沒有 Apple 開發者帳號時,使用自簽的程式碼簽署憑證:簽署身分固定,使用者對鑰匙圈
(staging / prod 連線的密碼)的「永遠允許」在更新後仍然有效;ad-hoc 簽署則每次更新都要重新授權。

```bash
openssl req -x509 -newkey rsa:3072 -nodes -days 3650 -keyout codesign.key -out codesign.crt \
  -subj "/CN=Datum Self-Signed Code Signing" -addext "basicConstraints=critical,CA:false" \
  -addext "keyUsage=critical,digitalSignature" -addext "extendedKeyUsage=critical,codeSigning"
openssl pkcs12 -export -legacy -inkey codesign.key -in codesign.crt \
  -name "Datum Self-Signed Code Signing" -out codesign.p12   # 需 OpenSSL 3;-legacy 讓 macOS 能匯入
base64 -i codesign.p12 | gh secret set MACOS_CERT_P12
gh secret set MACOS_CERT_PASSWORD   # 輸入匯出時設定的密碼
```

日後有了 Apple 開發者帳號,將 `MACOS_CERT_P12` 換成 Developer ID 憑證並設定公證 Secrets 即可,使用者無需重新安裝。

### 本機封裝與自我測試

```bash
script/bundle-macos.sh               # 未簽署的 Datum.app
script/bundle-macos.sh --universal   # Apple Silicon + Intel 通用套件
script/release-macos.sh              # ad-hoc 簽署 + 更新簽章 + latest.json(讀取 ~/.config/datum/update-signing.key)
DATUM_SIGN_IDENTITY="…" script/release-macos.sh --notes "…" --publish   # CI 無法使用時手動發版
```

產出物在 `target/bundle/`。App 啟動後會在背景檢查本儲存庫最新 Release 附件中的 `latest.json`
(`UPDATE_MANIFEST_URL`,見 `crates/core/src/update.rs`):

```jsonc
{
  "version": "0.0.1",
  "url": "<DMG 下載直接連結>",
  "zip_url": "<.app zip 下載直接連結>",
  "sha256": "<zip 的 SHA-256>",
  "signature": "<zip 的 Ed25519 簽章>",
  "notes": "本次更新內容"
}
```

App 內更新依序檢查:SHA-256 與清單一致、Ed25519 簽章有效、新套件與目前 App 的 Bundle ID 一致、
目前 App 有 Developer ID 簽署時新套件也須完整簽署且屬於同一開發者團隊,全部通過才原地替換。

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
