//! 造数引擎:按方案逐行生成,处理空值比例、长度 / 类型收口、唯一去重、外键取样、
//! 成对时间列的先后(created ≤ updated)。生成是确定的:同一方案 + 种子 + 上下文,结果一致。

use super::gen::{generate, Ctx, Generator, Value};
use super::infer::{clamp, tokens};
use super::meta::{BaseType, TableMeta};
use super::plan::{Plan, MAX_ROWS};
use super::rng::Rng;
use crate::error::{AppError, AppResult, ErrorKind};
use crate::sql::Dialect;
use rust_i18n::t;
use std::collections::{HashMap, HashSet};

/// 生成依赖的库里现状(只读查出):父表的键、唯一列已有的值。
#[derive(Debug, Clone, Default)]
pub struct Context {
    /// 外键约束名 → 父表里现有的键(每行与约束的列一一对应)
    pub fk_keys: HashMap<String, Vec<Vec<Option<String>>>>,
    /// 唯一列 → 表里已有的值(避免与现有数据冲突)
    pub existing: HashMap<String, HashSet<String>>,
}

/// 唯一列一个值最多重试几次。
const UNIQUE_RETRIES: u32 = 200;

fn err(key: &str) -> AppError {
    AppError::new(ErrorKind::Query, t!(key))
}

struct Col {
    /// 在表列里的序号
    meta_ix: usize,
    generator: Generator,
    null_ratio: f64,
    rng: Rng,
    max_len: Option<u32>,
    /// 唯一列:已用过的值(比较键)
    seen: Option<HashSet<String>>,
    /// 外键分组的序号
    fk: Option<usize>,
}

struct FkPick {
    keys: Vec<Vec<Option<String>>>,
    /// 约束列在插入列里的位置(没插入的列为 None),与 keys 的每一项对齐
    targets: Vec<Option<usize>>,
    rng: Rng,
    /// 父表为空且外键列可空:全部写 NULL
    all_null: bool,
    /// 外键列本身唯一(一对一):键不能重复使用
    unique: bool,
    used: HashSet<usize>,
}

pub struct Engine {
    dialect: Dialect,
    locale: super::gen::Locale,
    columns: Vec<String>,
    bases: Vec<BaseType>,
    cols: Vec<Col>,
    fks: Vec<FkPick>,
    /// 多列唯一:插入列序号 + 已用组合
    uniques: Vec<(Vec<usize>, HashSet<String>)>,
    /// 先后成对的时间列(插入列序号:早, 晚)
    pairs: Vec<(usize, usize)>,
    row: u64,
    rows: u64,
}

/// 唯一性比较键:MySQL 默认排序规则不分大小写、忽略尾部空格。
fn unique_key(v: &Value, d: Dialect) -> Option<String> {
    let s = v.display()?;
    Some(if d == Dialect::Mysql { s.trim_end().to_lowercase() } else { s })
}

/// 有限取值空间的大小(超过行数的无须预检)。
fn domain_size(g: &Generator) -> Option<u64> {
    match g {
        Generator::Fixed { .. } => Some(1),
        Generator::Boolean => Some(2),
        Generator::PickList { values } => Some(values.iter().collect::<HashSet<_>>().len() as u64),
        Generator::Integer { min, max } => Some((*max as i128 - *min as i128 + 1).clamp(0, u64::MAX as i128) as u64),
        _ => None,
    }
}

