//! 整库结构的批量查询:列速览(SQL 补全与 AI 上下文用)、外键与主键(ER 图用)。
//! 每方言一条 SQL,一次拉取整个连接,避免逐表 N+1。

use crate::sql::Dialect;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnBrief {
    pub name: String,
    pub data_type: String,
}

/// 表名 → 列简要信息(保持列序)。
pub type ColumnsByTable = BTreeMap<String, Vec<ColumnBrief>>;

pub fn columns_sql(d: Dialect) -> &'static str {
    match d {
        // pg 限定 public schema,与 list_tables 的范围一致
        Dialect::Pg => {
            "SELECT table_name, column_name, data_type FROM information_schema.columns \
             WHERE table_schema = 'public' ORDER BY table_name, ordinal_position"
        }
        Dialect::Mysql => {
            "SELECT TABLE_NAME, COLUMN_NAME, DATA_TYPE FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = DATABASE() ORDER BY TABLE_NAME, ORDINAL_POSITION"
        }
        Dialect::Sqlite => {
            "SELECT m.name, p.name, p.type FROM sqlite_master m \
             JOIN pragma_table_info(m.name) p WHERE m.type = 'table' ORDER BY m.name, p.cid"
        }
    }
}

/// 查询行(表名, 列名, 类型)→ 表→列映射;容忍 NULL 单元格。
pub fn rows_to_column_map(rows: &[Vec<Option<String>>]) -> ColumnsByTable {
    let mut map = ColumnsByTable::new();
    for row in rows {
        let (Some(Some(table)), Some(Some(column))) = (row.first(), row.get(1)) else {
            continue;
        };
        map.entry(table.clone()).or_default().push(ColumnBrief {
            name: column.clone(),
            data_type: row.get(2).cloned().flatten().unwrap_or_default(),
        });
    }
    map
}

/// 一条外键列配对(复合外键按约束展开为多行,保序)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKey {
    /// 约束名;sqlite 无名,合成 `fk_表名_序号`
    pub constraint: String,
    /// 引用方(外键所在表)
    pub table: String,
    pub column: String,
    /// 被引用表
    pub ref_table: String,
    /// sqlite 引用隐式主键时为空串
    pub ref_column: String,
    /// 按命名约定(xxx_id → xxx/xxxs.id)推断的逻辑外键,库里没有物理约束
    pub inferred: bool,
}

/// 表名 → 主键列(有序,支持复合主键)。
pub type PkByTable = BTreeMap<String, Vec<String>>;

/// ER 图所需的整库关系数据。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErdSchema {
    pub fks: Vec<ForeignKey>,
    pub pk_by_table: PkByTable,
}

pub fn foreign_keys_sql(d: Dialect) -> &'static str {
    match d {
        // 走 pg_constraint:information_schema 在复合外键上拿不到正确的列配对
        Dialect::Pg => {
            "SELECT con.conname, rel.relname, att.attname, frel.relname, fatt.attname \
             FROM pg_constraint con \
             JOIN pg_class rel  ON rel.oid  = con.conrelid \
             JOIN pg_class frel ON frel.oid = con.confrelid \
             JOIN pg_namespace ns ON ns.oid = rel.relnamespace \
             CROSS JOIN LATERAL unnest(con.conkey, con.confkey) WITH ORDINALITY AS k(attnum, fattnum, ord) \
             JOIN pg_attribute att  ON att.attrelid  = con.conrelid  AND att.attnum  = k.attnum \
             JOIN pg_attribute fatt ON fatt.attrelid = con.confrelid AND fatt.attnum = k.fattnum \
             WHERE con.contype = 'f' AND ns.nspname = 'public' \
             ORDER BY rel.relname, con.conname, k.ord"
        }
        Dialect::Mysql => {
            "SELECT CONSTRAINT_NAME, TABLE_NAME, COLUMN_NAME, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME \
             FROM information_schema.KEY_COLUMN_USAGE \
             WHERE TABLE_SCHEMA = DATABASE() AND REFERENCED_TABLE_NAME IS NOT NULL \
             ORDER BY TABLE_NAME, CONSTRAINT_NAME, ORDINAL_POSITION"
        }
        Dialect::Sqlite => {
            "SELECT 'fk_' || m.name || '_' || f.id, m.name, f.\"from\", f.\"table\", f.\"to\" \
             FROM sqlite_master m JOIN pragma_foreign_key_list(m.name) f \
             WHERE m.type = 'table' ORDER BY m.name, f.id, f.seq"
        }
    }
}

