#!/usr/bin/env bash
#
# 构建 Datum 的 macOS 发布包:.app → 签名 →(可选)公证 → DMG / 自更新 zip → 更新签名 → latest.json。
#
# 正式发版由 GitHub Actions 完成(.github/workflows/release.yml:推送 v* 标签触发),
# 它调用的就是本脚本。本地运行用于自测,或在 CI 不可用时手动发版。
#
# 用法:script/release-macos.sh [--universal] [--notes "更新说明"] [--skip-notarize] [--publish]
#   --universal      同时构建 arm64 + x86_64 的通用包
#   --notes          写进 latest.json 的更新说明(应用内「关于与更新」展示)
#   --skip-notarize  有 Developer ID 证书时也跳过公证
#   --publish        在 RELEASE_REPO 建 Release v版本号,上传 dmg / zip / latest.json
#
# 代码签名(可选):
#   DATUM_SIGN_IDENTITY  签名证书名称:
#     - "Developer ID Application: …":苹果证书,加时间戳,可公证(用户双击即可打开)
#     - 其他(如自签名代码签名证书):只签名不公证。签名身份固定,钥匙串授权在更新后保留
#     - 不设:ad-hoc 签名。每次更新签名身份都会变,钥匙串里的连接密码会重新弹授权
#   未公证的包,用户首次打开需在「系统设置 → 隐私与安全性」点「仍要打开」,之后自动更新不受影响
#   SIGN_KEYCHAIN        证书所在钥匙串路径(CI 导入到临时钥匙串时设置)
#   公证凭据二选一:NOTARY_KEY_PATH + NOTARY_KEY_ID + NOTARY_ISSUER(App Store Connect API Key),
#                  或钥匙串 profile NOTARY_PROFILE(默认 datum-notarize)
#
# 更新签名(必需):
#   DATUM_UPDATE_SIGNING_KEY       Ed25519 私钥(64 位十六进制);不设时读
#   DATUM_UPDATE_SIGNING_KEY_PATH  私钥文件,默认 ~/.config/datum/update-signing.key
#   私钥由 `cargo run -p datum-core --bin datum-sign -- keygen <文件>` 生成,
#   对应公钥必须与 crates/core/src/update.rs 的 UPDATE_PUBLIC_KEY 一致。
#
# 其他:
#   RELEASE_REPO         发布仓库,默认 wenlingang/datum(与 update.rs 的清单地址一致)
#   发布到 GitHub 时需要 gh 已登录(或设置 GH_TOKEN)且对 RELEASE_REPO 有写权限
#
set -euo pipefail

SIGN_IDENTITY="${DATUM_SIGN_IDENTITY:--}"
NOTARY_PROFILE="${NOTARY_PROFILE:-datum-notarize}"
RELEASE_REPO="${RELEASE_REPO:-wenlingang/datum}"

UNIVERSAL=""
NOTES=""
NOTARIZE=1
PUBLISH=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --universal) UNIVERSAL="--universal" ;;
    --notes) NOTES="${2:?--notes 需要内容}"; shift ;;
    --skip-notarize) NOTARIZE=0 ;;
    --publish) PUBLISH=1 ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
  shift
done
# 只有苹果的 Developer ID 证书能公证、能用苹果时间戳服务
DEVELOPER_ID=0
[[ "$SIGN_IDENTITY" == "Developer ID Application:"* ]] && DEVELOPER_ID=1
[[ $DEVELOPER_ID == 0 ]] && NOTARIZE=0