/// 值按列的类型收口:文本截到长度上限,数字 / 布尔按列类型换算。
fn coerce(v: Value, base: &BaseType, max_len: Option<u32>) -> Value {
    let v = match (v, base) {
        (Value::Text(s), BaseType::Int { .. }) => match s.trim().parse::<i64>() {
            Ok(i) => Value::Int(i),
            Err(_) => Value::Text(s),
        },
        (Value::Text(s), BaseType::Decimal { .. } | BaseType::Float) if s.trim().parse::<f64>().is_ok() => Value::Num(s.trim().to_string()),
        (Value::Text(s), BaseType::Bool) => match s.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "t" | "yes" | "y" => Value::Bool(true),
            "0" | "false" | "f" | "no" | "n" => Value::Bool(false),
            _ => Value::Text(s),
        },
        (Value::Bool(b), BaseType::Int { .. }) => Value::Int(i64::from(b)),
        (Value::Bool(b), BaseType::Text { .. } | BaseType::Other) => Value::Text(if b { "true" } else { "false" }.into()),
        (Value::Num(s), BaseType::Int { .. }) => Value::Int(s.parse::<f64>().map(|f| f.round() as i64).unwrap_or(0)),
        (Value::Int(i), BaseType::Text { .. } | BaseType::Other | BaseType::Enum) => Value::Text(i.to_string()),
        (Value::Num(s), BaseType::Text { .. } | BaseType::Other) => Value::Text(s),
        (Value::Float(f), BaseType::Text { .. } | BaseType::Other) => Value::Text(Value::Float(f).display().unwrap_or_default()),
        (Value::Int(i), BaseType::Bool) => Value::Bool(i != 0),
        (v, _) => v,
    };
    match (v, max_len) {
        (Value::Text(s), Some(n)) if s.chars().count() > n as usize => Value::Text(s.chars().take(n as usize).collect::<String>().trim_end().to_string()),
        (v, _) => v,
    }
}

/// 时间列的角色:(角色族, 是否开始一类, 去掉角色词后的词干)。
/// 族:0 = 创建 / 更新(created ↔ updated、deleted、expires……),1 = 开始 / 结束,2 = from / to。
fn time_role(name: &str) -> Option<(u8, bool, String)> {
    const FAMILIES: &[(&[&str], &[&str])] = &[
        (
            &["created", "create", "issued", "opened", "registered", "signup", "joined"],
            &["updated", "update", "modified", "deleted", "closed", "last", "expires", "expire", "expired", "due", "resolved", "completed"],
        ),
        (&["start", "started", "begin", "begins", "starts"], &["end", "ends", "ended", "finish", "finished", "stop", "stopped"]),
        (&["from", "since"], &["to", "until", "till"]),
    ];
    const NOISE: &[&str] = &["at", "time", "date", "on", "gmt", "ts", "datetime", "timestamp", "day"];
    let t = tokens(name);
    for (fam, (starts, ends)) in FAMILIES.iter().enumerate() {
        let start = t.iter().any(|x| starts.contains(&x.as_str()));
        let end = t.iter().any(|x| ends.contains(&x.as_str()));
        if start == end {
            continue;
        }
        let stem: Vec<&str> = t
            .iter()
            .map(String::as_str)
            .filter(|x| !starts.contains(x) && !ends.contains(x) && !NOISE.contains(x))
            .collect();
        return Some((fam as u8, start, stem.join("_")));
    }
    None
}

