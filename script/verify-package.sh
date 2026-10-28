#!/usr/bin/env bash
# 校验打好的 Datum.app 与 dmg;任何一项不通过即非零退出。
# 用法:script/verify-package.sh <app> <dmg> <签名身份> <版本号> [<指纹文件>]
#   指纹文件:存放预期证书的 SHA-1(script/release-cert.sha1);给出时,designated
#   requirement 必须恰好钉住这张证书。
set -euo pipefail
app=${1:?用法: verify-package.sh <app> <dmg> <签名身份> <版本号> [<指纹文件>]}
dmg=${2:?}
identity=${3:?}
version=${4:?}
pin_file=${5:-}
bundle_id="com.dbstudio.app"
fail() { echo "❌ $*" >&2; exit 1; }

archs=$(lipo -archs "$app/Contents/MacOS/datum" 2>&1) \
  || fail "读不出 $app/Contents/MacOS/datum 的架构: $archs"
[[ " $archs " == *" arm64 "* && " $archs " == *" x86_64 "* ]] \
  || fail "二进制必须同时包含 arm64 与 x86_64,实际: $archs"

verify_err=$(codesign --verify --strict --deep "$app" 2>&1) \
  || fail "$app 未通过 codesign --verify: $verify_err"
info=$(codesign -dvv "$app" 2>&1)
grep -qxF "Authority=$identity" <<<"$info" || fail "应用不是由 \"$identity\" 签名的"
grep -qxF "Identifier=$bundle_id" <<<"$info" || fail "签名标识不是 $bundle_id"
# 只有 designated requirement 钉住证书,已安装的应用才认可这个更新包、钥匙串授权才会保留
requirement=$(codesign -d -r- "$app" 2>&1)
# 自签名证书显示为 "certificate leaf = H…" 或 "certificate root = H…";ad-hoc 只有 cdhash
grep -Eq 'certificate (leaf|root) = H"' <<<"$requirement" \
  || fail "designated requirement 没有钉住证书: $requirement"
if [[ -n $pin_file ]]; then
  pin=$(tr -d '[:space:]' < "$pin_file" 2>/dev/null || true)
  [[ -n $pin ]] || fail "证书指纹文件 $pin_file 不存在或为空"
  signed=$(sed -nE 's/.*certificate (leaf|root) = H"([0-9A-Fa-f]+)".*/\2/p' <<<"$requirement" | head -n 1)
  [[ $(tr a-f A-F <<<"$signed") == "$(tr a-f A-F <<<"$pin")" ]] \
    || fail "证书指纹不符:签名用的是 H\"$signed\",应为 $pin(script/release-cert.sha1)"
fi

plist_version=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist" 2>&1) \
  || fail "读不出 $app/Contents/Info.plist 的 CFBundleShortVersionString: $plist_version"
[[ $plist_version == "$version" ]] || fail "Info.plist 版本是 $plist_version,应为 $version"

mnt=$(mktemp -d)
trap 'hdiutil detach -quiet "$mnt" 2>/dev/null || true; rmdir "$mnt" 2>/dev/null || true' EXIT
hdiutil attach -quiet -nobrowse -readonly -mountpoint "$mnt" "$dmg" || fail "无法挂载 dmg $dmg"
[[ -d "$mnt/Datum.app" && -L "$mnt/Applications" ]] \
  || fail "dmg 里必须有 Datum.app 和 Applications 链接"

echo "✅ 包校验通过:$identity,$archs,版本 $version${pin_file:+,证书指纹 $pin}"
