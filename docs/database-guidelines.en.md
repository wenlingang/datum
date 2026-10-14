# Database Development Guidelines

Follow these when AI generates or modifies SQL. Markers: [MUST] a violation makes the answer wrong; [MUST NOT] must never appear; [SHOULD] preferred, reasonable exceptions allowed (state why). Rules without a dialect apply to both PostgreSQL and MySQL; SQLite and Redis have their own sections.

## 0. General

- [MUST] Generate SQL for the connected database's type and version; never mix dialects (e.g. `::int` or `ILIKE` in MySQL; backticks, `AUTO_INCREMENT` or `LIMIT m, n` in PostgreSQL).
- [MUST] Write SQL only against known schemas; when unsure of tables, columns or types, query metadata first (`information_schema`, MySQL `SHOW CREATE TABLE`, SQLite `PRAGMA table_info`); never invent columns.
- [MUST] Application SQL passes values via placeholders (`$1` / `?`); [MUST NOT] concatenate user input into SQL.
- [MUST] For data changes, schema changes or deletions, state the impact and risks and give a way to verify or roll back.
- [SHOULD] Uppercase keywords, lowercase identifiers, indent complex statements; read-only requests get read-only statements.

## 1. Naming

- [MUST] Database, table, column and index names use only lowercase letters, digits and underscores (snake_case), starting with a letter; [MUST NOT] use uppercase, leading digits, spaces, hyphens or non-ASCII characters.
- [MUST NOT] use reserved words as identifiers (e.g. `user`, `order`, `group`, `desc`, `key`, `range`, `match`, `rank`); legacy names must be quoted.
- [MUST NOT] rely on quoted case-sensitive identifiers (in PG `"UserName"` and `username` differ).
- [SHOULD] Name tables "domain_entity" (e.g. `order_items`, `account_configs`), with consistent singular/plural style per database; name the database after the application.
- [SHOULD] Same-meaning columns share name and type across tables (e.g. always `user_id bigint`); no meaningless abbreviations.
- [SHOULD] Name boolean columns `is_xxx` / `has_xxx`, not `deleted`, `if_deleted` or `flag`; use the `_at` suffix for points in time and `_date` for dates.
- [MUST] Index naming: unique `uk_<table>_<columns…>`, regular `idx_<table>_<columns…>`. PG identifiers max 63 bytes, MySQL 64 characters; truncation may cause collisions, so abbreviate column names but keep the full table name.
- [SHOULD] Give temp/backup tables clear prefixes/suffixes (e.g. `tmp_`, `_bak_20250101`) and clean them up.

## 2. Tables and Column Types

### General

- [MUST] Every table has a primary key, named `id` by default; integer primary keys use `bigint`, not `int`.
- [MUST] Every table has `created_at` and `updated_at`, `NOT NULL` with defaults; `updated_at` changes on every UPDATE (MySQL `ON UPDATE`; PG a trigger or explicit `SET updated_at = now()`).
- [MUST] Comment the table and every column except `id`; enum/status comments list every value's meaning; update comments when a column's meaning or enum values change.
- [SHOULD] Columns default to `NOT NULL` with a sensible default; allow NULL only when "unknown" means something.
- [MUST] Use `decimal(p,s)` / `numeric(p,s)` for exact decimals such as money; [MUST NOT] store money in `float`/`double`/`real`; [MUST NOT] use the PG `money` type.
- [SHOULD] Use `smallint` (MySQL may use `tinyint`) for status/type columns, not native `ENUM` (changing values requires altering the table).
- [MUST NOT] store points in time as strings; write all times in UTC (exceptions such as date-only partition keys must document the format in the comment).
- [SHOULD] Move long text and large JSON out of tables with narrow, hot columns into a table joined by primary key; [MUST NOT] store images/files in the database, only their location.
- [SHOULD] Moderate redundancy to reduce JOINs is fine; redundant columns should rarely change and not be long text.
- [SHOULD] Use soft deletes (`is_deleted` or `deleted_at`) when traceability matters; unique indexes must include the flag or be partial; avoid granting hard-delete privileges to application accounts.
- [SHOULD] Avoid foreign keys and cascades (`ON DELETE CASCADE`); enforce referential integrity in the application; existing foreign key columns must be indexed.
- [SHOULD] Pick sufficient types and lengths up front; later type changes usually rewrite or lock the table. Keep tables under 50 columns.

### MySQL

