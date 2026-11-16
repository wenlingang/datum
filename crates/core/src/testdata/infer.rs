//! 按列名 + 类型自动挑生成器:先看约束(自增 / 外键 / 枚举 / CHECK 清单),再按列名语义,
//! 最后按类型兜底;范围一律收进类型与 CHECK 允许的区间。

use super::gen::{format_date, Generator};
use super::meta::{BaseType, ColumnMeta, TableMeta};
use super::plan::ColumnPlan;
use serde::{Deserialize, Serialize};

/// 推断时可用的库里现状(只读查询得到,不发给 AI)。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnHints {
    /// 低基数文本列的现有取值(状态、类型一类)
    #[serde(default)]
    pub distinct: Option<Vec<String>>,
    /// 整数唯一列的现有最大值(递增序列从它之后开始)
    #[serde(default)]
    pub max_int: Option<i64>,
}

/// 列名拆成小写词:`createdAt` / `created_at` / `Created-At` → ["created", "at"]。
pub fn tokens(name: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if c == '_' || c == '-' || c == ' ' || c == '.' {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn has(tokens: &[String], words: &[&str]) -> bool {
    tokens.iter().any(|t| words.contains(&t.as_str()))
}

/// 表名像不像「人」(决定裸 `name` 是人名还是商品 / 标题)。
fn person_table(table: &str) -> bool {
    let t = table.to_ascii_lowercase();
    [
        "user", "customer", "employee", "member", "author", "people", "person", "contact", "student", "staff",
        "account", "client", "patient", "teacher", "driver", "admin", "profile", "subscriber", "owner",
    ]
    .iter()
    .any(|k| t.contains(k))
}

/// 列名语义 → 生成器(不看类型;类型匹配在调用处检查)。`today` 为天数。
pub fn semantic(name: &str, table: &str, today: i64) -> Option<Generator> {
    let t = tokens(name);
    let n = t.join("_");
    let date = |from: i64, to: i64| (format_date(from), format_date(to));
    let recent = date(today - 730, today);
    let g = if has(&t, &["email", "mail"]) {
        Generator::Email
    } else if has(&t, &["phone", "mobile", "tel", "telephone", "cell", "cellphone", "fax"]) {
        Generator::Phone
    } else if n == "username" || n == "user_name" || n == "login" || has(&t, &["nickname", "handle", "nick"]) {
        Generator::Username
    } else if n.contains("first_name") || n == "firstname" || n == "given_name" || n == "fname" || n == "forename" {
        Generator::FirstName
    } else if n.contains("last_name") || n == "lastname" || n == "surname" || n == "family_name" || n == "lname" {
        Generator::LastName
    } else if has(&t, &["company", "organization", "organisation", "org", "employer", "vendor", "supplier", "brand", "publisher"]) {
        Generator::Company
    } else if n == "full_name" || n == "fullname" || n == "real_name" || n == "display_name" || has(&t, &["author", "contact", "recipient", "receiver", "sender"]) && has(&t, &["name"]) || n == "name" && person_table(table) || has(&t, &["customer", "user", "employee", "person", "owner", "manager"]) && has(&t, &["name"]) {
        Generator::FullName
    } else if has(&t, &["city", "town"]) {
        Generator::City
    } else if has(&t, &["country", "nation", "nationality"]) {
        Generator::Country
    } else if has(&t, &["zip", "zipcode", "postal", "postcode"]) {
        Generator::PostalCode
    } else if has(&t, &["address", "street", "addr"]) {
        Generator::Address
    } else if has(&t, &["url", "website", "homepage", "link", "avatar", "image", "photo", "picture", "logo", "thumbnail", "href"]) {
        Generator::Url
    } else if n == "ip" || n.contains("ip_address") || n == "ipaddr" || n.ends_with("_ip") {
        Generator::Ip
    } else if has(&t, &["uuid", "guid"]) {
        Generator::Uuid
    } else if has(&t, &["title", "subject", "headline", "caption"]) || n == "name" || n.ends_with("_name") {
        Generator::Title
    } else if has(&t, &["description", "content", "body", "bio", "biography", "summary", "details", "article"]) {
        Generator::Paragraph
    } else if has(&t, &["comment", "remark", "remarks", "note", "notes", "message", "memo", "reason", "feedback", "review"]) {
        Generator::Sentence
    } else if has(&t, &["tag", "label", "keyword", "slug"]) {
        Generator::Word
    } else if has(&t, &["sku", "code", "token", "serial", "barcode", "ref", "reference"]) {
        Generator::Text { min_len: 8, max_len: 12 }
    } else if has(&t, &["status", "state", "type", "kind", "category", "level", "role", "gender", "sex", "priority", "stage", "tier", "plan", "channel", "source", "method"]) {
        let values: &[&str] = if has(&t, &["gender", "sex"]) {
            &["male", "female", "other"]
        } else if has(&t, &["role"]) {
            &["admin", "editor", "user", "guest"]
        } else if has(&t, &["priority"]) {
            &["low", "medium", "high", "urgent"]
        } else if has(&t, &["level", "tier"]) {
            &["basic", "silver", "gold", "platinum"]
        } else if has(&t, &["status", "state", "stage"]) {
            &["active", "pending", "inactive", "archived"]
        } else {
            &["standard", "premium", "trial", "other"]
        };
        Generator::PickList { values: values.iter().map(|s| s.to_string()).collect() }
    } else if has(&t, &["latitude", "lat"]) {
        Generator::Decimal { min: -90.0, max: 90.0, scale: 6 }
    } else if has(&t, &["longitude", "lng", "lon", "long"]) {
        Generator::Decimal { min: -180.0, max: 180.0, scale: 6 }
    } else if has(&t, &["price", "amount", "cost", "total", "fee", "balance", "salary", "revenue", "subtotal", "tax", "discount", "payment", "budget", "wage", "income"]) {
        if has(&t, &["cents"]) {
            Generator::Integer { min: 100, max: 100_000 }
        } else {
            Generator::Decimal { min: 1.0, max: 1000.0, scale: 2 }
        }
    } else if has(&t, &["percent", "percentage", "ratio", "rate"]) {
        Generator::Decimal { min: 0.0, max: 100.0, scale: 2 }
    } else if has(&t, &["rating", "stars"]) {
        Generator::Integer { min: 1, max: 5 }
    } else if has(&t, &["score", "points"]) {
        Generator::Integer { min: 0, max: 100 }
    } else if has(&t, &["age"]) {
        Generator::Integer { min: 18, max: 80 }
    } else if has(&t, &["quantity", "qty", "count", "stock", "inventory", "views", "likes", "clicks", "visits", "units"]) {
        Generator::Integer { min: if has(&t, &["stock", "inventory", "views", "likes", "clicks", "visits"]) { 0 } else { 1 }, max: if has(&t, &["quantity", "qty", "units"]) { 20 } else { 500 } }
    } else if has(&t, &["year"]) {
        let (y, _, _) = super::gen::days_to_date(today);
        Generator::Integer { min: y - 30, max: y }
    } else if has(&t, &["birthday", "birthdate", "dob", "birth"]) {
        Generator::Date { from: format_date(today - 75 * 365), to: format_date(today - 18 * 365) }
    } else if t.first().is_some_and(|f| ["is", "has", "can", "should", "allow"].contains(&f.as_str()))
        || has(&t, &["active", "enabled", "disabled", "deleted", "verified", "visible", "published", "flag", "archived", "approved", "locked"]) && !has(&t, &["at", "time", "date", "on", "by"])
    {
        Generator::Boolean
    } else if has(&t, &["at", "time", "date", "datetime", "timestamp", "on", "created", "updated", "modified", "deleted", "expires", "expired"]) {
        Generator::Timestamp { from: recent.0, to: recent.1 }
    } else {
        return None;
    };
    Some(g)
}

/// 类型兜底的生成器。
pub fn by_type(col: &ColumnMeta, today: i64) -> Generator {
    let recent = (format_date(today - 730), format_date(today));
    match &col.base {
        BaseType::Int { .. } => Generator::Integer { min: 1, max: 10_000 },
        BaseType::Decimal { scale, .. } => Generator::Decimal { min: 0.0, max: 1000.0, scale: *scale },
        BaseType::Float => Generator::Float { min: 0.0, max: 1000.0 },
        BaseType::Bool => Generator::Boolean,
        BaseType::Text { max_len } => match max_len {
            Some(n) if *n < 8 => Generator::Text { min_len: 1, max_len: *n },
            Some(n) if *n < 40 => Generator::Word,
            _ => Generator::Sentence,
        },
        BaseType::Date => Generator::Date { from: recent.0, to: recent.1 },
        BaseType::Timestamp => Generator::Timestamp { from: recent.0, to: recent.1 },
        BaseType::Time => Generator::Time,
        BaseType::Json => Generator::Json,
        BaseType::Bytes { max_len } => Generator::Bytes { len: max_len.unwrap_or(16).min(16) },
        BaseType::Uuid => Generator::Uuid,
        BaseType::Inet => Generator::Ip,
        BaseType::Enum => Generator::PickList { values: col.values.clone() },
        BaseType::Other => Generator::Skip,
    }
}

/// 把生成器的参数收进类型、CHECK 允许的区间(界面改过的、AI 给的同样经过这里)。
pub fn clamp(g: Generator, col: &ColumnMeta) -> Generator {
    let (tmin, tmax) = match &col.base {
        BaseType::Int { min, max } => (*min as f64, *max as f64),
        BaseType::Decimal { precision: Some(p), scale } if p > scale => {
            let lim = 10f64.powi((*p - *scale) as i32) - 10f64.powi(-(*scale as i32));
            (-lim, lim)
        }
        _ => (f64::MIN, f64::MAX),
    };
    let lo = col.check_min.map_or(tmin, |m| m.max(tmin));
    let hi = col.check_max.map_or(tmax, |m| m.min(tmax));
    let fit = |a: f64, b: f64| -> (f64, f64) {
        let a = a.clamp(lo, hi);
        let b = b.clamp(lo, hi);
        if a <= b { (a, b) } else { (b, a) }
    };
    match g {
        Generator::Integer { min, max } => {
            let (a, b) = fit(min as f64, max as f64);
            // 严格不等号挪过的小量:整数上取整 / 下取整
            Generator::Integer { min: a.ceil() as i64, max: b.floor().max(a.ceil()) as i64 }
        }
        Generator::Decimal { min, max, scale } => {
            let scale = match &col.base {
                BaseType::Decimal { scale: s, .. } => scale.min(*s),
                BaseType::Int { .. } => 0,
                _ => scale,
            };
            let (a, b) = fit(min, max);
            Generator::Decimal { min: a, max: b, scale }
        }
        Generator::Float { min, max } => {
            let (a, b) = fit(min, max);
            Generator::Float { min: a, max: b }
        }
        Generator::Text { min_len, max_len } => {
            let cap = match &col.base {
                BaseType::Text { max_len: Some(n) } => *n,
                _ => u32::MAX,
            };
            let hi = max_len.min(cap).max(1);
            Generator::Text { min_len: min_len.min(hi), max_len: hi }
        }
        Generator::Bytes { len } => {
            let cap = match &col.base {
                BaseType::Bytes { max_len: Some(n) } => *n,
                _ => u32::MAX,
            };
            Generator::Bytes { len: len.min(cap).max(1) }
        }
        Generator::Sequence { start, step } => {
            let start = (start as f64).clamp(lo, hi) as i64;
            Generator::Sequence { start, step: if step == 0 { 1 } else { step } }
        }
        other => other,
    }
}

/// 一列的自动方案。
pub fn infer_column(col: &ColumnMeta, meta: &TableMeta, hints: &ColumnHints, today: i64) -> ColumnPlan {
    let plan = |generator: Generator, null_ratio: f64| ColumnPlan { name: col.name.clone(), generator, null_ratio };
    if col.auto.is_some() {
        return plan(Generator::Skip, 0.0);
    }
    if col.fk.is_some() {
        return plan(Generator::ForeignKey, 0.0);
    }
    let null_ratio = if col.nullable && !col.unique { 0.1 } else { 0.0 };
    if !col.values.is_empty() {
        let values = if col.base == BaseType::Bool { vec![] } else { col.values.clone() };
        if !values.is_empty() {
            return plan(Generator::PickList { values }, if col.nullable { 0.05 } else { 0.0 });
        }
    }
    // 整数唯一列(没有自增的主键之类):递增序列,接在现有最大值之后
    if col.unique && matches!(col.base, BaseType::Int { .. }) {
        let start = hints.max_int.map_or(1, |m| m.saturating_add(1)).max(col.check_min.map_or(i64::MIN, |m| m.ceil() as i64));
        return plan(clamp(Generator::Sequence { start, step: 1 }, col), 0.0);
    }
    let mut g = semantic(&col.name, &meta.table, today).filter(|g| g.fits(&col.base));
    // 文本列上的「状态 / 类型」:库里已有的少量取值比内置词更贴切
    if let (Some(Generator::PickList { .. }) | None, Some(d)) = (&g, &hints.distinct) {
        if !d.is_empty() && col.base.is_textual() && !col.unique && semantic_is_enumish(&col.name) {
            g = Some(Generator::PickList { values: d.clone() });
        }
    }
    // 小数列上语义给的是整数(评分):换成同范围的小数,保留一位
    if let Some(Generator::Integer { min, max }) = &g {
        let scale = match &col.base {
            BaseType::Decimal { scale, .. } => Some((*scale).min(1)),
            BaseType::Float => Some(1),
            _ => None,
        };
        if let Some(scale) = scale {
            g = Some(Generator::Decimal { min: *min as f64, max: *max as f64, scale });
        }
    }
    // 布尔语义落在整数列上(MySQL TINYINT、SQLite INTEGER):照样 0 / 1
    let g = g.unwrap_or_else(|| by_type(col, today));
    let g = match (&g, &col.base) {
        // 时间戳语义落在日期列上
        (Generator::Timestamp { from, to }, BaseType::Date) => Generator::Date { from: from.clone(), to: to.clone() },
        _ => g,
    };
    let g = clamp(g, col);
    let null_ratio = if matches!(g, Generator::Skip) { 0.0 } else { null_ratio };
    plan(g, null_ratio)
}

/// 列名像不像「枚举类」(值得去库里取样现有取值)。
pub fn semantic_is_enumish(name: &str) -> bool {
    has(
        &tokens(name),
        &["status", "state", "type", "kind", "category", "level", "role", "gender", "sex", "priority", "stage", "tier", "plan", "channel", "source", "method", "currency", "unit", "lang", "language", "locale"],
    )
}

/// 界面里能选的生成器类型(下拉的顺序)。
pub const KINDS: &[&str] = &[
    "skip", "null", "fixed", "pickList", "foreignKey", "fullName", "firstName", "lastName", "username", "email", "phone",
    "address", "city", "country", "postalCode", "company", "url", "ip", "uuid", "word", "title", "sentence", "paragraph",
    "text", "integer", "sequence", "decimal", "float", "boolean", "date", "timestamp", "time", "json", "bytes",
];

/// 按类型名给出带默认参数的生成器(界面改选、AI 建议用);名字不认识时为 None。
/// 参数按列的类型与 CHECK 收口。
pub fn generator_for_kind(kind: &str, col: &ColumnMeta, today: i64) -> Option<Generator> {
    let recent = (format_date(today - 730), format_date(today));
    let g = match kind {
        "skip" => Generator::Skip,
        "null" => Generator::Null,
        "fixed" => Generator::Fixed { value: String::new() },
        "pickList" => Generator::PickList { values: if col.values.is_empty() { vec!["A".into(), "B".into(), "C".into()] } else { col.values.clone() } },
        "foreignKey" => Generator::ForeignKey,
        "fullName" => Generator::FullName,
        "firstName" => Generator::FirstName,
        "lastName" => Generator::LastName,
        "username" => Generator::Username,
        "email" => Generator::Email,
        "phone" => Generator::Phone,
        "address" => Generator::Address,
        "city" => Generator::City,
        "country" => Generator::Country,
        "postalCode" => Generator::PostalCode,
        "company" => Generator::Company,
        "url" => Generator::Url,
        "ip" => Generator::Ip,
        "uuid" => Generator::Uuid,
        "word" => Generator::Word,
        "title" => Generator::Title,
        "sentence" => Generator::Sentence,
        "paragraph" => Generator::Paragraph,
        "text" => Generator::Text { min_len: 6, max_len: 12 },
        "integer" => Generator::Integer { min: 1, max: 10_000 },
        "sequence" => Generator::Sequence { start: 1, step: 1 },
        "decimal" => Generator::Decimal {
            min: 0.0,
            max: 1000.0,
            scale: match &col.base {
                BaseType::Decimal { scale, .. } => *scale,
                _ => 2,
            },
        },
        "float" => Generator::Float { min: 0.0, max: 1000.0 },
        "boolean" => Generator::Boolean,
        "date" => Generator::Date { from: recent.0, to: recent.1 },
        "timestamp" => Generator::Timestamp { from: recent.0, to: recent.1 },
        "time" => Generator::Time,
        "json" => Generator::Json,
        "bytes" => Generator::Bytes { len: 16 },
        _ => return None,
    };
    Some(clamp(g, col))
}

/// 整张表的自动方案(按表的列序)。
pub fn infer_table(meta: &TableMeta, hints: &std::collections::HashMap<String, ColumnHints>, today: i64) -> Vec<ColumnPlan> {
    let empty = ColumnHints::default();
    meta.columns
        .iter()
        .map(|c| infer_column(c, meta, hints.get(&c.name).unwrap_or(&empty), today))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdata::meta::AutoKind;

    pub fn col(name: &str, base: BaseType) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            data_type: String::new(),
            base,
            nullable: true,
            has_default: false,
            auto: None,
            primary: false,
            unique: false,
            values: vec![],
            check_min: None,
            check_max: None,
            fk: None,
        }
    }

    fn table(cols: Vec<ColumnMeta>, name: &str) -> TableMeta {
        TableMeta { table: name.into(), columns: cols, uniques: vec![], fks: vec![] }
    }

    const TODAY: i64 = 20_000;

    #[test]
    fn tokenizes_snake_and_camel() {
        assert_eq!(tokens("createdAt"), vec!["created", "at"]);
        assert_eq!(tokens("first_name"), vec!["first", "name"]);
        assert_eq!(tokens("IPAddress"), vec!["ipaddress"]);
        assert_eq!(tokens("user-ID"), vec!["user", "id"]);
    }

    #[test]
    fn picks_semantic_generators() {
        let text = BaseType::Text { max_len: Some(100) };
        let m = table(vec![], "users");
        let g = |n: &str, b: &BaseType| infer_column(&col(n, b.clone()), &m, &ColumnHints::default(), TODAY).generator;
        assert_eq!(g("email", &text), Generator::Email);
        assert_eq!(g("name", &text), Generator::FullName);
        assert_eq!(g("first_name", &text), Generator::FirstName);
        assert_eq!(g("mobile_phone", &text), Generator::Phone);
        assert_eq!(g("city", &text), Generator::City);
        assert_eq!(g("zip_code", &text), Generator::PostalCode);
        assert_eq!(g("homepage_url", &text), Generator::Url);
        assert!(matches!(g("created_at", &BaseType::Timestamp), Generator::Timestamp { .. }));
        assert!(matches!(g("birthday", &BaseType::Date), Generator::Date { .. }));
        assert!(matches!(g("created_on", &BaseType::Date), Generator::Date { .. }));
        assert_eq!(g("is_active", &BaseType::Bool), Generator::Boolean);
        assert_eq!(g("is_active", &BaseType::Int { min: -128, max: 127 }), Generator::Boolean);
        assert!(matches!(g("price", &BaseType::Decimal { precision: Some(10), scale: 2 }), Generator::Decimal { scale: 2, .. }));
        assert!(matches!(g("rating", &BaseType::Int { min: i64::MIN, max: i64::MAX }), Generator::Integer { min: 1, max: 5 }));
        // 邮箱语义但列是整数:不合适,退回类型兜底
        assert!(matches!(g("email", &BaseType::Int { min: 0, max: 10 }), Generator::Integer { .. }));
        // 商品表里的 name 是标题,不是人名
        let p = table(vec![], "products");
        assert_eq!(infer_column(&col("name", text.clone()), &p, &ColumnHints::default(), TODAY).generator, Generator::Title);
    }

    #[test]
    fn constraints_win_over_semantics() {
        let m = table(vec![], "t");
        let mut c = col("id", BaseType::Int { min: i64::MIN, max: i64::MAX });
        c.auto = Some(AutoKind::Identity);
        assert_eq!(infer_column(&c, &m, &ColumnHints::default(), TODAY).generator, Generator::Skip);
        let mut c = col("user_email", BaseType::Text { max_len: None });
        c.fk = Some("fk".into());
        assert_eq!(infer_column(&c, &m, &ColumnHints::default(), TODAY).generator, Generator::ForeignKey);
        let mut c = col("status", BaseType::Enum);
        c.values = vec!["a".into(), "b".into()];
        assert_eq!(
            infer_column(&c, &m, &ColumnHints::default(), TODAY).generator,
            Generator::PickList { values: vec!["a".into(), "b".into()] }
        );
        // 唯一整数列:序列接在现有最大值后
        let mut c = col("code", BaseType::Int { min: i64::MIN, max: i64::MAX });
        c.unique = true;
        let hints = ColumnHints { max_int: Some(41), ..Default::default() };
        assert_eq!(infer_column(&c, &m, &hints, TODAY).generator, Generator::Sequence { start: 42, step: 1 });
        // 现有取值优先于内置的状态词
        let c = col("order_status", BaseType::Text { max_len: Some(20) });
        let hints = ColumnHints { distinct: Some(vec!["PAID".into(), "NEW".into()]), ..Default::default() };
        assert_eq!(infer_column(&c, &m, &hints, TODAY).generator, Generator::PickList { values: vec!["PAID".into(), "NEW".into()] });
    }

    #[test]
    fn clamps_to_type_and_check() {
        let mut c = col("rating", BaseType::Int { min: -128, max: 127 });
        c.check_min = Some(0.0 + 1e-9);
        c.check_max = Some(3.0);
        assert_eq!(clamp(Generator::Integer { min: -50, max: 1000 }, &c), Generator::Integer { min: 1, max: 3 });
        let c = col("price", BaseType::Decimal { precision: Some(5), scale: 2 });
        let Generator::Decimal { max, scale, .. } = clamp(Generator::Decimal { min: 0.0, max: 1e9, scale: 4 }, &c) else { panic!() };
        assert!((max - 999.99).abs() < 1e-9);
        assert_eq!(scale, 2);
        let c = col("code", BaseType::Text { max_len: Some(5) });
        assert_eq!(clamp(Generator::Text { min_len: 8, max_len: 12 }, &c), Generator::Text { min_len: 5, max_len: 5 });
        // 不可空列没有空值比例
        let mut c = col("title", BaseType::Text { max_len: None });
        c.nullable = false;
        assert_eq!(infer_column(&c, &table(vec![], "t"), &ColumnHints::default(), TODAY).null_ratio, 0.0);
    }
}
