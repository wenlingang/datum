#!/usr/bin/env bash
# 构建通用(arm64 + x86_64)、已签名的 Datum.app,并打成 dmg 用于发版。
# 用法:script/package.sh [--identity <名称>]   (默认签名身份:"Datum Release")
#   用 "Datum Release" 签名时,证书必须与已提交的 script/release-cert.sha1 一致。
# 产物:target/release-dist/{Datum.app, Datum-<版本>.dmg, Datum-<版本>.dmg.sha256}
set -euo pipefail

identity="Datum Release"
while [[ $# -gt 0 ]]; do
  case $1 in
    --identity) identity=${2:?--identity 需要一个值}; shift 2 ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
done

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
dist="$root/target/release-dist"
app="$dist/Datum.app"
pin_args=()

# 发版证书不可替换:拒绝用任何别的 "Datum Release" 证书签名
if [[ $identity == "Datum Release" ]]; then
  pin_file="$root/script/release-cert.sha1"
  if [[ ! -s $pin_file ]]; then
    echo "❌ 缺少 script/release-cert.sha1:先运行一次 script/release-cert.sh 并提交它写出的指纹文件。" >&2
    exit 1
  fi
  pin_args=("$pin_file")
fi

# 绝不退回 ad-hoc 签名:那样的包没有固定证书,已安装的应用拒绝更新到它
if ! security find-identity -p codesigning | grep -q "\"$identity\""; then
  echo "❌ 钥匙串搜索列表里找不到签名身份 \"$identity\"。" >&2
  exit 1
fi

version="$(script/app-version.sh)"
dmg="$dist/Datum-$version.dmg"

script/bundle-macos.sh --universal
rm -rf "$dist"
mkdir -p "$dist"
ditto target/bundle/Datum.app "$app"

codesign --force --timestamp=none --sign "$identity" "$app"

staging="$dist/dmg"
mkdir -p "$staging"
cp -R "$app" "$staging/"
ln -s /Applications "$staging/Applications"
hdiutil create -quiet -volname Datum -srcfolder "$staging" -ov -format UDZO "$dmg"
rm -rf "$staging"
(cd "$dist" && shasum -a 256 "$(basename "$dmg")" > "$(basename "$dmg").sha256")

script/verify-package.sh "$app" "$dmg" "$identity" "$version" ${pin_args[@]+"${pin_args[@]}"}
echo "$dmg"