- [MUST] Engine `InnoDB`, charset `utf8mb4`; [MUST NOT] use `utf8`/`utf8mb3` (cannot store 4-byte characters such as emoji); collation matches the database.
- [MUST] Primary key `id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY`; [MUST NOT] use random UUID strings as the clustered primary key (page splits); if needed, add a unique column or store ordered UUIDs as `BINARY(16)`.
- [MUST] `created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP`, `updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP`; `DATETIME(3)` for milliseconds; [SHOULD] avoid `TIMESTAMP` (2038 limit, session time zone dependent).
- [MUST] Use `varchar(n)` with ample length (exact for fixed-length codes, 50 for short text, 255 in general); beyond ~5000 characters use `text` and consider a separate table.
- [SHOULD] Add `UNSIGNED` to integers that are never negative; use `tinyint` for booleans (1 = yes, 0 = no); use the `JSON` type, and index a generated column when querying by inner fields.
- [MUST] Column comments use `COMMENT '...'`, table comments use `COMMENT='...'`.

### PostgreSQL

- [MUST] Identity primary keys: `id BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY` (allows explicit inserts) or `GENERATED ALWAYS AS IDENTITY`; [MUST NOT] use `serial`/`bigserial`. UUID keys use native `uuid`, not `text`.
- [MUST] Points in time use `timestamptz` (`created_at TIMESTAMPTZ NOT NULL DEFAULT now()`); [MUST NOT] store points in time in `timestamp` without time zone; [MUST NOT] use `timetz` or `CURRENT_TIME`.
- [SHOULD] Use `text` for strings, adding `CHECK (length(col) <= n)` or using `varchar(n)` when a limit is needed; [MUST NOT] use `char(n)`.
- [SHOULD] Use `boolean` (or `smallint` to match existing tables); use `jsonb`, not `json`, and move frequently queried attributes into regular columns.
- [MUST] Comments use `COMMENT ON TABLE` / `COMMENT ON COLUMN t.c IS '...'`.
- [SHOULD] Put objects in an explicit schema, qualify as `schema.table` when needed rather than relying on `search_path`; express value constraints with `CHECK`.

## 3. Indexes

- [SHOULD] Beyond the primary key, index according to actual query conditions; [MUST NOT] index columns that no query uses.
- [MUST] Business-unique fields or combinations get a unique index; do not rely on application checks alone.
- [MUST] JOIN columns are indexed on the joined table, with identical type, charset and collation on both sides (else implicit conversion defeats the index).
- [MUST] Composite indexes follow the leftmost prefix: equality columns first, range columns last, more selective ones left; an `ORDER BY` column right after them avoids a sort; columns after a range condition cannot serve sorting.
- [MUST NOT] create redundant indexes: with `(a, b)` in place, do not add `(a)`; composite indexes differing only in column order need justification.
- [SHOULD] At most 4 columns per index and 5–8 indexes per table; fewer for write-heavy tables.
- [SHOULD] Do not index low-selectivity columns (status/type/boolean) alone; put them late in composite indexes; PG can use a partial index (`WHERE status = 1`); [MUST NOT] create Hash indexes on them.
- [SHOULD] Use covering indexes to avoid table lookups (PG can use `INCLUDE (...)`).
- [MUST NOT] index whole `text`/`blob` columns in MySQL; use prefix indexes `col(20)` for long `varchar`.
- [SHOULD] In PG, use expression indexes for function-based queries (e.g. `lower(email)`), GIN for `jsonb` containment, BRIN for huge time-appended tables; GiST etc. only for special cases like geometry and ranges.
- Common reasons an index goes unused (avoid them): implicit conversion (`varchar` column vs. number), functions or arithmetic on the column (`DATE(created_at) = ...`), leading wildcard `LIKE '%x'`, `<>`/`NOT IN`, `OR` across columns, breaking the leftmost prefix, overly broad conditions.
- [SHOULD] Include `EXPLAIN` with tuning advice; PG `EXPLAIN ANALYZE` really executes the statement, so wrap writes in a transaction and roll back.

## 4. Writing SQL

### Queries

