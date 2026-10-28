#!/usr/bin/env bash
# 打印 Datum 的版本号(工作区 Cargo.toml 的 version,发版版本的唯一来源)。
set -euo pipefail
cd "$(dirname "$0")/.."
# `cargo pkgid` 输出形如 "path+file:///…/crates/app#datum@0.0.1" 或 "…#0.0.1"
cargo pkgid -p datum | sed 's/.*[#@]//'
