#!/usr/bin/env bash
# 打印 Datum 的版本号(app/nsh.json 的 "version",发版版本的唯一来源)。
set -euo pipefail
cd "$(dirname "$0")/.."
python3 -c 'import json; print(json.load(open("app/nsh.json"))["version"])'
