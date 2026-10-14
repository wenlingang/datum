use crate::error::{AppError, AppResult, ErrorKind};
pub use crate::pg::client::QueryResult;
use rusqlite::types::ValueRef;
use rusqlite::Connection as SqliteConn;
use rust_i18n::t;
use std::collections::HashMap;
use std::sync::Mutex;

/// rusqlite::Connection 是 Send 但非 Sync;操作均为同步,放进 std Mutex 即可,
/// 命令侧不会跨 await 持锁,因此 Pool 满足 Send + Sync(Tauri State 要求)。
#[derive(Default)]
pub struct SqlitePool {
    conns: Mutex<HashMap<String, SqliteConn>>,
}

/// 把一个单元格的值统一成字符串(NULL → None)。二进制 BLOB 按 lossy UTF-8 呈现,
/// 与 Redis 侧对二进制值的处理一致。
fn cell(v: ValueRef<'_>) -> Option<String> {
    match v {
        ValueRef::Null => None,
        ValueRef::Integer(i) => Some(i.to_string()),
        ValueRef::Real(f) => Some(f.to_string()),
        ValueRef::Text(t) => Some(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Some(String::from_utf8_lossy(b).into_owned()),
    }
}

/// 验证 SQLite 文件可打开(不存在则报错,避免误建空库;不缓存)。
pub fn test_connect(path: &str) -> AppResult<()> {
    if path.is_empty() {
        return Err(AppError::new(
            ErrorKind::Connection,
            t!("core.err.sqlite_path_missing"),
        ));
    }
    if !std::path::Path::new(path).exists() {
        return Err(AppError::new(
            ErrorKind::Connection,
            t!("core.err.sqlite_file_not_found"),
        ));
    }
    SqliteConn::open(path).map(|_| ()).map_err(|e| {
        AppError::new(ErrorKind::Connection, t!("core.err.sqlite_open_failed"))
            .with_detail(e.to_string())
    })
}

impl SqlitePool {
    /// 打开(或创建)指定路径的 SQLite 文件并缓存到 id。
    pub fn connect(&self, id: &str, path: &str) -> AppResult<()> {
        let conn = SqliteConn::open(path).map_err(|e| {
            AppError::new(ErrorKind::Connection, t!("core.err.sqlite_open_failed"))
                .with_detail(e.to_string())
        })?;
        self.conns.lock().unwrap().insert(id.to_string(), conn);
        Ok(())
    }

    #[allow(dead_code)] // 目前仅测试使用,保留备将来重连/状态显示
    pub fn is_connected(&self, id: &str) -> bool {
        self.conns.lock().unwrap().contains_key(id)
    }

    /// 断开并移除缓存的连接(不存在时静默)。删库前强制断开残留连接用。
    pub fn disconnect(&self, id: &str) {
        self.conns.lock().unwrap().remove(id);
    }

    /// 在单事务里逐条执行导入 SQL:全成功提交,任一失败整体回滚(错误标注第几条)。
    pub fn import_sql(&self, id: &str, statements: &[String]) -> AppResult<usize> {
        let mut guard = self.conns.lock().unwrap();
        let conn = guard
            .get_mut(id)
            .ok_or_else(|| AppError::new(ErrorKind::Connection, t!("core.err.not_connected")))?;
        let tx = conn.transaction().map_err(|e| {
            AppError::new(ErrorKind::Query, t!("core.err.begin_tx_failed"))
                .with_detail(e.to_string())
        })?;
        for (i, sql) in statements.iter().enumerate() {
            // 出错直接返回:tx 被 drop 时自动回滚
            tx.execute_batch(sql).map_err(|e| {
                AppError::new(
                    ErrorKind::Query,
                    t!("core.err.stmt_failed_all_rolled_back", n = i + 1),
                )
                .with_detail(e.to_string())
            })?;
        }
        tx.commit().map_err(|e| {
            AppError::new(ErrorKind::Query, t!("core.err.commit_tx_failed"))
                .with_detail(e.to_string())
        })?;
        Ok(statements.len())
    }

    /// 执行任意 SQL。返回行的语句(SELECT/PRAGMA/RETURNING)给出列+行;
    /// 不返回行的语句(INSERT/UPDATE/DELETE/DDL)给出受影响行数。
    pub fn query(&self, id: &str, sql: &str) -> AppResult<QueryResult> {
        self.query_capped(id, sql, None)
    }

    /// 同 query,但结果行数不超过 max_rows(None = 不限)。
    /// 到达上限即停止读取游标 —— 大结果集不会被整体拉进内存。
    pub fn query_capped(
        &self,
        id: &str,
        sql: &str,
        max_rows: Option<u32>,
    ) -> AppResult<QueryResult> {
        let guard = self.conns.lock().unwrap();
        let conn = guard
            .get(id)
            .ok_or_else(|| AppError::new(ErrorKind::Connection, t!("core.err.not_connected")))?;
        run_capped(conn, sql, max_rows)
    }

    /// 只读执行:query_only 开启、执行、关闭三步在同一把锁里完成,
    /// 期间同一连接上的其他请求插不进来。
    pub fn query_read_only(
        &self,
        id: &str,
        sql: &str,
        max_rows: Option<u32>,
    ) -> AppResult<QueryResult> {
        let guard = self.conns.lock().unwrap();
        let conn = guard
            .get(id)
            .ok_or_else(|| AppError::new(ErrorKind::Connection, t!("core.err.not_connected")))?;
        let set = |on: bool| {
            conn.execute_batch(if on {
                "PRAGMA query_only = ON"
            } else {
                "PRAGMA query_only = OFF"
            })
            .map_err(|e| {
                AppError::new(ErrorKind::Query, t!("core.err.exec_failed"))
                    .with_detail(e.to_string())
            })
        };
        set(true)?;
        let result = run_capped(conn, sql, max_rows);
        set(false)?;
        result
    }
}

/// 在已拿到的连接上执行 SQL(调用方持有连接锁)。
fn run_capped(
    conn: &rusqlite::Connection,
    sql: &str,
    max_rows: Option<u32>,
) -> AppResult<QueryResult> {
    {
        let mut stmt = conn.prepare(sql).map_err(|e| {
            AppError::new(ErrorKind::Query, t!("core.err.query_failed")).with_detail(e.to_string())
        })?;
        let col_count = stmt.column_count();

        // 无结果列:当作执行型语句。用 execute_batch 以支持一次多条语句
        // (与 PG 的 simple_query 行为一致;单条 execute 会静默丢弃后续语句)。
        // 受影响行数取最近一条 INSERT/UPDATE/DELETE 的 changes();DDL 通常为 0。
        if col_count == 0 {
            drop(stmt);
            conn.execute_batch(sql).map_err(|e| {
                AppError::new(ErrorKind::Query, t!("core.err.exec_failed"))
                    .with_detail(e.to_string())
            })?;
            return Ok(QueryResult {
                columns: vec![],
                rows: vec![],
                affected: Some(conn.changes()),
            });
        }

        let columns: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let mut out: Vec<Vec<Option<String>>> = vec![];
        let mut rows = stmt.query([]).map_err(|e| {
            AppError::new(ErrorKind::Query, t!("core.err.query_failed")).with_detail(e.to_string())
        })?;
        let cap = max_rows.map(|n| n as usize).unwrap_or(usize::MAX);
        while out.len() < cap {
            let Some(row) = rows.next().map_err(|e| {
                AppError::new(ErrorKind::Query, t!("core.err.read_result_failed"))
                    .with_detail(e.to_string())
            })?
            else {
                break;
            };
            let r = (0..col_count)
                .map(|i| row.get_ref(i).ok().and_then(cell))
                .collect();
            out.push(r);
        }
        Ok(QueryResult {
            columns,
            rows: out,
            affected: None,
        })
    }
}

impl SqlitePool {
    /// 在一个事务里批量执行编辑与删除(全成功或全回滚)。返回受影响行数。
    /// new_value / pk_value 作为绑定参数传入,SQLite 依列亲和性自动转换标量类型;
    /// 标识符用双引号转义。先 UPDATE 后 DELETE。
    pub fn with_txn(
        &self,
        id: &str,
        edits: &[crate::sqlite::edit::CellEdit],
        deletes: &[crate::sqlite::edit::RowDelete],
    ) -> AppResult<u64> {
        let mut guard = self.conns.lock().unwrap();
        let conn = guard
            .get_mut(id)
            .ok_or_else(|| AppError::new(ErrorKind::Connection, t!("core.err.not_connected")))?;
        let tx = conn.transaction().map_err(|e| {
            AppError::new(ErrorKind::Edit, t!("core.err.begin_tx_failed"))
                .with_detail(e.to_string())
        })?;
        let mut n = 0u64;
        for e in edits {
            let sql = format!(
                "UPDATE \"{}\" SET \"{}\" = ? WHERE \"{}\" = ?",
                e.table.replace('"', "\"\""),
                e.column.replace('"', "\"\""),
                e.pk_col.replace('"', "\"\"")
            );
            let affected = tx
                .execute(&sql, rusqlite::params![e.new_value, e.pk_value])
                .map_err(|err| {
                    AppError::new(ErrorKind::Edit, t!("core.err.update_failed"))
                        .with_detail(err.to_string())
                })?;
            n += affected as u64;
        }
        for d in deletes {
            let sql = format!(
                "DELETE FROM \"{}\" WHERE \"{}\" = ?",
                d.table.replace('"', "\"\""),
                d.pk_col.replace('"', "\"\"")
            );
            let affected = tx
                .execute(&sql, rusqlite::params![d.pk_value])
                .map_err(|err| {
                    AppError::new(ErrorKind::Edit, t!("core.err.delete_failed"))
                        .with_detail(err.to_string())
                })?;
            n += affected as u64;
        }
        tx.commit().map_err(|e| {
            AppError::new(ErrorKind::Edit, t!("core.err.commit_tx_failed"))
                .with_detail(e.to_string())
        })?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::edit::CellEdit;

    /// 用内存库做单测:无需文件、无需外部依赖。
    fn pool() -> SqlitePool {
        let p = SqlitePool::default();
        p.connect("t", ":memory:").unwrap();
        p
    }

    #[test]
    fn connects_queries_and_counts() {
        let p = pool();
        assert!(p.is_connected("t"));
        p.query("t", "CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        let ins = p
            .query("t", "INSERT INTO users VALUES (1,'Alice'),(2,'Bob')")
            .unwrap();
        assert_eq!(ins.affected, Some(2));

        let r = p
            .query("t", "SELECT id, name FROM users ORDER BY id")
            .unwrap();
        assert_eq!(r.columns, vec!["id", "name"]);
        assert_eq!(r.rows[0], vec![Some("1".into()), Some("Alice".into())]);
        assert_eq!(r.affected, None);
    }

    #[test]
    fn null_becomes_none() {
        let p = pool();
        p.query("t", "CREATE TABLE t(a TEXT)").unwrap();
        p.query("t", "INSERT INTO t VALUES (NULL)").unwrap();
        let r = p.query("t", "SELECT a FROM t").unwrap();
        assert_eq!(r.rows[0][0], None);
    }

    #[test]
    fn disconnect_removes_cached_connection() {
        let p = pool();
        assert!(p.is_connected("t"));
        p.disconnect("t");
        assert!(!p.is_connected("t"));
        assert_eq!(
            p.query("t", "SELECT 1").unwrap_err().kind,
            ErrorKind::Connection
        );
    }

    #[test]
    fn max_rows_caps_result_and_none_means_unlimited() {
        let p = pool();
        p.query("t", "CREATE TABLE n(i INTEGER)").unwrap();
        p.query("t", "INSERT INTO n VALUES (1),(2),(3),(4),(5)")
            .unwrap();
        let capped = p
            .query_capped("t", "SELECT i FROM n ORDER BY i", Some(3))
            .unwrap();
        assert_eq!(capped.rows.len(), 3);
        assert_eq!(capped.rows[2][0].as_deref(), Some("3"));
        let all = p.query_capped("t", "SELECT i FROM n", None).unwrap();
        assert_eq!(all.rows.len(), 5);
    }

    #[test]
    fn query_error_surfaces_as_app_error() {
        let p = pool();
        let err = p.query("t", "SELECT * FROM no_such_table").unwrap_err();
        assert_eq!(err.kind, ErrorKind::Query);
    }

    #[test]
    fn test_connect_rejects_missing_and_empty_path() {
        assert_eq!(test_connect("").unwrap_err().kind, ErrorKind::Connection);
        assert_eq!(
            test_connect("/no/such/file.sqlite").unwrap_err().kind,
            ErrorKind::Connection
        );
    }

    #[test]
    fn txn_commits_edit() {
        let p = pool();
        p.query("t", "CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        p.query("t", "INSERT INTO users VALUES (1,'Alice')")
            .unwrap();
        let edits = vec![CellEdit {
            table: "users".into(),
            pk_col: "id".into(),
            pk_value: "1".into(),
            column: "name".into(),
            new_value: "Bobby".into(),
        }];
        assert_eq!(p.with_txn("t", &edits, &[]).unwrap(), 1);
        let r = p.query("t", "SELECT name FROM users WHERE id=1").unwrap();
        assert_eq!(r.rows[0][0].as_deref(), Some("Bobby"));
    }

    #[test]
    fn txn_rolls_back_on_error() {
        let p = pool();
        p.query(
            "t",
            "CREATE TABLE users(id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        )
        .unwrap();
        p.query("t", "INSERT INTO users VALUES (1,'Alice')")
            .unwrap();
        // 第二条引用不存在的列 → SQL 报错 → 整批回滚(第一条也不应生效)
        let bad = vec![
            CellEdit {
                table: "users".into(),
                pk_col: "id".into(),
                pk_value: "1".into(),
                column: "name".into(),
                new_value: "Changed".into(),
            },
            CellEdit {
                table: "users".into(),
                pk_col: "id".into(),
                pk_value: "1".into(),
                column: "no_such_col".into(),
                new_value: "x".into(),
            },
        ];
        assert!(p.with_txn("t", &bad, &[]).is_err());
        let r = p.query("t", "SELECT name FROM users WHERE id=1").unwrap();
        assert_eq!(r.rows[0][0].as_deref(), Some("Alice")); // 第一条也被回滚
    }
}
