//! 表结构里与造数据有关的部分:列的基础类型与取值范围、自增 / 标识 / 生成列、
//! 唯一约束、外键、枚举值与 CHECK 约束里能读懂的取值清单 / 范围。
//!
//! 每方言一组只读查询([`columns_sql`]、[`checks_sql`]),结果行交给纯函数解析,便于单测。

use crate::pg::browse::IndexInfo;
use crate::schema::ForeignKey;
use crate::sql::Dialect;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;

/// 列的基础类型(决定能生成什么值、值的上下限)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum BaseType {
    /// 整数,带类型本身的取值范围(无符号 BIGINT 截到 i64 上限)
    Int {
        #[serde(deserialize_with = "lenient_i64")]
        min: i64,
        #[serde(deserialize_with = "lenient_i64")]
        max: i64,
    },
    /// 定点小数;precision 为 None 表示不限(PG 的裸 numeric、SQLite)
    Decimal { precision: Option<u32>, scale: u32 },
    Float,
    Bool,
    /// 字符串;max_len 按字符数
    Text { max_len: Option<u32> },
    Date,
    Time,
    Timestamp,
    Json,
    Bytes { max_len: Option<u32> },
    Uuid,
    /// IP 地址(PG inet / cidr)
    Inet,
    /// 枚举(MySQL ENUM / SET、PG 枚举类型);取值在 [`ColumnMeta::values`]
    Enum,
    /// 不认识的类型(数组、几何、区间……):只能固定值或留给默认值
    Other,
}

/// 整数宽松反序列化:界面(JS)传回的大整数可能变成浮点数(i64 上限在 f64 里会进位溢出),截到 i64 范围。
pub fn lenient_i64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Num {
        I(i64),
        U(u64),
        F(f64),
    }
    Ok(match Num::deserialize(d)? {
        Num::I(i) => i,
        Num::U(u) => i64::try_from(u).unwrap_or(i64::MAX),
        Num::F(f) if f.is_nan() => 0,
        Num::F(f) => f.round().clamp(i64::MIN as f64, i64::MAX as f64) as i64,
    })
}

impl BaseType {
    pub fn is_textual(&self) -> bool {
        matches!(self, BaseType::Text { .. } | BaseType::Other)
    }
}

/// 不写入的列及原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AutoKind {
    /// PG 标识列 / serial(nextval 默认值)
    Identity,
    /// MySQL AUTO_INCREMENT、SQLite AUTOINCREMENT
    AutoIncrement,
    /// SQLite 的 INTEGER PRIMARY KEY(rowid 别名)
    RowId,
    /// 生成列(由表达式计算,不能写入)
    Generated,
}

/// 外键引用:一个约束可能有多列(复合外键)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FkGroup {
    pub constraint: String,
    pub columns: Vec<String>,
    pub ref_table: String,
    /// 与 columns 一一对应;SQLite 引用隐式主键时为空串(取父表主键补齐)
    pub ref_columns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnMeta {
    pub name: String,
    /// 原始类型文本(展示用)
    pub data_type: String,
    pub base: BaseType,
    pub nullable: bool,
    pub has_default: bool,
    pub auto: Option<AutoKind>,
    pub primary: bool,
    /// 单列唯一(主键或唯一索引)
    pub unique: bool,
    /// 取值清单:枚举标签,或 CHECK (col IN (...)) 里的值
    pub values: Vec<String>,
    /// CHECK 里读出的数值范围(闭区间,已处理严格不等号)
    pub check_min: Option<f64>,
    pub check_max: Option<f64>,
    /// 所在外键约束名(复合外键的各列都指向同一个)
    pub fk: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TableMeta {
    pub table: String,
    pub columns: Vec<ColumnMeta>,
    /// 多列唯一约束(单列的记在列上)
    pub uniques: Vec<Vec<String>>,
    pub fks: Vec<FkGroup>,
}

impl TableMeta {
    pub fn column(&self, name: &str) -> Option<&ColumnMeta> {
        self.columns.iter().find(|c| c.name == name)
    }
    pub fn fk(&self, constraint: &str) -> Option<&FkGroup> {
        self.fks.iter().find(|f| f.constraint == constraint)
    }
}

// ---------------------------------------------------------------- 类型

/// 括号里的数字参数:`varchar(40)` → [40],`numeric(10, 2)` → [10, 2]。
fn paren_nums(t: &str) -> Vec<u32> {
    let Some(start) = t.find('(') else { return vec![] };
    let Some(end) = t[start..].find(')') else { return vec![] };
    t[start + 1..start + end].split(',').filter_map(|s| s.trim().parse().ok()).collect()
}

/// 引号里的字符串清单:`enum('a','it''s')` → ["a", "it's"]。
pub fn quoted_list(s: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"'((?:[^']|'')*)'").unwrap());
    re.captures_iter(s).map(|c| c[1].replace("''", "'")).collect()
}

