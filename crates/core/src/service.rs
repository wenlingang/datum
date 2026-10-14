//! 按连接类型分发的统一入口:UI 只和这里打交道,不关心具体驱动。
//!
//! 所有异步函数都需要在 tokio 运行时里执行。

use crate::core::connection::DbKind;
use crate::error::{AppError, AppResult, ErrorKind};
use crate::pg::browse::TableDetail;
use crate::pg::client::QueryResult;
use crate::pg::edit::{CellEdit, RowDelete};
use crate::rds::client::{KeyDetail, KeyValue};
use crate::schema::{self, ColumnsByTable, ErdSchema};
use crate::sql::build::{build_insert_row, ident};
use crate::sql::Dialect;
use crate::{my, pg, rds, sqlite, AppState};
use rust_i18n::t;

fn unsupported() -> AppError {
    AppError::new(ErrorKind::Connection, t!("core.err.redis_unsupported"))
}

/// 建立连接(读取已保存配置与密码)。
pub async fn connect(state: &AppState, kind: &DbKind, id: &str) -> AppResult<()> {
    let id = id.to_string();
    match kind {
        DbKind::Pg => pg::commands::pg_connect(state, id).await,
        DbKind::Mysql => my::commands::my_connect(state, id).await,
        DbKind::Sqlite => sqlite::commands::sqlite_connect(state, id).await,
        DbKind::Redis => rds::commands::redis_connect(state, id).await,
    }
}

/// 断开并移除连接(服务器侧会话随之终止)。
pub async fn disconnect(state: &AppState, kind: &DbKind, id: &str) {
    let id = id.to_string();
    let _ = match kind {
        DbKind::Pg => pg::commands::pg_disconnect(state, id).await,
        DbKind::Mysql => my::commands::my_disconnect(state, id).await,
        DbKind::Sqlite => sqlite::commands::sqlite_disconnect(state, id).await,
        DbKind::Redis => Ok(()),
    };
}

/// 列出对象:关系库为表名,Redis 为全部 key。
pub async fn list_objects(state: &AppState, kind: &DbKind, id: &str) -> AppResult<Vec<String>> {
    let id = id.to_string();
    match kind {
        DbKind::Pg => pg::commands::pg_list_objects(state, id).await,
        DbKind::Mysql => my::commands::my_list_objects(state, id).await,
        DbKind::Sqlite => sqlite::commands::sqlite_list_objects(state, id).await,
        DbKind::Redis => rds::commands::redis_scan(state, id, "*".into()).await,
    }
}

/// 实例下的库列表(Redis 固定 0–15,SQLite 无库概念)。
pub async fn list_databases(state: &AppState, kind: &DbKind, id: &str) -> AppResult<Vec<String>> {
    let id = id.to_string();
    match kind {
        DbKind::Pg => pg::commands::pg_list_databases(state, id).await,
        DbKind::Mysql => my::commands::my_list_databases(state, id).await,
        DbKind::Redis => Ok((0..16).map(|i| i.to_string()).collect()),
        DbKind::Sqlite => Ok(vec![]),
    }
}

/// 执行任意 SQL;`max_rows` 为 None 时不限行数(导出等全量场景)。
pub async fn query(
    state: &AppState,
    kind: &DbKind,
    id: &str,
    sql: &str,
    max_rows: Option<u32>,
) -> AppResult<QueryResult> {
    let (id, sql) = (id.to_string(), sql.to_string());
    match kind {
        DbKind::Pg => pg::commands::pg_query(state, id, sql, max_rows).await,
        DbKind::Mysql => my::commands::my_query(state, id, sql, max_rows).await,
        DbKind::Sqlite => sqlite::commands::sqlite_query(state, id, sql, max_rows).await,
        DbKind::Redis => Err(unsupported()),
    }
}

/// 在只读模式下执行 SQL:免确认执行(AI 生成、静态检查判定为只读的语句)的最后一道保险。
/// 静态检查无法看穿视图里的写函数、重载运算符等语义副作用,这里让数据库强制拒绝写入:
/// 先把会话切到只读,执行后无论成败都恢复。用会话级设置而不是包一个事务,
/// 这样语句里即使出现 COMMIT,后续语句开的新事务仍然是只读的。
pub async fn query_read_only(
    state: &AppState,
    kind: &DbKind,
    id: &str,
    sql: &str,
    max_rows: Option<u32>,
) -> AppResult<QueryResult> {
    // 每个驱动都在持有同一连接的前提下完成「切只读 → 执行 → 恢复」,
    // 否则并发的另一次执行可能在中间把连接恢复成读写(PG/SQLite 锁住连接,MySQL 独占池中一条连接)
    match kind {
        DbKind::Pg => state.pg_pool.query_read_only(id, sql, max_rows).await,
        DbKind::Mysql => state.my_pool.query_read_only(id, sql, max_rows).await,
        DbKind::Sqlite => state.sqlite_pool.query_read_only(id, sql, max_rows),
        DbKind::Redis => Err(unsupported()),
    }
}

pub async fn table_detail(
    state: &AppState,
    kind: &DbKind,
    id: &str,
    table: &str,
) -> AppResult<TableDetail> {
    let (id, table) = (id.to_string(), table.to_string());
    match kind {
        DbKind::Pg => pg::commands::pg_table_detail(state, id, table).await,
        DbKind::Mysql => my::commands::my_table_detail(state, id, table).await,
        DbKind::Sqlite => sqlite::commands::sqlite_table_detail(state, id, table).await,
        DbKind::Redis => Err(unsupported()),
    }
}

