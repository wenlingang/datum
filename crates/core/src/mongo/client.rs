//! MongoDB 连接池:按连接 id 缓存 `mongodb::Client`(其内部自带连接池)及其当前库。

use super::convert::{docs_to_result, to_shell, values_to_result, MongoResult};
use super::parse::{pipeline_write_stage, MongoOp, Update};
use crate::error::{AppError, AppResult, ErrorKind};
use futures_util::TryStreamExt as _;
use mongodb::bson::{doc, Bson, Document};
use mongodb::options::{ClientOptions, UpdateModifications};
use mongodb::{Client, Cursor};
use rust_i18n::t;
use std::collections::HashMap;
use std::sync::Mutex as StdMutex;
use std::time::Duration;
use tokio::sync::Mutex;

const SYSTEM_DBS: [&str; 3] = ["admin", "local", "config"];
const DEFAULT_MAX_TIME: Duration = Duration::from_secs(60);

/// 集合浏览的一页。
pub struct FindPage {
    pub filter: Document,
    pub sort: Option<Document>,
    pub projection: Option<Document>,
    pub skip: u64,
    pub limit: i64,
}

pub struct MongoPool {
    conns: Mutex<HashMap<String, (Client, String)>>,
    /// 读操作的服务端时限;测试里可以调小
    max_time: StdMutex<Duration>,
}

impl Default for MongoPool {
    fn default() -> Self {
        Self {
            conns: Mutex::default(),
            max_time: StdMutex::new(DEFAULT_MAX_TIME),
        }
    }
}

fn server_code(e: &mongodb::error::Error) -> Option<i32> {
    use mongodb::error::{ErrorKind as K, WriteFailure};
    match e.kind.as_ref() {
        K::Command(c) => Some(c.code),
        K::Write(WriteFailure::WriteError(w)) => Some(w.code),
        _ => None,
    }
}

/// 驱动错误 → AppError:常见错误码附本地化提示,detail 以 `code ` 开头并保留原文。
pub(crate) fn merr(e: mongodb::error::Error) -> AppError {
    use mongodb::error::ErrorKind as K;
    let code = server_code(&e);
    let message = match (code, e.kind.as_ref()) {
        (Some(13), _) => t!("core.mongo.err.unauthorized"),
        (Some(11000), _) => t!("core.mongo.err.duplicate_key"),
        (Some(50), _) => t!("core.mongo.err.max_time"),
        (_, K::ServerSelection { .. }) => t!("core.mongo.err.server_selection"),
        _ => t!("core.mongo.err.command_failed"),
    };
    let detail = match code {
        Some(code) => format!("{code} {e}"),
        None => e.to_string(),
    };
    AppError::new(ErrorKind::Mongo, message).with_detail(detail)
}

/// 连接失败:`tls_rejected` 表示服务器不接受 TLS 握手(只有这时才可以退回明文)。
struct OpenError {
    err: AppError,
    tls_rejected: bool,
}

/// 监控连接报出的 I/O 错误是否为 TLS 握手失败(明文服务器收到 ClientHello 后直接断开)。
fn is_tls_handshake_failure(e: &mongodb::error::Error) -> bool {
    matches!(e.kind.as_ref(), mongodb::error::ErrorKind::Io(_))
        && e.to_string().to_ascii_lowercase().contains("tls")
}

