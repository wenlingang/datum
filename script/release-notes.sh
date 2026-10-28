#!/usr/bin/env bash
# 打印 GitHub Release 说明的固定部分(安装步骤、校验和),gh 生成的变更列表追加在后面。
# 用法:script/release-notes.sh <版本号> <sha256>
set -euo pipefail
version=${1:?用法: release-notes.sh <版本号> <sha256>}
sha=${2:?}
cat <<EOT
## Install

1. Download \`Datum-$version.dmg\` and drag **Datum** into Applications. Do not run it from inside the dmg.
2. Datum is not notarized by Apple, so macOS blocks the first launch. Either:
   - open System Settings › Privacy & Security and click **Open Anyway** at the bottom, or
   - before opening the dmg, run \`xattr -d com.apple.quarantine ~/Downloads/Datum-$version.dmg\` in Terminal.
3. Later versions install themselves: Settings › About › Check for Updates. Keychain access you granted
   ("Always Allow") is kept across updates.

中文安装说明:[README.zh-CN.md](https://github.com/wenlingang/datum/blob/main/README.zh-CN.md)

Universal build (Apple Silicon + Intel).

SHA-256 (\`Datum-$version.dmg\`): \`$sha\`
EOT
