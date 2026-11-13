#!/bin/sh
# 在临时配置目录里跑全部测试(绝不碰用户真实配置与数据库)。
#   app/testing/run-tests.sh            # 全部
#   app/testing/run-tests.sh tab        # 只跑某一块:workspace | tab | dialogs | chat | rust
#   app/testing/run-tests.sh core       # 核心(CI 用):rust + 查询/表浏览 + 快捷键 + 连接表单等对话框
#   app/testing/run-tests.sh native     # 真实窗口里的输入测试(开原生窗口,不在 all 里):快速连续输入不乱序
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
want() { [ "$ONLY" = all ] || [ "$ONLY" = "$1" ] || { [ "$ONLY" = core ] && case $1 in rust|tab|dialogs) true;; *) false;; esac; }; }
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
  sqlite3 "$D/roastery.db" "create table addresses(id integer primary key, city text); create table categories(id integer primary key, name text); create table customers(id integer primary key, name text, email text); create table order_items(id integer primary key, order_id int, product_id int, quantity int); create table orders(id integer primary key, customer_id int); create table payments(id integer primary key, amount real); create table products(id integer primary key, name text, country text); create table reviews(id integer primary key, product_id int, rating real); create table subscriptions_with_a_really_long_table_name_suffix(id integer primary key); insert into products(name,country) values('Boquete','Panama'),('Nyeri','Kenya'); insert into categories(name) values('beans'); create view v_kenya as select name from products where country = 'Kenya'; create trigger trg_review after insert on reviews begin update products set name = name where id = new.product_id; end;"
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
  run tab-unit "$D" tab/placeholders.test.ts
  # 自然语言筛选:本地起一个 OpenAI 兼容的假服务,固定回一段筛选 JSON
  D="$ROOT/tab-nl"; mkdir -p "$D"
  sqlite3 "$D/roastery.db" < "$HERE/roastery-seed.sql"
  printf '[%s]\n' "$(conn dev-sqlite roastery "$D/roastery.db")" > "$D/connections.json"
  printf '%s' '{"conj":"and","filters":[{"col":"Origin","op":"eq","value":"Kenya"},{"col":"roast_level","op":"eq","value":"medium"},{"col":"rating","op":"gt","value":"4"}],"note":"rating 列不存在"}' > "$D/reply.txt"
  python3 "$HERE/ai-mock.py" "$D/reply.txt" "$D/requests.log" "$D/port" & MOCK=$!
  for _ in 1 2 3 4 5 6 7 8 9 10; do [ -s "$D/port" ] && break; sleep 0.2; done
  printf '{"providers":[{"id":"mock","name":"Mock","protocol":"openai","baseUrl":"http://127.0.0.1:%s","apiKey":"sk-test","enabled":true,"models":[{"id":"mock-model","enabled":true}]}],"current":{"providerId":"mock","model":"mock-model"}}' "$(cat "$D/port")" > "$D/ai_settings.json"
  run tab-nl "$D" tab/nl.test.tsx
  kill $MOCK 2>/dev/null; wait $MOCK 2>/dev/null || true
  grep -q 'Request: Kenya medium' "$D/requests.log" || { echo "nl filter: 请求里没有用户原话"; FAILED=1; }
fi

if want dialogs; then
  D="$ROOT/dialogs"; mkdir -p "$D"
  sqlite3 "$D/test.db" "create table t(id integer primary key, name text); insert into t(name) values('a'),('b');"
  run dialogs "$D" dialogs/dialogs.test.tsx
fi

# 真实窗口:只在显式要求时跑(会开窗口,需要桌面环境)
if [ "$ONLY" = native ]; then
  D="$ROOT/native-tab"; mkdir -p "$D"
  sqlite3 "$D/roastery.db" < "$HERE/roastery-seed.sql"
  printf '[%s]\n' "$(conn dev-sqlite roastery "$D/roastery.db")" > "$D/connections.json"
  echo "==> native-tab"
  DATUM_CONFIG_DIR="$D" nsh test --target "$(uname | tr A-Z a-z | sed 's/darwin/macos/')" tab/typing.native.test.tsx || FAILED=1
  D="$ROOT/native-chat"; mkdir -p "$D"
  echo "==> native-chat"
  DATUM_CONFIG_DIR="$D" nsh test --target "$(uname | tr A-Z a-z | sed 's/darwin/macos/')" chat/typing.native.test.tsx || FAILED=1
