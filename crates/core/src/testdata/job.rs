//! 后台插入:在独立连接上分批写入,不超过 [`SINGLE_TX_MAX`] 行时整个过程一个事务,
//! 更多时每 [`COMMIT_EVERY`] 行提交一次。界面轮询进度;取消回滚当前事务
//! (单事务时什么都不留下,分批提交时已提交的批次保留,进度里给出保留的行数)。
//!
//! 与 SQLite 导入(`sqlite::import_data`)同一套做法:另开连接、后台线程、原子计数器报进度。
//! SQLite 用预编译 INSERT 逐行绑定;PG / MySQL 用多行 INSERT(值写成字面量)减少往返。

use super::engine::{insert_sql, Engine};
use super::gen::Value;
use super::plan::{COMMIT_EVERY, SINGLE_TX_MAX};
use crate::error::{AppError, AppResult, ErrorKind};
use crate::sql::Dialect;
use rust_i18n::t;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// 一批写入的目标连接。
pub trait Writer: Send {
    fn begin(&mut self) -> AppResult<()>;
    fn insert(&mut self, table: &str, columns: &[String], rows: &[Vec<Value>]) -> AppResult<()>;
    fn commit(&mut self) -> AppResult<()>;
    fn rollback(&mut self);
    /// 每批多少行。
    fn batch(&self) -> usize;
}

fn sql_err(key: &str, detail: impl ToString) -> AppError {
    AppError::new(ErrorKind::Query, t!(key)).with_detail(detail.to_string())
}

/// SQLite:另开的一条连接,预编译 INSERT 逐行绑定。
pub struct SqliteWriter {
    conn: rusqlite::Connection,
}

impl SqliteWriter {
    pub fn open(path: &str) -> AppResult<SqliteWriter> {
        let conn = rusqlite::Connection::open(path).map_err(|e| sql_err("core.err.sqlite_open_failed", e))?;
        let _ = conn.busy_timeout(std::time::Duration::from_secs(10));
        // 外键照常检查:生成的键都取自父表,违反了说明有并发改动,宁可失败
        let _ = conn.execute_batch("PRAGMA foreign_keys = ON");
        Ok(SqliteWriter { conn })
    }

    pub fn from_conn(conn: rusqlite::Connection) -> SqliteWriter {
        SqliteWriter { conn }
    }
}

fn bind(v: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as S;
    match v {
        Value::Null => S::Null,
        Value::Int(i) => S::Integer(*i),
        Value::Num(s) => s.parse::<f64>().map(S::Real).unwrap_or_else(|_| S::Text(s.clone())),
        Value::Float(f) => S::Real(*f),
        Value::Bool(b) => S::Integer(i64::from(*b)),
        Value::Text(s) => S::Text(s.clone()),
        Value::Bytes(b) => S::Blob(b.clone()),
    }
}

impl Writer for SqliteWriter {
    fn begin(&mut self) -> AppResult<()> {
        self.conn.execute_batch("BEGIN").map_err(|e| sql_err("core.err.begin_tx_failed", e))
    }
    fn insert(&mut self, table: &str, columns: &[String], rows: &[Vec<Value>]) -> AppResult<()> {
        let q = |s: &str| crate::sql::build::ident(s, Dialect::Sqlite);
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            q(table),
            columns.iter().map(|c| q(c)).collect::<Vec<_>>().join(", "),
            vec!["?"; columns.len()].join(", ")
        );
        let mut stmt = self.conn.prepare_cached(&sql).map_err(|e| sql_err("testdata.err.insert_failed", e))?;
        for r in rows {
            stmt.execute(rusqlite::params_from_iter(r.iter().map(bind)))
                .map_err(|e| sql_err("testdata.err.insert_failed", e))?;
        }
        Ok(())
    }
    fn commit(&mut self) -> AppResult<()> {
        self.conn.execute_batch("COMMIT").map_err(|e| sql_err("core.err.commit_tx_failed", e))
    }
    fn rollback(&mut self) {
        let _ = self.conn.execute_batch("ROLLBACK");
    }
    fn batch(&self) -> usize {
        1000
    }
}

/// PostgreSQL:独立会话上的多行 INSERT。后台线程经运行时句柄驱动异步客户端。
pub struct PgWriter {
    client: tokio_postgres::Client,
    rt: tokio::runtime::Handle,
}

impl PgWriter {
    pub fn new(client: tokio_postgres::Client, rt: tokio::runtime::Handle) -> PgWriter {
        PgWriter { client, rt }
    }
    fn exec(&self, sql: &str, key: &str) -> AppResult<()> {
        self.rt.block_on(self.client.simple_query(sql)).map(|_| ()).map_err(|e| {
            use std::error::Error as _;
            let mut s = e.to_string();
            let mut src = e.source();
            while let Some(c) = src {
                s.push_str(": ");
                s.push_str(&c.to_string());
                src = c.source();
            }
            sql_err(key, s)
        })
    }
}

impl Writer for PgWriter {
    fn begin(&mut self) -> AppResult<()> {
        self.exec("BEGIN", "core.err.begin_tx_failed")
    }
    fn insert(&mut self, table: &str, columns: &[String], rows: &[Vec<Value>]) -> AppResult<()> {
        self.exec(&insert_sql(table, columns, rows, Dialect::Pg), "testdata.err.insert_failed")
    }
    fn commit(&mut self) -> AppResult<()> {
        self.exec("COMMIT", "core.err.commit_tx_failed")
    }
    fn rollback(&mut self) {
        let _ = self.exec("ROLLBACK", "core.err.query_failed");
    }
    fn batch(&self) -> usize {
        500
    }
}

/// MySQL:连接池里取的独立连接上的多行 INSERT。
pub struct MyWriter {
    conn: mysql_async::Conn,
    rt: tokio::runtime::Handle,
}

