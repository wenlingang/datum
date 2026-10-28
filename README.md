# Datum

English | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md)

A clean, modern desktop database client for **PostgreSQL**, **MySQL**, **SQLite**, **Redis** and **MongoDB**.

This is the native Rust rewrite of Datum: the UI is built on [GPUI Kit](https://github.com/longbridge/gpui-kit) (GPUI + gpui-component),
and the data layer reuses the Rust backend of the original Tauri version — no WebView, no Node.js.

> Datum (/ˈdeɪtəm/, the singular of "data") — a tool focused on your data.

<p align="center">
  <img src="docs/screenshots/table.png" alt="Browsing a table with the row details panel open" width="100%">
</p>

## A look around

**Write SQL the way you think.** Completion knows your tables, aliases and columns; hit ⌘↵ to run just the statement under the cursor. Dark mode included.

![SQL editor with an aggregate query and its results](docs/screenshots/sql-editor.png)

**See how everything connects.** The ER diagram picks up real foreign keys and infers the ones you only expressed through `xxx_id` naming.

![ER diagram of a sample shop schema](docs/screenshots/er-diagram.png)

**MongoDB, the way you already query it.** Open a collection to page through documents with Filter / Sort / Projection, flip between a table and JSON, and edit whole documents without types silently changing. The query tab speaks a mongosh subset — `find`, `aggregate`, `updateMany` and friends.

![A MongoDB collection filtered and sorted, with the document details panel](docs/screenshots/mongo-collection.png)

![An aggregation pipeline in the Mongo query tab](docs/screenshots/mongo-query.png)

**Hard to shoot yourself in the foot.** Connections are tagged local / staging / prod, and anything destructive waits for an explicit confirmation.

<table>
  <tr>
    <td width="50%"><img src="docs/screenshots/new-connection.png" alt="New connection dialog"></td>
    <td width="50%"><img src="docs/screenshots/destructive-review.png" alt="Destructive statement review"></td>
  </tr>
</table>

<sub>Screenshots use a made-up coffee roastery database and were rendered with the headless snapshot mode described under <a href="#headless-screenshots">Development</a>.</sub>

## Features

- **Connections**: create / edit / delete / test connections with a live connection-string preview; SSL mode (disable / prefer / require / verify-full — for PG and MySQL, "prefer" falls back to plaintext when the server has no TLS); color-coded environments (local / staging / prod); credentials routed by environment — `local` stored in plain text on disk, `staging/prod` in the macOS Keychain; switch databases from the sidebar (reuses the same instance or clones the connection), export / drop a database from its context menu
- **Table browsing**: server-side pagination and sorting, column colors by type, result filters (AND / OR, many operators), full-cell viewer (JSON auto-formatted), staged edits and deletes committed in a single transaction with ⌘S, copy as INSERT, export to CSV / SQL / Markdown
- **SQL editor**: syntax highlighting, schema-aware completion (tables, `table.` / alias columns, in-scope columns, clause templates), keyword auto-uppercasing, run the statement under the cursor or the selection, formatting, Explain, row limit, per-database query history
- **Destructive statement review**: DROP / TRUNCATE / DELETE or UPDATE without WHERE, etc. require confirmation before running; Redis FLUSHALL / FLUSHDB are intercepted too; read-only execution is enforced by the database itself (PG `default_transaction_read_only`, MySQL `SESSION TRANSACTION READ ONLY`, SQLite `query_only`)
- **MongoDB**: connect with fields or a connection string (incl. Atlas `mongodb+srv://`; the password is split out and stored like other credentials); browse collections with Filter / Sort / Projection, server-side paging and counts, table or JSON view; edit, insert and delete whole documents in a mongosh-style editor that round-trips every BSON type (`NumberLong`, `NumberDecimal`, `ISODate`, `ObjectId`, …); a query tab for a mongosh subset (`find` with `sort/skip/limit/projection`, `findOne`, `countDocuments`, `distinct`, `aggregate`, `getIndexes`, `insertOne/Many`, `updateOne/Many`, `replaceOne`, `deleteOne/Many`, `show dbs/collections`) with collection-name completion; empty-filter bulk writes and `$out` / `$merge` pipelines go through the destructive review; reads are capped at 60 s server-side
- **Redis**: browse keys, type-aware value views, run any command (with command and key-name completion), key details (type / TTL / size)
- **AI assistant**: multiple providers (OpenAI-compatible / Anthropic / Gemini, with presets for DeepSeek, Qwen, Kimi and more), switch models inside the panel; per-connection conversations; table schemas and knowledge-base documents injected automatically (ships with database development guidelines, replaceable); generated SQL is shown as cards you confirm before running or open in the editor
- **ER diagram**: physical foreign keys plus logical ones inferred from `xxx_id` naming, layered layout, drag, zoom, search highlighting, schema view / engineering ER view, PNG export
- **Import**: import from a .sql file or pasted text in a single transaction (rolls back entirely on failure and points to the failing statement)
- **Interface**: English / 简体中文 / 繁體中文 (follows the system by default; AI prompts and the built-in guidelines switch with the language), light / dark / system theme, font and size settings, resizable panes, tabs, in-app updates

## Install

Download `Datum-<version>.dmg` (universal: Apple Silicon + Intel) from [Releases](https://github.com/wenlingang/datum/releases) and drag Datum into Applications.

Datum is not notarized by Apple yet, so macOS blocks it on first launch. Allow it once using either method (in-app updates are not affected afterwards):

- Open "System Settings → Privacy & Security", find the Datum notice near the bottom and click "Open Anyway";
- Or run `xattr -dr com.apple.quarantine /Applications/Datum.app` in Terminal.

The app then checks for updates automatically. An update is installed only if it is signed with the same release certificate as the running app.

## Layout

```text
crates/
├── core/   datum-core: connections, credentials, history, PG/MySQL/SQLite/Redis/MongoDB drivers, AI, self-update,
│           plus pure logic for SQL splitting / risk detection / generation / formatting,
│           filtering, sorting, export, ER layout and completion
└── app/    datum: GPUI Kit UI (workspace, tabs, result grid, dialogs, AI panel, ER diagram)
```

## Requirements

- **Rust** ≥ 1.88 (developed and verified with 1.96)
- **macOS** (current packaging target; GPUI itself supports Linux / Windows)
- Data-layer integration tests need reachable local PostgreSQL / Redis / MongoDB instances (or Docker)

## Development

```bash
cargo run                  # launch the app
cargo test --workspace     # unit tests (data-layer integration tests are ignored by default)
```

Configuration lives in the same directory as the old version (`~/Library/Application Support/com.dbstudio.app`), so connections, history and AI settings carry over when upgrading from the Tauri version.
Use `DATUM_CONFIG_DIR=/some/dir cargo run` to point development builds at a separate config directory.

### Headless screenshots

Where screen recording is unavailable (CI, remote sessions), GPUI's Metal headless renderer can run a script and take a screenshot:

```bash
DATUM_CONFIG_DIR=/tmp/datum-cfg \
DATUM_SNAPSHOT="conn:<connection id>;wait:600;table:users;wait:500;shot:/tmp/a.png" \
cargo run --features snapshot
```

See `snapshot_step` in `crates/app/src/workspace/mod.rs` for available steps (`query`, `sql:`, `run`, `detail`, `ai`, `erd`, `settings`, `theme:light`, `key:cmd-enter`, …).

### Integration tests (need databases)

```bash
cargo test -p datum-core -- --ignored --test-threads=1
```

Override the test connections with `DBSTUDIO_TEST_PG` (libpq connection string), `DBSTUDIO_TEST_MYSQL` (mysql URL) and `DBSTUDIO_TEST_REDIS` (redis URL).

The TLS tests connect to local databases by port; tests whose variable is unset are skipped:

```bash
DATUM_TLS_PG_PLAIN=5432 DATUM_TLS_PG_SSL=5444 DATUM_TLS_MYSQL=3306 DATUM_TLS_REDIS=6379 \
  cargo test -p datum-core --test tls -- --ignored --test-threads=1
```

`DATUM_TLS_PG_SSL` points to a PG with `ssl=on` (a self-signed certificate is fine); `DATUM_TLS_PG_PLAIN` / `DATUM_TLS_REDIS` point to instances without TLS.

The MongoDB tests run against a local instance on the given port:

```bash
docker run -d --name datum-mongo -p 27017:27017 mongo:7
DATUM_MONGO=27017 cargo test -p datum-core --test mongo -- --ignored --test-threads=1
```

Set `DATUM_MONGO_AUTH=<port>` as well to run the authentication cases against an instance started with a root user (see `crates/core/tests/mongo.rs` for the setup).

## Packaging and releases

### Releasing (GitHub Actions)

1. Bump `version` in the workspace `Cargo.toml`, run `cargo check` to update `Cargo.lock`, and commit.
2. Optional dry run: Actions › Release › Run workflow, then download the `datum-dmg` artifact.
3. Push an annotated tag with the same name. The tag message becomes the in-app release notes:

```bash
git tag -a v0.0.2 -m "What changed in this release"
git push origin v0.0.2
```

`.github/workflows/release.yml` runs on macOS: check the tag matches the version → run tests → build both architectures
(before the signing certificate is imported) → sign and package (`script/package.sh`) → verify (`script/verify-package.sh`)
→ create a Release with `Datum-<version>.dmg`, its `.sha256`, a versionless `Datum.dmg` and `latest.json`.
A version with a `-` (e.g. `v0.1.0-rc1`) is published as a pre-release, which installed copies never update to.

### Release certificate

Releases are signed with the self-signed certificate "Datum Release", stored in the `RELEASE_CERT_P12` / `RELEASE_CERT_PASSWORD` secrets.
**Never replace it**: installed copies only accept updates signed with the same certificate, and Keychain "Always Allow" grants
(staging / prod passwords) are tied to it. `script/release-cert.sha1` pins its SHA-1; packaging fails if the app is signed with any other certificate.

One-time setup:

```bash
script/release-cert.sh        # writes the certificate to ~/datum-release-cert/ and the pin to script/release-cert.sha1
# back up datum-release.p12 and password.txt, commit script/release-cert.sha1, then:
gh secret set RELEASE_CERT_P12 < ~/datum-release-cert/datum-release.p12.base64
gh secret set RELEASE_CERT_PASSWORD < ~/datum-release-cert/password.txt
```

### Local packaging and testing

```bash
script/bundle-macos.sh                               # unsigned Datum.app in target/bundle/
script/bundle-macos.sh --universal                   # universal Apple Silicon + Intel build
script/package.sh --identity "<your signing identity>"   # signed dmg in target/release-dist/
```

### In-app updates

The app silently checks `releases/latest/download/latest.json` (see `crates/core/src/update.rs`), written by `script/update-manifest.sh`:

```json
{ "version": "0.0.2", "dmg": "Datum-0.0.2.dmg", "sha256": "<SHA-256 of the dmg>", "size": 12345678, "notes": "What changed" }
```

The download URL is derived from the version, not taken from the manifest. Installing an update: download the dmg → check its SHA-256 →
mount it and copy `Datum.app` next to the running app → verify its signature against the running app's designated requirement
(which pins the release certificate) → check its version → swap the bundles. Copies that are ad-hoc signed (local builds) cannot self-update.

## Keyboard shortcuts

| Shortcut | Action |
| --- | --- |
| ⌘↵ | Run the statement under the cursor / the selection |
| ⌘⇧F | Format SQL |
| ⌘E | Explain |
| ⌘Y | Query history |
| ⌘⇧A | Send selected SQL to the AI assistant |
| ⌘F | Filter results (find inside the editor) |
| ⌘S | Commit staged edits and deletes |
| ⌘R | Refresh the current tab and table list |
| ⌘T / ⌘W | New query / close tab |
| ⌘B | Show / hide the sidebar |
| ⌘, | Settings |