/// 原始类型文本 → 基础类型。
pub fn classify(data_type: &str, dialect: Dialect) -> BaseType {
    let t = data_type.trim().to_ascii_lowercase();
    let unsigned = t.contains("unsigned");
    let nums = paren_nums(&t);
    let int = |bits: u32| {
        if unsigned {
            let max = if bits >= 64 { i64::MAX } else { ((1u64 << bits) - 1) as i64 };
            BaseType::Int { min: 0, max }
        } else if bits >= 64 {
            BaseType::Int { min: i64::MIN, max: i64::MAX }
        } else {
            BaseType::Int { min: -(1i64 << (bits - 1)), max: (1i64 << (bits - 1)) - 1 }
        }
    };
    let head = t.split(['(', ' ']).next().unwrap_or("");
    if t.ends_with("[]") {
        return BaseType::Other;
    }
    match dialect {
        Dialect::Mysql => {
            if head == "enum" || head == "set" {
                return BaseType::Enum;
            }
            if head == "tinyint" && nums.first() == Some(&1) || head == "bool" || head == "boolean" || t == "bit(1)" {
                return BaseType::Bool;
            }
            match head {
                "tinyint" => int(8),
                "smallint" => int(16),
                "mediumint" => int(24),
                "int" | "integer" => int(32),
                "bigint" => int(64),
                "year" => BaseType::Int { min: 1901, max: 2155 },
                "decimal" | "numeric" | "dec" | "fixed" => BaseType::Decimal {
                    precision: Some(nums.first().copied().unwrap_or(10)),
                    scale: nums.get(1).copied().unwrap_or(0),
                },
                "float" | "double" | "real" => BaseType::Float,
                "char" | "varchar" => BaseType::Text { max_len: nums.first().copied().or(Some(255)) },
                "tinytext" => BaseType::Text { max_len: Some(255) },
                // TEXT 系列按字节限长:多字节字符按 4 字节算,留足余量
                "text" => BaseType::Text { max_len: Some(16_000) },
                "mediumtext" | "longtext" => BaseType::Text { max_len: Some(100_000) },
                "binary" | "varbinary" => BaseType::Bytes { max_len: nums.first().copied() },
                "tinyblob" => BaseType::Bytes { max_len: Some(255) },
                "blob" | "mediumblob" | "longblob" => BaseType::Bytes { max_len: Some(65_535) },
                "date" => BaseType::Date,
                "time" => BaseType::Time,
                "datetime" | "timestamp" => BaseType::Timestamp,
                "json" => BaseType::Json,
                _ => BaseType::Other,
            }
        }
        Dialect::Pg => {
            if t.starts_with("timestamp") {
                return BaseType::Timestamp;
            }
            if t.starts_with("time") {
                return BaseType::Time;
            }
            if t.starts_with("character varying") || t.starts_with("varchar") {
                return BaseType::Text { max_len: nums.first().copied() };
            }
            if t.starts_with("character") || head == "char" || head == "bpchar" {
                return BaseType::Text { max_len: Some(nums.first().copied().unwrap_or(1)) };
            }
            if t == "double precision" {
                return BaseType::Float;
            }
            match head {
                "smallint" | "int2" | "smallserial" => int(16),
                "integer" | "int" | "int4" | "serial" => int(32),
                "bigint" | "int8" | "bigserial" => int(64),
                "numeric" | "decimal" => BaseType::Decimal {
                    precision: nums.first().copied(),
                    scale: nums.get(1).copied().unwrap_or(if nums.is_empty() { 2 } else { 0 }),
                },
                "real" | "float4" | "float8" => BaseType::Float,
                "boolean" | "bool" => BaseType::Bool,
                "text" | "citext" | "name" => BaseType::Text { max_len: None },
                "date" => BaseType::Date,
                "json" | "jsonb" => BaseType::Json,
                "bytea" => BaseType::Bytes { max_len: None },
                "uuid" => BaseType::Uuid,
                "inet" | "cidr" => BaseType::Inet,
                _ => BaseType::Other,
            }
        }
        Dialect::Sqlite => {
            // 先认常见的具体写法,再按 SQLite 的类型亲和规则兜底
            if head.is_empty() {
                return BaseType::Other;
            }
            if head == "boolean" || head == "bool" {
                return BaseType::Bool;
            }
            if head == "datetime" || head.starts_with("timestamp") {
                return BaseType::Timestamp;
            }
            if head == "date" {
                return BaseType::Date;
            }
            if head == "time" {
                return BaseType::Time;
            }
            if head == "json" || head == "jsonb" {
                return BaseType::Json;
            }
            if head == "uuid" {
                return BaseType::Uuid;
            }
            if head == "decimal" || head == "numeric" {
                return BaseType::Decimal { precision: nums.first().copied(), scale: nums.get(1).copied().unwrap_or(2) };
            }
            if t.contains("int") {
                return int(64);
            }
            if t.contains("char") || t.contains("clob") || t.contains("text") {
                return BaseType::Text { max_len: nums.first().copied() };
            }
            if t.contains("blob") {
                return BaseType::Bytes { max_len: None };
            }
            if t.contains("real") || t.contains("floa") || t.contains("doub") {
                return BaseType::Float;
            }
            BaseType::Other
        }
    }
}

