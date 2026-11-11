#!/bin/sh
# 生成截图 / 演示用的配置目录:咖啡烘焙店 SQLite 库 roastery.db、connections.json、ui_prefs.json。
# 用法:make-demo.sh <目录>   然后 DATUM_CONFIG_DIR=<目录> 启动 Datum 或跑 testing/demo/screenshots.test.tsx。
# 目录只用临时目录(mktemp -d),不要指向真实配置。
#
# 连接:第一个是 roastery(SQLite,local);其余 PG / MySQL / Redis 只为竖轨上能看到各类型图标,
# 指向本机不常用的端口,不需要也不应该连得上。
# 主题跟随系统(theme "system",截图测试用 open 的 colorScheme 决定明暗),语言英文。

set -eu

if [ $# -ne 1 ]; then
  echo "usage: $0 <dir>" >&2
  exit 2
fi
mkdir -p "$1"
DIR=$(cd "$1" && pwd)
DB="$DIR/roastery.db"
rm -f "$DB"

sqlite3 "$DB" <<'SQL'
PRAGMA foreign_keys = ON;

CREATE TABLE categories (
  id          INTEGER PRIMARY KEY,
  name        TEXT NOT NULL UNIQUE,
  slug        TEXT NOT NULL UNIQUE,
  description TEXT
);
INSERT INTO categories(name, slug, description) VALUES
  ('Single Origin', 'single-origin', 'Traceable lots from a single farm, washing station or cooperative'),
  ('Blend',         'blend',         'House blends roasted for espresso and filter'),
  ('Decaf',         'decaf',         'Chemical-free decaffeinated coffees'),
  ('Brewing Gear',  'gear',          'Drippers, kettles, grinders and scales');

CREATE TABLE products (
  id          INTEGER PRIMARY KEY,
  category_id INTEGER NOT NULL REFERENCES categories(id),
  sku         TEXT NOT NULL UNIQUE,
  name        TEXT NOT NULL,
  origin      TEXT,
  region      TEXT,
  roast_level TEXT CHECK (roast_level IN ('light', 'medium', 'medium-dark', 'dark')),
  price_cents INTEGER NOT NULL,
  stock       INTEGER NOT NULL DEFAULT 0,
  attributes  TEXT,
  active      BOOLEAN NOT NULL DEFAULT 1,
  created_at  TEXT NOT NULL
);
CREATE INDEX idx_products_category ON products(category_id);
INSERT INTO products(category_id, sku, name, origin, region, roast_level, price_cents, stock, attributes, active, created_at) VALUES
  (1, 'SO-ETH-001', 'Yirgacheffe Kochere', 'Ethiopia',   'Gedeo',            'light',       1950,  84, '{"process": "washed", "altitude_m": 2050, "varietal": "Heirloom", "notes": ["jasmine", "lemon", "black tea"], "weight_g": 250}', 1, '2025-01-08 09:30:00'),
  (1, 'SO-KEN-002', 'Nyeri Gichathaini',   'Kenya',      'Nyeri',            'light',       2400,  46, '{"process": "washed", "altitude_m": 1750, "varietal": "SL28", "notes": ["blackcurrant", "grapefruit"], "weight_g": 250}', 1, '2025-01-08 09:30:00'),
  (1, 'SO-COL-003', 'Huila La Esperanza',  'Colombia',   'Huila',            'medium',      1850, 132, '{"process": "washed", "altitude_m": 1800, "varietal": "Caturra", "notes": ["red apple", "panela"], "weight_g": 250}', 1, '2025-01-15 10:00:00'),
  (1, 'SO-GUA-004', 'Antigua San Miguel',  'Guatemala',  'Antigua',          'medium',      1800, 118, '{"process": "washed", "altitude_m": 1600, "varietal": "Bourbon", "notes": ["cocoa", "orange zest"], "weight_g": 250}', 1, '2025-02-03 11:20:00'),
  (1, 'SO-BRA-005', 'Cerrado Mineiro',     'Brazil',     'Minas Gerais',     'medium-dark', 1500, 210, '{"process": "natural", "altitude_m": 1150, "varietal": "Mundo Novo", "notes": ["hazelnut", "milk chocolate"], "weight_g": 250}', 1, '2025-02-03 11:20:00'),
  (1, 'SO-PAN-006', 'Boquete Geisha',      'Panama',     'Chiriquí',         'light',       4800,  22, '{"process": "washed", "altitude_m": 1700, "varietal": "Geisha", "notes": ["jasmine", "bergamot", "peach"], "weight_g": 250}', 1, '2025-02-20 08:45:00'),
  (1, 'SO-RWA-007', 'Nyamasheke Kanzu',    'Rwanda',     'Western Province', 'light',       2050,  63, '{"process": "washed", "altitude_m": 1900, "varietal": "Red Bourbon", "notes": ["hibiscus", "cranberry"], "weight_g": 250}', 1, '2025-03-01 09:10:00'),
  (1, 'SO-CRI-008', 'Tarrazú Honey',       'Costa Rica', 'Tarrazú',          'light',       2100,  77, '{"process": "honey", "altitude_m": 1850, "varietal": "Catuaí", "notes": ["honeysuckle", "mango"], "weight_g": 250}', 1, '2025-03-01 09:10:00'),
  (1, 'SO-IDN-009', 'Sumatra Gayo',        'Indonesia',  'Aceh',             'dark',        1700, 141, '{"process": "wet-hulled", "altitude_m": 1500, "varietal": "Ateng", "notes": ["cedar", "dark chocolate"], "weight_g": 250}', 1, '2025-03-12 14:00:00'),
  (1, 'SO-ETH-010', 'Guji Hambela Natural', 'Ethiopia',  'Guji',             'light',       2200,  58, '{"process": "natural", "altitude_m": 2150, "varietal": "Heirloom", "notes": ["blueberry", "strawberry jam"], "weight_g": 250}', 1, '2025-04-02 10:30:00'),
  (1, 'SO-HND-011', 'Santa Bárbara Pacas', 'Honduras',   'Santa Bárbara',    'medium',      1900,  92, '{"process": "washed", "altitude_m": 1650, "varietal": "Pacas", "notes": ["caramel", "plum"], "weight_g": 250}', 1, '2025-04-02 10:30:00'),
  (1, 'SO-CHN-012', 'Pu''er Natural',      'China',      'Yunnan',           'light',       1750, 104, '{"process": "natural", "altitude_m": 1400, "varietal": "Catimor", "notes": ["raisin", "brown sugar"], "weight_g": 250}', 1, '2025-04-24 09:00:00'),
  (1, 'SO-USA-013', 'Kona Extra Fancy',    'USA',        'Hawaii',           'medium',      5200,  15, '{"process": "washed", "altitude_m": 600, "varietal": "Typica", "notes": ["macadamia", "brown butter"], "weight_g": 227}', 0, '2025-05-06 16:15:00'),
  (2, 'BL-014',     'House Espresso',      'Blend',      NULL,               'medium-dark', 1600, 260, '{"components": ["Brazil", "Colombia", "Ethiopia"], "notes": ["dark chocolate", "cherry"], "weight_g": 500}', 1, '2025-01-05 08:00:00'),
  (2, 'BL-015',     'Morning Ritual',      'Blend',      NULL,               'medium',      1450, 198, '{"components": ["Guatemala", "Brazil"], "notes": ["toffee", "almond"], "weight_g": 500}', 1, '2025-01-05 08:00:00'),
  (2, 'BL-016',     'Night Owl',           'Blend',      NULL,               'dark',        1450, 175, '{"components": ["Sumatra", "Brazil"], "notes": ["molasses", "smoke"], "weight_g": 500}', 1, '2025-01-05 08:00:00'),
  (2, 'BL-017',     'Cold Brew Blend',     'Blend',      NULL,               'medium',      1550, 120, '{"components": ["Colombia", "Rwanda"], "notes": ["cola", "brown sugar"], "weight_g": 500}', 1, '2025-05-20 12:00:00'),
  (3, 'DC-018',     'Swiss Water Colombia', 'Colombia',  'Cauca',            'medium',      1750,  66, '{"process": "Swiss Water", "notes": ["caramel", "red grape"], "weight_g": 250}', 1, '2025-02-11 13:40:00'),
  (3, 'DC-019',     'Mountain Water Mexico', 'Mexico',   'Chiapas',          'medium-dark', 1700,  54, '{"process": "Mountain Water", "notes": ["cocoa", "walnut"], "weight_g": 250}', 1, '2025-02-11 13:40:00'),
  (4, 'GR-020',     'Hario V60 Dripper',   NULL,         NULL,               NULL,          2800,  40, '{"material": "ceramic", "size": "02", "color": "white"}', 1, '2025-01-20 10:00:00'),
  (4, 'GR-021',     'Fellow Stagg EKG Kettle', NULL,     NULL,               NULL,         16500,  12, '{"capacity_ml": 900, "variable_temp": true, "color": "matte black"}', 1, '2025-01-20 10:00:00'),
  (4, 'GR-022',     'Comandante C40 Grinder', NULL,      NULL,               NULL,         27500,   8, '{"burr": "Nitro Blade", "capacity_g": 40}', 1, '2025-03-18 15:00:00'),
  (4, 'GR-023',     'Acaia Pearl Scale',   NULL,         NULL,               NULL,         17500,  10, '{"resolution_g": 0.1, "bluetooth": true}', 1, '2025-03-18 15:00:00');

CREATE TABLE customers (
  id               INTEGER PRIMARY KEY,
  first_name       TEXT NOT NULL,
  last_name        TEXT NOT NULL,
  email            TEXT NOT NULL UNIQUE,
  country          TEXT NOT NULL,
  marketing_opt_in BOOLEAN NOT NULL DEFAULT 0,
  created_at       TEXT NOT NULL
);
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 180),
f(a) AS (SELECT '["Ava","Liam","Mia","Noah","Chloé","Mateo","Hana","Lucas","Sofía","Kenji","Amara","Oliver","Ingrid","Diego","Priya","Jonas","Léa","Ethan","Yuki","Omar","Freya","Tomás","Nina","Arjun","Elena","Felix","Zara","Malik","Clara","Sven","Isla","Wei"]'),
l(a) AS (SELECT '["Okafor","Lindqvist","Moreau","Tanaka","Herrera","Novak","Fischer","Kowalski","Silva","Haddad","Nguyen","Brennan","Rossi","Park","Mensah","Jansen","Duarte","Kaur","Weber","Ito","Castillo","Murphy","Andersen","Costa","Zhang","Dubois","Ali","Svensson","Romero"]'),
c(a) AS (SELECT '["United States","Germany","United Kingdom","Japan","Canada","France","Australia","Netherlands","Sweden","Spain","Singapore","Brazil"]')
INSERT INTO customers(first_name, last_name, email, country, marketing_opt_in, created_at)
SELECT fn, ln,
       lower(replace(replace(replace(replace(fn, 'é', 'e'), 'í', 'i'), 'á', 'a'), 'ó', 'o')) || '.' || lower(ln) || i || '@' ||
         json_extract('["gmail.com","outlook.com","proton.me","icloud.com","fastmail.com"]', '$[' || (i * 7 % 5) || ']'),
       json_extract(c.a, '$[' || (i * 5 % 12) || ']'),
       (i * 3 % 5) < 3,
       datetime('2025-01-03 08:00:00', '+' || (i * 37 % 300) || ' days', '+' || (i * 53 % 720) || ' minutes')
