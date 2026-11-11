#!/usr/bin/env bash
# 发版用:把 nsh package 出的 ad hoc 签名 Datum.app 改用固定的 "Datum Release" 证书签名,
# 重新打出同名的更新包(.tar.gz),并做一个首次安装用的 dmg。
#
# 用固定证书是为了让每个版本的签名一致:钥匙串里对连接密码的「始终允许」授权跟随签名,
# 换了签名每次升级都会重新弹授权。
#
# 用法:script/sign-macos.sh <out 目录> <arch>        (arch:aarch64 | x86_64)
# 产物(在 out 目录):Datum-<版本>-macos-<arch>.tar.gz(覆盖)、Datum-<版本>-macos-<arch>.dmg
set -euo pipefail

out=${1:?用法: sign-macos.sh <out 目录> <arch>}
arch=${2:?用法: sign-macos.sh <out 目录> <arch>}
root="$(cd "$(dirname "$0")/.." && pwd)"
identity="Datum Release"
version="$("$root/script/app-version.sh")"
app="$out/Datum.app"
archive="$out/Datum-$version-macos-$arch.tar.gz"
dmg="$out/Datum-$version-macos-$arch.dmg"

[[ -d $app ]] || { echo "❌ 找不到 $app,先运行 nsh package --target macos" >&2; exit 1; }
[[ -f $archive ]] || { echo "❌ 找不到 $archive(nsh.json 需配置 \"updater\")" >&2; exit 1; }

# 发版证书不可替换:钥匙串里的 "Datum Release" 必须与已提交的指纹一致
pin=$(tr -d '[:space:]' < "$root/script/release-cert.sha1")
# 先把整段输出收进变量再解析:管道里用 head 提前关闭会让上游收到 SIGPIPE,在 pipefail 下整个脚本静默退出
certs=$(security find-certificate -c "$identity" -Z 2>/dev/null || true)
found=$(printf '%s\n' "$certs" | awk '/SHA-1 hash:/ && !seen {print $3; seen=1}')
if [[ -z $found ]]; then
  echo "❌ 钥匙串里找不到签名身份 \"$identity\"" >&2
  exit 1
fi
if [[ $found != "$pin" ]]; then
  echo "❌ \"$identity\" 证书指纹 $found 与 script/release-cert.sha1($pin)不一致,拒绝签名" >&2
  exit 1
fi

codesign --force --deep --timestamp=none --sign "$identity" "$app"
codesign --verify --deep --strict "$app"
info=$(codesign -dvvv "$app" 2>&1)
signed=$(printf '%s\n' "$info" | awk -F= '/^Authority=/ && !seen {print $2; seen=1}')
[[ $signed == "$identity" ]] || { echo "❌ 签名身份是 \"$signed\",不是 \"$identity\"" >&2; exit 1; }

# 更新包:与 nsh 写出的同名、同结构(顶层就是 Datum.app/)
rm -f "$archive"
tar -C "$out" -czf "$archive" Datum.app

# 首次安装用的 dmg(拖进「应用程序」)
staging=$(mktemp -d)
trap 'rm -rf "$staging"' EXIT
cp -R "$app" "$staging/"
ln -s /Applications "$staging/Applications"
hdiutil create -quiet -volname Datum -srcfolder "$staging" -ov -format UDZO "$dmg"

echo "✅ $archive"
echo "✅ $dmg"