/// 单事务提交暂存的编辑与删除,返回受影响行数。
pub async fn commit_edits(
    state: &AppState,
    kind: &DbKind,
    id: &str,
    edits: Vec<CellEdit>,
    deletes: Vec<RowDelete>,
) -> AppResult<u64> {
    let id = id.to_string();
    match kind {
        DbKind::Pg => pg::commands::pg_commit_edits(state, id, edits, deletes).await,
        DbKind::Mysql => my::commands::my_commit_edits(state, id, edits, deletes).await,
        DbKind::Sqlite => sqlite::commands::sqlite_commit_edits(state, id, edits, deletes).await,
        DbKind::Redis => Err(unsupported()),
    }
}

/// 单事务导入:全成功提交,任一失败回滚;返回执行的语句数。
pub async fn import_sql(
    state: &AppState,
    kind: &DbKind,
    id: &str,
    statements: Vec<String>,
) -> AppResult<usize> {
    let id = id.to_string();
    match kind {
        DbKind::Pg => pg::commands::pg_import_sql(state, id, statements).await,
        DbKind::Mysql => my::commands::my_import_sql(state, id, statements).await,
        DbKind::Sqlite => sqlite::commands::sqlite_import_sql(state, id, statements).await,
        DbKind::Redis => Err(unsupported()),
    }
}

/// 建表语句:MySQL 用 SHOW CREATE TABLE,其余由表详情合成。
pub async fn create_table_ddl(
    state: &AppState,
    kind: &DbKind,
    id: &str,
    table: &str,
) -> AppResult<String> {
    if *kind == DbKind::Mysql {
        let r = query(
            state,
            kind,
            id,
            &format!("SHOW CREATE TABLE {}", ident(table, Dialect::Mysql)),
            None,
        )
        .await?;
        return Ok(r
            .rows
            .first()
            .and_then(|row| row.get(1).cloned().flatten())
            .unwrap_or_default());
    }
    let detail = table_detail(state, kind, id, table).await?;
    Ok(crate::sql::build::build_create_table(table, &detail))
}

/// 整库列速览(补全与 AI 上下文用)。
pub async fn fetch_columns(state: &AppState, kind: &DbKind, id: &str) -> AppResult<ColumnsByTable> {
    let dialect = Dialect::from_kind(kind).ok_or_else(unsupported)?;
    let r = query(state, kind, id, schema::columns_sql(dialect), None).await?;
    Ok(schema::rows_to_column_map(&r.rows))
}

/// 整库外键 + 主键(ER 图用)。
pub async fn fetch_erd_schema(state: &AppState, kind: &DbKind, id: &str) -> AppResult<ErdSchema> {
    let dialect = Dialect::from_kind(kind).ok_or_else(unsupported)?;
    let fks = query(state, kind, id, schema::foreign_keys_sql(dialect), None).await?;
    let pks = query(state, kind, id, schema::primary_keys_sql(dialect), None).await?;
    Ok(ErdSchema {
        fks: schema::rows_to_fk_list(&fks.rows),
        pk_by_table: schema::rows_to_pk_map(&pks.rows),
    })
}

/// 导出整个数据库为 INSERT 语句:逐表查询全部行,表间用注释分隔;单表失败不影响其他表。
pub async fn export_database(
    state: &AppState,
    kind: &DbKind,
    id: &str,
    tables: &[String],
) -> String {
    let Some(dialect) = Dialect::from_kind(kind) else {
        return String::new();
    };
    let mut parts = Vec::new();
    for t in tables {
        match query(
            state,
            kind,
            id,
            &format!("SELECT * FROM {}", ident(t, dialect)),
            None,
        )
        .await
        {
            Ok(r) => {
                parts.push(
                    t!("core.export.table_rows", table = t, count = r.rows.len()).to_string(),
                );
                if !r.rows.is_empty() {
                    parts.push(
                        r.rows
                            .iter()
                            .map(|row| build_insert_row(t, &r.columns, row, dialect))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    );
                }
            }
            Err(e) => parts
                .push(t!("core.export.table_failed", table = t, error = e.display()).to_string()),
        }
    }
    parts.join("\n\n")
}

pub async fn redis_get_key(
    state: &AppState,
    id: &str,
    key: &str,
) -> AppResult<(KeyValue, KeyDetail)> {
    let value = rds::commands::redis_get_key(state, id.to_string(), key.to_string()).await?;
    let detail = rds::commands::redis_key_detail(state, id.to_string(), key.to_string()).await?;
    Ok((value, detail))
}

pub async fn redis_exec(state: &AppState, id: &str, command: &str) -> AppResult<String> {
    rds::commands::redis_exec(state, id.to_string(), command.to_string()).await
}

/// 测试连通性(用表单当前参数,不保存)。
pub async fn test_connection(
    conn: &crate::core::connection::Connection,
    password: &str,
) -> AppResult<()> {
    let kind = match conn.kind {
        DbKind::Pg => "pg",
        DbKind::Mysql => "mysql",
        DbKind::Redis => "redis",
        DbKind::Sqlite => "sqlite",
    };
    crate::commands::test_connection(
        kind.into(),
        conn.host.clone(),
        conn.port,
        conn.user.clone(),
        password.into(),
        conn.database.clone(),
        conn.file_path.clone(),
        conn.ssl_mode,
    )
    .await
}
