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
sips --cropToHeightWidth 1212 1128 --cropOffset 316 876 "$OUT/destructive-review.png" --out "$DOCS/destructive-review.png" >/dev/null

# 0.2.x 新功能:关系图整窗;各数据库专属功能按重点区域裁出,放进介绍页的数据库卡片
sips -z 1024 1600 "$OUT/erd.png" --out "$DOCS/erd.png" >/dev/null
crop() { # 名称 高 宽 上 左 [缩到的宽度]
  sips --cropToHeightWidth "$2" "$3" --cropOffset "$4" "$5" "$OUT/$1.png" --out "$DOCS/$1.png" >/dev/null
  if [ -n "${6:-}" ]; then sips --resampleWidth "$6" "$DOCS/$1.png" >/dev/null; fi
}
crop jsonb 884 1740 96 1140             # 筛选条件 + 右侧 JSON 树(保留 2x)
crop mysql-alter 976 1120 434 880        # 风险评估对话框(保留 2x)
crop sqlite-alter 1469 1920 187 480 1400 # 改表对话框
crop redis 710 1500 170 660              # key 标题 + Hash 字段表(保留 2x)

for f in sql-editor table ai-assistant new-connection destructive-review erd jsonb mysql-alter sqlite-alter redis; do
  printf '%s: ' "$f"
  sips -g pixelWidth -g pixelHeight "$DOCS/$f.png" | awk '/pixel/ {printf "%s ", $2} END {print ""}'
done
