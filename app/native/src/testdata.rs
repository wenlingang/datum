//! 测试数据生成:分析表结构给出自动方案、改选生成器、预览、INSERT 文本、后台插入与进度、AI 建议。

use crate::runtime::{err, from, json, spawn, state};
use datum_core::core::connection::DbKind;
use datum_core::testdata::{api, infer, ColumnMeta, Locale, Plan};
use nsh_core::JsonValue;
use nsh_core::prelude::*;

fn kind_of(kind: &str) -> Result<DbKind, String> {
    from(JsonValue::String(kind.to_string()), "kind")
}

/// 造测试数据。
#[module]
pub struct TestData;

#[module]
impl TestData {
    /// 分析一张表:`{ meta, plan, parentEmpty, kinds }`(plan 为自动方案,行数 100、随机种子)。
    /// `locale`:"en" | "zh"。
    pub async fn analyze(kind: String, id: String, table: String, locale: String) -> Result<JsonValue, String> {
        let k = kind_of(&kind)?;
        let locale: Locale = from(JsonValue::String(locale), "locale")?;
        spawn(async move { api::analyze(&state(), &k, &id, &table, locale).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// 某列改选生成器类型时的默认参数(按列的类型与 CHECK 收口);类型名不认识返回 null。
    pub fn generator_for(column: JsonValue, gen_kind: String) -> Result<JsonValue, String> {
        let col: ColumnMeta = from(column, "column")?;
        json(infer::generator_for_kind(&gen_kind, &col, api::today()))
    }

    /// 预览实际插入时的前 10 行:`{ columns, rows }`。
    pub async fn preview(kind: String, id: String, plan: JsonValue) -> Result<JsonValue, String> {
        let k = kind_of(&kind)?;
        let plan: Plan = from(plan, "plan")?;
        spawn(async move { api::preview(&state(), &k, &id, &plan).await })
            .await?
            .map_err(err)
            .and_then(json)
    }

    /// 整份 INSERT 文本(最多 1000 行)。
    pub async fn insert_sql(kind: String, id: String, plan: JsonValue) -> Result<String, String> {
        let k = kind_of(&kind)?;
        let plan: Plan = from(plan, "plan")?;
        spawn(async move { api::insert_script(&state(), &k, &id, &plan).await })
            .await?
            .map_err(err)
    }

    /// 后台插入;用 `progress` 轮询。
    pub async fn start(kind: String, id: String, plan: JsonValue) -> Result<(), String> {
        let k = kind_of(&kind)?;
        let plan: Plan = from(plan, "plan")?;
        spawn(async move { api::start(&state(), &k, &id, &plan).await })
            .await?
            .map_err(err)
    }

    /// 进度 `{ rows, committed, total, batched, done, cancelled, error }`;没开始过为 null。
    pub fn progress(id: String) -> Result<JsonValue, String> {
        json(api::progress(&state(), &id))
    }

    /// 取消进行中的插入(回滚当前事务)。
    pub fn cancel(id: String) -> Result<(), String> {
        api::cancel(&state(), &id);
        Ok(())
    }

    /// 让 AI 按建表语句建议生成器(只发结构,不发数据):`{ columns, dropped, note }`。
    pub async fn ai_suggest(kind: String, id: String, table: String) -> Result<JsonValue, String> {
        let k = kind_of(&kind)?;
        spawn(async move { api::ai_suggest(&state(), &k, &id, &table).await })
            .await?
            .and_then(json)
    }

    /// 新的随机种子。
    pub fn random_seed() -> f64 {
        datum_core::testdata::rng::random_seed() as f64
    }
}
