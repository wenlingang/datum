# Datum

English | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md)

A clean, modern desktop database client for **PostgreSQL**, **MySQL**, **SQLite** and **Redis**, on **macOS**, **Windows** and **Linux**.

The UI is written in TSX on [nsh](https://nsh.dev) (Native Shell), which draws it with each platform's own widgets — AppKit on macOS,
Win32 / Direct2D on Windows, GTK 4 on Linux. The data layer is Rust (`crates/core`), exposed to the UI through a native module. No WebView, no Node.js.

> Datum (/ˈdeɪtəm/, the singular of "data") — a tool focused on your data.

<p align="center">
  <img src="docs/screenshots/table.png" alt="Browsing a table with the row details panel open" width="100%">
</p>

## A look around

**Write SQL the way you think.** Completion knows your tables, aliases and columns; hit ⌘↵ to run just the statement under the cursor. Dark mode included.

![SQL editor with an aggregate query and its results](docs/screenshots/sql-editor.png)

**See how everything connects.** The ER diagram picks up real foreign keys and infers the ones you only expressed through `xxx_id` naming. (Not in the current version yet: it is still being ported to the new UI.)

![ER diagram of a sample shop schema](docs/screenshots/er-diagram.png)

**Hard to shoot yourself in the foot.** Connections are tagged local / staging / prod, and anything destructive waits for an explicit confirmation.

<table>
  <tr>
    <td width="50%"><img src="docs/screenshots/new-connection.png" alt="New connection dialog"></td>
    <td width="50%"><img src="docs/screenshots/destructive-review.png" alt="Destructive statement review"></td>
  </tr>
</table>

<sub>Screenshots use a made-up coffee roastery database and were taken on macOS with the earlier build (v0.0.1).</sub>

## Features

- **Connections**: create / edit / delete / test connections with a live connection-string preview; SSL mode (disable / prefer / require / verify-full — for PG and MySQL, "prefer" falls back to plaintext when the server has no TLS); color-coded environments (local / staging / prod); credentials routed by environment — `local` stored in plain text on disk, `staging/prod` in the system keychain (macOS Keychain / Windows Credential Manager / Linux Secret Service); switch databases from the sidebar (reuses the same instance or clones the connection), export / drop a database from its context menu
- **Table browsing**: server-side pagination and sorting, column colors by type, result filters (AND / OR, many operators), full-cell viewer (JSON auto-formatted), staged edits and deletes committed in a single transaction with ⌘S, copy as INSERT, export to CSV / SQL / Markdown
- **SQL editor**: syntax highlighting, schema-aware completion (tables, `table.` / alias columns, in-scope columns, clause templates), keyword auto-uppercasing, run the statement under the cursor or the selection, formatting, Explain, row limit, per-database query history
- **Destructive statement review**: DROP / TRUNCATE / DELETE or UPDATE without WHERE, etc. require confirmation before running; Redis FLUSHALL / FLUSHDB are intercepted too; read-only execution is enforced by the database itself (PG `default_transaction_read_only`, MySQL `SESSION TRANSACTION READ ONLY`, SQLite `query_only`)
- **Redis**: browse keys, type-aware value views, run any command (with command and key-name completion), key details (type / TTL / size)
- **AI assistant**: multiple providers (OpenAI-compatible / Anthropic / Gemini, with presets for DeepSeek, Qwen, Kimi and more), switch models inside the panel; per-connection conversations; table schemas and knowledge-base documents injected automatically (ships with database development guidelines, replaceable); generated SQL is shown as cards you confirm before running or open in the editor
- **ER diagram** (not available yet — opening it shows a placeholder while it is being ported): physical foreign keys plus logical ones inferred from `xxx_id` naming, layered layout, drag, zoom, search highlighting, schema view / engineering ER view, PNG export
- **Import**: import from a .sql file or pasted text in a single transaction (rolls back entirely on failure and points to the failing statement)
- **Interface**: English / 简体中文 / 繁體中文 (follows the system by default; AI prompts and the built-in guidelines switch with the language), light / dark / system theme, font and size settings, resizable panes, tabs, in-app updates

MongoDB is no longer supported. Mongo connections saved by earlier versions stay in the config file but are hidden.

## Install

Download from [Releases](https://github.com/wenlingang/datum/releases):

- **macOS** 14 or later: `Datum-<version>-macos-aarch64.dmg` (Apple Silicon) or `Datum-<version>-macos-x86_64.dmg` (Intel), then drag Datum into Applications.
- **Windows** 10 1809 or later / Windows 11: run `Datum-setup.exe`, or unzip `Datum-<version>-windows-x86_64.zip`.
- **Linux** (x86_64, GTK 4.14 or later and WebKitGTK 6.0): unpack `Datum-<version>-linux-x86_64.tar.gz` and run `Datum/Datum`. The folder also has a `.desktop` entry and the icon, should you want it in your launcher.

Datum is not notarized by Apple yet, so macOS blocks it on first launch. Allow it once using either method (in-app updates are not affected afterwards):

- Open "System Settings → Privacy & Security", find the Datum notice near the bottom and click "Open Anyway";
- Or run `xattr -dr com.apple.quarantine /Applications/Datum.app` in Terminal.

The Windows installer is not code-signed either: if SmartScreen stops it, click "More info" → "Run anyway".

The app then checks for updates automatically (Settings › About & Updates). Copies of v0.0.1 cannot update themselves to 0.1 — download the new version once by hand.

## Layout

```text
crates/
└── core/        datum-core: connections, credentials, history, PG/MySQL/SQLite/Redis drivers, AI,
                 plus pure logic for SQL splitting / risk detection / generation / formatting,
                 filtering, sorting, export, ER layout and completion
                 (the old MongoDB driver is still here, but the app no longer uses it)
app/             the nsh app (entry App.tsx, nsh.json, theme.json, locales/)
├── native/      datum-native: Rust crate exposing datum-core to the UI as the `datum/native` module
├── lib/         typed wrappers for datum/native and the shapes that cross the boundary
├── store/       global state (prefs, connections, history, AI, updates, …)
├── ui/          styled building blocks shared by every page
├── workspace/   shell: top bar, connection rail, sidebar, tab strip, status bar
├── tab/         query / table tab: editor, result grid, toolbar, filters, history, cell editing
├── dialogs/     connection form, settings, import, risk review
├── chat/        AI assistant panel
└── testing/     run-tests.sh and its fixtures
```

## Requirements

- **Rust** ≥ 1.97 (nsh's minimum)
- The **nsh CLI**, built from the same commit as the nsh crates pinned in `app/native/Cargo.toml` (the host and the CLI must match): `script/install-nsh.sh` installs it
- On Linux, GTK 4 development files and clang (`libgtk-4-dev clang` on Debian / Ubuntu); on Windows, Visual Studio Build Tools with the C++ workload and the Windows SDK. See the nsh docs for each platform
- `sqlite3` for the UI tests
- Data-layer integration tests need reachable local PostgreSQL / MySQL / Redis instances (or Docker)

## Development

Run nsh commands from `app/`:

```bash
cd app
nsh check                                  # type-check the TSX
nsh build                                  # build; read every warning
nsh test                                   # UI tests
DATUM_CONFIG_DIR=/tmp/datum-dev nsh run    # launch the app against a separate config directory
```

```bash
app/testing/run-tests.sh          # every UI test, each block in its own temporary config directory
app/testing/run-tests.sh tab      # one block: workspace | tab | dialogs | chat | rust
cargo test --workspace            # datum-core unit tests (data-layer integration tests are ignored by default)
```

Configuration lives in `com.dbstudio.app` under the system config directory (`~/Library/Application Support` on macOS,
`%APPDATA%` on Windows, `~/.config` on Linux), shared with earlier versions. `DATUM_CONFIG_DIR` points a run at another directory,
so development and tests never touch your real connections.

The design notes for the nsh rewrite are in `docs/superpowers/specs/2026-11-10-nsh-rewrite-design.md`.

### Integration tests (need databases)

```bash
cargo test -p datum-core --lib -- --ignored --test-threads=1
```

Override the test connections with `DBSTUDIO_TEST_PG` (libpq connection string), `DBSTUDIO_TEST_MYSQL` (`host:port:user:password:db`) and `DBSTUDIO_TEST_REDIS` (redis URL).

Read-only execution against PG / MySQL (SQLite runs without these):

```bash
DATUM_TEST_PG_DB=<db> DATUM_TEST_MYSQL_DB=<db> cargo test -p datum-core --test read_only -- --ignored
```

The TLS tests connect to local databases by port; tests whose variable is unset are skipped:

```bash
DATUM_TLS_PG_PLAIN=5432 DATUM_TLS_PG_SSL=5444 DATUM_TLS_MYSQL=3306 DATUM_TLS_REDIS=6379 \
  cargo test -p datum-core --test tls -- --ignored --test-threads=1
```

`DATUM_TLS_PG_SSL` points to a PG with `ssl=on` (a self-signed certificate is fine); `DATUM_TLS_PG_PLAIN` / `DATUM_TLS_REDIS` point to instances without TLS.

## Packaging and releases

### Releasing (GitHub Actions)

1. Bump `version` in `app/nsh.json` (the only source of the version number; `script/app-version.sh` prints it) and commit.
2. Optional dry run: Actions › Release › Run workflow. It packages without publishing; download the artifacts to try them.
3. Push an annotated tag `v<version>`. The tag message becomes the in-app release notes:

```bash
git tag -a v0.1.0 -m "What changed in this release"
git push origin v0.1.0
```

`.github/workflows/release.yml`: check the tag matches the version → run tests (`cargo test`, `nsh check` / `nsh build` with no warnings,
`app/testing/run-tests.sh`) → `nsh package` on macOS (aarch64 and x86_64), Windows and Linux runners → on macOS, re-sign with the release
certificate and build the dmg (`script/sign-macos.sh`) → sign `latest.json` with `nsh update-manifest` → create the Release.

Release assets:

| File | Purpose |
| --- | --- |
| `Datum-<version>-macos-aarch64.tar.gz` / `-x86_64.tar.gz` | macOS update packages (downloaded by in-app updates) |
| `Datum-<version>-macos-aarch64.dmg` / `-x86_64.dmg` | macOS first install |
| `Datum.dmg` | versionless copy of the Apple Silicon dmg, so the site's download link never goes stale |
| `Datum-<version>-windows-x86_64.zip`, `Datum-setup.exe` | Windows update package and installer |
| `Datum-<version>-linux-x86_64.tar.gz` | Linux |
| `latest.json`, `latest.json.sig` | update manifest and its signature |
| `SHA256SUMS` | checksums |

A version with a `-` (e.g. `v0.2.0-rc1`) is published as a pre-release, which installed copies never update to.

Repository secrets (Settings → Secrets and variables → Actions):

- `RELEASE_CERT_P12` / `RELEASE_CERT_PASSWORD`: the self-signed macOS release certificate "Datum Release"
- `UPDATE_SIGNING_KEY`: the private key that signs the update manifest (contents of the `update.secret` written by `nsh keygen`)

### Release certificate (macOS)

macOS builds are re-signed with the self-signed certificate "Datum Release". **Never replace it**: Keychain "Always Allow" grants
(staging / prod passwords) follow the signature, so a new certificate would make every user grant access again.
`script/release-cert.sha1` pins its SHA-1; `script/sign-macos.sh` refuses to sign with any other certificate.

One-time setup:

```bash
script/release-cert.sh        # writes the certificate to ~/datum-release-cert/ and the pin to script/release-cert.sha1
# back up datum-release.p12 and password.txt, commit script/release-cert.sha1, then:
gh secret set RELEASE_CERT_P12 < ~/datum-release-cert/datum-release.p12.base64
gh secret set RELEASE_CERT_PASSWORD < ~/datum-release-cert/password.txt
```

### Update signing key

Keep the key outside the repository (`~/datum-update-key/update.secret`); its public half is `updater.publicKey` in `app/nsh.json`,
and installed copies refuse manifests not signed by it.

```bash
gh secret set UPDATE_SIGNING_KEY < ~/datum-update-key/update.secret
```

### Local packaging

nsh packages for the platform it runs on, from `app/`:

```bash
cd app
nsh package --target macos       # or windows / linux; output in app/.nsh/out/
../script/sign-macos.sh .nsh/out aarch64   # optional, macOS: re-sign with "Datum Release" and build the dmg
```

`nsh package` signs the macOS app ad hoc, which is fine for trying it locally.

### In-app updates

Updates go through `nsh/updater` (see `app/store/update.ts`): `nsh.json`'s `updater` points at this GitHub repository and carries the
public key. A packaged app checks 10 seconds after launch and then every 4 hours for `latest.json` in the latest Release, verifies its
signature, then downloads the archive for its platform, checks its size and SHA-256, replaces itself and restarts. Development runs
(`nsh run`) never update. This is not compatible with the updater in v0.0.1, whose users need to download the new version once by hand.

## Keyboard shortcuts

On Windows and Linux, use Ctrl in place of ⌘.

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