- [MUST NOT] use `SELECT *` (except for interactive exploration and `EXISTS` subqueries); list the needed columns explicitly.
- [MUST] Qualify every column with a table name or alias in multi-table queries.
- [MUST] Count with `COUNT(*)`, distinct counts with `COUNT(DISTINCT col)`; `SUM` over all-NULL values is NULL, so use `COALESCE(SUM(col), 0)` when needed.
- [MUST] Test for null with `IS NULL` / `IS NOT NULL`; [MUST NOT] use `= NULL`; in PG use `IS [NOT] DISTINCT FROM` for null-safe comparison.
- [MUST NOT] use `NOT IN (SELECT ...)` when the subquery may return NULL (the result is empty); use `NOT EXISTS`.
- [MUST] Constants in conditions match the column type (string literals for string columns).
- [SHOULD] Use half-open time ranges `col >= :start AND col < :end`; do not wrap the column in functions such as `DATE()`.
- [MUST NOT] use `LIKE '%x'` / `'%x%'` in online queries on large tables; for fuzzy search use full-text (MySQL FULLTEXT, PG `pg_trgm`/`tsvector`) or a search engine.
- [SHOULD] Keep `IN (...)` to a few hundred elements, otherwise JOIN a temp table/`VALUES` or batch; use `=` for a single value.
- [SHOULD] Consider rewriting `OR` across columns as `UNION ALL`; prefer `UNION ALL` over `UNION` when deduplication is unnecessary.
- [SHOULD] `ORDER BY` must be deterministic (append the primary key), or pagination duplicates/drops rows.
- [SHOULD] In `GROUP BY` queries, SELECT only grouping columns or aggregate expressions (compatible with `ONLY_FULL_GROUP_BY`).
- [MUST NOT] use `ORDER BY RAND()` / `random()` on large tables.

### Pagination

- [MUST] User-facing list queries must have a `LIMIT`; read large result sets (e.g. exports) in batches.
- [SHOULD] Replace deep pagination with keyset pagination: `WHERE id < :last_id ORDER BY id DESC LIMIT n`, comparing `(created_at, id)` for multi-column sorts; otherwise use a deferred join (primary keys first, then rows).
- [SHOULD] Query the total count separately from the list; avoid an exact `COUNT(*)` on every request for large tables.

### JOINs and Subqueries

- [SHOULD] Join at most 3 tables per statement; beyond that split the query or add redundant columns.
- [MUST] Specify the `ON` condition for every JOIN; [MUST NOT] produce unintended Cartesian products.
- [MUST] Filters on the right table of a `LEFT JOIN` go in `ON`; putting them in `WHERE` degrades it into an inner join.
- [SHOULD] Check existence with `EXISTS`, not `COUNT(*) > 0`; rewrite correlated subqueries on large tables as JOINs; use CTEs (`WITH`) for readability.

### Bulk Writes

- [MUST] `INSERT` lists column names explicitly and does not rely on column order.
- [SHOULD] Bulk insert with multi-row `INSERT ... VALUES (...), (...)`, 500–1000 rows per batch; use `COPY` for large PG imports.
- [SHOULD] Use upsert for idempotent writes: PG/SQLite `ON CONFLICT (cols) DO UPDATE/NOTHING`, MySQL `ON DUPLICATE KEY UPDATE`; [SHOULD] avoid MySQL `REPLACE INTO` (delete + insert changes the primary key).
- [SHOULD] When the written result is needed, use `RETURNING` in PG/SQLite instead of querying again.

## 5. Write Safety

- [MUST] `UPDATE` / `DELETE` must have a `WHERE` that precisely targets the rows; [MUST NOT] modify a whole table with no condition or an always-true condition (`WHERE 1=1`).
- [MUST] Before `UPDATE` / `DELETE`, first give a `SELECT COUNT(*)` with the same condition (plus details if needed) to confirm the affected rows; run it only if as expected.
- [MUST] Run large updates/deletes (over ~10,000 rows) in batches of 1000–5000 rows by primary key range or `LIMIT`, committing each batch; PG `UPDATE/DELETE` lacks `LIMIT`: use `WHERE id IN (SELECT id ... LIMIT n)` or key ranges.
- [SHOULD] Back up affected rows before changing important data (`CREATE TABLE t_bak_<date> AS SELECT ...`) and give a rollback statement.
- [SHOULD] Do manual data fixes in an explicit transaction: `BEGIN` → execute → check the affected row count → `COMMIT` or `ROLLBACK`.
- [MUST NOT] join multiple assignments in `UPDATE ... SET` with `AND` (MySQL silently evaluates it as a boolean expression); separate them with commas.
- [SHOULD] Use `TRUNCATE` to empty a table (faster than unconditional `DELETE`), but it resets auto-increment and cannot be rolled back in MySQL; only for maintenance, never in application code.
- [MUST] Confirm no code uses a table/column before dropping it; `RENAME` a table first, observe a while, then `DROP` if nothing breaks; [MUST NOT] directly `DROP TABLE` / `DROP DATABASE` production objects.
- [MUST NOT] execute `DROP`, `TRUNCATE`, `GRANT` and similar statements in application code.

