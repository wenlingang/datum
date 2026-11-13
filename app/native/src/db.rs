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

    /// MySQL / MariaDB 改表结构语句的风险评估(只读版本与表信息,不执行 ALTER):
    /// `{ ops, algorithm, rebuild, blocksWrites, risk, rows, bytes, duration, guarded, … }`;
    /// 不是 MySQL 或不是改表结构的语句返回 null。
    pub async fn alter_assess(kind: String, id: String, sql: String) -> Result<JsonValue, String> {
        let k = kind_of(&kind)?;
        spawn(async move { service::alter_assess(&state(), &k, &id, &sql).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// 执行写语句前的撤销准备:抓取会被改动的原始行,生成反向语句
    /// `{ statements, rows, reason }`(不能撤销时 statements 为空、reason 说明原因)。
    pub async fn undo_prepare(kind: String, id: String, sql: String) -> Result<JsonValue, String> {
        let k = kind_of(&kind)?;
        spawn(async move { service::undo_prepare(&state(), &k, &id, &sql).await })
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

    /// SQLite 导入 CSV / JSON:预览文件开头 `{ format, delimiter, jsonLines, columns, rows, tableName, badEncoding, … }`。
    pub async fn sqlite_import_preview(source: JsonValue) -> Result<JsonValue, String> {
        let src = from(source, "source")?;
        spawn(async move { service::sqlite_import_preview(&src) })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// SQLite 导入 CSV / JSON:后台开始(建表或追加,单事务),用 `sqlite_import_progress` 轮询。
    pub async fn sqlite_import_start(id: String, plan: JsonValue) -> Result<(), String> {
        let plan = from(plan, "plan")?;
        spawn(async move { service::sqlite_import_start(&state(), &id, plan) })
            .await?
            .map_err(err)
    }

    /// SQLite 导入的进度 `{ rows, bytes, totalBytes, done, cancelled, error, skipped }`;没开始过为 null。
    pub fn sqlite_import_progress(id: String) -> Result<JsonValue, String> {
        json(service::sqlite_import_progress(&state(), &id))
    }

    /// 取消进行中的 SQLite 导入(整体回滚)。
    pub fn sqlite_import_cancel(id: String) -> Result<(), String> {
        service::sqlite_import_cancel(&state(), &id);
        Ok(())
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

    /// Redis 分析排行榜里的 key:查看值(集群模式下 `node` 是它所在的主节点;集合类最多取 1000 个元素)。
    pub async fn redis_key_on(id: String, node: Option<String>, key: String) -> Result<JsonValue, String> {
        spawn(async move { service::redis_key_on(&state(), &id, node.as_deref(), &key).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// Redis 分析排行榜里的 key:删除(UNLINK,老版本 DEL)。返回删掉的个数。
    pub async fn redis_delete_key(id: String, node: Option<String>, key: String) -> Result<i64, String> {
        spawn(async move { service::redis_delete_key(&state(), &id, node.as_deref(), &key).await })
            .await?
            .map_err(err)
    }

    /// Redis:key 详情 `{ key, keyType, ttl, size }`(集群模式下在 `node` 上读)。
    pub async fn redis_key_detail_on(id: String, node: Option<String>, key: String) -> Result<JsonValue, String> {
        spawn(async move { service::redis_key_detail_on(&state(), &id, node.as_deref(), &key).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// Redis:元素级编辑,`edit` 形如 `{ op: "hashSet", field, value, oldField }`(见 core 的 `KeyEdit`)。
    /// 返回 `{ changed, id }`。集群模式下在 key 所在的 `node` 上执行。
    pub async fn redis_edit(id: String, node: Option<String>, key: String, edit: JsonValue) -> Result<JsonValue, String> {
        let e: datum_core::rds::edit::KeyEdit = from(edit, "edit")?;
        spawn(async move { service::redis_edit(&state(), &id, node.as_deref(), &key, &e).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// Redis:设置 TTL(秒,EXPIRE);`seconds` 为 null 时移除(PERSIST)。
    pub async fn redis_set_ttl(id: String, node: Option<String>, key: String, seconds: Option<f64>) -> Result<(), String> {
        let secs = seconds.map(|s| s as i64);
        spawn(async move { service::redis_set_ttl(&state(), &id, node.as_deref(), &key, secs).await })
            .await?
            .map_err(err)
    }

    /// Redis:重命名 key(RENAMENX,目标已存在时报错)。
    pub async fn redis_rename(id: String, node: Option<String>, key: String, new_key: String) -> Result<(), String> {
        spawn(async move { service::redis_rename(&state(), &id, node.as_deref(), &key, &new_key).await })
            .await?
            .map_err(err)
    }

    /// Redis:新建 key,`spec` 为 `{ key, keyType, items, ttl }`(见 core 的 `NewKey`)。
    pub async fn redis_create_key(id: String, node: Option<String>, spec: JsonValue) -> Result<(), String> {
        let s: datum_core::rds::edit::NewKey = from(spec, "spec")?;
        spawn(async move { service::redis_create_key(&state(), &id, node.as_deref(), &s).await })
            .await?
            .map_err(err)
    }

    /// Redis Stream:按 ID 范围取一页 `{ entries: [{ id, fields }], next, length }`;`rev` 为倒序(XREVRANGE)。
    pub async fn redis_stream_range(
        id: String,
        node: Option<String>,
        key: String,
        start: String,
        end: String,
        count: u32,
        rev: bool,
    ) -> Result<JsonValue, String> {
        let q = datum_core::rds::edit::StreamRange { start, end, count: count as usize, rev };
        spawn(async move { service::redis_stream_range(&state(), &id, node.as_deref(), &key, &q).await })
        .await?
        .map_err(err)
        .and_then(json)
    }

    /// Redis Stream:消费组(XINFO GROUPS)、各组消费者(XINFO CONSUMERS)与待确认摘要(XPENDING)。
    pub async fn redis_stream_groups(id: String, node: Option<String>, key: String) -> Result<JsonValue, String> {
        spawn(async move { service::redis_stream_groups(&state(), &id, node.as_deref(), &key).await })
            .await?
            .map_err(err)
            .and_then(json)
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
    /// 诊断:某个库支持的分区(PG / MySQL / SQLite;其他类型为空数组)。
    pub fn diag_sections(kind: String) -> Result<JsonValue, String> {
        json(service::diag_sections(&kind_of(&kind)?))
    }

    /// 诊断:查询一个分区 `{ columns, rows, unavailable }`。
    pub async fn diag_run(kind: String, id: String, section: String) -> Result<JsonValue, String> {
        let k = kind_of(&kind)?;
        spawn(async move { service::diag_run(&state(), &k, &id, &section).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// 诊断:取消 / 终止会话、VACUUM 等;`args` 取自分区声明的目标列。
    pub async fn diag_act(kind: String, id: String, action: String, args: Vec<String>) -> Result<(), String> {
        let k = kind_of(&kind)?;
        let a: datum_core::diag::Action = from(JsonValue::String(action), "action")?;
        spawn(async move { service::diag_act(&state(), &k, &id, a, &args).await })
            .await?
            .map_err(err)
    }

    /// 补全用的外键:物理外键 + 按命名推断的(`[{ constraint, table, column, refTable, refColumn, inferred }]`)。
    pub async fn foreign_keys(kind: String, id: String) -> Result<JsonValue, String> {
        let k = kind_of(&kind)?;
        spawn(async move { service::foreign_keys(&state(), &k, &id).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// 补全用:低基数列的实际取值(最多 30 个);不是枚举类的列为 null。
    pub async fn distinct_values(
        kind: String,
        id: String,
        table: String,
        column: String,
        data_type: String,
    ) -> Result<JsonValue, String> {
        let k = kind_of(&kind)?;
        spawn(async move { service::distinct_values(&state(), &k, &id, &table, &column, &data_type).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// 补全 JSON 键名用的键树(取前若干行推断,只读 + 限时):`{ keys: [{ name, count, tree }], items? }`;
    /// 只做 PG,其他库为 null。
    pub async fn json_key_tree(kind: String, id: String, table: String, column: String) -> Result<JsonValue, String> {
        let k = kind_of(&kind)?;
        spawn(async move { service::json_key_tree(&state(), &k, &id, &table, &column).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// 执行计划:`{ tree, raw, analyzed, totalMs }`;`analyze` 会真正执行(只接受只读语句,只读会话里限时)。
    pub async fn explain_plan(kind: String, id: String, sql: String, analyze: bool) -> Result<JsonValue, String> {
        let k = kind_of(&kind)?;
        spawn(async move { datum_core::explain::explain(&state(), &k, &id, &sql, analyze).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// 表结构对比:`{ tables: [{ name, status, changes }], same, sql }`;脚本让目标库与源库一致。
    pub async fn schema_diff(kind: String, source_id: String, target_id: String) -> Result<JsonValue, String> {
        let k = kind_of(&kind)?;
        spawn(async move { datum_core::schema_diff::compare(&state(), &k, &source_id, &target_id).await })
            .await?
            .map_err(err)
            .and_then(json)
    }
}