/// `probe_tls`:这是 `Prefer` 的加密尝试。服务器一拒绝握手就立即返回 `tls_rejected`,
/// 不等完整的 server selection 超时;单纯慢(握手没失败)则照常等到超时,不降级。
async fn open(uri: &str, probe_tls: bool) -> Result<Client, OpenError> {
    crate::tls::install_default_provider();
    let mut opts = ClientOptions::parse(uri).await.map_err(|e| OpenError {
        err: AppError::new(ErrorKind::Connection, t!("core.mongo.err.invalid_uri"))
            .with_detail(e.to_string()),
        tls_rejected: false,
    })?;
    opts.app_name = Some("Datum".into());
    opts.server_selection_timeout = Some(Duration::from_secs(5));
    opts.connect_timeout = Some(Duration::from_secs(10));
    let rejected = std::sync::Arc::new(tokio::sync::Notify::new());
    if probe_tls {
        let notify = rejected.clone();
        opts.sdam_event_handler = Some(mongodb::event::EventHandler::callback(
            move |ev: mongodb::event::sdam::SdamEvent| {
                if let mongodb::event::sdam::SdamEvent::ServerDescriptionChanged(ev) = ev {
                    if ev
                        .new_description
                        .error()
                        .is_some_and(is_tls_handshake_failure)
                    {
                        notify.notify_one();
                    }
                }
            },
        ));
    }
    let client = Client::with_options(opts).map_err(|e| OpenError {
        err: merr(e),
        tls_rejected: false,
    })?;
    let failed = |detail: String| {
        AppError::new(
            ErrorKind::Connection,
            t!("core.err.connect_failed", db = "MongoDB"),
        )
        .with_detail(detail)
    };
    let admin = client.database("admin");
    tokio::select! {
        r = admin.run_command(doc! { "ping": 1 }) => match r {
            Ok(_) => Ok(client),
            Err(e) => Err(OpenError { err: failed(merr(e).display()), tls_rejected: false }),
        },
        _ = rejected.notified() => Err(OpenError {
            err: failed(t!("core.mongo.err.tls_rejected").to_string()),
            tls_rejected: true,
        }),
    }
}

/// 依次尝试候选连接串。`Prefer` 有两个(先加密、再明文):
/// 只有服务器明确拒绝 TLS 握手时才退回明文,超时、认证失败等一律如实报错,不降级。
async fn open_first(uris: &[String]) -> AppResult<Client> {
    let mut last = AppError::new(ErrorKind::Connection, t!("core.mongo.err.invalid_uri"));
    for (i, u) in uris.iter().enumerate() {
        let has_fallback = i + 1 < uris.len();
        match open(u, has_fallback).await {
            Ok(c) => return Ok(c),
            Err(OpenError { err, tls_rejected }) => {
                if !(has_fallback && tls_rejected) {
                    return Err(err);
                }
                last = err;
            }
        }
    }
    Err(last)
}

pub async fn test_connect(uris: &[String]) -> AppResult<()> {
    let client = open_first(uris).await?;
    client.shutdown().await;
    Ok(())
}

/// 读取至多 `max` 篇;还有剩余时返回 truncated = true。
async fn collect(mut cursor: Cursor<Document>, max: usize) -> AppResult<(Vec<Document>, bool)> {
    let mut out = vec![];
    while let Some(d) = cursor.try_next().await.map_err(merr)? {
        if out.len() == max {
            return Ok((out, true));
        }
        out.push(d);
    }
    Ok((out, false))
}

enum Step {
    Read(MongoResult),
    Summary(String),
}

fn to_modifications(u: Update) -> UpdateModifications {
    match u {
        Update::Doc(d) => UpdateModifications::Document(d),
        Update::Pipeline(p) => UpdateModifications::Pipeline(p),
    }
}

fn sorted_names(mut names: Vec<String>) -> Vec<Bson> {
    names.sort();
    names.into_iter().map(Bson::String).collect()
}

impl MongoPool {
    pub fn set_max_time(&self, d: Duration) {
        *self.max_time.lock().unwrap() = d;
    }

    fn max_time(&self) -> Duration {
        *self.max_time.lock().unwrap()
    }

    pub async fn connect(&self, id: &str, uris: &[String], db: String) -> AppResult<()> {
        let client = open_first(uris).await?;
        // 先放开池锁再关旧连接:关闭要等进行中的操作结束,持锁会卡住所有连接
        let old = self.conns.lock().await.insert(id.to_string(), (client, db));
        if let Some((old, _)) = old {
            old.shutdown().await;
        }
        Ok(())
    }

    pub async fn disconnect(&self, id: &str) {
        let removed = self.conns.lock().await.remove(id);
        if let Some((c, _)) = removed {
            c.shutdown().await;
        }
    }