FROM (SELECT i,
             json_extract(f.a, '$[' || (i % 32) || ']') AS fn,
             json_extract(l.a, '$[' || (i * 11 % 29) || ']') AS ln
      FROM n, f, l), c;

CREATE TABLE addresses (
  id          INTEGER PRIMARY KEY,
  customer_id INTEGER NOT NULL REFERENCES customers(id),
  label       TEXT NOT NULL,
  line1       TEXT NOT NULL,
  city        TEXT NOT NULL,
  postal_code TEXT NOT NULL,
  country     TEXT NOT NULL,
  is_default  BOOLEAN NOT NULL DEFAULT 0
);
WITH cities(country, a) AS (VALUES
  ('United States',  '["Portland","Seattle","Brooklyn","Austin","Oakland"]'),
  ('Germany',        '["Berlin","Hamburg","Munich","Leipzig","Cologne"]'),
  ('United Kingdom', '["London","Bristol","Manchester","Edinburgh","Leeds"]'),
  ('Japan',          '["Tokyo","Kyoto","Osaka","Fukuoka","Sapporo"]'),
  ('Canada',         '["Toronto","Vancouver","Montréal","Calgary","Ottawa"]'),
  ('France',         '["Paris","Lyon","Bordeaux","Nantes","Lille"]'),
  ('Australia',      '["Melbourne","Sydney","Brisbane","Perth","Adelaide"]'),
  ('Netherlands',    '["Amsterdam","Utrecht","Rotterdam","Haarlem","Leiden"]'),
  ('Sweden',         '["Stockholm","Gothenburg","Malmö","Uppsala","Lund"]'),
  ('Spain',          '["Madrid","Barcelona","Valencia","Seville","Bilbao"]'),
  ('Singapore',      '["Singapore","Singapore","Singapore","Singapore","Singapore"]'),
  ('Brazil',         '["São Paulo","Rio de Janeiro","Belo Horizonte","Curitiba","Recife"]')),