// ---------------------------------------------------------------- CHECK

/// 一列在 CHECK 约束里的限制。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CheckRule {
    pub values: Vec<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// 从 CHECK 表达式里读出简单的限制:`col IN (...)`、`col = ANY (ARRAY[...])`、
/// `col BETWEEN a AND b`、`col >= a`、`col < b`。读不懂的部分忽略(插入时数据库仍会把关)。
/// 含 OR 的表达式只认 IN 清单(范围在 OR 下不成立)。
pub fn parse_check(expr: &str) -> HashMap<String, CheckRule> {
    static CAST: OnceLock<Regex> = OnceLock::new();
    static INTRO: OnceLock<Regex> = OnceLock::new();
    static IN_LIST: OnceLock<Regex> = OnceLock::new();
    static ANY: OnceLock<Regex> = OnceLock::new();
    static BETWEEN: OnceLock<Regex> = OnceLock::new();
    static CMP: OnceLock<Regex> = OnceLock::new();
    let cast = CAST.get_or_init(|| {
        Regex::new(r"(?i)::(character varying|double precision|timestamp with(out)? time zone|[a-z_][a-z0-9_]*)(\(\d+(,\s*\d+)?\))?(\[\])?").unwrap()
    });
    // MySQL 字符集前缀:_utf8mb4'x'
    let intro = INTRO.get_or_init(|| Regex::new(r"(?i)_[a-z0-9]+'").unwrap());
    let in_list = IN_LIST.get_or_init(|| Regex::new(r"(?i)\(?\b([a-z_][a-z0-9_]*)\)?\s+in\s*\(([^()]*)\)").unwrap());
    let any = ANY.get_or_init(|| Regex::new(r"(?i)\(?\b([a-z_][a-z0-9_]*)\)?\s*=\s*any\s*\(\s*\(*\s*array\s*\[([^\]]*)\]").unwrap());
    let between = BETWEEN.get_or_init(|| {
        Regex::new(r"(?i)\(?\b([a-z_][a-z0-9_]*)\)?\s+between\s+\(?(-?\d+(?:\.\d+)?)\)?\s+and\s+\(?(-?\d+(?:\.\d+)?)").unwrap()
    });
    let cmp = CMP.get_or_init(|| Regex::new(r"(?i)\(?\b([a-z_][a-z0-9_]*)\)?\s*(>=|<=|>|<)\s*\(?(-?\d+(?:\.\d+)?)").unwrap());

    let s = expr.replace(['"', '`'], "");
    let s = cast.replace_all(&s, "");
    let s = intro.replace_all(&s, "'").to_string();
    let lower = s.to_ascii_lowercase();
    let has_or = lower.contains(" or ");
    let mut out: HashMap<String, CheckRule> = HashMap::new();
    let list_values = |inner: &str| -> Vec<String> {
        let quoted = quoted_list(inner);
        if !quoted.is_empty() {
            return quoted;
        }
        inner.split(',').map(|x| x.trim().to_string()).filter(|x| x.parse::<f64>().is_ok()).collect()
    };
    for c in in_list.captures_iter(&s).chain(any.captures_iter(&s)) {
        let vals = list_values(&c[2]);
        if !vals.is_empty() {
            out.entry(c[1].to_string()).or_default().values = vals;
        }
    }
    if has_or {
        return out;
    }
    for c in between.captures_iter(&s) {
        let r = out.entry(c[1].to_string()).or_default();
        r.min = c[2].parse().ok();
        r.max = c[3].parse().ok();
    }
    for c in cmp.captures_iter(&s) {
        let col = c[1].to_ascii_lowercase();
        if col == "and" || col == "between" {
            continue;
        }
        let Ok(n) = c[3].parse::<f64>() else { continue };
        let r = out.entry(c[1].to_string()).or_default();
        // 严格不等号:整数列靠上取整处理,这里先挪一个很小的量
        match &c[2] {
            ">=" => r.min = Some(r.min.map_or(n, |m| m.max(n))),
            ">" => r.min = Some(r.min.map_or(n + 1e-9, |m| m.max(n + 1e-9))),
            "<=" => r.max = Some(r.max.map_or(n, |m| m.min(n))),
            _ => r.max = Some(r.max.map_or(n - 1e-9, |m| m.min(n - 1e-9))),
        }
    }
    out
}

