#!/bin/sh
# 把 screenshots.test.tsx 拍到 testing/demo/out/ 的原图(1440×922 点窗口的 2x 图,2880×1844)
# 整理到 docs/screenshots/:整窗图缩到 1600 宽;对话框图按对话框所在区域裁出(保留 2x 像素)。
# 裁剪坐标对应当前对话框的布局与 1440×922 窗口,界面改了要重新量。只用 macOS 自带的 sips。

set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
OUT="$HERE/out"
DOCS=$(cd "$HERE/../../../docs/screenshots" && pwd)

for name in sql-editor table ai-assistant; do
  sips -z 1024 1600 "$OUT/$name.png" --out "$DOCS/$name.png" >/dev/null
done

# sips --cropToHeightWidth 高 宽 --cropOffset 上 左
sips --cropToHeightWidth 1110 1214 --cropOffset 368 833 "$OUT/new-connection.png" --out "$DOCS/new-connection.png" >/dev/null
sips --cropToHeightWidth 525 1150 --cropOffset 660 866 "$OUT/destructive-review.png" --out "$DOCS/destructive-review.png" >/dev/null

for f in sql-editor table ai-assistant new-connection destructive-review; do
  printf '%s: ' "$f"
  sips -g pixelWidth -g pixelHeight "$DOCS/$f.png" | awk '/pixel/ {printf "%s ", $2} END {print ""}'
done
