//! 改表结构风险评估:在真实 MySQL 上核对规则表的判定。
//! 需要本机 MySQL(用户 root、空密码),默认忽略;只用自建的库 datum_alter_test,结束时删掉:
//!   DATUM_ALTER_MYSQL=端口 cargo test -p datum-core --test alter -- --include-ignored --test-threads=1
//!
//! 做法:评估(只读)→ 确认表没被改动 → 再由测试自己逐个算法真跑一遍,看判定是否与服务器一致:
//! INSTANT 能否成功、INPLACE + LOCK=NONE 能否成功、只能 LOCK=SHARED 还是只能 COPY,以及是否重建(TABLE_ID 变了)。

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::service;
use datum_core::sql::alter::{Algorithm, Risk};

const DB: &str = "datum_alter_test";

fn state() -> AppState {
    let dir = std::env::temp_dir().join(format!("datum-alter-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    AppState::new(dir)
}

async fn connect(st: &AppState, port: u16, db: &str) -> String {
    let c = Connection {
        id: format!("alter-{}", uuid::Uuid::new_v4()),
        name: "alter".into(),
        kind: DbKind::Mysql,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port,
        user: "root".into(),
        database: db.into(),
        plaintext_password: None,
        file_path: None,
        ssl_mode: SslMode::Disable,
        uri: None,
        auth_source: None,
        ssh: None,
    };
    let id = c.id.clone();
    commands::save_connection(st, c, Some(String::new())).unwrap();
    service::connect(st, &DbKind::Mysql, &id).await.unwrap();
    id
}

async fn exec(st: &AppState, id: &str, sql: &str) -> Result<(), String> {
    service::query(st, &DbKind::Mysql, id, sql, None).await.map(|_| ()).map_err(|e| e.display())
}

async fn one(st: &AppState, id: &str, sql: &str) -> Option<String> {
    let r = service::query(st, &DbKind::Mysql, id, sql, None).await.unwrap();
    r.rows.first().and_then(|r| r.first().cloned().flatten())
}

const SETUP: &[&str] = &[
    "DROP TABLE IF EXISTS t2",
    "DROP TABLE IF EXISTS t",
    "DROP TABLE IF EXISTS p",
    "CREATE TABLE p (id INT PRIMARY KEY)",
    "CREATE TABLE t (id INT NOT NULL, a INT, b VARCHAR(20) CHARACTER SET utf8mb4, v60 VARCHAR(60) CHARACTER SET utf8mb4, \
     l VARCHAR(100) CHARACTER SET latin1, n INT NOT NULL DEFAULT 0, e ENUM('x','y'), txt TEXT, g POINT NOT NULL SRID 0, pid INT, \
     PRIMARY KEY (id), KEY ka (a), CONSTRAINT fk_p FOREIGN KEY (pid) REFERENCES p(id)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
    "INSERT INTO p VALUES (1), (2)",
    "INSERT INTO t (id, a, b, g, pid) VALUES (1, 1, 'x', POINT(1, 1), 1), (2, 2, 'y', POINT(2, 2), 1)",
];

async fn reset(st: &AppState, id: &str) {
    for s in SETUP {
        exec(st, id, s).await.unwrap();
    }
}

async fn table_id(st: &AppState, id: &str) -> Option<String> {
    one(st, id, &format!("SELECT TABLE_ID FROM information_schema.INNODB_TABLES WHERE NAME = '{DB}/t'")).await
}

/// 在新建的表上真跑一次:返回 (是否成功, 是否重建)
async fn attempt(st: &AppState, id: &str, alter: &str, suffix: &str) -> (bool, bool) {
    reset(st, id).await;
    let before = table_id(st, id).await;
    let ok = exec(st, id, &format!("{alter}{suffix}")).await.is_ok();
    (ok, ok && table_id(st, id).await != before)
}

/// 服务器的实际表现:(默认算法下的算法, 是否重建, 是否允许并发 DML)
async fn observed(st: &AppState, id: &str, alter: &str) -> (Algorithm, bool, bool) {
    let (default_ok, default_rebuild) = attempt(st, id, alter, "").await;
    assert!(default_ok, "{alter} 本身应能执行");
    if attempt(st, id, alter, ", ALGORITHM=INSTANT").await.0 {
        return (Algorithm::Instant, default_rebuild, true);
    }
    if attempt(st, id, alter, ", ALGORITHM=INPLACE, LOCK=NONE").await.0 {
        return (Algorithm::Inplace, default_rebuild, true);
    }
    if attempt(st, id, alter, ", ALGORITHM=INPLACE, LOCK=SHARED").await.0 {
        return (Algorithm::Inplace, default_rebuild, false);
    }
    (Algorithm::Copy, default_rebuild, false)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn mysql_rules_match_server() {
    let Some(port) = std::env::var("DATUM_ALTER_MYSQL").ok().and_then(|p| p.parse().ok()) else {
        return;
    };
    let st = state();
    let boot = connect(&st, port, "").await;
    exec(&st, &boot, &format!("DROP DATABASE IF EXISTS {DB}")).await.unwrap();
    exec(&st, &boot, &format!("CREATE DATABASE {DB}")).await.unwrap();
    let id = connect(&st, port, DB).await;

    // 只核对规则表给出确定结论的操作
    let cases = [
        "ALTER TABLE t ADD COLUMN c INT",
        "ALTER TABLE t ADD COLUMN c INT FIRST",
        "ALTER TABLE t ADD COLUMN c INT NOT NULL DEFAULT 0 AFTER a",
        "ALTER TABLE t ADD COLUMN c INT NOT NULL AUTO_INCREMENT, ADD UNIQUE KEY (c)",
        "ALTER TABLE t ADD COLUMN c INT AS (a + 1) VIRTUAL",
        "ALTER TABLE t ADD COLUMN c INT AS (a + 1) STORED",
        "ALTER TABLE t DROP COLUMN txt",
        "ALTER TABLE t DROP COLUMN a",
        "ALTER TABLE t MODIFY a BIGINT",
        "ALTER TABLE t MODIFY a INT NOT NULL",
        "ALTER TABLE t MODIFY n INT NULL DEFAULT 0",
        "ALTER TABLE t MODIFY a INT DEFAULT 5",
        "ALTER TABLE t MODIFY a INT(11) COMMENT 'hi'",
        "ALTER TABLE t MODIFY a INT AFTER txt",
        "ALTER TABLE t MODIFY b VARCHAR(63) CHARACTER SET utf8mb4",
        "ALTER TABLE t MODIFY b VARCHAR(64) CHARACTER SET utf8mb4",
        "ALTER TABLE t MODIFY v60 VARCHAR(100)",
        "ALTER TABLE t MODIFY l VARCHAR(255) CHARACTER SET latin1",
        "ALTER TABLE t MODIFY l VARCHAR(256) CHARACTER SET latin1",
        "ALTER TABLE t MODIFY l VARCHAR(100)",
        "ALTER TABLE t MODIFY b VARCHAR(10)",
        "ALTER TABLE t MODIFY b VARCHAR(20) CHARACTER SET latin1",
        "ALTER TABLE t MODIFY e ENUM('x','y','z')",
        "ALTER TABLE t MODIFY e ENUM('z','x','y')",
        "ALTER TABLE t MODIFY b VARCHAR(30) NOT NULL",
        "ALTER TABLE t CHANGE a a2 INT",
        "ALTER TABLE t CHANGE a a2 BIGINT",
        "ALTER TABLE t RENAME COLUMN a TO a2",
        "ALTER TABLE t ALTER COLUMN a SET DEFAULT 3",
        "ALTER TABLE t ALTER COLUMN n DROP DEFAULT",
        "ALTER TABLE t ALTER COLUMN a SET INVISIBLE",
        "ALTER TABLE t ADD INDEX kb (b)",
        "ALTER TABLE t ADD UNIQUE INDEX kb (b)",
        "ALTER TABLE t DROP INDEX ka",
        "ALTER TABLE t RENAME INDEX ka TO kz",
        "ALTER TABLE t ALTER INDEX ka INVISIBLE",
        "ALTER TABLE t ADD FULLTEXT INDEX ft (txt)",
        "ALTER TABLE t ADD SPATIAL INDEX sp (g)",
        "ALTER TABLE t DROP PRIMARY KEY",
        "ALTER TABLE t DROP PRIMARY KEY, ADD PRIMARY KEY (id, n)",
        "ALTER TABLE t ADD CONSTRAINT fk2 FOREIGN KEY (a) REFERENCES p(id)",
        "ALTER TABLE t DROP FOREIGN KEY fk_p",
        "ALTER TABLE t CONVERT TO CHARACTER SET utf8mb4",
        "ALTER TABLE t DEFAULT CHARACTER SET latin1",
        "ALTER TABLE t ENGINE=InnoDB",
        "ALTER TABLE t FORCE",
        "ALTER TABLE t ROW_FORMAT=COMPACT",
        "ALTER TABLE t KEY_BLOCK_SIZE=8",
        "ALTER TABLE t AUTO_INCREMENT=100",
        "ALTER TABLE t STATS_PERSISTENT=0",
        "ALTER TABLE t COMMENT='hello'",
        "ALTER TABLE t ADD CONSTRAINT ck CHECK (a > 0)",
        "ALTER TABLE t ADD COLUMN c INT, ADD INDEX kb (b)",
        "ALTER TABLE t ADD COLUMN c INT, MODIFY a BIGINT",
    ];
    let mut mismatches = Vec::new();
    for sql in cases {
        reset(&st, &id).await;
        let cols = one(&st, &id, &format!("SELECT COUNT(*) FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = '{DB}' AND TABLE_NAME = 't'")).await;
        let before = table_id(&st, &id).await;
        let a = service::alter_assess(&st, &DbKind::Mysql, &id, sql).await.unwrap().unwrap();
        // 评估只读:表没被改动
        assert_eq!(table_id(&st, &id).await, before, "{sql}:评估不应改动表");
        assert_eq!(
            one(&st, &id, &format!("SELECT COUNT(*) FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = '{DB}' AND TABLE_NAME = 't'")).await,
            cols,
            "{sql}:评估不应改动表"
        );
        let Some(predicted) = a.algorithm else {
            mismatches.push(format!("{sql}: 判为未知({:?})", a.ops.iter().map(|o| o.note).collect::<Vec<_>>()));
            continue;
        };
        let want = (predicted, a.rebuild.unwrap(), !a.blocks_writes.unwrap());
        let got = observed(&st, &id, sql).await;
        if want != got {
            mismatches.push(format!("{sql}: 判定 {want:?},实际 {got:?}"));
        }
    }
    // 表有全文索引时加列只能 COPY;压缩行格式不能 INSTANT
    for (prep, sql) in [
        ("ALTER TABLE t ADD FULLTEXT INDEX ft (txt)", "ALTER TABLE t ADD COLUMN c INT"),
        ("ALTER TABLE t ROW_FORMAT=COMPRESSED", "ALTER TABLE t ADD COLUMN c INT"),
    ] {
        reset(&st, &id).await;
        exec(&st, &id, prep).await.unwrap();
        let a = service::alter_assess(&st, &DbKind::Mysql, &id, sql).await.unwrap().unwrap();
        // 在同结构的副本上真跑
        let instant = {
            exec(&st, &id, "DROP TABLE IF EXISTS t2").await.unwrap();
            exec(&st, &id, "CREATE TABLE t2 LIKE t").await.unwrap();
            exec(&st, &id, "ALTER TABLE t2 ADD COLUMN c INT, ALGORITHM=INSTANT").await.is_ok()
        };
        let inplace = exec(&st, &id, "ALTER TABLE t2 ADD COLUMN c2 INT, ALGORITHM=INPLACE, LOCK=NONE").await.is_ok();
        let got = if instant { Algorithm::Instant } else if inplace { Algorithm::Inplace } else { Algorithm::Copy };
        if a.algorithm != Some(got) {
            mismatches.push(format!("{prep} 后 {sql}: 判定 {:?},实际 {got:?}", a.algorithm));
        }
    }
    assert!(mismatches.is_empty(), "与服务器不一致:\n{}", mismatches.join("\n"));

    // 表大小:行数与体积从 information_schema 取到;表不存在时判为未知
    reset(&st, &id).await;
    exec(&st, &id, "ANALYZE TABLE t").await.unwrap();
    let a = service::alter_assess(&st, &DbKind::Mysql, &id, "ALTER TABLE t MODIFY a BIGINT").await.unwrap().unwrap();
    assert!(a.bytes.is_some_and(|b| b > 0) && a.rows.is_some());
    assert_eq!((a.risk, a.suggest_osc), (Risk::High, true));
    assert_eq!(a.guarded.as_deref(), Some("ALTER TABLE t MODIFY a BIGINT, ALGORITHM=INPLACE, LOCK=NONE"));
    // 保护写法确实会让服务器报错而不是锁表
    assert!(exec(&st, &id, a.guarded.as_deref().unwrap()).await.is_err());
    let a = service::alter_assess(&st, &DbKind::Mysql, &id, &format!("ALTER TABLE `{DB}`.missing ADD c INT")).await.unwrap().unwrap();
    assert_eq!((a.algorithm, a.notes.as_slice()), (None, ["table_not_found"].as_slice()));
    // 不是改表结构的语句
    assert!(service::alter_assess(&st, &DbKind::Mysql, &id, "UPDATE t SET a = 1").await.unwrap().is_none());

    exec(&st, &id, &format!("DROP DATABASE {DB}")).await.unwrap();
}