if [[ $NOTARIZE == 1 ]]; then
  if [[ -n "${NOTARY_KEY_PATH:-}" ]]; then
    : "${NOTARY_KEY_ID:?设置了 NOTARY_KEY_PATH 时还需要 NOTARY_KEY_ID}"
    : "${NOTARY_ISSUER:?设置了 NOTARY_KEY_PATH 时还需要 NOTARY_ISSUER}"
    NOTARY_AUTH=(--key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER")
  else
    NOTARY_AUTH=(--keychain-profile "$NOTARY_PROFILE")
  fi
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# codesign 只在钥匙串搜索列表里找签名身份,--keychain 单独不够:指定了 SIGN_KEYCHAIN 时
# 临时把它加到列表最前,脚本结束(含出错)时恢复原列表。
if [[ -n "${SIGN_KEYCHAIN:-}" ]]; then
  ORIG_KEYCHAINS=()
  while IFS= read -r k; do
    k="${k#"${k%%[![:space:]]*}"}"; k="${k#\"}"; k="${k%\"}"
    [[ -n "$k" && "$k" != "$SIGN_KEYCHAIN" ]] && ORIG_KEYCHAINS+=("$k")
  done < <(security list-keychains -d user)
  security list-keychains -d user -s "$SIGN_KEYCHAIN" "${ORIG_KEYCHAINS[@]}"
  trap 'security list-keychains -d user -s "${ORIG_KEYCHAINS[@]}"' EXIT
fi

if [[ -z "${DATUM_UPDATE_SIGNING_KEY:-}" ]]; then
  KEY_FILE="${DATUM_UPDATE_SIGNING_KEY_PATH:-$HOME/.config/datum/update-signing.key}"
  [[ -r "$KEY_FILE" ]] || { echo "缺少更新签名私钥:设置 DATUM_UPDATE_SIGNING_KEY 或提供 $KEY_FILE" >&2; exit 2; }
  DATUM_UPDATE_SIGNING_KEY="$(tr -d '[:space:]' < "$KEY_FILE")"
fi
export DATUM_UPDATE_SIGNING_KEY

echo "==> 0/7 核对更新签名密钥"
cargo build --release -q -p datum-core --bin datum-sign
SIGN_TOOL="$ROOT/target/release/datum-sign"
# 常量可能被 rustfmt 折成多行:取声明后第一个 64 位十六进制串
EXPECTED_PK="$(grep -A2 'pub const UPDATE_PUBLIC_KEY' crates/core/src/update.rs | grep -oE '[0-9a-f]{64}' | head -1)"
ACTUAL_PK="$("$SIGN_TOOL" pubkey)"
if [[ -z "$EXPECTED_PK" || "$ACTUAL_PK" != "$EXPECTED_PK" ]]; then
  echo "更新签名私钥与应用内置公钥(update.rs UPDATE_PUBLIC_KEY)不匹配,发出的更新将无法通过校验" >&2
  exit 2
fi

echo "==> 1/7 构建 .app"
"$ROOT/script/bundle-macos.sh" $UNIVERSAL

OUT="$ROOT/target/bundle"
APP="$OUT/Datum.app"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
ARCH="$(uname -m)"; [[ "$ARCH" == "arm64" ]] && ARCH="aarch64"
[[ -n "$UNIVERSAL" ]] && ARCH="universal"
DMG_NAME="Datum_${VERSION}_${ARCH}.dmg"
ZIP_NAME="Datum_${VERSION}_${ARCH}.app.zip"
DMG="$OUT/$DMG_NAME"
ZIP="$OUT/$ZIP_NAME"
MANIFEST="$OUT/latest.json"

if [[ "$SIGN_IDENTITY" == "-" ]]; then
  echo "==> 2/7 ad-hoc 签名(未设置 DATUM_SIGN_IDENTITY)"
  codesign --force --deep --sign - "$APP"
elif [[ $DEVELOPER_ID == 1 ]]; then
  echo "==> 2/7 Developer ID 签名(hardened runtime + 时间戳)"
  codesign --force --deep --options runtime --timestamp --sign "$SIGN_IDENTITY" \
    ${SIGN_KEYCHAIN:+--keychain "$SIGN_KEYCHAIN"} "$APP"
else
  echo "==> 2/7 签名:$SIGN_IDENTITY(hardened runtime,不公证)"
  codesign --force --deep --options runtime --sign "$SIGN_IDENTITY" \
    ${SIGN_KEYCHAIN:+--keychain "$SIGN_KEYCHAIN"} "$APP"
fi
codesign --verify --deep --strict --verbose=2 "$APP"

echo "==> 3/7 制作 DMG"
STAGE="$(mktemp -d)"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
hdiutil create -volname "Datum" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
if [[ $DEVELOPER_ID == 1 ]]; then
  codesign --force --timestamp --sign "$SIGN_IDENTITY" \
    ${SIGN_KEYCHAIN:+--keychain "$SIGN_KEYCHAIN"} "$DMG"
fi

if [[ $NOTARIZE == 1 ]]; then
  echo "==> 4/7 提交公证(notarytool,可能需要几分钟)"
  xcrun notarytool submit "$DMG" "${NOTARY_AUTH[@]}" --wait
  echo "==> 5/7 装订公证票据(staple)"
  xcrun stapler staple "$DMG"
  xcrun stapler staple "$APP"
else
  echo "==> 4-5/7 不公证"
fi

echo "==> 6/7 打包应用内自更新 zip、签名并生成 latest.json"
# 必须在 staple 之后打 zip,票据才会进包;--keepParent 让 zip 根目录是 Datum.app,
# 与 App 内更新器的解压查找逻辑匹配。
rm -f "$ZIP"
ditto -c -k --keepParent "$APP" "$ZIP"
SHA256="$(shasum -a 256 "$ZIP" | awk '{print $1}')"
SIGNATURE="$("$SIGN_TOOL" sign "$ZIP")"
"$SIGN_TOOL" verify "$EXPECTED_PK" "$ZIP" "$SIGNATURE" >/dev/null
BASE_URL="https://github.com/${RELEASE_REPO}/releases/download/v${VERSION}"
VERSION="$VERSION" DMG_URL="$BASE_URL/$DMG_NAME" ZIP_URL="$BASE_URL/$ZIP_NAME" \
  SHA256="$SHA256" SIGNATURE="$SIGNATURE" NOTES="$NOTES" python3 - "$MANIFEST" <<'PY'
import json, os, sys
m = {
    "version": os.environ["VERSION"],
    "url": os.environ["DMG_URL"],
    "zip_url": os.environ["ZIP_URL"],
    "sha256": os.environ["SHA256"],
    "signature": os.environ["SIGNATURE"],
}
if os.environ["NOTES"]:
    m["notes"] = os.environ["NOTES"]
with open(sys.argv[1], "w") as f:
    json.dump(m, f, ensure_ascii=False, indent=2)
PY

if [[ $PUBLISH == 1 ]]; then
  echo "==> 7/7 发布到 $RELEASE_REPO(v$VERSION)"
  [[ $NOTARIZE == 0 ]] && echo "⚠️  本次未公证:用户首次打开需在「系统设置 → 隐私与安全性」点「仍要打开」"
  # 清单通过 releases/latest/download/latest.json 取最新 Release 的同名附件,
  # 所以 latest.json 必须作为附件上传到这个 Release。
  gh release create "v$VERSION" "$DMG" "$ZIP" "$MANIFEST" \
    -R "$RELEASE_REPO" --title "Datum $VERSION" --notes "${NOTES:-Datum $VERSION}" --latest
else
  echo "==> 7/7 未发布(加 --publish 自动上传到 $RELEASE_REPO)"
fi

echo
echo "✅ 完成:"
echo "    $DMG"
echo "    $ZIP   (应用内自更新包,已签名)"
echo "    $MANIFEST"
if [[ $NOTARIZE == 1 ]]; then
  xcrun stapler validate "$DMG" && echo "staple 校验通过 ✅"
fi
