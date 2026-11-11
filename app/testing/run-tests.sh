#!/bin/sh
# 在临时配置目录里跑全部测试(绝不碰用户真实配置与数据库)。
#   app/testing/run-tests.sh            # 全部
#   app/testing/run-tests.sh tab        # 只跑某一块:workspace | tab | dialogs | chat | rust
#   app/testing/run-tests.sh core       # 核心(CI 用):rust + 查询/表浏览 + 快捷键 + 连接表单等对话框
# 每块测试各自的夹具:
#   workspace:两个 SQLite 连接(c-roastery 带 9 张表、c-shop)
#   tab      :一个 SQLite 连接(dev-sqlite),数据来自 roastery-seed.sql
#   dialogs  :空配置 + test.db
#   chat     :空配置(AI 与连接都是测试替身)
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
APP=$(dirname "$HERE")
ROOT=$(mktemp -d "${TMPDIR:-/tmp}/datum-test.XXXXXX")
trap 'rm -rf "$ROOT"' EXIT
cd "$APP"
ONLY=${1:-all}
# core = 核心流程:不跑外壳界面(workspace)与 AI 面板(chat)
want() { [ "$ONLY" = all ] || [ "$ONLY" = "$1" ] || { [ "$ONLY" = core ] && case $1 in rust|tab|erd|dialogs) true;; *) false;; esac; }; }
FAILED=0

conn() { # id name file
  printf '{"id":"%s","name":"%s","kind":"sqlite","env":"local","host":"","port":0,"user":"","database":"","filePath":"%s","sslMode":"disable"}' "$1" "$2" "$3"
}

run() { # 名称 配置目录 测试文件
  echo "==> $1"
  DATUM_CONFIG_DIR="$2" nsh test "$3" || FAILED=1
}

if want rust; then
  echo "==> rust"
  (cd native && cargo test --lib -q) || FAILED=1
fi

if want workspace; then
  D="$ROOT/workspace"; mkdir -p "$D"
  sqlite3 "$D/roastery.db" "create table addresses(id integer primary key, city text); create table categories(id integer primary key, name text); create table customers(id integer primary key, name text, email text); create table order_items(id integer primary key, order_id int, product_id int, quantity int); create table orders(id integer primary key, customer_id int); create table payments(id integer primary key, amount real); create table products(id integer primary key, name text, country text); create table reviews(id integer primary key, product_id int, rating real); create table subscriptions_with_a_really_long_table_name_suffix(id integer primary key); insert into products(name,country) values('Boquete','Panama'),('Nyeri','Kenya'); insert into categories(name) values('beans');"
  sqlite3 "$D/shop.db" "create table items(id integer primary key, title text); insert into items(title) values('a');"
  printf '[%s,%s]\n' "$(conn c-roastery roastery "$D/roastery.db")" "$(conn c-shop shop "$D/shop.db")" > "$D/connections.json"
  run workspace "$D" workspace/Workspace.test.tsx
fi

if want tab; then
  D="$ROOT/tab"; mkdir -p "$D"
  sqlite3 "$D/roastery.db" < "$HERE/roastery-seed.sql"
  printf '[%s]\n' "$(conn dev-sqlite roastery "$D/roastery.db")" > "$D/connections.json"
  run tab "$D" tab/tab.test.tsx
  run tab-keys "$D" tab/keys.test.tsx
fi

if want erd; then
  D="$ROOT/erd"; mkdir -p "$D"
  sqlite3 "$D/erd.db" "create table customers(id integer primary key, name text); create table orders(id integer primary key, customer_id int references customers(id), total real); create table order_items(id integer primary key, order_id int references orders(id), product_id int, qty int); create table products(id integer primary key, c2 text, c3 text, c4 text, c5 text, c6 text, c7 text, c8 text, c9 text, c10 text, c11 text, c12 text, c13 text, c14 text, c15_last text); create table notes(id integer primary key, body text);"
  printf '[%s]\n' "$(conn c-erd erd "$D/erd.db")" > "$D/connections.json"
  run erd "$D" erd/erd.test.tsx
fi

if want dialogs; then
  D="$ROOT/dialogs"; mkdir -p "$D"
  sqlite3 "$D/test.db" "create table t(id integer primary key, name text); insert into t(name) values('a'),('b');"
  run dialogs "$D" dialogs/dialogs.test.tsx
fi

if want chat; then
  D="$ROOT/chat"; mkdir -p "$D"
  run chat "$D" chat/chat.test.tsx
fi

exit $FAILED