    async fn handle(&self, id: &str) -> AppResult<(Client, String)> {
        self.conns
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::new(ErrorKind::Connection, t!("core.err.not_connected")))
    }

    /// 当前库;未选库时报错(集合级操作都需要库)。
    async fn db(&self, id: &str) -> AppResult<mongodb::Database> {
        let (client, db) = self.handle(id).await?;
        if db.is_empty() {
            return Err(AppError::new(
                ErrorKind::Mongo,
                t!("core.mongo.err.no_database"),
            ));
        }
        Ok(client.database(&db))
    }

    /// 库列表:隐藏系统库(当前库除外),并保证当前库在列。
    /// 无权列库(code 13)时退回只显示配置的库。
    pub async fn list_databases(&self, id: &str) -> AppResult<Vec<String>> {
        let (client, current) = self.handle(id).await?;
        match client.list_database_names().await {
            Ok(mut names) => {
                names.retain(|n| *n == current || !SYSTEM_DBS.contains(&n.as_str()));
                // 受限用户只能看到有权限且已有数据的库,当前库为空时不会出现在结果里
                if !current.is_empty() && !names.contains(&current) {
                    names.push(current);
                }
                names.sort();
                Ok(names)
            }
            Err(e) if server_code(&e) == Some(13) => {
                if current.is_empty() {
                    Err(AppError::new(
                        ErrorKind::Mongo,
                        t!("core.mongo.err.cannot_list_dbs"),
                    ))
                } else {
                    Ok(vec![current])
                }
            }
            Err(e) => Err(merr(e)),
        }
    }

    /// 当前库的集合(含视图),未选库时为空。
    pub async fn list_collections(&self, id: &str) -> AppResult<Vec<String>> {
        let (client, db) = self.handle(id).await?;
        if db.is_empty() {
            return Ok(vec![]);
        }
        let mut names = client
            .database(&db)
            .list_collection_names()
            .await
            .map_err(merr)?;
        names.sort();
        Ok(names)
    }

    pub async fn find_page(&self, id: &str, coll: &str, page: FindPage) -> AppResult<MongoResult> {
        let c = self.db(id).await?.collection::<Document>(coll);
        let mut q = c
            .find(page.filter)
            .skip(page.skip)
            .limit(page.limit)
            .max_time(self.max_time());
        if let Some(s) = page.sort {
            q = q.sort(s);
        }
        if let Some(p) = page.projection {
            q = q.projection(p);
        }
        let (docs, _) = collect(q.await.map_err(merr)?, page.limit.max(0) as usize).await?;
        Ok(docs_to_result(docs, false))
    }

    /// 无过滤条件时用估算计数(快,不精确),有条件时精确计数。
    pub async fn count(
        &self,
        id: &str,
        coll: &str,
        filter: Option<Document>,
    ) -> AppResult<(u64, bool)> {
        let c = self.db(id).await?.collection::<Document>(coll);
        let mt = self.max_time();
        match filter {
            None => c
                .estimated_document_count()
                .max_time(mt)
                .await
                .map(|n| (n, false))
                .map_err(merr),
            Some(f) => c
                .count_documents(f)
                .max_time(mt)
                .await
                .map(|n| (n, true))
                .map_err(merr),
        }
    }

    /// 按 `_id` 取完整文档(mongosh 文本);文档已不存在时为 None。
    /// 编辑器必须用它而不是当前页的文档:当前页可能带了 Projection,原样保存会删掉其余字段。
    pub async fn find_by_id(
        &self,
        id: &str,
        coll: &str,
        doc_id: Bson,
    ) -> AppResult<Option<String>> {
        let c = self.db(id).await?.collection::<Document>(coll);
        let found = c
            .find_one(doc! { "_id": doc_id })
            .max_time(self.max_time())
            .await
            .map_err(merr)?;
        Ok(found.map(|d| to_shell(&Bson::Document(d), true)))
    }

    pub async fn replace_one(
        &self,
        id: &str,
        coll: &str,
        doc_id: Bson,
        doc: Document,
    ) -> AppResult<()> {
        let c = self.db(id).await?.collection::<Document>(coll);
        let r = c
            .replace_one(doc! { "_id": doc_id }, doc)
            .await
            .map_err(merr)?;
        if r.matched_count == 0 {
            return Err(AppError::new(
                ErrorKind::Mongo,
                t!("core.mongo.err.doc_gone"),
            ));
        }
        Ok(())
    }

    pub async fn insert_one(&self, id: &str, coll: &str, doc: Document) -> AppResult<Bson> {
        let c = self.db(id).await?.collection::<Document>(coll);
        Ok(c.insert_one(doc).await.map_err(merr)?.inserted_id)
    }

    pub async fn delete_one(&self, id: &str, coll: &str, doc_id: Bson) -> AppResult<()> {
        let c = self.db(id).await?.collection::<Document>(coll);
        let r = c.delete_one(doc! { "_id": doc_id }).await.map_err(merr)?;
        if r.deleted_count == 0 {
            return Err(AppError::new(
                ErrorKind::Mongo,
                t!("core.mongo.err.doc_gone"),
            ));
        }
        Ok(())
    }

    /// 依次执行;返回最后一个读操作的结果,所有写操作的摘要合并到 `summary`。
    /// `read_only` 时含写操作的脚本整体拒绝(MongoDB 没有会话级只读,只能在客户端拦)。
    pub async fn execute(
        &self,
        id: &str,
        ops: Vec<MongoOp>,
        max_rows: u32,
        read_only: bool,
    ) -> AppResult<MongoResult> {
        if read_only && ops.iter().any(MongoOp::is_write) {
            return Err(AppError::new(
                ErrorKind::Mongo,
                t!("core.mongo.err.read_only"),
            ));
        }
        let max = max_rows as usize;
        let mut last_read: Option<MongoResult> = None;
        let mut summaries = vec![];
        for (i, op) in ops.into_iter().enumerate() {
            let step = self.execute_one(id, op, max).await.map_err(|e| AppError {
                message: t!(
                    "core.mongo.err.statement_failed",
                    n = i + 1,
                    message = e.message
                )
                .to_string(),
                ..e
            })?;
            match step {
                Step::Read(r) => last_read = Some(r),
                Step::Summary(s) => summaries.push(s),
            }
        }
        let mut out = last_read.unwrap_or_default();
        if !summaries.is_empty() {
            out.summary = Some(summaries.join("\n"));
        }
        Ok(out)
    }

    async fn execute_one(&self, id: &str, op: MongoOp, max: usize) -> AppResult<Step> {
        let mt = self.max_time();
        if let MongoOp::ShowDbs = op {
            let names = self.list_databases(id).await?;
            return Ok(Step::Read(values_to_result("name", sorted_names(names))));
        }
        let db = self.db(id).await?;
        let coll = |name: &str| db.collection::<Document>(name);
        Ok(match op {
            MongoOp::ShowDbs => unreachable!("handled above"),
            MongoOp::ShowCollections => {
                let names = db.list_collection_names().await.map_err(merr)?;
                Step::Read(values_to_result("name", sorted_names(names)))
            }
            MongoOp::Find {
                coll: c,
                filter,
                projection,
                sort,
                skip,
                limit,
            } => {
                let cap = limit
                    .filter(|l| *l > 0)
                    .map_or(max, |l| (l as usize).min(max));
                let col = coll(&c);
                let mut q = col.find(filter).max_time(mt).limit(cap as i64 + 1);
                if let Some(p) = projection {
                    q = q.projection(p);
                }
                if let Some(s) = sort {
                    q = q.sort(s);
                }
                if let Some(s) = skip {
                    q = q.skip(s);
                }
                let (docs, truncated) = collect(q.await.map_err(merr)?, cap).await?;
                // 用户自己写的 limit 正好取满不算截断
                Step::Read(docs_to_result(docs, truncated && cap == max))
            }
            MongoOp::FindOne {
                coll: c,
                filter,
                projection,
            } => {
                let col = coll(&c);
                let mut q = col.find_one(filter).max_time(mt);
                if let Some(p) = projection {
                    q = q.projection(p);
                }
                let found = q.await.map_err(merr)?;
                Step::Read(docs_to_result(found.into_iter().collect(), false))
            }
            MongoOp::Count { coll: c, filter } => {
                let n = coll(&c)
                    .count_documents(filter)
                    .max_time(mt)
                    .await
                    .map_err(merr)?;
                Step::Read(values_to_result("count", vec![Bson::Int64(n as i64)]))
            }
            MongoOp::Distinct {
                coll: c,
                field,
                filter,
            } => {
                let vals = coll(&c)
                    .distinct(&field, filter)
                    .max_time(mt)
                    .await
                    .map_err(merr)?;
                Step::Read(values_to_result(&field, vals))
            }
            MongoOp::Aggregate { coll: c, pipeline } => {
                let writes = pipeline_write_stage(&pipeline).is_some();
                let cursor = coll(&c)
                    .aggregate(pipeline)
                    .max_time(mt)
                    .await
                    .map_err(merr)?;
                let (docs, truncated) = collect(cursor, max).await?;
                if writes {
                    Step::Summary(t!("core.mongo.summary.aggregate_written").to_string())
                } else {
                    Step::Read(docs_to_result(docs, truncated))
                }
            }
            MongoOp::GetIndexes { coll: c } => {
                let r = db
                    .run_command(doc! { "listIndexes": &c })
                    .await
                    .map_err(merr)?;
                let docs = r
                    .get_document("cursor")
                    .and_then(|cur| cur.get_array("firstBatch"))
                    .map(|a| a.iter().filter_map(|b| b.as_document().cloned()).collect())
                    .unwrap_or_default();
                Step::Read(docs_to_result(docs, false))
            }
            MongoOp::InsertOne { coll: c, doc } => {
                coll(&c).insert_one(doc).await.map_err(merr)?;
                Step::Summary(t!("core.mongo.summary.inserted", n = 1).to_string())
            }
            MongoOp::InsertMany { coll: c, docs } => {
                let r = coll(&c).insert_many(docs).await.map_err(merr)?;
                Step::Summary(
                    t!("core.mongo.summary.inserted", n = r.inserted_ids.len()).to_string(),
                )
            }
            MongoOp::UpdateOne {
                coll: c,
                filter,
                update,
            } => {
                let r = coll(&c)
                    .update_one(filter, to_modifications(update))
                    .await
                    .map_err(merr)?;
                Step::Summary(
                    t!(
                        "core.mongo.summary.updated",
                        matched = r.matched_count,
                        modified = r.modified_count
                    )
                    .to_string(),
                )
            }
            MongoOp::UpdateMany {
                coll: c,
                filter,
                update,
            } => {
                let r = coll(&c)
                    .update_many(filter, to_modifications(update))
                    .await
                    .map_err(merr)?;
                Step::Summary(
                    t!(
                        "core.mongo.summary.updated",
                        matched = r.matched_count,
                        modified = r.modified_count
                    )
                    .to_string(),
                )
            }
            MongoOp::ReplaceOne {
                coll: c,
                filter,
                doc,
            } => {
                let r = coll(&c).replace_one(filter, doc).await.map_err(merr)?;
                Step::Summary(
                    t!(
                        "core.mongo.summary.updated",
                        matched = r.matched_count,
                        modified = r.modified_count
                    )
                    .to_string(),
                )
            }
            MongoOp::DeleteOne { coll: c, filter } => {
                let r = coll(&c).delete_one(filter).await.map_err(merr)?;
                Step::Summary(t!("core.mongo.summary.deleted", n = r.deleted_count).to_string())
            }
            MongoOp::DeleteMany { coll: c, filter } => {
                let r = coll(&c).delete_many(filter).await.map_err(merr)?;
                Step::Summary(t!("core.mongo.summary.deleted", n = r.deleted_count).to_string())
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tokio::io::AsyncReadExt as _;

    /// 慢 TLS 服务器:接受连接、读走首包、然后一直不回应。
    /// TLS ClientHello 以 0x16 开头;收到别的首字节说明客户端退回了明文。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn prefer_does_not_downgrade_when_tls_is_merely_slow() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let plaintext_seen = Arc::new(AtomicBool::new(false));
        let seen = plaintext_seen.clone();
        tokio::spawn(async move {
            let mut held = vec![];
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut first = [0u8; 1];
                if sock.read_exact(&mut first).await.is_ok() && first[0] != 0x16 {
                    seen.store(true, Ordering::SeqCst);
                }
                held.push(sock);
            }
        });
        let uris = crate::mongo::uri::form_uris(
            "127.0.0.1",
            port,
            "u",
            "pw",
            None,
            crate::core::connection::SslMode::Prefer,
        );
        assert!(test_connect(&uris).await.is_err());
        assert!(
            !plaintext_seen.load(Ordering::SeqCst),
            "credentials were sent over plaintext after a slow TLS handshake"
        );
    }
}