streets(a) AS (SELECT '["Maple Street","Harbour Road","Linden Allee","Rue des Écoles","Canal Street","Elm Avenue","Kings Road","Sakura-dori","Calle Mayor","Queen Street"]')
INSERT INTO addresses(customer_id, label, line1, city, postal_code, country, is_default)
SELECT cu.id, 'Home',
       (cu.id * 17 % 180 + 2) || ' ' || json_extract(streets.a, '$[' || (cu.id % 10) || ']'),
       json_extract(cities.a, '$[' || (cu.id * 3 % 5) || ']'),
       printf('%05d', cu.id * 7919 % 99999),
       cu.country, 1
FROM customers cu JOIN cities ON cities.country = cu.country, streets;
WITH cities(country, a) AS (VALUES
  ('United States', '["Portland","Seattle","Brooklyn","Austin","Oakland"]'),
  ('Germany',       '["Berlin","Hamburg","Munich","Leipzig","Cologne"]'),
  ('Japan',         '["Tokyo","Kyoto","Osaka","Fukuoka","Sapporo"]'),
  ('France',        '["Paris","Lyon","Bordeaux","Nantes","Lille"]'))
INSERT INTO addresses(customer_id, label, line1, city, postal_code, country, is_default)
SELECT cu.id, 'Office', (cu.id * 13 % 90 + 1) || ' Market Street, Floor ' || (cu.id % 9 + 2),
       json_extract(cities.a, '$[' || (cu.id * 2 % 5) || ']'),
       printf('%05d', cu.id * 4211 % 99999), cu.country, 0
