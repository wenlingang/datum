#!/usr/bin/env bash
# 标签必须恰好是 "v<版本号>",否则失败。
# 用法:script/check-version.sh <标签>
set -euo pipefail
tag=${1:?用法: check-version.sh <标签>}
version=$("$(dirname "$0")/app-version.sh")
if [[ $tag != "v$version" ]]; then
  echo "❌ 标签 \"$tag\" 与 Cargo.toml 版本 \"$version\" 不一致(应为 \"v$version\")。" >&2
  exit 1
fi
echo "✅ 标签 $tag 与版本 $version 一致。"