impl Engine {
    /// 校验方案并准备生成。`ctx` 带父表键与唯一列现有值。
    pub fn new(plan: &Plan, meta: &TableMeta, ctx: &Context, dialect: Dialect) -> AppResult<Engine> {
        if plan.rows == 0 || plan.rows > MAX_ROWS {
            return Err(AppError::new(ErrorKind::Query, t!("testdata.err.row_count", max = MAX_ROWS)));
        }
        let mut plans: HashMap<&str, &super::plan::ColumnPlan> = HashMap::new();
        for p in &plan.columns {
            if meta.column(&p.name).is_none() {
                return Err(AppError::new(ErrorKind::Query, t!("testdata.err.unknown_column", column = p.name)));
            }
            plans.insert(p.name.as_str(), p);
        }
        let mut cols = vec![];
        let mut columns = vec![];
        let mut bases = vec![];
        for (ix, m) in meta.columns.iter().enumerate() {
            let generator = plans.get(m.name.as_str()).map(|p| p.generator.clone()).unwrap_or(Generator::Skip);
            let null_ratio = plans.get(m.name.as_str()).map(|p| p.null_ratio).unwrap_or(0.0);
            if m.auto == Some(super::meta::AutoKind::Generated) && generator != Generator::Skip {
                return Err(AppError::new(ErrorKind::Query, t!("testdata.err.generated_column", column = m.name)));
            }
            let required = !m.nullable && !m.has_default && m.auto.is_none();
            match &generator {
                Generator::Skip if required => {
                    return Err(AppError::new(ErrorKind::Query, t!("testdata.err.not_null_skipped", column = m.name)))
                }
                Generator::Skip => continue,
                Generator::Null if !m.nullable => {
                    return Err(AppError::new(ErrorKind::Query, t!("testdata.err.not_null_skipped", column = m.name)))
                }
                Generator::ForeignKey if m.fk.is_none() => {
                    return Err(AppError::new(ErrorKind::Query, t!("testdata.err.generator_mismatch", column = m.name, generator = generator.kind())))
                }
                g if !g.fits(&m.base) => {
                    return Err(AppError::new(ErrorKind::Query, t!("testdata.err.generator_mismatch", column = m.name, generator = g.kind())))
                }
                _ => {}
            }
            let generator = clamp(generator, m);
            let unique = m.unique && generator != Generator::Null;
            if unique && !matches!(generator, Generator::Sequence { .. } | Generator::ForeignKey) {
                if let Some(n) = domain_size(&generator) {
                    if n < plan.rows {
                        return Err(AppError::new(
                            ErrorKind::Query,
                            t!("testdata.err.unique_domain", column = m.name, count = n, rows = plan.rows),
                        ));
                    }
                }
            }
            let max_len = match &m.base {
                BaseType::Text { max_len } => *max_len,
                _ => None,
            };
            let seen = unique.then(|| {
                ctx.existing
                    .get(&m.name)
                    .map(|s| s.iter().map(|v| if dialect == Dialect::Mysql { v.trim_end().to_lowercase() } else { v.clone() }).collect())
                    .unwrap_or_default()
            });
            columns.push(m.name.clone());
            bases.push(m.base.clone());
            cols.push(Col {
                meta_ix: ix,
                null_ratio: if m.nullable && !unique { null_ratio.clamp(0.0, 1.0) } else { 0.0 },
                rng: Rng::derive(plan.seed, &m.name),
                generator,
                max_len,
                seen,
                fk: None,
            });
        }
        if cols.is_empty() {
            return Err(err("testdata.err.no_columns"));
        }

        // 外键:每个约束一组,一行里各列取父表同一行的键
        let mut fks = vec![];
        for g in &meta.fks {
            let targets: Vec<Option<usize>> = g
                .columns
                .iter()
                .map(|c| cols.iter().position(|x| meta.columns[x.meta_ix].name == *c && x.generator == Generator::ForeignKey))
                .collect();
            if targets.iter().all(Option::is_none) {
                continue;
            }
            let keys: Vec<Vec<Option<String>>> = ctx
                .fk_keys
                .get(&g.constraint)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|k| k.iter().all(Option::is_some))
                .collect();
            let nullable = g.columns.iter().all(|c| meta.column(c).is_some_and(|m| m.nullable));
            if keys.is_empty() && !nullable {
                let column = g.columns.join(", ");
                return Err(AppError::new(ErrorKind::Query, t!("testdata.err.parent_empty", column = column, parent = g.ref_table)));
            }
            let unique = g.columns.len() == 1 && meta.column(&g.columns[0]).is_some_and(|m| m.unique)
                || meta.uniques.iter().any(|u| u.len() == g.columns.len() && u.iter().all(|c| g.columns.contains(c)));
            if unique && !keys.is_empty() && (keys.len() as u64) < plan.rows {
                return Err(AppError::new(
                    ErrorKind::Query,
                    t!("testdata.err.unique_domain", column = g.columns.join(", "), count = keys.len(), rows = plan.rows),
                ));
            }
            let fi = fks.len();
            for t in targets.iter().flatten() {
                cols[*t].fk = Some(fi);
                // 唯一性由取键时的「不重复使用」保证
                cols[*t].seen = None;
            }
            fks.push(FkPick {
                all_null: keys.is_empty(),
                keys,
                targets,
                rng: Rng::derive(plan.seed, &format!("fk:{}", g.constraint)),
                unique,
                used: HashSet::new(),
            });
        }