FROM customers cu JOIN cities ON cities.country = cu.country
WHERE cu.id % 3 = 0;

CREATE TABLE orders (
  id             INTEGER PRIMARY KEY,
  customer_id    INTEGER NOT NULL REFERENCES customers(id),
  status         TEXT NOT NULL CHECK (status IN ('pending', 'paid', 'shipped', 'delivered', 'cancelled', 'refunded')),
  channel        TEXT NOT NULL,
  subtotal_cents INTEGER NOT NULL DEFAULT 0,
  shipping_cents INTEGER NOT NULL DEFAULT 0,
  total_cents    INTEGER NOT NULL DEFAULT 0,
  currency       TEXT NOT NULL DEFAULT 'USD',
  placed_at      TEXT NOT NULL
);
CREATE INDEX idx_orders_customer ON orders(customer_id);
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 460)
INSERT INTO orders(customer_id, status, channel, shipping_cents, placed_at)
SELECT (i * 97 % 180) + 1,
       CASE WHEN i % 23 = 0 THEN 'cancelled' WHEN i % 41 = 0 THEN 'refunded' WHEN i > 450 THEN 'pending'
            WHEN i > 438 THEN 'paid' WHEN i > 420 THEN 'shipped' ELSE 'delivered' END,
       json_extract('["web","web","web","mobile","mobile","cafe","wholesale"]', '$[' || (i * 5 % 7) || ']'),
       CASE WHEN i % 4 = 0 THEN 0 ELSE 595 END,
       datetime('2025-03-01 07:00:00', '+' || (i * 600 / 460) || ' days', '+' || (i * 211 % 840) || ' minutes')
