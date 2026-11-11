CREATE TABLE categories(id INTEGER PRIMARY KEY, name TEXT NOT NULL);
INSERT INTO categories(name) VALUES ('Single Origin'),('Blend'),('Decaf'),('Gear');
CREATE TABLE products(id INTEGER PRIMARY KEY, category_id INTEGER NOT NULL, sku TEXT, name TEXT NOT NULL, origin TEXT, roast_level TEXT, price_cents INTEGER, stock INTEGER, attributes TEXT, active BOOLEAN, created_at TEXT);
CREATE INDEX idx_products_cat ON products(category_id);
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 250)
INSERT INTO products(category_id, sku, name, origin, roast_level, price_cents, stock, attributes, active, created_at)
SELECT (i % 4)+1, printf('SO-%03d', i), 'Coffee ' || i,
 CASE i % 6 WHEN 0 THEN 'Ethiopia' WHEN 1 THEN 'Kenya' WHEN 2 THEN 'Colombia' WHEN 3 THEN NULL WHEN 4 THEN 'Brazil' ELSE '巴拿马' END,
 CASE i % 3 WHEN 0 THEN 'light' WHEN 1 THEN 'medium' ELSE 'dark' END,
 1000 + (i * 37) % 5000, (i * 13) % 200,
 '{"process":"washed","altitude_m":' || (1200 + i) || '}', i % 5 != 0, '2025-04-' || printf('%02d', (i % 28)+1) || ' 08:00:00'
FROM n;
CREATE TABLE orders(id INTEGER PRIMARY KEY, product_id INTEGER, quantity INTEGER, note TEXT);
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 400)
INSERT INTO orders(product_id, quantity, note) SELECT (i % 250)+1, (i % 7)+1, CASE WHEN i % 9 = 0 THEN NULL ELSE 'order ' || i END FROM n;
