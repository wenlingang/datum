#!/usr/bin/env bash
# 打印 latest.json:应用内更新器的发布清单,作为附件上传到每个 GitHub Release。
# 应用从 releases/latest/download/latest.json 读取它(见 crates/core/src/update.rs)。
# 用法:script/update-manifest.sh <版本号> <sha256> <字节数> [<更新说明文件>]
set -euo pipefail
version=${1:?用法: update-manifest.sh <版本号> <sha256> <字节数> [<更新说明文件>]}
sha=${2:?}
size=${3:?}
notes_file=${4:-}
notes=""
if [[ -n $notes_file ]]; then
  notes=$(<"$notes_file")
fi
jq -n --arg version "$version" --arg dmg "Datum-$version.dmg" --arg sha256 "$sha" \
  --argjson size "$size" --arg notes "$notes" \
  '{version: $version, dmg: $dmg, sha256: $sha256, size: $size, notes: $notes}'
