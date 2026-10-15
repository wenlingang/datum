//! 真实 MongoDB 上的行为。需要本机库,默认忽略:
//!   docker run -d --name datum-mongo -p 27017:27017 mongo:7
//!   DATUM_MONGO=27017 cargo test -p datum-core --test mongo -- --ignored --test-threads=1 --nocapture

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::error::{AppResult, ErrorKind};
use datum_core::mongo::convert::MongoResult;
use datum_core::mongo::parse::{parse_document, parse_script};
use datum_core::service::{self, FindPage};
use mongodb::bson::doc;

fn port() -> u16 {
    std::env::var("DATUM_MONGO")
        .expect("DATUM_MONGO")
        .parse()
        .unwrap()
}

fn base_conn(id: &str, db: &str) -> Connection {
    Connection {
        id: id.into(),
        name: "m".into(),
        kind: DbKind::Mongo,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port: port(),
        user: String::new(),
        database: db.into(),
        plaintext_password: None,
        file_path: None,
        ssl_mode: SslMode::Disable,
        uri: None,
        auth_source: None,
    }
}

fn state() -> AppState {
    let dir = std::env::temp_dir().join(format!("datum-mongo-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    AppState::new(dir)
}

async fn setup(db: &str) -> (AppState, String) {
    let s = state();
    let conn = base_conn(&format!("m-{db}"), db);
    let id = conn.id.clone();
    commands::save_connection(&s, conn, Some(String::new())).unwrap();
    service::connect(&s, &DbKind::Mongo, &id).await.unwrap();
    (s, id)
}

async fn run(s: &AppState, id: &str, src: &str, read_only: bool) -> AppResult<MongoResult> {
    let ops = parse_script(src)
        .unwrap()
        .into_iter()
        .map(|st| st.op)
        .collect();
    service::mongo_execute(s, id, ops, 1000, read_only).await
}

fn page(filter: mongodb::bson::Document) -> FindPage {
    FindPage {
        filter,
        sort: None,
        projection: None,
        skip: 0,
        limit: 10,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn crud_round_trip_keeps_types() {
    let (s, id) = setup("datum_it_crud").await;
    run(&s, &id, "db.t.deleteMany({x: {$exists: true}})", false)
        .await
        .unwrap();
    let doc = doc! {
        "x": 1_i64,
        "d": mongodb::bson::DateTime::from_millis(5),
        "dec": "1.10".parse::<mongodb::bson::Decimal128>().unwrap(),
        "n": { "a": [1.0_f64] },
    };
    let new_id = service::mongo_insert_one(&s, &id, "t", doc).await.unwrap();
    let r = service::mongo_find_page(&s, &id, "t", page(doc! { "_id": new_id.clone() }))
        .await
        .unwrap();
    assert_eq!(r.ids, vec![Some(new_id.clone())]);
    // 文档编辑器的路径:展示文本 → 解析 → replaceOne → 再读回,类型不变
    let edited = parse_document(&r.docs[0]).unwrap();
    service::mongo_replace_one(&s, &id, "t", new_id.clone(), edited.clone())
        .await
        .unwrap();
    let again = service::mongo_find_page(&s, &id, "t", page(doc! { "_id": new_id.clone() }))
        .await
        .unwrap();
    assert_eq!(parse_document(&again.docs[0]).unwrap(), edited);
    service::mongo_delete_one(&s, &id, "t", new_id.clone())
        .await
        .unwrap();
    // 再删一次:文档已不存在
    assert!(service::mongo_delete_one(&s, &id, "t", new_id)
        .await
        .is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn paging_filter_sort_projection_and_count() {
    let (s, id) = setup("datum_it_page").await;
    run(&s, &id, "db.p.deleteMany({i: {$gte: 0}})", false)
        .await
        .unwrap();
    let docs: Vec<String> = (0..25)
        .map(|i| format!("{{i: {i}, odd: {}}}", i % 2 == 1))
        .collect();
    let r = run(
        &s,
        &id,
        &format!("db.p.insertMany([{}])", docs.join(",")),
        false,
    )
    .await
    .unwrap();
    assert!(r.summary.is_some());
    let req = FindPage {
        filter: doc! { "odd": true },
        sort: Some(doc! { "i": -1 }),
        projection: Some(doc! { "_id": 0, "i": 1 }),
        skip: 5,
        limit: 3,
    };
    let r = service::mongo_find_page(&s, &id, "p", req).await.unwrap();
    assert_eq!(r.columns, vec!["i"]);
    assert_eq!(
        r.rows,
        vec![
            vec![Some("13".into())],
            vec![Some("11".into())],
            vec![Some("9".into())]
        ]
    );
    assert_eq!(r.ids, vec![None, None, None]);
    assert_eq!(
        service::mongo_count(&s, &id, "p", Some(doc! { "odd": true }))
            .await
            .unwrap(),
        (12, true)
    );
    assert_eq!(
        service::mongo_count(&s, &id, "p", None).await.unwrap().0,
        25
    );
    let colls = service::list_objects(&s, &DbKind::Mongo, &id)
        .await
        .unwrap();
    assert!(colls.contains(&"p".to_string()));
    let dbs = service::list_databases(&s, &DbKind::Mongo, &id)
        .await
        .unwrap();
    assert!(!dbs.contains(&"admin".to_string()));
    let idx = run(&s, &id, "db.p.getIndexes()", false).await.unwrap();
    assert!(idx.columns.contains(&"name".to_string()));
    assert!(idx.rows.iter().flatten().flatten().any(|v| v == "_id_"));
    // 查询 tab 的 find 截断标记
    let r = run(&s, &id, "db.p.find()", false).await.unwrap();
    assert!(!r.truncated);
    let ops = parse_script("db.p.find()")
        .unwrap()
        .into_iter()
        .map(|st| st.op)
        .collect();
    let r = service::mongo_execute(&s, &id, ops, 10, false)
        .await
        .unwrap();
    assert!(r.truncated);
    assert_eq!(r.rows.len(), 10);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn read_only_blocks_out_stage_on_client() {
    let (s, id) = setup("datum_it_ro").await;
    let e = run(&s, &id, "db.p.aggregate([{$out: 'q'}])", true)
        .await
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Mongo);
    let colls = service::list_objects(&s, &DbKind::Mongo, &id)
        .await
        .unwrap();
    assert!(!colls.contains(&"q".to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn statement_failure_reports_index() {
    let (s, id) = setup("datum_it_err").await;
    run(&s, &id, "db.u.deleteMany({k: 1})", false)
        .await
        .unwrap();
    let e = run(
        &s,
        &id,
        "db.u.insertOne({_id: 'dup', k: 1})\ndb.u.insertOne({_id: 'dup', k: 1})",
        false,
    )
    .await
    .unwrap_err();
    assert!(e.message.contains('2'), "{}", e.display());
    assert!(e.detail.unwrap_or_default().starts_with("11000"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn max_time_aborts_slow_query() {
    let (s, id) = setup("datum_it_slow").await;
    run(&s, &id, "db.big.deleteMany({k: {$exists: true}})", false)
        .await
        .unwrap();
    let batch: Vec<String> = (0..1000)
        .map(|i| format!("{{k: 'key-{i}-xxxxxxxxxxxxxxxxxxxx'}}"))
        .collect();
    for _ in 0..100 {
        run(
            &s,
            &id,
            &format!("db.big.insertMany([{}])", batch.join(",")),
            false,
        )
        .await
        .unwrap();
    }
    // k 上没有索引:按 k 排序必须扫完 10 万篇,1ms 内不可能完成
    s.mongo_pool
        .set_max_time(std::time::Duration::from_millis(1));
    let e = run(
        &s,
        &id,
        "db.big.find({k: /x+$/}).sort({k: -1}).limit(1)",
        false,
    )
    .await
    .unwrap_err();
    assert!(
        e.detail.clone().unwrap_or_default().starts_with("50"),
        "{}",
        e.display()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn uri_mode_and_prefer_fallback_connect() {
    let s = state();
    let mut c = base_conn("uri", "datum_it_uri");
    c.uri = Some(format!("mongodb://127.0.0.1:{}/", port()));
    c.host = String::new();
    c.port = 0;
    commands::save_connection(&s, c, Some(String::new())).unwrap();
    service::connect(&s, &DbKind::Mongo, "uri").await.unwrap();
    // 本地库不支持 TLS:Prefer 先试加密失败,再退回明文
    let mut p = base_conn("prefer", "datum_it_uri");
    p.ssl_mode = SslMode::Prefer;
    let started = std::time::Instant::now();
    service::test_connection(&p, "").await.unwrap();
    // 加密探测不能白等完整的 server selection 超时(5s)
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "prefer fallback took {:?}",
        started.elapsed()
    );
    let mut r = base_conn("require", "datum_it_uri");
    r.ssl_mode = SslMode::Require;
    assert!(service::test_connection(&r, "").await.is_err());
}

/// 开了认证的库的端口(`DATUM_MONGO_AUTH`),未设置时相关用例跳过。准备方式:
///   docker run -d --name datum-mongo-auth -p 27018:27017 \
///     -e MONGO_INITDB_ROOT_USERNAME=root -e MONGO_INITDB_ROOT_PASSWORD=rootpw mongo:7
///   docker exec datum-mongo-auth mongosh -u root -p rootpw --eval \
///     'db.getSiblingDB("datum_it_auth").createUser({user:"narrow",pwd:"pw",roles:[{role:"read",db:"datum_it_auth"}]})'
fn auth_port() -> Option<u16> {
    std::env::var("DATUM_MONGO_AUTH").ok()?.parse().ok()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn listing_databases_without_permission_falls_back_to_configured_db() {
    let Some(port) = auth_port() else { return };
    let s = state();
    let mut c = base_conn("narrow", "datum_it_auth");
    c.port = port;
    c.user = "narrow".into();
    c.auth_source = Some("datum_it_auth".into());
    commands::save_connection(&s, c, Some("pw".into())).unwrap();
    service::connect(&s, &DbKind::Mongo, "narrow")
        .await
        .unwrap();
    let dbs = service::list_databases(&s, &DbKind::Mongo, "narrow")
        .await
        .unwrap();
    assert_eq!(dbs, vec!["datum_it_auth".to_string()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn wrong_password_is_reported_without_plaintext_fallback() {
    let Some(port) = auth_port() else { return };
    let mut c = base_conn("badpw", "admin");
    c.port = port;
    c.user = "root".into();
    c.ssl_mode = SslMode::Prefer;
    let started = std::time::Instant::now();
    // 服务器没开 TLS:加密探测失败后退回明文,明文这次认证失败,应如实报认证错误
    let e = service::test_connection(&c, "wrong").await.unwrap_err();
    assert!(e.display().contains("Authentication"), "{}", e.display());
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
    c.ssl_mode = SslMode::Disable;
    let started = std::time::Instant::now();
    let e = service::test_connection(&c, "wrong").await.unwrap_err();
    assert!(e.display().contains("Authentication"), "{}", e.display());
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    service::test_connection(&c, "rootpw").await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn editing_after_projection_uses_full_document() {
    let (s, id) = setup("datum_it_proj").await;
    run(&s, &id, "db.t.deleteMany({k: 1})", false)
        .await
        .unwrap();
    let new_id = service::mongo_insert_one(
        &s,
        &id,
        "t",
        doc! { "k": 1, "name": "a", "price": "9.50".parse::<mongodb::bson::Decimal128>().unwrap() },
    )
    .await
    .unwrap();
    let req = FindPage {
        filter: doc! { "_id": new_id.clone() },
        sort: None,
        projection: Some(doc! { "name": 1 }),
        skip: 0,
        limit: 10,
    };
    let shown = service::mongo_find_page(&s, &id, "t", req).await.unwrap();
    assert!(!shown.docs[0].contains("price"));
    // 编辑器打开前按 _id 取完整文档:原样保存后其余字段必须还在
    let full = service::mongo_find_by_id(&s, &id, "t", new_id.clone())
        .await
        .unwrap()
        .expect("document exists");
    assert!(full.contains("price") && full.contains("k: 1"), "{full}");
    let edited = parse_document(&full).unwrap();
    service::mongo_replace_one(&s, &id, "t", new_id.clone(), edited)
        .await
        .unwrap();
    let back = service::mongo_find_page(&s, &id, "t", page(doc! { "_id": new_id.clone() }))
        .await
        .unwrap();
    assert!(back.docs[0].contains("price"));
    assert!(
        service::mongo_find_by_id(&s, &id, "t", mongodb::bson::Bson::Int32(-1))
            .await
            .unwrap()
            .is_none()
    );
}