        let uniques = meta
            .uniques
            .iter()
            .filter_map(|u| {
                let ix: Option<Vec<usize>> = u.iter().map(|c| columns.iter().position(|x| x == c)).collect();
                // 组合里有不写的列(自增 / 默认值):各行取值不定,不检查
                let ix = ix?;
                // 整组都是外键(已按键去重)不再检查
                if ix.iter().all(|i| cols[*i].fk.is_some()) && fks.iter().any(|f| f.unique) {
                    return None;
                }
                Some((ix, HashSet::new()))
            })
            .collect();

        // 成对的时间列:created / updated、start / end、valid_from / valid_to……
        let mut pairs = vec![];
        for i in 0..cols.len() {
            let Some((fam, true, stem)) = time_role(&columns[i]) else { continue };
            for j in 0..cols.len() {
                let same_kind = std::mem::discriminant(&cols[i].generator) == std::mem::discriminant(&cols[j].generator);
                if i == j || !same_kind || !matches!(cols[i].generator, Generator::Date { .. } | Generator::Timestamp { .. }) {
                    continue;
                }
                if time_role(&columns[j]) == Some((fam, false, stem.clone())) {
                    pairs.push((i, j));
                }
            }
        }

        Ok(Engine { dialect, locale: plan.locale, columns, bases, cols, fks, uniques, pairs, row: 0, rows: plan.rows })
    }

    /// 实际写入的列(表的列序,跳过的不在内)。
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    fn one(&mut self, i: usize, attempt: u32) -> Value {
        let base = &self.bases[i];
        let c = &mut self.cols[i];
        if c.null_ratio > 0.0 && c.rng.chance(c.null_ratio) {
            return Value::Null;
        }
        let ctx = Ctx { locale: self.locale, base, row: self.row, attempt };
        let v = generate(&c.generator, &mut c.rng, &ctx);
        coerce(v, base, c.max_len)
    }

    fn unique_one(&mut self, i: usize) -> AppResult<Value> {
        for attempt in 0..UNIQUE_RETRIES {
            let v = self.one(i, attempt);
            let Some(seen) = self.cols[i].seen.as_mut() else { return Ok(v) };
            match unique_key(&v, self.dialect) {
                None => return Ok(v),
                Some(k) => {
                    if seen.insert(k) {
                        return Ok(v);
                    }
                }
            }
        }
        Err(AppError::new(
            ErrorKind::Query,
            t!("testdata.err.unique_exhausted", column = self.columns[i], row = self.row + 1),
        ))
    }

    fn pick_fk(&mut self, fi: usize, row: &mut [Value]) -> AppResult<()> {
        let f = &mut self.fks[fi];
        if f.all_null {
            for t in f.targets.iter().flatten() {
                row[*t] = Value::Null;
            }
            return Ok(());
        }
        let n = f.keys.len() as u64;
        let mut k = f.rng.below(n) as usize;
        if f.unique {
            // 一对一:按随机起点往后找没用过的键(键数已确认不少于行数)
            let mut tries = 0;
            while f.used.contains(&k) {
                k = (k + 1) % n as usize;
                tries += 1;
                if tries > n {
                    let col = self.columns[f.targets.iter().flatten().next().copied().unwrap_or(0)].clone();
                    return Err(AppError::new(ErrorKind::Query, t!("testdata.err.unique_exhausted", column = col, row = self.row + 1)));
                }
            }
            f.used.insert(k);
        }
        let key = f.keys[k].clone();
        for (t, v) in f.targets.iter().zip(key) {
            if let Some(t) = t {
                let v = v.map(Value::Text).unwrap_or(Value::Null);
                row[*t] = coerce(v, &self.bases[*t], None);
            }
        }
        Ok(())
    }

    /// 生成下一行(与 [`Self::columns`] 对齐)。
    pub fn next_row(&mut self) -> AppResult<Vec<Value>> {
        let n = self.cols.len();
        let mut row = vec![Value::Null; n];
        for fi in 0..self.fks.len() {
            self.pick_fk(fi, &mut row)?;
        }
        for (i, cell) in row.iter_mut().enumerate() {
            if self.cols[i].fk.is_none() {
                *cell = self.unique_one(i)?;
            }
        }
        for &(a, b) in &self.pairs {
            if let (Some(x), Some(y)) = (row[a].display(), row[b].display()) {
                if y < x {
                    row.swap(a, b);
                }
            }
        }
        // 多列唯一:组合撞了就重抽组内的非外键列
        for u in 0..self.uniques.len() {
            let ix = self.uniques[u].0.clone();
            let mut attempt = 0;
            loop {
                let key: Vec<String> = ix.iter().map(|i| unique_key(&row[*i], self.dialect).unwrap_or_else(|| "\u{0}NULL".into())).collect();
                // 任何一列为 NULL 时数据库不认为重复
                if ix.iter().any(|i| row[*i] == Value::Null) || self.uniques[u].1.insert(key.join("\u{1f}")) {
                    break;
                }
                attempt += 1;
                let free: Vec<usize> = ix.iter().copied().filter(|i| self.cols[*i].fk.is_none()).collect();
                if attempt > UNIQUE_RETRIES || free.is_empty() {
                    return Err(AppError::new(
                        ErrorKind::Query,
                        t!("testdata.err.unique_exhausted", column = ix.iter().map(|i| self.columns[*i].as_str()).collect::<Vec<_>>().join(", "), row = self.row + 1),
                    ));
                }
                for i in free {
                    row[i] = self.one(i, attempt);
                }
            }
        }
        self.row += 1;
        Ok(row)
    }

    /// 还剩多少行。
    pub fn remaining(&self) -> u64 {
        self.rows - self.row
    }
}

