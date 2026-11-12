//! 按连接类型分发的数据库操作(都在 tokio 线程池上执行)。
//!
//! `kind` 是连接类型的 JSON 字符串形式:`"pg"`、`"mysql"`、`"sqlite"`、`"redis"`、`"mongo"`。

use crate::runtime::{err, from, json, spawn, state};
use datum_core::core::connection::DbKind;
use datum_core::pg::edit::{CellEdit, RowDelete};
use datum_core::service;
use nsh_core::JsonValue;
use nsh_core::prelude::*;

fn kind_of(kind: &str) -> Result<DbKind, String> {
    from(JsonValue::String(kind.to_string()), "kind")
}

/// 数据库操作:连接、取对象、执行 SQL、表结构、提交编辑、Redis。
#[module]
pub struct Db;

#[module]
impl Db {
    /// 建立连接(读取已保存配置与密码)。
    pub async fn connect(kind: String, id: String) -> Result<(), String> {
        let k = kind_of(&kind)?;
        spawn(async move { service::connect(&state(), &k, &id).await })
            .await?
            .map_err(err)
    }

    /// 断开连接。
    pub async fn disconnect(kind: String, id: String) -> Result<(), String> {
        let k = kind_of(&kind)?;
        spawn(async move { service::disconnect(&state(), &k, &id).await }).await
    }

    /// 对象列表:关系库为表名,Redis 为 key,Mongo 为集合。
    pub async fn list_objects(kind: String, id: String) -> Result<JsonValue, String> {
        let k = kind_of(&kind)?;
        spawn(async move { service::list_objects(&state(), &k, &id).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// 实例下的库列表。
    pub async fn list_databases(kind: String, id: String) -> Result<JsonValue, String> {
        let k = kind_of(&kind)?;
        spawn(async move { service::list_databases(&state(), &k, &id).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// 整库的列信息:`{ 表名: [{ name, dataType }] }`(补全与 AI 上下文)。
    pub async fn fetch_columns(kind: String, id: String) -> Result<JsonValue, String> {
        let k = kind_of(&kind)?;
        spawn(async move { service::fetch_columns(&state(), &k, &id).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// 执行 SQL:`{ columns, rows, affected }`。`max_rows` 为 null 时不限行数。
    pub async fn query(kind: String, id: String, sql: String, max_rows: Option<u32>) -> Result<JsonValue, String> {
        let k = kind_of(&kind)?;
        spawn(async move { service::query(&state(), &k, &id, &sql, max_rows).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// 在只读会话里执行 SQL(免确认执行的最后一道保险)。
    pub async fn query_read_only(
        kind: String,
        id: String,
        sql: String,
        max_rows: Option<u32>,
    ) -> Result<JsonValue, String> {
        let k = kind_of(&kind)?;
        spawn(async move { service::query_read_only(&state(), &k, &id, &sql, max_rows).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// 预估一条写语句的影响范围(只读执行改写后的 COUNT 与预览):
    /// `{ count, wholeTable, columns, rows }`;改写不了的语句返回 null。
    pub async fn write_impact(kind: String, id: String, sql: String) -> Result<JsonValue, String> {
        let k = kind_of(&kind)?;
        spawn(async move { service::write_impact(&state(), &k, &id, &sql).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// 表结构:列、主键、索引等。
    pub async fn table_detail(kind: String, id: String, table: String) -> Result<JsonValue, String> {
        let k = kind_of(&kind)?;
        spawn(async move { service::table_detail(&state(), &k, &id, &table).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// 单事务提交暂存的编辑与删除,返回受影响行数。
    pub async fn commit_edits(
        kind: String,
        id: String,
        edits: JsonValue,
        deletes: JsonValue,
    ) -> Result<f64, String> {
        let k = kind_of(&kind)?;
        let edits: Vec<CellEdit> = from(edits, "edits")?;
        let deletes: Vec<RowDelete> = from(deletes, "deletes")?;
        spawn(async move { service::commit_edits(&state(), &k, &id, edits, deletes).await })
            .await?
            .map(|n| n as f64)
            .map_err(err)
    }

    /// 逐条执行导入的语句,返回成功条数。
    pub async fn import_sql(kind: String, id: String, statements: Vec<String>) -> Result<f64, String> {
        let k = kind_of(&kind)?;
        spawn(async move { service::import_sql(&state(), &k, &id, statements).await })
            .await?
            .map(|n| n as f64)
            .map_err(err)
    }

    /// 建表语句(DDL)。
    pub async fn create_table_ddl(kind: String, id: String, table: String) -> Result<String, String> {
        let k = kind_of(&kind)?;
        spawn(async move { service::create_table_ddl(&state(), &k, &id, &table).await })
            .await?
            .map_err(err)
    }

    /// 导出整库(结构 + 数据)为 SQL 文本。
    pub async fn export_database(kind: String, id: String, tables: Vec<String>) -> Result<String, String> {
        let k = kind_of(&kind)?;
        spawn(async move { service::export_database(&state(), &k, &id, &tables).await }).await
    }

    /// Redis:读取一个 key 的值与详情 `[value, detail]`。
    pub async fn redis_get_key(id: String, key: String) -> Result<JsonValue, String> {
        spawn(async move { service::redis_get_key(&state(), &id, &key).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// Redis:执行一条命令,返回文本结果。
    pub async fn redis_exec(id: String, command: String) -> Result<String, String> {
        spawn(async move { service::redis_exec(&state(), &id, &command).await })
            .await?
            .map_err(err)
    }

    /// Redis 分析:概览(INFO 里的内存、命中率、key 数等)。
    pub async fn redis_overview(id: String) -> Result<JsonValue, String> {
        spawn(async move { service::redis_overview(&state(), &id).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// Redis 分析:最近 50 条慢日志。
    pub async fn redis_slowlog(id: String) -> Result<JsonValue, String> {
        spawn(async move { service::redis_slowlog(&state(), &id).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// Redis 分析:开始整库扫描(后台进行,用 `redis_analysis_report` 轮询)。
    pub async fn redis_analysis_start(id: String) -> Result<(), String> {
        spawn(async move { service::redis_analysis_start(&state(), &id).await })
            .await?
            .map_err(err)
    }

    /// Redis 分析:当前汇总;没开始过为 null。
    pub async fn redis_analysis_report(id: String) -> Result<JsonValue, String> {
        spawn(async move { service::redis_analysis_report(&state(), &id).await })
            .await
            .and_then(json)
    }

    /// Redis 分析:取消进行中的扫描。
    pub async fn redis_analysis_cancel(id: String) -> Result<(), String> {
        spawn(async move { service::redis_analysis_cancel(&state(), &id).await }).await
    }
}