fi

# 真实 MySQL 上的诊断操作与改表结构评估:需要本机 MySQL(root 无密码),设 DATUM_TEST_MYSQL=端口 才跑
if [ -n "${DATUM_TEST_MYSQL:-}" ] && want mysql; then
  D="$ROOT/mysql"; mkdir -p "$D"
  M="mysql -h127.0.0.1 -P$DATUM_TEST_MYSQL -uroot"
  # 上一次留下的 SLEEP 会话结束时会记进慢 SQL 排行:先结束掉
  kill_sleepers() {
    $M -N -e "SELECT CONCAT('KILL ', id, ';') FROM information_schema.processlist WHERE info LIKE '%datum_sleeper%' AND info NOT LIKE '%processlist%'" | $M >/dev/null 2>&1 || true
  }
  kill_sleepers
  $M -e "SET SESSION cte_max_recursion_depth = 5000; DROP DATABASE IF EXISTS datum_ui_test; CREATE DATABASE datum_ui_test; USE datum_ui_test;
    CREATE TABLE products (id INT PRIMARY KEY, name VARCHAR(40), price INT, INDEX idx_unused_name (name));
    INSERT INTO products VALUES (1,'a',1),(2,'b',2),(3,'c',3);
    CREATE TABLE big (id INT PRIMARY KEY, v INT);
    INSERT INTO big WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 3000) SELECT n, n % 997 FROM s;
    TRUNCATE performance_schema.events_statements_summary_by_digest;
    SELECT COUNT(*) FROM big a JOIN big b ON a.v < b.v WHERE a.id > 10;" >/dev/null
  # 一个占着的会话:诊断「会话」里能看到它的 SQL
  $M -e "SELECT SLEEP(40) AS datum_sleeper" >/dev/null 2>&1 & SLEEPER=$!
  printf '[{"id":"my","name":"mysql","kind":"mysql","env":"local","host":"127.0.0.1","port":%s,"user":"root","database":"datum_ui_test","plaintextPassword":"","sslMode":"disable"}]\n' "$DATUM_TEST_MYSQL" > "$D/connections.json"
  run mysql "$D" tab/diag-mysql.test.tsx
  # 改表结构风险评估:表在诊断测试之后再建,不打乱诊断里按大小排的表清单
  $M datum_ui_test -e "CREATE TABLE alter_demo (id INT PRIMARY KEY, v INT, note VARCHAR(20)); INSERT INTO alter_demo VALUES (1, 1, 'a'), (2, 2, 'b');" >/dev/null
  run mysql-alter "$D" tab/alter-mysql.test.tsx
  # 字符集体检:utf8mb3 的表,外键两侧字符集不同(只能先建一致的外键,再关掉 foreign_key_checks 改一侧)
  $M datum_ui_test -e "CREATE TABLE members (id INT PRIMARY KEY, user_name VARCHAR(40) NOT NULL, UNIQUE KEY uk_user_name (user_name)) CHARACTER SET utf8mb4;
    CREATE TABLE legacy_users (id INT PRIMARY KEY, user_name VARCHAR(40) CHARACTER SET utf8mb4 NOT NULL, name VARCHAR(40),
      CONSTRAINT fk_legacy_member FOREIGN KEY (user_name) REFERENCES members (user_name)) CHARACTER SET utf8mb3;
    SET foreign_key_checks = 0;
    ALTER TABLE legacy_users MODIFY user_name VARCHAR(40) CHARACTER SET utf8mb3 NOT NULL;" >/dev/null
  run mysql-charset "$D" tab/charset-mysql.test.tsx
  kill $SLEEPER 2>/dev/null; wait $SLEEPER 2>/dev/null || true
  kill_sleepers
  $M -e "DROP DATABASE IF EXISTS datum_ui_test" >/dev/null
fi