## 6. DDL and Schema Changes

- [MUST] Write changes as re-runnable scripts: `IF NOT EXISTS` for creating tables/indexes, `IF EXISTS` for dropping.
- [SHOULD] Keep DDL and DML out of the same transaction or script batch (except seed data); MySQL DDL implicitly commits.
- [MUST] When changing indexes, create the new one first and drop the old one after confirming it works; [MUST NOT] drop first (the gap means full table scans).
- [MUST] Rename, retype or drop columns via expand–migrate–contract: add the new column and dual-write → backfill → switch reads → drop the old column.
- [SHOULD] Add new columns at the end, nullable or with a constant default, to avoid rewrites; change large tables off-peak.

### PostgreSQL

- [MUST] Create/drop indexes on production tables with `CREATE INDEX CONCURRENTLY` / `DROP INDEX CONCURRENTLY`; they cannot run in a transaction block and must run alone.
- [MUST] A failed `CONCURRENTLY` leaves an `INVALID` index; drop it before recreating. Statements depending on it (e.g. `ADD CONSTRAINT ... USING INDEX`) run separately after it succeeds.
- [SHOULD] Run `SET lock_timeout = '3s'` before DDL so it fails and retries when the lock is unavailable, rather than a queued DDL blocking all later reads and writes.
- [SHOULD] Add constraints in two steps: `ADD CONSTRAINT ... NOT VALID`, then `VALIDATE CONSTRAINT` (non-blocking); for `NOT NULL`, first add and validate `CHECK (col IS NOT NULL) NOT VALID`, then `SET NOT NULL` (PG 12+ skips the full table scan).
- [SHOULD] In PG 11+, adding a column with a constant default does not rewrite the table; volatile defaults (e.g. `random()`) and most type changes rewrite the table and hold an exclusive lock.
- [SHOULD] Run `ANALYZE <table>` after large data changes.

### MySQL

- [SHOULD] Specify `ALGORITHM=INSTANT` or `ALGORITHM=INPLACE, LOCK=NONE` in `ALTER TABLE` so it errors when unsupported instead of falling back to a table-locking `COPY`.
- [SHOULD] In 8.0, adding columns and changing defaults are usually `INSTANT`, adding secondary indexes `INPLACE, LOCK=NONE`; changing column types, charsets or the primary key usually rebuilds the table.
- [MUST] Online DDL still briefly takes a metadata lock (MDL): first confirm no long transactions or queries are on the table, or the DDL and every later request will block.
- [SHOULD] Use `gh-ost` / `pt-online-schema-change` for large tables; merge multiple changes to the same table into one `ALTER TABLE`.

## 7. Transactions and Locks

- [MUST] Keep transactions short: no RPC, file I/O or waiting for user input inside; [MUST NOT] stay idle in a transaction for long (PG `idle in transaction`).
- [SHOULD] Avoid long transactions: they hold locks, block dead tuple cleanup in PG (bloat) and bloat undo in MySQL.
- [SHOULD] Put statements that lock a lot or update hot rows late in the transaction to shorten lock time.
- [SHOULD] Update rows in the same order across transactions (e.g. by primary key) to reduce deadlocks; retry deadlocks/serialization failures a limited number of times.
- [SHOULD] `SELECT ... FOR UPDATE` conditions must use an index (MySQL otherwise locks many rows); queue consumers use `FOR UPDATE SKIP LOCKED`.
- [SHOULD] Mind the default isolation level: MySQL InnoDB is `REPEATABLE READ` (with gap locks), PG is `READ COMMITTED`.
- [SHOULD] Update counters atomically (`SET n = n + 1`) or with version-based optimistic locking (`WHERE id = ? AND version = ?`) to avoid lost updates from read-then-write.

## 8. SQLite Notes

