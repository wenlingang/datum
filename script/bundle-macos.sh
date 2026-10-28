#!/usr/bin/env bash
#
# 构建 Datum.app(未签名,本机自测用)。正式发布用 package.sh(会调用本脚本)。
#
# 用法:script/bundle-macos.sh [--universal]
# 产物:target/bundle/Datum.app
# 环境变量 BUNDLE_FEATURES:额外的 cargo features(如自更新端到端测试用 snapshot)
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
BUNDLE_ID="com.dbstudio.app"   # 沿用 Tauri 版的标识符:配置目录与钥匙串条目保持兼容
OUT="$ROOT/target/bundle"
APP="$OUT/Datum.app"

if [[ "${1:-}" == "--universal" ]]; then
  rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
  cargo build --release --locked --target aarch64-apple-darwin -p datum ${BUNDLE_FEATURES:+--features "$BUNDLE_FEATURES"}
  cargo build --release --locked --target x86_64-apple-darwin -p datum ${BUNDLE_FEATURES:+--features "$BUNDLE_FEATURES"}
  BIN_TMP="$(mktemp -d)/datum"
  lipo -create -output "$BIN_TMP" \
    target/aarch64-apple-darwin/release/datum \
    target/x86_64-apple-darwin/release/datum
  BIN="$BIN_TMP"
else
  cargo build --release --locked -p datum ${BUNDLE_FEATURES:+--features "$BUNDLE_FEATURES"}
  BIN="target/release/datum"
fi

echo "==> 组装 $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/datum"
cp assets/icons/icon.icns "$APP/Contents/Resources/icon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Datum</string>
  <key>CFBundleDisplayName</key><string>Datum</string>
  <key>CFBundleIdentifier</key><string>${BUNDLE_ID}</string>
  <key>CFBundleExecutable</key><string>datum</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>10.15.7</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
PLIST

echo "✅ $APP (v${VERSION})"
