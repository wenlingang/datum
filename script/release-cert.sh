#!/usr/bin/env bash
# 一次性:生成 CI 用的长期自签名代码签名证书 "Datum Release"。
# 证书不导入你的钥匙串。把 .p12 和密码存进 GitHub secrets 并另行备份:证书丢失或更换后,
# 已安装的应用无法再自动更新(新包签名对不上),用户钥匙串里的「始终允许」授权也会失效。
# 同时把证书 SHA-1 写入 script/release-cert.sha1;提交该文件后,CI 拒绝发布任何
# 由另一张 "Datum Release" 证书签名的包。
# 用法:script/release-cert.sh [输出目录]   (默认 ~/datum-release-cert;必须不存在或为空)
# 环境变量:DATUM_CERT_PIN_FILE  指纹文件路径(默认 <仓库>/script/release-cert.sha1)
set -euo pipefail
umask 077

NAME="Datum Release"
root="$(cd "$(dirname "$0")/.." && pwd)"
pin_file="${DATUM_CERT_PIN_FILE:-$root/script/release-cert.sha1}"
out="${1:-$HOME/datum-release-cert}"
p12="$out/datum-release.p12"
# LibreSSL 写出的 PKCS#12 `security import` 能导入;OpenSSL 3 的默认格式不行
openssl=/usr/bin/openssl

if [[ -e $pin_file ]]; then
  echo "❌ $pin_file 已存在:发版证书已经生成过,拒绝再生成一张。" >&2
  exit 1
fi
if [[ -e $p12 ]]; then
  echo "❌ $p12 已存在,拒绝覆盖发版证书。" >&2
  exit 1
fi
if [[ -e $out && ( ! -d $out || -n "$(ls -A "$out")" ) ]]; then
  echo "❌ $out 已存在且非空,请换一个新目录。" >&2
  exit 1
fi
mkdir -p "$out"
chmod 700 "$out"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

cat > "$tmp/cfg" <<CFG
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = ${NAME}
[ext]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
CFG
"$openssl" req -x509 -newkey rsa:2048 -nodes -days 3650 \
  -keyout "$tmp/key.pem" -out "$out/datum-release.cer.pem" -config "$tmp/cfg" 2>/dev/null
password=$("$openssl" rand -hex 24)
DATUM_P12_PASSWORD=$password \
"$openssl" pkcs12 -export -inkey "$tmp/key.pem" -in "$out/datum-release.cer.pem" \
  -name "$NAME" -out "$p12" -passout env:DATUM_P12_PASSWORD
printf '%s' "$password" > "$out/password.txt"
base64 -i "$p12" > "$out/datum-release.p12.base64"

# 自检:按 CI 的方式导入一个临时钥匙串
kc="$tmp/check.keychain-db"
security create-keychain -p check "$kc"
security import "$p12" -k "$kc" -P "$password" -T /usr/bin/codesign >/dev/null
security find-identity -p codesigning "$kc" | grep -q "\"$NAME\"" \
  || { echo "❌ 生成的 .p12 无法作为签名身份导入。" >&2; security delete-keychain "$kc"; exit 1; }
security delete-keychain "$kc"

# 指纹:DER 证书的大写十六进制 SHA-1,与 codesign 显示的 `certificate leaf = H"…"` 一致
fingerprint=$("$openssl" x509 -in "$out/datum-release.cer.pem" -noout -fingerprint -sha1)
fingerprint=${fingerprint#*=}
fingerprint=${fingerprint//:/}
printf '%s\n' "$fingerprint" | tr a-f A-F > "$pin_file"
chmod 644 "$pin_file"

cat <<MSG
✅ 已在 $out 生成 "$NAME"

接下来:
  1. 立刻把 datum-release.p12 和 password.txt 备份到密码管理器。
     证书丢失后,已安装的应用将无法自动更新。
  2. 设置 secrets 之前先提交证书指纹:
       git add script/release-cert.sha1 && git commit -m "固定发版签名证书指纹"
     (已写入 $pin_file;不要再改它,签名证书与它不符时 CI 会失败。)
  3. 存为 GitHub secrets:
       gh secret set RELEASE_CERT_P12 < "$out/datum-release.p12.base64"
       gh secret set RELEASE_CERT_PASSWORD < "$out/password.txt"
  4. 两步都完成后删除 $out。
MSG