- [MUST] Foreign keys are off by default; when needed run `PRAGMA foreign_keys = ON` per connection (not persisted).
- [SHOULD] For concurrent reads and writes enable `PRAGMA journal_mode = WAL` (persisted in the file) and set `busy_timeout` to avoid `database is locked`; do not use WAL on network file systems.
- [SHOULD] Column types are only "affinities", not enforced; for strict typing (3.37+) add `STRICT` to the table.
- [MUST] Write auto-increment primary keys as `id INTEGER PRIMARY KEY` (it must be `INTEGER`, not `INT`, to be a rowid alias); add `AUTOINCREMENT` only when deleted IDs must never be reused.
- [SHOULD] No native date or boolean types: store all times consistently as ISO-8601 UTC text or Unix timestamp integers, booleans as 0/1.
- [MUST] `ALTER TABLE` is limited to renaming tables, `RENAME COLUMN` (3.25+), `ADD COLUMN` and `DROP COLUMN` (3.35+, restricted); otherwise "create new table – copy data – drop old – rename" in one transaction.
- [SHOULD] Wrap bulk writes in one transaction (otherwise each row is flushed separately, orders of magnitude slower); only one writer at a time, so avoid long write transactions.
- [SHOULD] `COMMENT` is not supported, so put descriptions in script comments; use `COLLATE NOCASE` for case-insensitive comparison.
- [SHOULD] After large deletions `VACUUM` reclaims space (rewrites the whole file and locks the database; not at peak times).

## 9. Redis Notes

- [MUST] Layer keys with colons: `domain:object:ID[:field]` (e.g. `app:user:10086:profile`); use only printable ASCII without spaces or newlines, short and readable (≤ 128 bytes recommended).
- [MUST] Except data meant to stay resident, every key gets an expiration, set in one step with `SET k v EX <seconds>`; add a random offset for keys written in the same batch to avoid mass expiry (avalanche).
- [MUST NOT] run blocking/dangerous commands in production (`KEYS`, `FLUSHALL`, `FLUSHDB`, `CONFIG SET`, `DEBUG`, `SHUTDOWN`); iterate keys in batches with `SCAN` + `MATCH` + `COUNT`.
- [MUST NOT] run `HGETALL`, `SMEMBERS`, `LRANGE 0 -1` or `ZRANGE 0 -1` directly on large collections; use `HSCAN`/`SSCAN`/`ZSCAN` or read in ranges.
- [SHOULD] Avoid big keys: string values ≤ 10KB, hash/list/set/zset ≤ 5000 elements; beyond that split them, e.g. by ID modulo.
- [MUST] Delete big keys with `UNLINK` (4.0+, async) or shrink them in batches first; [MUST NOT] `DEL` a big key directly; bulk-delete by pattern with `SCAN` + `UNLINK`.
- [SHOULD] Spread hot-key load with a local cache, read replicas or several copy keys; prevent cache penetration by caching empty values (short TTL) or a Bloom filter.
- [SHOULD] Use `MGET`/`MSET`/pipelines for bulk access, a few hundred per batch; in cluster mode multi-key commands need the same slot, so use hash tags `{...}` if necessary.
- [SHOULD] Use Lua or `MULTI`/`EXEC` for multi-step atomic operations, keeping scripts short; distributed locks use `SET key token NX PX <ms>` and are released by Lua that checks the token before deleting.
- [SHOULD] Pick structures by data shape: hash for objects, `INCR` for counters, zset for rankings, set/HyperLogLog for dedup; never use Redis as the only persistent store.

## 10. Examples (follow this style when generating)

MySQL table:

```sql
CREATE TABLE IF NOT EXISTS orders (
  id          BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  order_no    VARCHAR(32)     NOT NULL COMMENT 'Order number',
  user_id     BIGINT UNSIGNED NOT NULL COMMENT 'ID of the ordering user',
  status      TINYINT         NOT NULL DEFAULT 0 COMMENT 'Status: 0 pending payment, 1 paid, 2 cancelled',
  amount      DECIMAL(12,2)   NOT NULL DEFAULT 0 COMMENT 'Order amount (CNY)',
  paid_at     DATETIME        NULL COMMENT 'Payment time (UTC), NULL if unpaid',
  is_deleted  TINYINT         NOT NULL DEFAULT 0 COMMENT 'Deleted: 0 no, 1 yes',
  created_at  DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP COMMENT 'Created at (UTC)',
  updated_at  DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP COMMENT 'Updated at (UTC)',
  UNIQUE KEY uk_orders_order_no (order_no),
  KEY idx_orders_user_id_created_at (user_id, created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_0900_ai_ci COMMENT='Orders';
```

