#!/bin/sh
# 介绍页截图用的演示数据库:在 make-demo.sh 写进 connections.json 的端口上起容器并灌数据
# (PG analytics 55432、MySQL shop 53306、Redis cache 56379),都是咖啡烘焙店主题的虚构数据。
# 用法:make-demo-servers.sh up | down
# 只用于本机拍截图;容器名带 datum-demo- 前缀,down 时全部删掉。
set -eu

PG=datum-demo-pg
MY=datum-demo-mysql
RD=datum-demo-redis

down() {
  docker rm -f "$PG" "$MY" "$RD" >/dev/null 2>&1 || true
}

wait_for() { # 描述 命令…
  what=$1; shift
  for _ in $(seq 1 90); do
    if "$@" >/dev/null 2>&1; then return 0; fi
    sleep 2
  done
  echo "$what 没有就绪" >&2
  exit 1
}

up() {
  down
  docker run -d --name "$PG" -e POSTGRES_HOST_AUTH_METHOD=trust -e POSTGRES_DB=analytics -p 55432:5432 postgres:16-alpine >/dev/null
  docker run -d --name "$MY" -e MYSQL_ALLOW_EMPTY_PASSWORD=yes -e MYSQL_DATABASE=shop -p 53306:3306 mysql:8.4 >/dev/null
  docker run -d --name "$RD" -p 56379:6379 redis:7-alpine >/dev/null

  wait_for PostgreSQL docker exec "$PG" psql -U postgres -d analytics -c "select 1"
  docker exec -i "$PG" psql -q -U postgres -d analytics -v ON_ERROR_STOP=1 <<'SQL'
CREATE TABLE web_events (
  id          int PRIMARY KEY,
  occurred_at timestamptz NOT NULL,
  event       text NOT NULL,
  payload     jsonb NOT NULL
);
INSERT INTO web_events
SELECT g,
       timestamptz '2025-11-01 08:00+00' + (g * interval '17 minutes'),
       (ARRAY['page_view', 'add_to_cart', 'checkout', 'subscribe'])[1 + g % 4],
       jsonb_build_object(
         'customer', jsonb_build_object(
           'id', 1000 + (g * 37) % 180,
           'tier', (ARRAY['gold', 'silver', 'basic', 'basic', 'silver'])[1 + g % 5],
           'city', (ARRAY['Portland', 'Seattle', 'Oakland', 'Austin', 'Denver', 'Brooklyn'])[1 + g % 6]),
         'device', jsonb_build_object(
           'os', (ARRAY['iOS', 'Android', 'macOS', 'Windows'])[1 + g % 4],
           'app_version', '5.' || (g % 3) || '.' || (g % 7)),
         'cart', jsonb_build_object(
           'items', jsonb_build_array(
             jsonb_build_object('sku', (ARRAY['SO-PAN-006', 'SO-ETH-001', 'BL-014', 'SO-KEN-002', 'BL-015'])[1 + g % 5], 'qty', 1 + g % 3)),
           'total_cents', 1450 + (g * 113) % 4000),
         'campaign', jsonb_build_object(
           'source', (ARRAY['newsletter', 'instagram', 'search', 'direct'])[1 + g % 4],
           'medium', (ARRAY['email', 'social', 'cpc', 'none'])[1 + g % 4]))
FROM generate_series(1, 400) g;
ANALYZE web_events;
SQL

  wait_for MySQL docker exec "$MY" mysql -uroot -e "select 1"
  docker exec -i "$MY" mysql -uroot shop <<'SQL'
SET SESSION cte_max_recursion_depth = 500000;
CREATE TABLE orders (
  id          INT PRIMARY KEY,
  customer_id INT NOT NULL,
  status      VARCHAR(16) NOT NULL,
  total_cents INT NOT NULL,
  note        VARCHAR(120),
  created_at  DATETIME NOT NULL,
  KEY idx_orders_customer (customer_id)
);
INSERT INTO orders
WITH RECURSIVE s(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM s WHERE n < 400000)
SELECT n, 1000 + n % 1800,
       ELT(1 + n % 5, 'paid', 'shipped', 'delivered', 'cancelled', 'refunded'),
       1450 + (n * 113) % 9000,
       IF(n % 7 = 0, 'Gift wrap, please', NULL),
       TIMESTAMP('2024-01-01') + INTERVAL (n * 97) SECOND
FROM s;
ANALYZE TABLE orders;
SQL

  wait_for Redis docker exec "$RD" redis-cli ping
  docker exec -i "$RD" redis-cli >/dev/null <<'REDIS'
HSET customer:1042 name "Maya Okafor" email "maya@example.com" tier gold points 2840 city Portland favorite_roast light last_order 2025-11-12 subscription "Nyeri Gichathaini · biweekly"
HSET customer:1043 name "Jonas Berg" email "jonas@example.com" tier silver points 910 city Seattle
ZADD leaderboard:roasts 418 "Boquete Geisha" 392 "Yirgacheffe Kochere" 355 "Nyeri Gichathaini" 301 "House Espresso" 240 "Huila La Esperanza"
SET session:7f3a2c '{"customer_id":1042,"cart":[{"sku":"SO-PAN-006","qty":1},{"sku":"BL-014","qty":2}],"expires_at":"2025-11-15T18:00:00Z"}'
EXPIRE session:7f3a2c 86400
RPUSH cart:1042 SO-PAN-006 BL-014 BL-014
XADD stream:orders * order_id 90211 customer_id 1042 total_cents 7800
XADD stream:orders * order_id 90212 customer_id 1077 total_cents 1850
SADD tags:SO-PAN-006 floral geisha panama washed
REDIS
  echo "demo servers ready"
}

case "${1:-}" in
  up) up ;;
  down) down ;;
  *) echo "usage: $0 up|down" >&2; exit 2 ;;
esac
