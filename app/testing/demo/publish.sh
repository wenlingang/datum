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
sips --cropToHeightWidth 1188 1232 --cropOffset 328 824 "$OUT/new-connection.png" --out "$DOCS/new-connection.png" >/dev/null
sips --cropToHeightWidth 1288 1156 --cropOffset 278 862 "$OUT/destructive-review.png" --out "$DOCS/destructive-review.png" >/dev/null

# 0.2.x 新功能:关系图整窗;各数据库专属功能按重点区域裁出,放进介绍页的数据库卡片
sips -z 1024 1600 "$OUT/erd.png" --out "$DOCS/erd.png" >/dev/null
crop() { # 名称 高 宽 上 左 [缩到的宽度]
  sips --cropToHeightWidth "$2" "$3" --cropOffset "$4" "$5" "$OUT/$1.png" --out "$DOCS/$1.png" >/dev/null
  if [ -n "${6:-}" ]; then sips --resampleWidth "$6" "$DOCS/$1.png" >/dev/null; fi
}
crop jsonb 1210 2240 180 640 1740       # 筛选条件 + 行详情弹窗里的 JSON 树
crop mysql-alter 1038 1160 404 860       # 风险评估对话框(保留 2x)
crop sqlite-alter 1469 1920 187 480 1400 # 改表对话框
crop redis 680 2200 180 660 1500         # key 标题行(含 TTL / 重命名 / 刷新)+ Hash 字段表
crop diagnostics 1000 2880 0 0 1600     # 诊断页上部:分区标签 + 锁等待表(下面是空白)

# 0.2.4 新功能:整窗缩到 1600×1024
for name in masking data-compare index-advisor journal test-data; do
  sips -z 1024 1600 "$OUT/$name.png" --out "$DOCS/$name.png" >/dev/null
done

for f in sql-editor table ai-assistant new-connection destructive-review erd jsonb mysql-alter sqlite-alter redis masking data-compare index-advisor journal test-data diagnostics; do
  printf '%s: ' "$f"
  sips -g pixelWidth -g pixelHeight "$DOCS/$f.png" | awk '/pixel/ {printf "%s ", $2} END {print ""}'
done