// ---------------------------------------------------------------- SQL

/// 值写成方言的字面量。
pub fn literal(v: &Value, d: Dialect) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::Int(i) => i.to_string(),
        Value::Num(s) => s.clone(),
        Value::Float(_) => v.display().unwrap_or_else(|| "0".into()),
        Value::Bool(b) => match d {
            Dialect::Pg => if *b { "TRUE" } else { "FALSE" }.into(),
            _ => if *b { "1" } else { "0" }.into(),
        },
        Value::Text(s) => crate::sql::build::literal(s, d),
        Value::Bytes(b) => match d {
            Dialect::Pg => format!("decode('{}', 'hex')", super::gen::hex(b)),
            _ => format!("X'{}'", super::gen::hex(b)),
        },
    }
}

/// 一条多行 INSERT。
pub fn insert_sql(table: &str, columns: &[String], rows: &[Vec<Value>], d: Dialect) -> String {
    let q = |s: &str| crate::sql::build::ident(s, d);
    let cols: Vec<String> = columns.iter().map(|c| q(c)).collect();
    let values: Vec<String> = rows
        .iter()
        .map(|r| format!("({})", r.iter().map(|v| literal(v, d)).collect::<Vec<_>>().join(", ")))
        .collect();
    format!("INSERT INTO {} ({}) VALUES\n{};", q(table), cols.join(", "), values.join(",\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdata::gen::Locale;
    use crate::testdata::meta::{AutoKind, ColumnMeta, FkGroup};
    use crate::testdata::plan::ColumnPlan;

    fn col(name: &str, base: BaseType, nullable: bool) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            data_type: String::new(),
            base,
            nullable,
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

    fn plan(rows: u64, seed: u64, cols: Vec<(&str, Generator, f64)>) -> Plan {
        Plan {
            table: "t".into(),
            rows,
            seed,
            locale: Locale::En,
            columns: cols.into_iter().map(|(n, g, r)| ColumnPlan { name: n.into(), generator: g, null_ratio: r }).collect(),
        }
    }

    fn all(e: &mut Engine) -> Vec<Vec<Value>> {
        let mut out = vec![];
        while e.remaining() > 0 {
            out.push(e.next_row().unwrap());
        }
        out
    }

    fn meta(cols: Vec<ColumnMeta>) -> TableMeta {
        TableMeta { table: "t".into(), columns: cols, uniques: vec![], fks: vec![] }
    }

    #[test]
    fn deterministic_with_seed() {
        let m = meta(vec![col("name", BaseType::Text { max_len: None }, true), col("n", BaseType::Int { min: 0, max: 100 }, false)]);
        let p = |seed| plan(50, seed, vec![("name", Generator::FullName, 0.2), ("n", Generator::Integer { min: 0, max: 100 }, 0.0)]);
        let a = all(&mut Engine::new(&p(7), &m, &Context::default(), Dialect::Pg).unwrap());
        let b = all(&mut Engine::new(&p(7), &m, &Context::default(), Dialect::Pg).unwrap());
        let c = all(&mut Engine::new(&p(8), &m, &Context::default(), Dialect::Pg).unwrap());
        assert_eq!(a, b);
        assert_ne!(a, c);
        // 有空值,但不可空列没有
        assert!(a.iter().any(|r| r[0] == Value::Null));
        assert!(a.iter().all(|r| r[1] != Value::Null));
    }

    #[test]
    fn respects_type_limits() {
        let m = meta(vec![
            col("code", BaseType::Text { max_len: Some(4) }, false),
            col("tiny", BaseType::Int { min: -128, max: 127 }, false),
            col("price", BaseType::Decimal { precision: Some(5), scale: 2 }, false),
            col("flag", BaseType::Bool, false),
            col("raw", BaseType::Bytes { max_len: Some(8) }, false),
        ]);
        let p = plan(
            300,
            1,
            vec![
                ("code", Generator::Paragraph, 0.0),
                ("tiny", Generator::Integer { min: -1000, max: 1000 }, 0.0),
                ("price", Generator::Decimal { min: 0.0, max: 1e9, scale: 6 }, 0.0),
                ("flag", Generator::Boolean, 0.0),
                ("raw", Generator::Bytes { len: 64 }, 0.0),
            ],
        );
        for r in all(&mut Engine::new(&p, &m, &Context::default(), Dialect::Mysql).unwrap()) {
            let Value::Text(s) = &r[0] else { panic!() };
            assert!(s.chars().count() <= 4);
            let Value::Int(i) = r[1] else { panic!() };
            assert!((-128..=127).contains(&i));
            let Value::Num(s) = &r[2] else { panic!() };
            let f: f64 = s.parse().unwrap();
            assert!(f <= 999.99 && s.split('.').nth(1).unwrap().len() == 2, "{s}");
            assert!(matches!(r[3], Value::Bool(_)));
            let Value::Bytes(b) = &r[4] else { panic!() };
            assert_eq!(b.len(), 8);
        }
    }

    #[test]
    fn unique_dedupes_and_fails_on_small_domain() {
        let mut c = col("email", BaseType::Text { max_len: None }, false);
        c.unique = true;
        let mut city = col("city", BaseType::Text { max_len: None }, false);
        city.unique = true;
        let m = meta(vec![c, city]);
        let mut ctx = Context::default();
        let p = plan(2000, 3, vec![("email", Generator::Email, 0.0), ("city", Generator::City, 0.0)]);
        let first = Engine::new(&p, &m, &ctx, Dialect::Pg).unwrap().next_row().unwrap();
        // 与表里已有的值也不重复
        ctx.existing.insert("email".into(), [first[0].display().unwrap()].into_iter().collect());
        let rows = all(&mut Engine::new(&p, &m, &ctx, Dialect::Pg).unwrap());
        let emails: HashSet<String> = rows.iter().map(|r| r[0].display().unwrap()).collect();
        let cities: HashSet<String> = rows.iter().map(|r| r[1].display().unwrap()).collect();
        assert_eq!(emails.len(), 2000);
        assert_eq!(cities.len(), 2000, "城市词表不够时加后缀凑出不同的值");
        assert!(!emails.contains(&first[0].display().unwrap()));

        // 取值空间比行数小:一开始就报错
        let mut n = col("n", BaseType::Int { min: 0, max: 100 }, false);
        n.unique = true;
        let m = meta(vec![n]);
        let p = plan(20, 1, vec![("n", Generator::Integer { min: 1, max: 10 }, 0.0)]);
        let e = Engine::new(&p, &m, &Context::default(), Dialect::Pg).err().unwrap();
        assert!(e.display().contains('n'), "{}", e.display());
        let p = plan(20, 1, vec![("n", Generator::PickList { values: vec!["1".into(), "2".into()] }, 0.0)]);
        assert!(Engine::new(&p, &m, &Context::default(), Dialect::Pg).is_err());
    }

    #[test]
    fn fk_samples_parent_keys_including_composite() {
        let mut a = col("user_id", BaseType::Int { min: 0, max: i64::MAX }, false);
        a.fk = Some("fk_user".into());
        let mut x = col("org", BaseType::Text { max_len: None }, true);
        x.fk = Some("fk_pair".into());
        let mut y = col("dept", BaseType::Int { min: 0, max: 100 }, true);
        y.fk = Some("fk_pair".into());
        let mut m = meta(vec![a, x, y]);
        m.fks = vec![
            FkGroup { constraint: "fk_user".into(), columns: vec!["user_id".into()], ref_table: "users".into(), ref_columns: vec!["id".into()] },
            FkGroup {
                constraint: "fk_pair".into(),
                columns: vec!["org".into(), "dept".into()],
                ref_table: "depts".into(),
                ref_columns: vec!["org".into(), "id".into()],
            },
        ];
        let mut ctx = Context::default();
        ctx.fk_keys.insert("fk_user".into(), vec![vec![Some("3".into())], vec![Some("9".into())]]);
        ctx.fk_keys.insert("fk_pair".into(), vec![vec![Some("acme".into()), Some("1".into())], vec![Some("zen".into()), Some("2".into())]]);
        let p = plan(100, 5, vec![("user_id", Generator::ForeignKey, 0.0), ("org", Generator::ForeignKey, 0.0), ("dept", Generator::ForeignKey, 0.0)]);
        let rows = all(&mut Engine::new(&p, &m, &ctx, Dialect::Sqlite).unwrap());
        for r in &rows {
            assert!(r[0] == Value::Int(3) || r[0] == Value::Int(9));
            let pair = (r[1].display().unwrap(), r[2].clone());
            assert!(pair == ("acme".into(), Value::Int(1)) || pair == ("zen".into(), Value::Int(2)), "{pair:?}");
        }
        assert!(rows.iter().any(|r| r[0] == Value::Int(3)) && rows.iter().any(|r| r[0] == Value::Int(9)));

        // 父表空:不可空外键报错,可空外键写 NULL
        ctx.fk_keys.insert("fk_user".into(), vec![]);
        let e = Engine::new(&p, &m, &ctx, Dialect::Sqlite).err().unwrap();
        assert!(e.display().contains("users"), "{}", e.display());
        ctx.fk_keys.insert("fk_user".into(), vec![vec![Some("3".into())]]);
        ctx.fk_keys.insert("fk_pair".into(), vec![]);
        let rows = all(&mut Engine::new(&p, &m, &ctx, Dialect::Sqlite).unwrap());
        assert!(rows.iter().all(|r| r[1] == Value::Null && r[2] == Value::Null));
    }

    #[test]
    fn created_before_updated_and_composite_unique() {
        let m = meta(vec![
            col("created_at", BaseType::Timestamp, false),
            col("updated_at", BaseType::Timestamp, false),
            col("start_date", BaseType::Date, false),
            col("end_date", BaseType::Date, false),
        ]);
        let ts = || Generator::Timestamp { from: "2024-01-01".into(), to: "2024-12-31".into() };
        let d = || Generator::Date { from: "2024-01-01".into(), to: "2024-12-31".into() };
        let p = plan(500, 11, vec![("created_at", ts(), 0.0), ("updated_at", ts(), 0.0), ("start_date", d(), 0.0), ("end_date", d(), 0.0)]);
        for r in all(&mut Engine::new(&p, &m, &Context::default(), Dialect::Pg).unwrap()) {
            assert!(r[0].display() <= r[1].display());
            assert!(r[2].display() <= r[3].display());
        }

        let mut m = meta(vec![col("a", BaseType::Int { min: 0, max: 9 }, false), col("b", BaseType::Int { min: 0, max: 9 }, false)]);
        m.uniques = vec![vec!["a".into(), "b".into()]];
        let p = plan(60, 2, vec![("a", Generator::Integer { min: 0, max: 9 }, 0.0), ("b", Generator::Integer { min: 0, max: 9 }, 0.0)]);
        let rows = all(&mut Engine::new(&p, &m, &Context::default(), Dialect::Pg).unwrap());
        let pairs: HashSet<(String, String)> = rows.iter().map(|r| (r[0].display().unwrap(), r[1].display().unwrap())).collect();
        assert_eq!(pairs.len(), 60);
    }

    #[test]
    fn validates_plan() {
        let mut id = col("id", BaseType::Int { min: 0, max: 10 }, false);
        id.auto = Some(AutoKind::Generated);
        let m = meta(vec![id, col("title", BaseType::Text { max_len: None }, false), col("n", BaseType::Int { min: 0, max: 9 }, true)]);
        let ok = vec![("title", Generator::Title, 0.0)];
        assert!(Engine::new(&plan(1, 1, ok.clone()), &m, &Context::default(), Dialect::Pg).is_ok());
        // 不可空、无默认值的列不能跳过
        assert!(Engine::new(&plan(1, 1, vec![("n", Generator::Integer { min: 0, max: 1 }, 0.0)]), &m, &Context::default(), Dialect::Pg).is_err());
        // 生成列不能写
        let mut bad = ok.clone();
        bad.push(("id", Generator::Integer { min: 0, max: 1 }, 0.0));
        assert!(Engine::new(&plan(1, 1, bad), &m, &Context::default(), Dialect::Pg).is_err());
        // 类型不合的生成器
        assert!(Engine::new(&plan(1, 1, vec![("title", Generator::Title, 0.0), ("n", Generator::Email, 0.0)]), &m, &Context::default(), Dialect::Pg).is_err());
        assert!(Engine::new(&plan(0, 1, ok.clone()), &m, &Context::default(), Dialect::Pg).is_err());
        assert!(Engine::new(&plan(MAX_ROWS + 1, 1, ok), &m, &Context::default(), Dialect::Pg).is_err());
    }

    #[test]
    fn renders_insert_sql_per_dialect() {
        let rows = vec![vec![Value::Int(1), Value::Text("O'Neil \\".into()), Value::Bool(true), Value::Null, Value::Bytes(vec![0xde, 0xad])]];
        let cols: Vec<String> = ["id", "name", "ok", "x", "b"].iter().map(|s| s.to_string()).collect();
        let pg = insert_sql("t", &cols, &rows, Dialect::Pg);
        assert!(pg.contains("(1, 'O''Neil \\', TRUE, NULL, decode('dead', 'hex'))"), "{pg}");
        let my = insert_sql("t", &cols, &rows, Dialect::Mysql);
        assert!(my.starts_with("INSERT INTO `t` (`id`"), "{my}");
        assert!(my.contains("'O''Neil \\\\', 1, NULL, X'dead'"), "{my}");
    }
}