pub fn primary_keys_sql(d: Dialect) -> &'static str {
    match d {
        Dialect::Pg => {
            "SELECT tc.table_name, kcu.column_name \
             FROM information_schema.table_constraints tc \
             JOIN information_schema.key_column_usage kcu USING (constraint_name, table_schema) \
             WHERE tc.constraint_type = 'PRIMARY KEY' AND tc.table_schema = 'public' \
             ORDER BY tc.table_name, kcu.ordinal_position"
        }
        Dialect::Mysql => {
            "SELECT TABLE_NAME, COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE \
             WHERE TABLE_SCHEMA = DATABASE() AND CONSTRAINT_NAME = 'PRIMARY' \
             ORDER BY TABLE_NAME, ORDINAL_POSITION"
        }
        Dialect::Sqlite => {
            "SELECT m.name, p.name FROM sqlite_master m \
             JOIN pragma_table_info(m.name) p \
             WHERE m.type = 'table' AND p.pk > 0 ORDER BY m.name, p.pk"
        }
    }
}

/// 查询行(约束, 表, 列, 被引表, 被引列)→ 外键列表;关键字段为 NULL 的行跳过。
pub fn rows_to_fk_list(rows: &[Vec<Option<String>>]) -> Vec<ForeignKey> {
    rows.iter()
        .filter_map(|row| {
            let get = |i: usize| row.get(i).cloned().flatten();
            Some(ForeignKey {
                constraint: get(0)?,
                table: get(1)?,
                column: get(2)?,
                ref_table: get(3)?,
                ref_column: get(4).unwrap_or_default(),
                inferred: false,
            })
        })
        .collect()
}

/// 查询行(表, 主键列)→ 表→主键列映射。
pub fn rows_to_pk_map(rows: &[Vec<Option<String>>]) -> PkByTable {
    let mut map = PkByTable::new();
    for row in rows {
        if let (Some(Some(table)), Some(Some(column))) = (row.first(), row.get(1)) {
            map.entry(table.clone()).or_default().push(column.clone());
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(cells: &[Option<&str>]) -> Vec<Option<String>> {
        cells.iter().map(|c| c.map(String::from)).collect()
    }

    #[test]
    fn column_map_groups_and_tolerates_nulls() {
        let map = rows_to_column_map(&[
            r(&[Some("users"), Some("id"), Some("integer")]),
            r(&[None, Some("x"), Some("int")]),
            r(&[Some("users"), Some("name"), None]),
        ]);
        assert_eq!(
            map["users"]
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["id", "name"]
        );
        assert_eq!(map["users"][1].data_type, "");
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn fk_rows_skip_incomplete_and_default_ref_column() {
        let fks = rows_to_fk_list(&[
            r(&[
                Some("fk_a"),
                Some("orders"),
                Some("user_id"),
                Some("users"),
                None,
            ]),
            r(&[
                None,
                Some("orders"),
                Some("user_id"),
                Some("users"),
                Some("id"),
            ]),
        ]);
        assert_eq!(fks.len(), 1);
        assert_eq!(fks[0].ref_column, "");
    }

    #[test]
    fn pk_map_keeps_order() {
        let map = rows_to_pk_map(&[r(&[Some("t"), Some("a")]), r(&[Some("t"), Some("b")])]);
        assert_eq!(map["t"], vec!["a", "b"]);
    }
}
