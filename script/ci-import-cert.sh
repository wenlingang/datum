#!/usr/bin/env bash
# 仅 CI 用:把 $P12(base64)/ $P12_PASSWORD 里的发版证书导入临时钥匙串
# $RUNNER_TEMP/release.keychain-db,并信任它用于代码签名。
set -euo pipefail
: "${P12:?未设置 RELEASE_CERT_P12 secret}" "${P12_PASSWORD:?未设置 RELEASE_CERT_PASSWORD secret}"
: "${RUNNER_TEMP:?}"
keychain="$RUNNER_TEMP/release.keychain-db"
kc_pass=$(openssl rand -hex 16)
p12="$RUNNER_TEMP/release.p12"
pem="$RUNNER_TEMP/release.cer.pem"
trap 'rm -f "$p12" "$pem"' EXIT

printf '%s' "$P12" | base64 --decode > "$p12"
security create-keychain -p "$kc_pass" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$kc_pass" "$keychain"
# -x:私钥在该钥匙串里不可导出;codesign 仍能使用
security import "$p12" -k "$keychain" -P "$P12_PASSWORD" -T /usr/bin/codesign -x
security set-key-partition-list -S apple-tool:,apple: -s -k "$kc_pass" "$keychain" >/dev/null
# 放到用户搜索列表最前,codesign 才找得到签名身份
# shellcheck disable=SC2046 # 有意按空白拆成多个钥匙串路径
security list-keychains -d user -s "$keychain" $(security list-keychains -d user | tr -d '"')

/usr/bin/openssl pkcs12 -in "$p12" -nokeys -passin "pass:$P12_PASSWORD" -out "$pem"
sudo security add-trusted-cert -d -r trustRoot -p codeSign -k /Library/Keychains/System.keychain "$pem"
security find-identity -v -p codesigning "$keychain"
