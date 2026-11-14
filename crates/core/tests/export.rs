//! 流式导出:逐行写文件的结果与一次性渲染(grid::to_*、export_database)逐字节一致。
//! SQLite 默认就跑;PG / MySQL 需要本机数据库,默认忽略:
//!   DATUM_TEST_PG=端口(postgres、空密码)  DATUM_TEST_MYSQL=端口(root、空密码、库 datum_test)
//!   cargo test -p datum-core --test export -- --include-ignored

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::export::Format;
use datum_core::grid;
use datum_core::service;

fn conn(kind: DbKind, port: u16, user: &str, db: &str, file: Option<String>) -> Connection {
    Connection {
        id: format!("export-{kind:?}-{}", uuid::Uuid::new_v4()),
        name: "export".into(),
        kind,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port,
        user: user.into(),
        database: db.into(),
        plaintext_password: None,
        file_path: file,
        ssl_mode: SslMode::Disable,
        uri: None,
        auth_source: None,
        ssh: None,
    }
}

async fn check(st: &AppState, kind: DbKind, id: &str, dir: &std::path::Path) {
    let q = |sql: String| async move { service::query(st, &kind, id, &sql, None).await.unwrap() };
    q("DROP TABLE IF EXISTS exp_t".into()).await;
    q("DROP TABLE IF EXISTS exp_empty".into()).await;
    q("CREATE TABLE exp_t (id INT, name VARCHAR(50), note VARCHAR(50), amount VARCHAR(30))".into()).await;
    q("CREATE TABLE exp_empty (id INT)".into()).await;
    q(r#"INSERT INTO exp_t VALUES
        (1, 'plain', NULL, '42'),
        (2, 'a,b', 'say "hi"', '007'),
        (3, 'line1
line2', 'x|y', '12345678901234567890'),
        (4, '中文', '', '-0.5')"#
        .into())
    .await;
    let all = q("SELECT * FROM exp_t".into()).await;
    let refs: Vec<&Vec<grid::Cell>> = all.rows.iter().collect();
    for (format, expected) in [
        (Format::Csv, grid::to_csv(&all.columns, &refs)),
        (Format::Json, grid::to_json(&all.columns, &refs)),
        (Format::Markdown, grid::to_markdown(&all.columns, &refs)),
    ] {
        let path = dir.join(format!("{kind:?}-{format:?}"));
        let n = service::export_table_to_file(st, &kind, id, "exp_t", format, &path).await.unwrap();
        assert_eq!(n, 4);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected, "{kind:?} {format:?}");
    }
    // 空表的 JSON 是 []
    let path = dir.join(format!("{kind:?}-empty.json"));
    assert_eq!(service::export_table_to_file(st, &kind, id, "exp_empty", Format::Json, &path).await.unwrap(), 0);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "[]");
    // Excel:能写出、能被 zip 打开(xlsx 是 zip)
    let path = dir.join(format!("{kind:?}.xlsx"));
    service::export_table_to_file(st, &kind, id, "exp_t", Format::Xlsx, &path).await.unwrap();
    assert_eq!(&std::fs::read(&path).unwrap()[..2], b"PK");
    // 不存在的表:报错,不留下半截文件
    let path = dir.join(format!("{kind:?}-missing.csv"));
    assert!(service::export_table_to_file(st, &kind, id, "no_such_table", Format::Csv, &path).await.is_err());
    assert!(!path.exists());

    // 整库:与一次性拼字符串的版本一致(含失败的表)
    let tables = vec!["exp_t".to_string(), "exp_empty".to_string(), "no_such_table".to_string()];
    let expected = service::export_database(st, &kind, id, &tables).await;
    let path = dir.join(format!("{kind:?}-db.sql"));
    service::export_database_to_file(st, &kind, id, &tables, &path).await.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), expected, "{kind:?} 整库");
    // 没有留下暂存文件
    let leftovers: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".part")).collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    q("DROP TABLE exp_t".into()).await;
    q("DROP TABLE exp_empty".into()).await;
}

async fn run(kind: DbKind, port: u16, user: &str, db: &str) {
    let dir = std::env::temp_dir().join(format!("datum-export-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let st = AppState::new(dir.join("cfg"));
    let file = (kind == DbKind::Sqlite).then(|| {
        let f = dir.join("t.db");
        rusqlite::Connection::open(&f).unwrap();
        f.to_string_lossy().into_owned()
    });
    let c = conn(kind, port, user, db, file);
    let id = c.id.clone();
    commands::save_connection(&st, c, Some(String::new())).unwrap();
    service::connect(&st, &kind, &id).await.unwrap();
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    check(&st, kind, &id, &out).await;
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_streaming_export_matches() {
    run(DbKind::Sqlite, 0, "", "").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn pg_streaming_export_matches() {
    let port = std::env::var("DATUM_TEST_PG").unwrap().parse().unwrap();
    run(DbKind::Pg, port, "postgres", "postgres").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn mysql_streaming_export_matches() {
    let port = std::env::var("DATUM_TEST_MYSQL").unwrap().parse().unwrap();
    run(DbKind::Mysql, port, "root", "datum_test").await;
}
