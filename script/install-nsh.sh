#!/usr/bin/env bash
# CI 用:安装与 app/native/Cargo.toml 中 nsh 依赖同一提交的 nsh CLI(宿主与 CLI 必须同源)。
set -euo pipefail
cd "$(dirname "$0")/.."
rev=$(sed -n 's/^nsh-core = { git = "https:\/\/github.com\/longbridge\/nsh", rev = "\([0-9a-f]*\)" }$/\1/p' app/native/Cargo.toml)
[[ -n $rev ]] || { echo "❌ 没在 app/native/Cargo.toml 里找到 nsh 的 git rev" >&2; exit 1; }
if command -v nsh >/dev/null && nsh --version | grep -q "(${rev:0:7})"; then
  echo "nsh ${rev:0:7} 已安装"
  exit 0
fi
cargo install --git https://github.com/longbridge/nsh --rev "$rev" nsh-cli --locked --force
nsh --version
