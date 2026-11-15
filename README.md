# Datum

English | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md)

A lightweight, fast-starting desktop database client for **PostgreSQL**, **MySQL**, **SQLite** and **Redis**, on **macOS**, **Windows** and **Linux**.

**👉 Overview, screenshots, features and downloads: [wenlingang.github.io/datum](https://wenlingang.github.io/datum/)**

<p align="center">
  <a href="https://wenlingang.github.io/datum/"><img src="docs/screenshots/sql-editor.png" alt="Datum" width="100%"></a>
</p>

## Screenshots

<table>
  <tr>
    <td width="50%" valign="top"><img src="docs/screenshots/erd.png" alt="Relationship diagram: every table and foreign key, including ones inferred from column names"><br><sub>Relationship diagram: every table and foreign key, including ones inferred from column names</sub></td>
    <td width="50%" valign="top"><img src="docs/screenshots/jsonb.png" alt="PostgreSQL JSONB: click a field in the tree to filter the whole table on it"><br><sub>PostgreSQL JSONB: click a field in the tree to filter the whole table on it</sub></td>
  </tr>
  <tr>
    <td width="50%" valign="top"><img src="docs/screenshots/mysql-alter.png" alt="MySQL: see whether an ALTER TABLE copies the table and blocks writes before it runs"><br><sub>MySQL: see whether an ALTER TABLE copies the table and blocks writes before it runs</sub></td>
    <td width="50%" valign="top"><img src="docs/screenshots/sqlite-alter.png" alt="SQLite: change columns in a form; Datum writes the table rebuild"><br><sub>SQLite: change columns in a form; Datum writes the table rebuild</sub></td>
  </tr>
  <tr>
    <td width="50%" valign="top"><img src="docs/screenshots/redis.png" alt="Redis: edit hashes, lists, sets, sorted sets and streams as tables"><br><sub>Redis: edit hashes, lists, sets, sorted sets and streams as tables</sub></td>
    <td width="50%" valign="top"><img src="docs/screenshots/diagnostics.png" alt="Diagnostics: sessions, lock waits, slow SQL and unused indexes"><br><sub>Diagnostics: sessions, lock waits, slow SQL and unused indexes</sub></td>
  </tr>
</table>

## Download

Get the package for your platform from [Releases](https://github.com/wenlingang/datum/releases/latest); installation notes are on [the website](https://wenlingang.github.io/datum/#install).

## Development

The UI is written in TSX on [nsh](https://nsh.dev) and drawn with each platform's native views; the data layer is Rust (`crates/core`), exposed to the UI through the native module in `app/native`.

```text
crates/core/   data layer: connections, credentials, history, PG/MySQL/SQLite/Redis drivers, AI, SQL risk checks, formatting…
app/           the nsh app: App.tsx, nsh.json, theme.json, locales/
  native/      the datum/native native module (Rust)
  workspace/   shell   tab/ query & table tabs   dialogs/   chat/ AI panel   ui/ store/ lib/
site/          the website (GitHub Pages)
```

You need Rust ≥ 1.97 and the nsh CLI, built from the same commit as the nsh crates pinned in `app/native/Cargo.toml` (install it with `script/install-nsh.sh`).

```bash
cd app
nsh check && nsh build                     # type check and build
DATUM_CONFIG_DIR=/tmp/datum-dev nsh run    # run with a separate config dir, away from your real connections
cd .. && app/testing/run-tests.sh core     # core tests
```

Testing, packaging, releases, certificates and keys: see [docs/release.md](docs/release.md) (Simplified Chinese).