FROM n;

CREATE TABLE order_items (
  id               INTEGER PRIMARY KEY,
  order_id         INTEGER NOT NULL REFERENCES orders(id),
  product_id       INTEGER NOT NULL REFERENCES products(id),
  quantity         INTEGER NOT NULL CHECK (quantity > 0),
  unit_price_cents INTEGER NOT NULL
);
CREATE INDEX idx_items_order ON order_items(order_id);
CREATE INDEX idx_items_product ON order_items(product_id);
-- 每单 1-3 行;按权重挑商品,单品豆里热门的(Geisha、肯尼亚、哥伦比亚)出现得更多
WITH RECURSIVE k(j) AS (SELECT 0 UNION ALL SELECT j + 1 FROM k WHERE j < 2),
w(a, n) AS (SELECT '[1,1,2,2,2,3,3,3,4,5,5,6,6,6,7,8,8,9,10,10,11,12,13,14,14,14,14,15,15,15,16,16,17,17,18,19,20,21,22,23,1,6,2,8,3]', 45)
INSERT INTO order_items(order_id, product_id, quantity, unit_price_cents)
SELECT o.id, p.id, q, p.price_cents
FROM (SELECT o.id AS oid, k.j,
             json_extract(w.a, '$[' || ((o.id * 31 + k.j * 17 + o.id * k.j * 7) % w.n) || ']') AS pid,
             CASE WHEN (o.id + k.j) % 5 = 0 THEN 3 WHEN (o.id + k.j) % 3 = 0 THEN 2 ELSE 1 END AS q
      FROM orders o, k, w
      WHERE k.j < 1 + (o.id * 7 % 3)) x
JOIN orders o ON o.id = x.oid
JOIN products p ON p.id = x.pid;

UPDATE orders SET subtotal_cents = (SELECT COALESCE(SUM(quantity * unit_price_cents), 0) FROM order_items WHERE order_id = orders.id);
UPDATE orders SET shipping_cents = 0 WHERE subtotal_cents >= 5000;
UPDATE orders SET total_cents = subtotal_cents + shipping_cents;

CREATE TABLE payments (
  id           INTEGER PRIMARY KEY,
  order_id     INTEGER NOT NULL REFERENCES orders(id),
  method       TEXT NOT NULL,
  provider_ref TEXT,
  amount_cents INTEGER NOT NULL,
  status       TEXT NOT NULL,
  paid_at      TEXT
);
INSERT INTO payments(order_id, method, provider_ref, amount_cents, status, paid_at)
SELECT id,
       json_extract('["card","card","card","apple_pay","paypal","google_pay"]', '$[' || (id * 7 % 6) || ']'),
       'pi_' || lower(hex(randomblob(6))),
       total_cents,
       CASE status WHEN 'pending' THEN 'authorized' WHEN 'cancelled' THEN 'voided' WHEN 'refunded' THEN 'refunded' ELSE 'captured' END,
       CASE WHEN status IN ('pending', 'cancelled') THEN NULL ELSE datetime(placed_at, '+' || (id % 9 + 1) || ' minutes') END
FROM orders;