impl MyWriter {
    pub fn new(conn: mysql_async::Conn, rt: tokio::runtime::Handle) -> MyWriter {
        MyWriter { conn, rt }
    }
    fn exec(&mut self, sql: &str, key: &str) -> AppResult<()> {
        use mysql_async::prelude::Queryable;
        let conn = &mut self.conn;
        self.rt.block_on(conn.query_drop(sql)).map_err(|e| sql_err(key, e))
    }
}

impl Writer for MyWriter {
    fn begin(&mut self) -> AppResult<()> {
        self.exec("START TRANSACTION", "core.err.begin_tx_failed")
    }
    fn insert(&mut self, table: &str, columns: &[String], rows: &[Vec<Value>]) -> AppResult<()> {
        self.exec(&insert_sql(table, columns, rows, Dialect::Mysql), "testdata.err.insert_failed")
    }
    fn commit(&mut self) -> AppResult<()> {
        self.exec("COMMIT", "core.err.commit_tx_failed")
    }
    fn rollback(&mut self) {
        let _ = self.exec("ROLLBACK", "core.err.query_failed");
    }
    fn batch(&self) -> usize {
        500
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// 已写入的行数(含未提交的)
    pub rows: u64,
    /// 已提交(取消 / 失败后仍会保留)的行数
    pub committed: u64,
    pub total: u64,
    /// 分批提交(超过单事务上限)
    pub batched: bool,
    pub done: bool,
    pub cancelled: bool,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct Job {
    cancel: AtomicBool,
    rows: AtomicU64,
    committed: AtomicU64,
    state: Mutex<Progress>,
}

impl Job {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    pub fn progress(&self) -> Progress {
        let mut p = self.state.lock().unwrap().clone();
        p.rows = self.rows.load(Ordering::Relaxed);
        p.committed = self.committed.load(Ordering::Relaxed);
        p
    }
}

fn cancelled_err() -> AppError {
    AppError::new(ErrorKind::Query, t!("testdata.err.cancelled"))
}

/// 把引擎剩下的行全部写入;返回提交的行数。出错或取消时回滚当前事务后返回错误
/// (分批提交时,之前提交的批次保留,行数见 `job` 的 committed)。
pub fn run(engine: &mut Engine, table: &str, writer: &mut dyn Writer, job: &Job) -> AppResult<u64> {
    let total = engine.remaining();
    let batched = total > SINGLE_TX_MAX;
    {
        let mut s = job.state.lock().unwrap();
        s.total = total;
        s.batched = batched;
    }
    let columns = engine.columns().to_vec();
    let size = writer.batch().max(1) as u64;
    writer.begin()?;
    let mut written = 0u64;
    let mut committed = 0u64;
    let fail = |writer: &mut dyn Writer, e: AppError| -> AppResult<u64> {
        writer.rollback();
        Err(e)
    };
    while engine.remaining() > 0 {
        if job.cancelled() {
            return fail(writer, cancelled_err());
        }
        let n = size.min(engine.remaining());
        let mut rows = Vec::with_capacity(n as usize);
        for _ in 0..n {
            match engine.next_row() {
                Ok(r) => rows.push(r),
                Err(e) => return fail(writer, e),
            }
        }
        if let Err(e) = writer.insert(table, &columns, &rows) {
            let e = AppError::new(ErrorKind::Query, t!("testdata.err.rows_failed", from = written + 1, to = written + n))
                .with_detail(e.detail.unwrap_or(e.message));
            return fail(writer, e);
        }
        written += n;
        job.rows.store(written, Ordering::Relaxed);
        if batched && written - committed >= COMMIT_EVERY && engine.remaining() > 0 {
            if let Err(e) = writer.commit() {
                return fail(writer, e);
            }
            committed = written;
            job.committed.store(committed, Ordering::Relaxed);
            writer.begin()?;
        }
    }
    if job.cancelled() {
        return fail(writer, cancelled_err());
    }
    writer.commit()?;
    job.committed.store(written, Ordering::Relaxed);
    Ok(written)
}

/// 各连接正在进行 / 最近一次的造数任务。
#[derive(Default)]
pub struct Jobs {
    jobs: Mutex<HashMap<String, Arc<Job>>>,
}

impl Jobs {
    pub fn running(&self, id: &str) -> bool {
        self.jobs.lock().unwrap().get(id).is_some_and(|j| !j.state.lock().unwrap().done)
    }

    /// 在后台线程里写入。同一连接上已有进行中的任务则报错。
    pub fn start(&self, id: &str, mut engine: Engine, table: String, mut writer: Box<dyn Writer>) -> AppResult<()> {
        let mut jobs = self.jobs.lock().unwrap();
        if jobs.get(id).is_some_and(|j| !j.state.lock().unwrap().done) {
            return Err(AppError::new(ErrorKind::Query, t!("testdata.err.running")));
        }
        let job = Arc::new(Job::default());
        job.state.lock().unwrap().total = engine.remaining();
        jobs.insert(id.to_string(), job.clone());
        std::thread::spawn(move || {
            let result = run(&mut engine, &table, writer.as_mut(), &job);
            drop(writer);
            let mut s = job.state.lock().unwrap();
            s.done = true;
            match result {
                Ok(_) => {}
                Err(_) if job.cancelled() => s.cancelled = true,
                Err(e) => s.error = Some(e.display()),
            }
        });
        Ok(())
    }

    pub fn progress(&self, id: &str) -> Option<Progress> {
        self.jobs.lock().unwrap().get(id).map(|j| j.progress())
    }

    pub fn cancel(&self, id: &str) {
        if let Some(j) = self.jobs.lock().unwrap().get(id) {
            j.cancel();
        }
    }
}