PostgreSQL table:

```sql
CREATE TABLE IF NOT EXISTS orders (
  id          BIGINT GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
  order_no    TEXT          NOT NULL CHECK (length(order_no) <= 32),
  user_id     BIGINT        NOT NULL,
  status      SMALLINT      NOT NULL DEFAULT 0 CHECK (status IN (0, 1, 2)),
  amount      NUMERIC(12,2) NOT NULL DEFAULT 0,
  paid_at     TIMESTAMPTZ,
  is_deleted  BOOLEAN       NOT NULL DEFAULT false,
  created_at  TIMESTAMPTZ   NOT NULL DEFAULT now(),
  updated_at  TIMESTAMPTZ   NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS uk_orders_order_no ON orders (order_no);
CREATE INDEX IF NOT EXISTS idx_orders_user_id_created_at ON orders (user_id, created_at);
COMMENT ON TABLE orders IS 'Orders';
COMMENT ON COLUMN orders.status IS 'Status: 0 pending payment, 1 paid, 2 cancelled';
COMMENT ON COLUMN orders.amount IS 'Order amount (CNY)';
COMMENT ON COLUMN orders.paid_at IS 'Payment time, NULL if unpaid';
```

SQLite table:

```sql
CREATE TABLE IF NOT EXISTS orders (
  id          INTEGER PRIMARY KEY,
  order_no    TEXT    NOT NULL UNIQUE,
  user_id     INTEGER NOT NULL,
  status      INTEGER NOT NULL DEFAULT 0,  -- 0 pending payment, 1 paid, 2 cancelled
  amount      TEXT    NOT NULL DEFAULT '0', -- amount stored as a decimal string to avoid floating-point error
  created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;
CREATE INDEX IF NOT EXISTS idx_orders_user_id ON orders (user_id);
```

Keyset pagination (next page; the last row of the previous page is :last_created_at / :last_id):

```sql
SELECT o.id, o.order_no, o.amount, o.created_at
FROM orders o
WHERE o.user_id = :user_id
  AND (o.created_at, o.id) < (:last_created_at, :last_id)
ORDER BY o.created_at DESC, o.id DESC
LIMIT 20;
```

Safe data modification (confirm the affected row count first, then execute in a transaction and verify):

```sql
SELECT COUNT(*) FROM orders WHERE status = 0 AND created_at < '2025-01-01';
BEGIN;
UPDATE orders SET status = 2, updated_at = now()  -- in MySQL use CURRENT_TIMESTAMP or omit it
WHERE status = 0 AND created_at < '2025-01-01';
-- COMMIT if the affected row count matches the count above, otherwise ROLLBACK
COMMIT;
```

Deleting large amounts of data in batches (commit each batch separately; repeat until 0 rows are affected):

```sql
-- MySQL
DELETE FROM operation_logs WHERE created_at < '2025-01-01' ORDER BY id LIMIT 2000;
-- PostgreSQL
DELETE FROM operation_logs
WHERE id IN (SELECT id FROM operation_logs WHERE created_at < '2025-01-01' ORDER BY id LIMIT 2000);
```

Online schema changes:

```sql
-- PostgreSQL: create indexes and add constraints without blocking reads/writes (run each statement separately, not in a transaction block)
SET lock_timeout = '3s';
CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_orders_paid_at ON orders (paid_at);
ALTER TABLE orders ADD CONSTRAINT ck_orders_amount_non_negative CHECK (amount >= 0) NOT VALID;
ALTER TABLE orders VALIDATE CONSTRAINT ck_orders_amount_non_negative;
-- MySQL 8.0: use INSTANT whenever possible; if unsupported it errors out instead of locking the table
ALTER TABLE orders ADD COLUMN remark VARCHAR(255) NOT NULL DEFAULT '' COMMENT 'Remark', ALGORITHM=INSTANT;
ALTER TABLE orders ADD INDEX idx_orders_status_created_at (status, created_at), ALGORITHM=INPLACE, LOCK=NONE;
```

Redis:

```text
SET app:order:10086:detail '{"status":1}' EX 3600        # set the expiration on write
SCAN 0 MATCH app:order:*:detail COUNT 500                 # iterate with SCAN, not KEYS
SET lock:order:10086 <random-token> NX PX 30000           # distributed lock; release via Lua that checks the token
UNLINK app:order:10086:items                              # delete big keys asynchronously
```
