#!/usr/bin/env bash
# 把介绍页(site/)和截图、图标组装到一个目录,供 GitHub Pages 部署。
# 用法:script/build-site.sh [输出目录]   (默认 target/site)
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$root/target/site}"

rm -rf "$out"
mkdir -p "$out/screenshots"
cp "$root"/site/* "$out/"
cp "$root"/docs/screenshots/*.png "$out/screenshots/"
cp "$root/assets/icons/128x128@2x.png" "$out/icon.png"
echo "介绍页已生成到 $out"