# 真实 PostgreSQL 上的诊断操作:需要开了 pg_stat_statements 的 PG(postgres 用户免密),设 DATUM_TEST_PG=端口 才跑;
# 本机没有 psql 时用 DATUM_TEST_PSQL 指定,如 "docker exec -i 容器名 psql -U postgres"
if [ -n "${DATUM_TEST_PG:-}" ] && want pg; then
  D="$ROOT/pg"; mkdir -p "$D"
  P="${DATUM_TEST_PSQL:-psql -h 127.0.0.1 -p $DATUM_TEST_PG -U postgres} -v ON_ERROR_STOP=1 -q"
  $P -d postgres -c "DROP DATABASE IF EXISTS datum_ui_test" -c "CREATE DATABASE datum_ui_test" >/dev/null
  $P -d datum_ui_test >/dev/null <<'SQL'
CREATE EXTENSION IF NOT EXISTS pg_stat_statements;
CREATE TABLE products (id int PRIMARY KEY, name text, price int);
CREATE INDEX idx_unused_name ON products (name);
CREATE TABLE big AS SELECT g AS id, g % 997 AS v FROM generate_series(1, 3000) g;
-- JSONB:250 行,第 n 行 customer.zipcode = 10000 + n,只有前 3 行 tier 是 gold
CREATE TABLE events (id int PRIMARY KEY, payload jsonb, note text);
INSERT INTO events SELECT g, jsonb_build_object(
    'kind', CASE WHEN g % 2 = 0 THEN 'click' ELSE 'view' END,
    'amount', g * 10,
    'flags', jsonb_build_array(g % 3 = 0),
    'customer', jsonb_build_object('tier', CASE WHEN g <= 3 THEN 'gold' ELSE 'basic' END, 'zipcode', 10000 + g)),
  'n' || g FROM generate_series(1, 250) g;
SELECT pg_stat_statements_reset();
SELECT count(*) FROM big a JOIN big b ON a.v < b.v WHERE a.id > 10;
SQL
  printf '[{"id":"pg","name":"pg","kind":"pg","env":"local","host":"127.0.0.1","port":%s,"user":"postgres","database":"datum_ui_test","plaintextPassword":"","sslMode":"disable"}]\n' "$DATUM_TEST_PG" > "$D/connections.json"
  run pg "$D" tab/diag-pg.test.tsx
  run pg-jsonb "$D" tab/jsonb-pg.test.tsx
  $P -d postgres -c "DROP DATABASE IF EXISTS datum_ui_test WITH (FORCE)" >/dev/null
fi

# Redis 分析(多级前缀、删除大 key、集群逐节点扫描):DATUM_TEST_REDIS=单机端口(会清空库 0,只用测试实例);
# DATUM_TEST_REDIS_CLUSTER=三主节点集群的首个端口(各节点端口依次 +1,宣告地址 127.0.0.1)
if { [ -n "${DATUM_TEST_REDIS:-}" ] || [ -n "${DATUM_TEST_REDIS_CLUSTER:-}" ]; } && want redis; then
  D="$ROOT/redis"; mkdir -p "$D"
  rconn() { printf '{"id":"%s","name":"%s","kind":"redis","env":"local","host":"127.0.0.1","port":%s,"user":"","database":"0","plaintextPassword":"","sslMode":"disable"}' "$1" "$2" "$3"; }
  LIST=""
  [ -n "${DATUM_TEST_REDIS:-}" ] && LIST="$(rconn rs redis-single "$DATUM_TEST_REDIS")"
  if [ -n "${DATUM_TEST_REDIS_CLUSTER:-}" ]; then
    C=$DATUM_TEST_REDIS_CLUSTER
    for i in 0 1 2; do LIST="${LIST:+$LIST,}$(rconn "rc$i" "cluster-$i" $((C + i)))"; done
  fi
  printf '[%s]\n' "$LIST" > "$D/connections.json"
  run redis "$D" tab/redis-insight.test.tsx
fi

if want chat; then
  D="$ROOT/chat"; mkdir -p "$D"
  run chat "$D" chat/chat.test.tsx
fi

exit $FAILED