CREATE TABLE reviews (
  id          INTEGER PRIMARY KEY,
  product_id  INTEGER NOT NULL REFERENCES products(id),
  customer_id INTEGER NOT NULL REFERENCES customers(id),
  rating      INTEGER NOT NULL CHECK (rating BETWEEN 1 AND 5),
  title       TEXT,
  body        TEXT,
  created_at  TEXT NOT NULL
);
WITH t(a) AS (SELECT '["Stunning cup","Bright and juicy","My new daily","Great as espresso","A little flat this time","Perfect for pour-over","Smooth and sweet","Not for me","Worth every cent","Lovely florals"]'),
b(a) AS (SELECT '["Dialed in at 1:16 on the V60 and it sang.","Arrived two days after roast, beautifully packed.","Sweet, clean, and very forgiving with grind size.","Works nicely with milk; chocolate notes come through.","Tasted a bit roasty compared to the last bag.","Fruity without being sour. Will reorder.","Exactly what the tasting notes promised.","Too bright for my taste, but well roasted."]')
INSERT INTO reviews(product_id, customer_id, rating, title, body, created_at)
SELECT oi.product_id, o.customer_id,
       CASE WHEN (oi.id * 7) % 10 < 5 THEN 5 WHEN (oi.id * 7) % 10 < 8 THEN 4 WHEN (oi.id * 7) % 10 < 9 THEN 3 ELSE 2 END
         - CASE WHEN oi.product_id IN (9, 16) AND oi.id % 4 = 0 THEN 1 ELSE 0 END,
       json_extract(t.a, '$[' || (oi.id % 10) || ']'),
       json_extract(b.a, '$[' || (oi.id * 3 % 8) || ']'),
       datetime(o.placed_at, '+' || (oi.id % 12 + 5) || ' days')
FROM order_items oi JOIN orders o ON o.id = oi.order_id, t, b
WHERE oi.id % 3 = 0 AND o.status = 'delivered';

CREATE TABLE subscriptions (
  id                INTEGER PRIMARY KEY,
  customer_id       INTEGER NOT NULL REFERENCES customers(id),
  product_id        INTEGER NOT NULL REFERENCES products(id),
  cadence           TEXT NOT NULL CHECK (cadence IN ('weekly', 'biweekly', 'monthly')),
  bags_per_delivery INTEGER NOT NULL DEFAULT 1,
  status            TEXT NOT NULL,
  started_at        TEXT NOT NULL,
  next_delivery     TEXT
);
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 72)
INSERT INTO subscriptions(customer_id, product_id, cadence, bags_per_delivery, status, started_at, next_delivery)
SELECT (i * 43 % 180) + 1,
       json_extract('[14,15,1,2,3,16,17,5,8,14,18]', '$[' || (i % 11) || ']'),
       json_extract('["monthly","biweekly","weekly","monthly","biweekly"]', '$[' || (i % 5) || ']'),
       i % 3 + 1,
       CASE WHEN i % 9 = 0 THEN 'cancelled' WHEN i % 7 = 0 THEN 'paused' ELSE 'active' END,
       date('2025-02-01', '+' || (i * 4) || ' days'),
       CASE WHEN i % 9 = 0 THEN NULL ELSE date('2025-11-12', '+' || (i % 14) || ' days') END
FROM n;
SQL

conn() { # id name kind host port user database filePath
  printf '{"id":"%s","name":"%s","kind":"%s","env":"local","host":"%s","port":%s,"user":"%s","database":"%s","filePath":%s,"sslMode":"disable"}' \
    "$1" "$2" "$3" "$4" "$5" "$6" "$7" "$8"
}

{
  printf '[\n  %s' "$(conn demo-roastery roastery sqlite '' 0 '' '' "\"$DB\"")"
  printf ',\n  %s' "$(conn demo-analytics analytics pg 127.0.0.1 55432 postgres analytics null)"
  printf ',\n  %s' "$(conn demo-shop shop mysql 127.0.0.1 53306 root shop null)"
  printf ',\n  %s' "$(conn demo-cache cache redis 127.0.0.1 56379 '' 0 null)"
  printf '\n]\n'
} > "$DIR/connections.json"

cat > "$DIR/ui_prefs.json" <<'JSON'
{
  "theme": "system",
  "language": "en"
}
JSON

echo "demo config ready: $DIR"
