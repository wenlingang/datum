//! 敏感数据遮罩与演示模式:识别 / 遮罩都在 datum-core 的 `mask` 模块,这里只做 JSON 边界与设置读写。

use crate::runtime::{from, json, state};
use datum_core::mask::{self, MaskKind, MaskRepo, Override};
use datum_core::schema::ColumnsByTable;
use nsh_core::JsonValue;
use nsh_core::prelude::*;

type Rows = Vec<Vec<Option<String>>>;

fn kind_of(s: &str) -> Result<MaskKind, String> {
    from(JsonValue::String(s.to_string()), "kind")
}

/// 改一个连接的设置并落盘(配置写入串行化)。
fn update(conn_id: &str, f: impl FnOnce(&mut mask::ConnMask)) -> Result<(), String> {
    let st = state();
    let _g = st.lock.lock().unwrap();
    let repo = MaskRepo::new(&st.config_dir);
    let mut s = repo.load();
    f(s.connections.entry(conn_id.to_string()).or_default());
    repo.save(&s)
}

/// 敏感数据遮罩与演示模式。
#[module]
pub struct Mask;

#[module]
impl Mask {
    /// 一份结果每列的遮罩方式(与 `columns` 对应):`"secret" | "email" | "phone" | "idNumber" | "card" |
    /// "address" | "name"` 或 null(不遮罩)。`table` 为来源表(手动覆盖按 `表.列` 匹配),查询结果给 null
    /// 或单表查询猜出的表名;`rows` 用来按取值抽样识别。遮罩关着时全为 null。
    pub fn plan(conn_id: String, table: Option<String>, columns: Vec<String>, rows: JsonValue) -> Result<JsonValue, String> {
        let rows: Rows = from(rows, "rows")?;
        json(mask::policy(&state(), &conn_id).plan(table.as_deref(), &columns, &rows))
    }

    /// 遮罩一个值(NULL 仍是 NULL)。
    pub fn apply(kind: String, value: Option<String>) -> Result<Option<String>, String> {
        let k = kind_of(&kind)?;
        Ok(value.map(|v| mask::mask_value(k, &v)))
    }

    /// 按 `plan`(同 [`Self::plan`] 的返回)遮罩整份行(复制、导出用)。
    pub fn mask_rows(plan: JsonValue, rows: JsonValue) -> Result<JsonValue, String> {
        let plan: Vec<Option<MaskKind>> = from(plan, "plan")?;
        let mut rows: Rows = from(rows, "rows")?;
        mask::apply_plan(&plan, &mut rows);
        json(rows)
    }

    /// 来源表:单表查询的表名,带 JOIN 的为 null。
    pub fn source_table(sql: String) -> Option<String> {
        mask::source_table(&sql)
    }

    /// 连接的遮罩设置:`{ enabled, explicit, autoDetect, overrides: { "表.列": "mask" | "plain" }, demo }`。
    /// `enabled` 为当前生效的开关(含环境默认与演示模式),`explicit` 为用户设的值(null = 跟随环境)。
    pub fn settings(conn_id: String) -> Result<JsonValue, String> {
        let st = state();
        let s = MaskRepo::new(&st.config_dir).load();
        let c = s.conn(&conn_id);
        let p = mask::policy(&st, &conn_id);
        Ok(serde_json::json!({
            "enabled": p.enabled,
            "explicit": c.enabled,
            "autoDetect": c.auto_detect,
            "overrides": c.overrides,
            "demo": s.demo_mode,
        }))
    }

    /// 打开 / 关闭连接的遮罩;null 恢复按环境默认。
    pub fn set_enabled(conn_id: String, enabled: Option<bool>) -> Result<(), String> {
        update(&conn_id, |c| c.enabled = enabled)
    }

    /// 打开 / 关闭自动识别(关掉后只遮罩手动标记的列)。
    pub fn set_auto_detect(conn_id: String, on: bool) -> Result<(), String> {
        update(&conn_id, |c| c.auto_detect = on)
    }

    /// 某列(`表.列`)的手动覆盖:`"mask"` 强制遮罩、`"plain"` 永不遮罩、null 取消。
    pub fn set_override(conn_id: String, key: String, mode: Option<String>) -> Result<(), String> {
        let mode: Option<Override> = match mode {
            Some(m) => Some(from(JsonValue::String(m), "mode")?),
            None => None,
        };
        update(&conn_id, |c| match mode {
            Some(m) => {
                c.overrides.insert(key, m);
            }
            None => {
                c.overrides.remove(&key);
            }
        })
    }

    /// 演示模式是否打开。
    pub fn demo_mode() -> bool {
        MaskRepo::new(&state().config_dir).load().demo_mode
    }

    /// 打开 / 关闭演示模式(全局)。
    pub fn set_demo_mode(on: bool) -> Result<(), String> {
        let st = state();
        let _g = st.lock.lock().unwrap();
        let repo = MaskRepo::new(&st.config_dir);
        let mut s = repo.load();
        s.demo_mode = on;
        repo.save(&s)
    }

    /// 「数据与隐私」面板:已加载的列里按列名认出的敏感列与手动覆盖过的列,
    /// `[{ table, column, kind, override }]`(kind 为 null 表示自动识别没认出,只有覆盖)。
    /// 只看列名(不查数据),与遮罩开关无关。
    pub fn detect_columns(conn_id: String, columns: JsonValue) -> Result<JsonValue, String> {
        let cols: ColumnsByTable = if columns.is_null() { Default::default() } else { from(columns, "columns")? };
        let p = mask::policy(&state(), &conn_id);
        let mut out = vec![];
        for (table, list) in &cols {
            for c in list {
                let key = format!("{table}.{}", c.name);
                let ov = p.overrides.get(&key).copied();
                let kind = mask::detect_name(&c.name);
                if kind.is_some() || ov.is_some() {
                    out.push(serde_json::json!({ "table": table, "column": c.name, "kind": kind, "override": ov }));
                }
            }
        }
        // 覆盖了但当前没加载到的列(换库、表被删)也列出来,方便取消
        for (key, ov) in &p.overrides {
            let listed = out.iter().any(|x| format!("{}.{}", x["table"].as_str().unwrap_or(""), x["column"].as_str().unwrap_or("")) == *key);
            if !listed {
                let (t, c) = key.rsplit_once('.').unwrap_or(("", key.as_str()));
                out.push(serde_json::json!({ "table": t, "column": c, "kind": mask::detect_name(c), "override": ov }));
            }
        }
        Ok(JsonValue::Array(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_and_mask_rows() {
        assert_eq!(Mask::apply("email".into(), Some("bob@x.io".into())).unwrap().as_deref(), Some("b***@x.io"));
        assert_eq!(Mask::apply("phone".into(), None).unwrap(), None);
        assert!(Mask::apply("nope".into(), Some("x".into())).is_err());
        let rows = Mask::mask_rows(serde_json::json!([null, "secret"]), serde_json::json!([["1", "pw"], ["2", null]])).unwrap();
        assert_eq!(rows, serde_json::json!([["1", "••••••"], ["2", null]]));
    }
}