/// SQLite 建表语句里的 CHECK 表达式(列级与表级),按括号配对截出。
pub fn sqlite_checks(create_sql: &str) -> Vec<String> {
    let lower = create_sql.to_ascii_lowercase();
    let bytes = create_sql.as_bytes();
    let mut out = vec![];
    let mut from = 0;
    while let Some(i) = lower[from..].find("check") {
        let at = from + i;
        from = at + 5;
        let Some(open) = create_sql[from..].find('(').map(|j| from + j) else { break };
        if create_sql[from..open].trim().is_empty() {
            let mut depth = 0;
            for (k, b) in bytes.iter().enumerate().skip(open) {
                match b {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            out.push(create_sql[open..=k].to_string());
                            from = k;
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------- 结构查询

fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn mysql_lit(s: &str) -> String {
    crate::sql::build::literal(s, Dialect::Mysql)
}

/// 列信息的查询。结果列统一为:
/// 列名, 类型, 可空(t/1/YES), 有默认值(t/1), 默认值表达式, 自增/标识标记, 生成列标记, 枚举标签(\x1f 分隔)。
pub fn columns_sql(d: Dialect, table: &str) -> String {
    match d {
        Dialect::Pg => format!(
            "SELECT a.attname, format_type(a.atttypid, a.atttypmod), NOT a.attnotnull, a.atthasdef, \
             COALESCE(pg_get_expr(d.adbin, d.adrelid), ''), a.attidentity::text, a.attgenerated::text, \
             (SELECT string_agg(e.enumlabel, E'\\x1f' ORDER BY e.enumsortorder) FROM pg_enum e WHERE e.enumtypid = a.atttypid) \
             FROM pg_attribute a \
             LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE a.attrelid = {}::regclass AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum",
            lit(&format!("public.\"{}\"", table.replace('"', "\"\"")))
        ),
        Dialect::Mysql => format!(
            "SELECT COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, IF(COLUMN_DEFAULT IS NULL, '0', '1'), COALESCE(COLUMN_DEFAULT, ''), \
             EXTRA, GENERATION_EXPRESSION, '' FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = {} ORDER BY ORDINAL_POSITION",
            mysql_lit(table)
        ),
        Dialect::Sqlite => format!(
            "SELECT name, type, CASE WHEN \"notnull\" = 0 THEN '1' ELSE '0' END, \
             CASE WHEN dflt_value IS NULL THEN '0' ELSE '1' END, COALESCE(dflt_value, ''), pk, hidden, '' \
             FROM pragma_table_xinfo({}) ORDER BY cid",
            lit(table)
        ),
    }
}

/// CHECK 约束表达式的查询(每行一个表达式;SQLite 取建表语句,另行截取)。
pub fn checks_sql(d: Dialect, table: &str) -> String {
    match d {
        Dialect::Pg => format!(
            "SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE contype = 'c' AND conrelid = {}::regclass",
            lit(&format!("public.\"{}\"", table.replace('"', "\"\"")))
        ),
        Dialect::Mysql => format!(
            "SELECT cc.CHECK_CLAUSE FROM information_schema.TABLE_CONSTRAINTS tc \
             JOIN information_schema.CHECK_CONSTRAINTS cc \
             ON cc.CONSTRAINT_SCHEMA = tc.CONSTRAINT_SCHEMA AND cc.CONSTRAINT_NAME = tc.CONSTRAINT_NAME \
             WHERE tc.TABLE_SCHEMA = DATABASE() AND tc.TABLE_NAME = {} AND tc.CONSTRAINT_TYPE = 'CHECK'",
            mysql_lit(table)
        ),
        Dialect::Sqlite => format!("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = {}", lit(table)),
    }
}

fn truthy(v: Option<&str>) -> bool {
    matches!(v.map(|s| s.trim().to_ascii_lowercase()).as_deref(), Some("t" | "1" | "yes" | "true"))
}

/// 由各查询结果拼出表的造数视图。`indexes` 来自表详情;`fks` 是整库外键里这张表的那些行。
pub fn build_meta(
    d: Dialect,
    table: &str,
    column_rows: &[Vec<Option<String>>],
    check_exprs: &[String],
    indexes: &[IndexInfo],
    fks: &[ForeignKey],
) -> TableMeta {
    let cell = |r: &Vec<Option<String>>, i: usize| r.get(i).cloned().flatten().unwrap_or_default();
    let mut checks: HashMap<String, CheckRule> = HashMap::new();
    for e in check_exprs {
        let exprs = if d == Dialect::Sqlite { sqlite_checks(e) } else { vec![e.clone()] };
        for e in exprs {
            for (col, rule) in parse_check(&e) {
                let r = checks.entry(col.to_ascii_lowercase()).or_default();
                if !rule.values.is_empty() {
                    r.values = rule.values;
                }
                r.min = rule.min.or(r.min);
                r.max = rule.max.or(r.max);
            }
        }
    }
    let sqlite_pk_count = if d == Dialect::Sqlite {
        column_rows.iter().filter(|r| cell(r, 5) != "0" && !cell(r, 5).is_empty()).count()
    } else {
        0
    };
    let autoinc = d == Dialect::Sqlite && check_exprs.iter().any(|s| s.to_ascii_lowercase().contains("autoincrement"));

    let pk_cols: Vec<String> = indexes
        .iter()
        .find(|i| i.is_primary)
        .map(|i| i.columns.split(',').map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect())
        .unwrap_or_default();
    let mut single_unique: Vec<String> = vec![];
    let mut uniques: Vec<Vec<String>> = vec![];
    for i in indexes.iter().filter(|i| i.is_unique || i.is_primary) {
        let cols: Vec<String> = i.columns.split(',').map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect();
        match cols.len() {
            0 => {}
            1 => single_unique.push(cols[0].clone()),
            _ => {
                if !uniques.contains(&cols) {
                    uniques.push(cols)
                }
            }
        }
    }

    // 外键按约束分组(复合外键保序)
    let mut groups: Vec<FkGroup> = vec![];
    for fk in fks.iter().filter(|f| f.table == table && !f.inferred) {
        match groups.iter_mut().find(|g| g.constraint == fk.constraint) {
            Some(g) => {
                g.columns.push(fk.column.clone());
                g.ref_columns.push(fk.ref_column.clone());
            }
            None => groups.push(FkGroup {
                constraint: fk.constraint.clone(),
                columns: vec![fk.column.clone()],
                ref_table: fk.ref_table.clone(),
                ref_columns: vec![fk.ref_column.clone()],
            }),
        }
    }

    let columns = column_rows
        .iter()
        .map(|r| {
            let name = cell(r, 0);
            let data_type = cell(r, 1);
            let mut base = classify(&data_type, d);
            let nullable = truthy(Some(&cell(r, 2)));
            let has_default = truthy(Some(&cell(r, 3)));
            let default_expr = cell(r, 4).to_ascii_lowercase();
            let flag = cell(r, 5).to_ascii_lowercase();
            let generated = cell(r, 6).to_ascii_lowercase();
            let enum_labels: Vec<String> = cell(r, 7).split('\u{1f}').filter(|s| !s.is_empty()).map(String::from).collect();
            let primary = pk_cols.iter().any(|c| c == &name) || (d == Dialect::Sqlite && flag != "0" && !flag.is_empty());
            let auto = match d {
                Dialect::Pg => {
                    if !generated.is_empty() {
                        Some(AutoKind::Generated)
                    } else if !flag.is_empty() || default_expr.starts_with("nextval(") {
                        Some(AutoKind::Identity)
                    } else {
                        None
                    }
                }
                Dialect::Mysql => {
                    if flag.contains("generated") && !flag.contains("default_generated") || !generated.trim().is_empty() {
                        Some(AutoKind::Generated)
                    } else if flag.contains("auto_increment") {
                        Some(AutoKind::AutoIncrement)
                    } else {
                        None
                    }
                }
                Dialect::Sqlite => {
                    if generated == "2" || generated == "3" {
                        Some(AutoKind::Generated)
                    } else if primary && sqlite_pk_count == 1 && data_type.trim().eq_ignore_ascii_case("integer") {
                        Some(if autoinc { AutoKind::AutoIncrement } else { AutoKind::RowId })
                    } else {
                        None
                    }
                }
            };
            let mut values = vec![];
            if d == Dialect::Mysql && base == BaseType::Enum {
                values = quoted_list(&data_type);
            } else if !enum_labels.is_empty() {
                base = BaseType::Enum;
                values = enum_labels;
            }
            let rule = checks.get(&name.to_ascii_lowercase()).cloned().unwrap_or_default();
            if values.is_empty() {
                values = rule.values.clone();
            }
            let fk = groups.iter().find(|g| g.columns.contains(&name)).map(|g| g.constraint.clone());
            ColumnMeta {
                unique: single_unique.contains(&name),
                name,
                data_type,
                base,
                nullable: nullable && !primary,
                has_default: has_default || auto.is_some(),
                auto,
                primary,
                values,
                check_min: rule.min,
                check_max: rule.max,
                fk,
            }
        })
        .collect();
    TableMeta { table: table.to_string(), columns, uniques, fks: groups }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(cells: &[&str]) -> Vec<Option<String>> {
        cells.iter().map(|c| Some(c.to_string())).collect()
    }

    #[test]
    fn classifies_types_per_dialect() {
        use BaseType::*;
        assert_eq!(classify("character varying(40)", Dialect::Pg), Text { max_len: Some(40) });
        assert_eq!(classify("numeric(10,2)", Dialect::Pg), Decimal { precision: Some(10), scale: 2 });
        assert_eq!(classify("smallint", Dialect::Pg), Int { min: -32768, max: 32767 });
        assert_eq!(classify("timestamp with time zone", Dialect::Pg), Timestamp);
        assert_eq!(classify("time without time zone", Dialect::Pg), Time);
        assert_eq!(classify("jsonb", Dialect::Pg), Json);
        assert_eq!(classify("text[]", Dialect::Pg), Other);
        assert_eq!(classify("character(2)", Dialect::Pg), Text { max_len: Some(2) });
        assert_eq!(classify("int(10) unsigned", Dialect::Mysql), Int { min: 0, max: 4_294_967_295 });
        assert_eq!(classify("tinyint(1)", Dialect::Mysql), Bool);
        assert_eq!(classify("tinyint", Dialect::Mysql), Int { min: -128, max: 127 });
        assert_eq!(classify("bigint unsigned", Dialect::Mysql), Int { min: 0, max: i64::MAX });
        assert_eq!(classify("enum('a','b')", Dialect::Mysql), Enum);
        assert_eq!(classify("decimal(8,3)", Dialect::Mysql), Decimal { precision: Some(8), scale: 3 });
        assert_eq!(classify("varbinary(16)", Dialect::Mysql), Bytes { max_len: Some(16) });
        assert_eq!(classify("INTEGER", Dialect::Sqlite), Int { min: i64::MIN, max: i64::MAX });
        assert_eq!(classify("VARCHAR(12)", Dialect::Sqlite), Text { max_len: Some(12) });
        assert_eq!(classify("DATETIME", Dialect::Sqlite), Timestamp);
        assert_eq!(classify("", Dialect::Sqlite), Other);
        assert_eq!(classify("BLOB", Dialect::Sqlite), Bytes { max_len: None });
    }

    #[test]
    fn big_integers_from_js_deserialize_leniently() {
        // JS 里 i64 上限会变成 9223372036854775808(浮点),camelCase 字段名与界面一致
        let b: BaseType = serde_json::from_str(r#"{"kind":"int","min":-9223372036854775808,"max":9.223372036854776e18}"#).unwrap();
        assert_eq!(b, BaseType::Int { min: i64::MIN, max: i64::MAX });
        let t: BaseType = serde_json::from_str(r#"{"kind":"text","maxLen":12}"#).unwrap();
        assert_eq!(t, BaseType::Text { max_len: Some(12) });
        let g: crate::testdata::Generator = serde_json::from_str(r#"{"kind":"integer","min":1.0,"max":1e30}"#).unwrap();
        assert_eq!(g, crate::testdata::Generator::Integer { min: 1, max: i64::MAX });
        let g: crate::testdata::Generator = serde_json::from_str(r#"{"kind":"text","minLen":2,"maxLen":5}"#).unwrap();
        assert_eq!(g, crate::testdata::Generator::Text { min_len: 2, max_len: 5 });
    }

    #[test]
    fn parses_check_lists_and_ranges() {
        let m = parse_check("CHECK ((status)::text = ANY ((ARRAY['active'::character varying, 'it''s'::character varying])::text[]))");
        assert_eq!(m["status"].values, vec!["active", "it's"]);
        let m = parse_check("(`kind` in (_utf8mb4'a',_utf8mb4'b'))");
        assert_eq!(m["kind"].values, vec!["a", "b"]);
        let m = parse_check("CHECK (rating >= 1 AND rating <= 5)");
        assert_eq!((m["rating"].min, m["rating"].max), (Some(1.0), Some(5.0)));
        let m = parse_check("CHECK ((qty > 0))");
        assert!(m["qty"].min.unwrap() > 0.0 && m["qty"].max.is_none());
        let m = parse_check("CHECK (score BETWEEN 0 AND 100)");
        assert_eq!((m["score"].min, m["score"].max), (Some(0.0), Some(100.0)));
        let m = parse_check("CHECK (level IN (1, 2, 3))");
        assert_eq!(m["level"].values, vec!["1", "2", "3"]);
        // OR 下范围不成立,不读
        let m = parse_check("CHECK (a > 5 OR a < 0)");
        assert!(!m.contains_key("a"));
    }

    #[test]
    fn extracts_sqlite_checks() {
        let sql = "CREATE TABLE t(id INTEGER PRIMARY KEY, s TEXT CHECK (s IN ('x','y')), n INT, CHECK(n BETWEEN 1 AND (9)))";
        let cs = sqlite_checks(sql);
        assert_eq!(cs, vec!["(s IN ('x','y'))".to_string(), "(n BETWEEN 1 AND (9))".to_string()]);
    }

    #[test]
    fn builds_sqlite_meta_with_rowid_unique_and_fk() {
        let rows = vec![
            r(&["id", "INTEGER", "1", "0", "", "1", "0", ""]),
            r(&["email", "VARCHAR(80)", "0", "0", "", "0", "0", ""]),
            r(&["status", "TEXT", "1", "1", "'draft'", "0", "0", ""]),
            r(&["user_id", "INTEGER", "1", "0", "", "0", "0", ""]),
            r(&["total", "REAL", "1", "0", "", "0", "2", ""]),
        ];
        let create = "CREATE TABLE t(id INTEGER PRIMARY KEY, email VARCHAR(80) NOT NULL UNIQUE, status TEXT DEFAULT 'draft' CHECK (status IN ('draft','paid')), user_id INTEGER REFERENCES users(id), total REAL GENERATED ALWAYS AS (1))".to_string();
        let idx = vec![IndexInfo { name: "u".into(), columns: "email".into(), is_primary: false, is_unique: true }];
        let fks = vec![ForeignKey {
            constraint: "fk_t_0".into(),
            table: "t".into(),
            column: "user_id".into(),
            ref_table: "users".into(),
            ref_column: "id".into(),
            inferred: false,
        }];
        let m = build_meta(Dialect::Sqlite, "t", &rows, &[create], &idx, &fks);
        let c = |n: &str| m.column(n).unwrap();
        assert_eq!(c("id").auto, Some(AutoKind::RowId));
        assert!(c("email").unique && !c("email").nullable);
        assert_eq!(c("email").base, BaseType::Text { max_len: Some(80) });
        assert_eq!(c("status").values, vec!["draft", "paid"]);
        assert!(c("status").has_default);
        assert_eq!(c("user_id").fk.as_deref(), Some("fk_t_0"));
        assert_eq!(c("total").auto, Some(AutoKind::Generated));
        assert_eq!(m.fks[0].ref_table, "users");
    }

    #[test]
    fn builds_pg_and_mysql_meta() {
        let rows = vec![
            r(&["id", "integer", "f", "t", "nextval('t_id_seq'::regclass)", "", "", ""]),
            r(&["code", "bigint", "f", "f", "", "a", "", ""]),
            r(&["mood", "mood", "t", "f", "", "", "", "sad\u{1f}ok\u{1f}happy"]),
            r(&["total", "numeric(10,2)", "t", "f", "", "", "s", ""]),
        ];
        let m = build_meta(Dialect::Pg, "t", &rows, &[], &[], &[]);
        assert_eq!(m.columns[0].auto, Some(AutoKind::Identity));
        assert_eq!(m.columns[1].auto, Some(AutoKind::Identity));
        assert_eq!(m.columns[2].base, BaseType::Enum);
        assert_eq!(m.columns[2].values, vec!["sad", "ok", "happy"]);
        assert_eq!(m.columns[3].auto, Some(AutoKind::Generated));

        let rows = vec![
            r(&["id", "int", "NO", "0", "", "auto_increment", "", ""]),
            r(&["state", "enum('new','done')", "NO", "1", "new", "", "", ""]),
            r(&["created", "timestamp", "NO", "1", "CURRENT_TIMESTAMP", "DEFAULT_GENERATED", "", ""]),
            r(&["v", "int", "YES", "0", "", "VIRTUAL GENERATED", "id + 1", ""]),
        ];
        let idx = vec![IndexInfo { name: "uq".into(), columns: "state, created".into(), is_primary: false, is_unique: true }];
        let m = build_meta(Dialect::Mysql, "t", &rows, &[], &idx, &[]);
        assert_eq!(m.columns[0].auto, Some(AutoKind::AutoIncrement));
        assert_eq!(m.columns[1].values, vec!["new", "done"]);
        assert_eq!(m.columns[2].auto, None);
        assert_eq!(m.columns[3].auto, Some(AutoKind::Generated));
        assert_eq!(m.uniques, vec![vec!["state".to_string(), "created".to_string()]]);
    }
}
