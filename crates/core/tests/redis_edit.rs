//! Redis 按类型编辑 key 的真实执行。需要一个可随意写入的 Redis,默认忽略:
//!   DATUM_TEST_REDIS=端口   例如 docker run --rm -p 16379:6379 redis:7-alpine
//!   cargo test -p datum-core --test redis_edit -- --ignored --nocapture
//! 用库 14(格式识别的测试用库 13),开始前清空。

use datum_core::commands::{self, AppState};
use datum_core::core::connection::{Connection, DbKind, Env, SslMode};
use datum_core::rds::client::RawCell;
use datum_core::rds::decode::{self, Decoded, Format};
use datum_core::rds::edit::{KeyEdit, NewKey, StreamRange};
use datum_core::service;

const ID: &str = "rds-edit";

async fn setup() -> Option<AppState> {
    setup_db(ID, "14").await
}

/// 连上测试 Redis 的库 `db`(连接 id 为 `id`)并清空。不同测试用不同的库,并行跑时互不清空对方的数据。
async fn setup_db(id: &str, db: &str) -> Option<AppState> {
    let port: u16 = std::env::var("DATUM_TEST_REDIS").ok()?.parse().ok()?;
    let dir = std::env::temp_dir().join(format!("datum-rds-edit-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let st = AppState::new(dir);
    let c = Connection {
        id: id.into(),
        name: "rds".into(),
        kind: DbKind::Redis,
        env: Env::Local,
        host: "127.0.0.1".into(),
        port,
        user: String::new(),
        database: db.into(),
        plaintext_password: None,
        file_path: None,
        ssl_mode: SslMode::Disable,
        uri: None,
        auth_source: None,
        ssh: None,
    };
    commands::save_connection(&st, c, Some(String::new())).unwrap();
    service::connect(&st, &DbKind::Redis, id).await.unwrap();
    service::redis_exec(&st, id, "FLUSHDB").await.unwrap();
    Some(st)
}

async fn edit(st: &AppState, key: &str, json: &str) -> Result<i64, String> {
    let e: KeyEdit = serde_json::from_str(json).unwrap();
    service::redis_edit(st, ID, None, key, &e).await.map(|r| r.changed).map_err(|e| e.display())
}

fn range(start: &str, end: &str, count: usize, rev: bool) -> StreamRange {
    StreamRange { start: start.into(), end: end.into(), count, rev }
}

async fn exec(st: &AppState, cmd: &str) -> String {
    service::redis_exec(st, ID, cmd).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn edits_each_type() {
    let Some(st) = setup().await else {
        return;
    };

    // string:保存保留 TTL;key 换了类型时报错而不是覆盖
    exec(&st, "SET s hello EX 500").await;
    edit(&st, "s", r#"{"op":"stringSet","value":"二进制\u0000安全"}"#).await.unwrap();
    assert_eq!(exec(&st, "STRLEN s").await, "16");
    let ttl: i64 = exec(&st, "TTL s").await.parse().unwrap();
    assert!(ttl > 400, "SET 后 TTL 应保留,实际 {ttl}");
    exec(&st, "RPUSH notstring a").await;
    let err = edit(&st, "notstring", r#"{"op":"stringSet","value":"x"}"#).await.unwrap_err();
    assert!(err.contains("list"), "{err}");
    assert!(edit(&st, "missing", r#"{"op":"stringSet","value":"x"}"#).await.is_err());
    assert_eq!(exec(&st, "EXISTS missing").await, "0");

    // hash:新增不覆盖、改值、改字段名(新名已存在时报错)、删除
    exec(&st, "HSET h a 1 b 2").await;
    assert!(edit(&st, "h", r#"{"op":"hashAdd","field":"a","value":"x"}"#).await.is_err());
    edit(&st, "h", r#"{"op":"hashAdd","field":"c","value":"3"}"#).await.unwrap();
    edit(&st, "h", r#"{"op":"hashSet","field":"a","value":"10","oldField":"a"}"#).await.unwrap();
    assert_eq!(exec(&st, "HGET h a").await, "10");
    assert!(edit(&st, "h", r#"{"op":"hashSet","field":"b","value":"x","oldField":"a"}"#).await.is_err());
    edit(&st, "h", r#"{"op":"hashSet","field":"z","value":"26","oldField":"a"}"#).await.unwrap();
    assert_eq!(exec(&st, "HEXISTS h a").await, "0");
    assert_eq!(exec(&st, "HGET h z").await, "26");
    assert_eq!(edit(&st, "h", r#"{"op":"hashDel","field":"b"}"#).await.unwrap(), 1);
    assert_eq!(edit(&st, "h", r#"{"op":"hashDel","field":"b"}"#).await.unwrap(), 0);

    // list:按下标改 / 删(有重复值时删的是选中的那一格),旧值对不上时拒绝;头尾插入
    exec(&st, "RPUSH l x y x").await;
    edit(&st, "l", r#"{"op":"listDel","index":2,"old":"x"}"#).await.unwrap();
    assert_eq!(exec(&st, "LRANGE l 0 -1").await, "x\ny");
    assert!(edit(&st, "l", r#"{"op":"listSet","index":1,"old":"stale","value":"q"}"#).await.is_err());
    edit(&st, "l", r#"{"op":"listSet","index":1,"old":"y","value":"q"}"#).await.unwrap();
    edit(&st, "l", r#"{"op":"listPush","value":"h","head":true}"#).await.unwrap();
    edit(&st, "l", r#"{"op":"listPush","value":"t","head":false}"#).await.unwrap();
    assert_eq!(exec(&st, "LRANGE l 0 -1").await, "h\nx\nq\nt");
    assert!(edit(&st, "nolist", r#"{"op":"listPush","value":"v","head":true}"#).await.is_err());
    assert_eq!(exec(&st, "EXISTS nolist").await, "0");

    // set
    exec(&st, "SADD st a").await;
    edit(&st, "st", r#"{"op":"setAdd","member":"b"}"#).await.unwrap();
    assert!(edit(&st, "st", r#"{"op":"setAdd","member":"b"}"#).await.is_err());
    assert_eq!(edit(&st, "st", r#"{"op":"setDel","member":"a"}"#).await.unwrap(), 1);
    assert_eq!(exec(&st, "SMEMBERS st").await, "b");

    // zset:新增不覆盖、改分数、改成员名、非法分数
    exec(&st, "ZADD z 1 a 2 b").await;
    assert!(edit(&st, "z", r#"{"op":"zsetAdd","member":"a","score":"9"}"#).await.is_err());
    assert!(edit(&st, "z", r#"{"op":"zsetAdd","member":"c","score":"abc"}"#).await.is_err());
    edit(&st, "z", r#"{"op":"zsetAdd","member":"c","score":"-inf"}"#).await.unwrap();
    edit(&st, "z", r#"{"op":"zsetSet","member":"a","score":"5.5","oldMember":"a"}"#).await.unwrap();
    assert_eq!(exec(&st, "ZSCORE z a").await, "5.5");
    edit(&st, "z", r#"{"op":"zsetSet","member":"aa","score":"7","oldMember":"a"}"#).await.unwrap();
    assert_eq!(exec(&st, "ZRANGE z 0 -1").await, "c\nb\naa");
    edit(&st, "z", r#"{"op":"zsetDel","member":"b"}"#).await.unwrap();

    // TTL / 重命名
    service::redis_set_ttl(&st, ID, None, "st", Some(100)).await.unwrap();
    assert!(exec(&st, "TTL st").await.parse::<i64>().unwrap() > 90);
    service::redis_set_ttl(&st, ID, None, "st", None).await.unwrap();
    assert_eq!(exec(&st, "TTL st").await, "-1");
    assert!(service::redis_set_ttl(&st, ID, None, "st", Some(0)).await.is_err());
    assert!(service::redis_set_ttl(&st, ID, None, "gone", None).await.is_err());
    let e = service::redis_rename(&st, ID, None, "st", "h").await.unwrap_err().display();
    assert!(e.contains('h'), "{e}");
    assert_eq!(exec(&st, "TYPE h").await, "hash");
    service::redis_rename(&st, ID, None, "st", "st2").await.unwrap();
    assert_eq!(exec(&st, "EXISTS st st2").await, "1");
    assert!(service::redis_rename(&st, ID, None, "st", "st3").await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn creates_keys() {
    let Some(st) = setup().await else {
        return;
    };
    let new = |key: &str, ty: &str, items: Vec<Vec<&str>>, ttl: Option<i64>| NewKey {
        key: key.into(),
        key_type: ty.into(),
        items: items.into_iter().map(|it| it.into_iter().map(String::from).collect()).collect(),
        ttl,
    };
    service::redis_create_key(&st, ID, None, &new("ns", "string", vec![vec!["v"]], Some(60))).await.unwrap();
    assert!(exec(&st, "TTL ns").await.parse::<i64>().unwrap() > 50);
    assert!(service::redis_create_key(&st, ID, None, &new("ns", "string", vec![vec!["w"]], None)).await.is_err());
    assert_eq!(exec(&st, "GET ns").await, "v");
    service::redis_create_key(&st, ID, None, &new("nh", "hash", vec![vec!["f", "1"], vec!["g", "2"]], None)).await.unwrap();
    assert_eq!(exec(&st, "HLEN nh").await, "2");
    assert!(service::redis_create_key(&st, ID, None, &new("nh", "set", vec![vec!["m"]], None)).await.is_err());
    service::redis_create_key(&st, ID, None, &new("nl", "list", vec![vec!["a"], vec!["b"]], None)).await.unwrap();
    assert_eq!(exec(&st, "LRANGE nl 0 -1").await, "a\nb");
    service::redis_create_key(&st, ID, None, &new("nz", "zset", vec![vec!["m", "3"]], Some(30))).await.unwrap();
    assert_eq!(exec(&st, "ZSCORE nz m").await, "3");
    assert!(exec(&st, "TTL nz").await.parse::<i64>().unwrap() > 0);
    assert!(service::redis_create_key(&st, ID, None, &new("bad", "zset", vec![vec!["m", "x"]], None)).await.is_err());
    assert!(service::redis_create_key(&st, ID, None, &new("empty", "set", vec![], None)).await.is_err());
    assert_eq!(exec(&st, "EXISTS bad empty").await, "0");
    service::redis_create_key(&st, ID, None, &new("nx", "stream", vec![vec!["a", "1"]], None)).await.unwrap();
    assert_eq!(exec(&st, "XLEN nx").await, "1");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn stream_pages_and_groups() {
    let Some(st) = setup().await else {
        return;
    };
    for i in 1..=5 {
        exec(&st, &format!("XADD x {i}-0 n {i}")).await;
    }
    let ids = |p: &datum_core::rds::edit::StreamPage| p.entries.iter().map(|e| e.id.clone()).collect::<Vec<_>>();
    let p = service::redis_stream_range(&st, ID, None, "x", &range("-", "+", 2, false)).await.unwrap();
    assert_eq!(ids(&p), ["1-0", "2-0"]);
    assert_eq!(p.next.as_deref(), Some("3-0"));
    assert_eq!(p.length, 5);
    let p = service::redis_stream_range(&st, ID, None, "x", &range("3-0", "+", 2, false)).await.unwrap();
    assert_eq!(ids(&p), ["3-0", "4-0"]);
    let p = service::redis_stream_range(&st, ID, None, "x", &range("5-0", "+", 2, false)).await.unwrap();
    assert_eq!(ids(&p), ["5-0"]);
    assert_eq!(p.next, None);
    assert_eq!(p.entries[0].fields, vec![("n".to_string(), "5".to_string())]);
    // 倒序:最新的在前,下一页的 end
    let p = service::redis_stream_range(&st, ID, None, "x", &range("-", "+", 2, true)).await.unwrap();
    assert_eq!(ids(&p), ["5-0", "4-0"]);
    assert_eq!(p.next.as_deref(), Some("3-0"));
    let p = service::redis_stream_range(&st, ID, None, "x", &range("-", "3-0", 2, true)).await.unwrap();
    assert_eq!(ids(&p), ["3-0", "2-0"]);

    // 追加 / 删除条目
    let e: KeyEdit = serde_json::from_str(r#"{"op":"streamAdd","id":"","fields":[["k","v"],["k2","v2"]]}"#).unwrap();
    let r = service::redis_edit(&st, ID, None, "x", &e).await.unwrap();
    assert!(r.id.is_some());
    let e: KeyEdit = serde_json::from_str(r#"{"op":"streamAdd","id":"1-0","fields":[["k","v"]]}"#).unwrap();
    assert!(service::redis_edit(&st, ID, None, "x", &e).await.is_err(), "ID 不递增应报错");
    let e: KeyEdit = serde_json::from_str(r#"{"op":"streamDel","id":"1-0"}"#).unwrap();
    assert_eq!(service::redis_edit(&st, ID, None, "x", &e).await.unwrap().changed, 1);

    // 消费组:g1 读了 2 条未确认
    exec(&st, "XGROUP CREATE x g1 0").await;
    exec(&st, "XGROUP CREATE x g2 $").await;
    exec(&st, "XREADGROUP GROUP g1 alice COUNT 2 STREAMS x >").await;
    let groups = service::redis_stream_groups(&st, ID, None, "x").await.unwrap();
    assert_eq!(groups.len(), 2);
    let g1 = groups.iter().find(|g| g.name == "g1").unwrap();
    assert_eq!(g1.pending, 2);
    assert_eq!(g1.consumers, 1);
    assert_eq!(g1.pending_min.as_deref(), Some("2-0"));
    assert_eq!(g1.pending_max.as_deref(), Some("3-0"));
    assert_eq!(g1.consumer_list.len(), 1);
    assert_eq!(g1.consumer_list[0].name, "alice");
    assert_eq!(g1.consumer_list[0].pending, 2);
    let g2 = groups.iter().find(|g| g.name == "g2").unwrap();
    assert_eq!(g2.pending, 0);
    assert_eq!(g2.pending_min, None);
    assert!(g2.consumer_list.is_empty());
    // 不是 stream 的 key
    exec(&st, "SET notstream 1").await;
    assert!(service::redis_stream_range(&st, ID, None, "notstream", &range("-", "+", 10, false)).await.is_err());
}

// ---------------------------------------------------------------- 格式识别(查看路径拿到原始字节)

const DECODE_ID: &str = "rds-decode";

fn gzip(b: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
    e.write_all(b).unwrap();
    e.finish().unwrap()
}

fn zlib(b: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::ZlibEncoder::new(vec![], flate2::Compression::default());
    e.write_all(b).unwrap();
    e.finish().unwrap()
}

/// {"id":7,"name":"Ann"}
const MSGPACK: &[u8] = &[0x82, 0xa2, b'i', b'd', 7, 0xa4, b'n', b'a', b'm', b'e', 0xa3, b'A', b'n', b'n'];
/// 1: 150、2: "hi"、3: { 1: 1 }
const PROTOBUF: &[u8] = &[0x08, 0x96, 0x01, 0x12, 0x02, b'h', b'i', 0x1a, 0x02, 0x08, 0x01];

/// 单元格解码:非 UTF-8 的取 raw 里的原始字节,其余用文本(与界面的做法一致)。
fn decode_cell(text: &str, raw: &[RawCell], row: usize, col: usize) -> Decoded {
    let b64 = raw.iter().find(|c| c.row == row && c.col == col).map(|c| c.b64.as_str());
    decode::decode_cell(text, b64, Format::Auto).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn binary_values_reach_the_decoder_intact() {
    let Some(st) = setup_db(DECODE_ID, "13").await else {
        return;
    };
    // 二进制安全地写入:直接用 redis 客户端(redis_exec 按空白拆分命令,写不了任意字节)
    let port = std::env::var("DATUM_TEST_REDIS").unwrap();
    let client = redis::Client::open(format!("redis://127.0.0.1:{port}/13")).unwrap();
    let mut con = client.get_multiplexed_async_connection().await.unwrap();
    let gz_json = gzip(br#"{"a":[1,2]}"#);
    let () = redis::pipe()
        .cmd("SET").arg("s:mp").arg(MSGPACK).ignore()
        .cmd("SET").arg("s:gz").arg(&gz_json).ignore()
        .cmd("SET").arg("s:pb").arg(PROTOBUF).ignore()
        .cmd("SET").arg("s:json").arg(r#"{"b":true}"#).ignore()
        .cmd("HSET").arg("h").arg("f1").arg(MSGPACK).arg("f2").arg("plain").ignore()
        .cmd("RPUSH").arg("l").arg("x").arg(zlib(PROTOBUF)).ignore()
        .cmd("SADD").arg("set").arg(PROTOBUF).ignore()
        .cmd("ZADD").arg("z").arg(1).arg(MSGPACK).ignore()
        .cmd("XADD").arg("x").arg("1-0").arg("a").arg(&gz_json).arg("b").arg("text").ignore()
        .cmd("HSET").arg("plainh").arg("k").arg("[1,2]").ignore()
        .query_async(&mut con)
        .await
        .unwrap();

    async fn get(st: &AppState, key: &str) -> datum_core::rds::client::KeyValue {
        service::redis_get_key(st, DECODE_ID, key).await.unwrap().0
    }

    // String
    for (key, want, layers) in [
        ("s:mp", Format::Msgpack, vec![]),
        ("s:gz", Format::Json, vec![Format::Gzip]),
        ("s:pb", Format::Protobuf, vec![]),
        ("s:json", Format::Json, vec![]),
    ] {
        let v = get(&st, key).await;
        let d = decode_cell(&v.rows[0][0], &v.raw, 0, 0);
        assert_eq!((d.format, d.layers), (want, layers), "{key}");
    }
    let v = get(&st, "s:mp").await;
    assert!(v.binary, "非 UTF-8 的值仍标记 binary,编辑器据此只读");
    let d = decode_cell(&v.rows[0][0], &v.raw, 0, 0);
    assert!(d.tree.unwrap().contains("\"Ann\""));
    // 合法 UTF-8 的值不带原始字节,也不只读
    let v = get(&st, "s:json").await;
    assert!(v.raw.is_empty() && !v.binary);

    // Hash:值列;文本的值不带原始字节
    let v = get(&st, "h").await;
    let i = v.rows.iter().position(|r| r[0] == "f1").unwrap();
    assert_eq!(decode_cell(&v.rows[i][1], &v.raw, i, 1).format, Format::Msgpack);
    let j = v.rows.iter().position(|r| r[0] == "f2").unwrap();
    assert!(!v.raw.iter().any(|c| c.row == j));
    assert_eq!(decode_cell(&v.rows[j][1], &v.raw, j, 1).format, Format::Text);
    let v = get(&st, "plainh").await;
    assert_eq!(decode_cell(&v.rows[0][1], &v.raw, 0, 1).format, Format::Json);
    assert!(!v.binary);

    // List(zlib 里是 Protobuf)、Set、ZSet 成员
    let v = get(&st, "l").await;
    let d = decode_cell(&v.rows[1][1], &v.raw, 1, 1);
    assert_eq!((d.format, d.layers), (Format::Protobuf, vec![Format::Zlib]));
    let v = get(&st, "set").await;
    assert_eq!(decode_cell(&v.rows[0][0], &v.raw, 0, 0).format, Format::Protobuf);
    let v = get(&st, "z").await;
    assert_eq!(decode_cell(&v.rows[0][0], &v.raw, 0, 0).format, Format::Msgpack);
    assert_eq!(v.rows[0][1], "1");

    // Stream 字段值
    let p = service::redis_stream_range(&st, DECODE_ID, None, "x", &range("-", "+", 10, false)).await.unwrap();
    let e = &p.entries[0];
    assert_eq!(e.fields[1], ("b".to_string(), "text".to_string()));
    let d = decode_cell(&e.fields[0].1, &e.raw, 0, 1);
    assert_eq!((d.format, d.layers), (Format::Json, vec![Format::Gzip]));
    assert!(!e.raw.iter().any(|c| c.row == 1));
}
