//! mongosh 语法子集解析(纯函数,不连网):宽松 JSON → BSON,`db.<coll>.<method>(…)` → [`MongoOp`]。
//!
//! 只接受白名单里的语法,不认识的一律报错并附行列。这样读写判断与风险审查
//! 可以完全基于解析结果,不存在"分析器漏看、服务端照跑"的写法。

use base64::Engine as _;
use mongodb::bson::{
    self, oid::ObjectId, spec::BinarySubtype, Binary, Bson, Decimal128, Document, Regex, Timestamp,
};
use rust_i18n::t;

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub line: usize,
    pub col: usize,
    pub message: String,
}

impl ParseError {
    /// 一行可读文案:`第 3 行第 6 列:…`。
    pub fn display(&self) -> String {
        t!(
            "core.mongo.parse.at",
            line = self.line,
            col = self.col,
            message = self.message
        )
        .to_string()
    }
}

/// 字节偏移 → 1 起的行列(列按字符计)。
fn err_at(src: &str, pos: usize, message: impl Into<String>) -> ParseError {
    let before = &src[..pos.min(src.len())];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    ParseError {
        line,
        col,
        message: message.into(),
    }
}

pub(crate) struct P<'a> {
    pub(crate) src: &'a str,
    pub(crate) pos: usize,
}

impl<'a> P<'a> {
    pub(crate) fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }
    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }
    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }
    pub(crate) fn err(&self, msg: impl Into<String>) -> ParseError {
        err_at(self.src, self.pos, msg)
    }
    pub(crate) fn err_at(&self, pos: usize, msg: impl Into<String>) -> ParseError {
        err_at(self.src, pos, msg)
    }
    pub(crate) fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    /// 跳过空白与 `//`、`/* */` 注释。
    pub(crate) fn ws(&mut self) {
        loop {
            let r = self.rest();
            let t = r.trim_start();
            self.pos += r.len() - t.len();
            if t.starts_with("//") {
                self.pos += t.find('\n').unwrap_or(t.len());
            } else if let Some(body) = t.strip_prefix("/*") {
                self.pos += body.find("*/").map_or(t.len(), |i| i + 4);
            } else {
                break;
            }
        }
    }

    /// 匹配就前进;不匹配时位置不变(连空白也不吞,供语句分隔判断用)。
    pub(crate) fn eat(&mut self, s: &str) -> bool {
        let save = self.pos;
        self.ws();
        if self.rest().starts_with(s) {
            self.pos += s.len();
            true
        } else {
            self.pos = save;
            false
        }
    }

    pub(crate) fn expect(&mut self, s: &str) -> Result<(), ParseError> {
        if self.eat(s) {
            Ok(())
        } else {
            self.ws();
            Err(self.err(t!("core.mongo.parse.expected", what = s)))
        }
    }

    /// 标识符:字母 / 数字 / `_` / `$`,不能以数字开头。
    pub(crate) fn ident(&mut self) -> Option<&'a str> {
        let save = self.pos;
        self.ws();
        let r = self.rest();
        let end = r
            .char_indices()
            .find(|(_, c)| !(c.is_alphanumeric() || *c == '_' || *c == '$'))
            .map_or(r.len(), |(i, _)| i);
        if end == 0 || r.starts_with(|c: char| c.is_ascii_digit()) {
            self.pos = save;
            return None;
        }
        self.pos += end;
        Some(&r[..end])
    }

    pub(crate) fn value(&mut self) -> Result<Bson, ParseError> {
        self.ws();
        match self.peek() {
            Some('{') => self.object().map(Bson::Document),
            Some('[') => self.array().map(Bson::Array),
            Some('"') | Some('\'') => self.string().map(Bson::String),
            Some('/') => self.regex(),
            Some(c) if c == '-' || c == '+' || c == '.' || c.is_ascii_digit() => self.number(),
            Some(_) => self.word_value(),
            None => Err(self.err(t!("core.mongo.parse.unexpected_end"))),
        }
    }

    fn object(&mut self) -> Result<Document, ParseError> {
        self.expect("{")?;
        let mut doc = Document::new();
        loop {
            if self.eat("}") {
                return Ok(doc);
            }
            let key = self.key()?;
            self.expect(":")?;
            let v = self.value()?;
            doc.insert(key, v);
            if !self.eat(",") {
                self.expect("}")?;
                return Ok(doc);
            }
        }
    }

    fn key(&mut self) -> Result<String, ParseError> {
        self.ws();
        match self.peek() {
            Some('"') | Some('\'') => self.string(),
            _ => self
                .ident()
                .map(str::to_string)
                .ok_or_else(|| self.err(t!("core.mongo.parse.expected_key"))),
        }
    }

    pub(crate) fn array(&mut self) -> Result<Vec<Bson>, ParseError> {
        self.expect("[")?;
        let mut out = vec![];
        loop {
            if self.eat("]") {
                return Ok(out);
            }
            out.push(self.value()?);
            if !self.eat(",") {
                self.expect("]")?;
                return Ok(out);
            }
        }
    }

    pub(crate) fn string(&mut self) -> Result<String, ParseError> {
        self.ws();
        let start = self.pos;
        let Some(q) = self.bump().filter(|c| *c == '"' || *c == '\'') else {
            self.pos = start;
            return Err(self.err_at(start, t!("core.mongo.parse.expected_string")));
        };
        let mut s = String::new();
        loop {
            match self.bump() {
                None => {
                    return Err(self.err_at(start, t!("core.mongo.parse.unterminated_string")));
                }
                Some(c) if c == q => return Ok(s),
                Some('\\') => {
                    let esc_pos = self.pos - 1;
                    match self.bump() {
                        Some('n') => s.push('\n'),
                        Some('t') => s.push('\t'),
                        Some('r') => s.push('\r'),
                        Some('b') => s.push('\u{8}'),
                        Some('f') => s.push('\u{c}'),
                        Some('0') => s.push('\0'),
                        Some('u') => s.push(self.unicode_escape(esc_pos)?),
                        Some(c) => s.push(c),
                        None => {
                            return Err(
                                self.err_at(start, t!("core.mongo.parse.unterminated_string"))
                            );
                        }
                    }
                }
                Some(c) => s.push(c),
            }
        }
    }

    /// `\uXXXX`(已吞掉 `\u`),处理代理对。
    fn unicode_escape(&mut self, esc_pos: usize) -> Result<char, ParseError> {
        let bad = |p: &Self| p.err_at(esc_pos, t!("core.mongo.parse.bad_escape"));
        let hex4 = |p: &mut Self| -> Option<u32> {
            let h = p.rest().get(..4)?;
            let n = u32::from_str_radix(h, 16).ok()?;
            p.pos += 4;
            Some(n)
        };
        let hi = hex4(self).ok_or_else(|| bad(self))?;
        if (0xD800..0xDC00).contains(&hi) {
            if !self.rest().starts_with("\\u") {
                return Err(bad(self));
            }
            self.pos += 2;
            let lo = hex4(self).ok_or_else(|| bad(self))?;
            if !(0xDC00..0xE000).contains(&lo) {
                return Err(bad(self));
            }
            let c = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
            return char::from_u32(c).ok_or_else(|| bad(self));
        }
        char::from_u32(hi).ok_or_else(|| bad(self))
    }

    /// 数字 token 的原文(含符号、小数点、指数,以及 Infinity)。
    fn number_text(&mut self) -> &'a str {
        self.ws();
        let r = self.rest();
        let b = r.as_bytes();
        let end = r
            .char_indices()
            .find(|&(i, c)| {
                let sign_ok = (c == '-' || c == '+') && (i == 0 || matches!(b[i - 1], b'e' | b'E'));
                !(c.is_ascii_alphanumeric() || c == '.' || sign_ok)
            })
            .map_or(r.len(), |(i, _)| i);
        self.pos += end;
        &r[..end]
    }

    fn number(&mut self) -> Result<Bson, ParseError> {
        self.ws();
        let start = self.pos;
        let text = self.number_text();
        parse_number(text)
            .ok_or_else(|| self.err_at(start, t!("core.mongo.parse.bad_number", text = text)))
    }

    fn regex(&mut self) -> Result<Bson, ParseError> {
        let start = self.pos;
        self.bump(); // '/'
        let mut pattern = String::new();
        let mut in_class = false;
        loop {
            match self.bump() {
                None | Some('\n') => {
                    return Err(self.err_at(start, t!("core.mongo.parse.unterminated_regex")));
                }
                Some('\\') => {
                    pattern.push('\\');
                    if let Some(c) = self.bump() {
                        pattern.push(c);
                    }
                }
                Some('[') => {
                    in_class = true;
                    pattern.push('[');
                }
                Some(']') => {
                    in_class = false;
                    pattern.push(']');
                }
                Some('/') if !in_class => break,
                Some(c) => pattern.push(c),
            }
        }
        let r = self.rest();
        let end = r
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(r.len());
        let options = sorted_flags(&r[..end]);
        self.pos += end;
        Ok(Bson::RegularExpression(Regex { pattern, options }))
    }

    /// 字符串参数或裸数字参数的原文(`NumberLong(5)` 与 `NumberLong("5")` 都可以)。
    fn num_or_str_arg(&mut self) -> Result<String, ParseError> {
        self.ws();
        match self.peek() {
            Some('"') | Some('\'') => self.string(),
            _ => Ok(self.number_text().to_string()),
        }
    }

    fn int_arg(&mut self) -> Result<i64, ParseError> {
        self.ws();
        let pos = self.pos;
        let s = self.num_or_str_arg()?;
        s.trim()
            .parse::<i64>()
            .map_err(|_| self.err_at(pos, t!("core.mongo.parse.bad_number", text = s)))
    }

    fn word_value(&mut self) -> Result<Bson, ParseError> {
        self.ws();
        let start = self.pos;
        let Some(word) = self.ident() else {
            let ch = self.peek().unwrap_or(' ');
            return Err(self.err(t!("core.mongo.parse.unexpected", ch = ch)));
        };
        match word {
            "true" => return Ok(Bson::Boolean(true)),
            "false" => return Ok(Bson::Boolean(false)),
            "null" => return Ok(Bson::Null),
            "NaN" => return Ok(Bson::Double(f64::NAN)),
            "Infinity" => return Ok(Bson::Double(f64::INFINITY)),
            "new" => {
                if self.ident() != Some("Date") {
                    return Err(
                        self.err_at(start, t!("core.mongo.parse.unsupported_ctor", name = "new"))
                    );
                }
                return self.date_ctor();
            }
            "ISODate" => return self.date_ctor(),
            _ => {}
        }
        self.expect("(")?;
        self.ws();
        let arg_pos = self.pos;
        let bad = |p: &Self| p.err_at(arg_pos, t!("core.mongo.parse.bad_ctor_arg", name = word));
        let v = match word {
            "ObjectId" => {
                let s = self.string()?;
                Bson::ObjectId(ObjectId::parse_str(&s).map_err(|_| bad(self))?)
            }
            "NumberInt" | "Int32" => {
                Bson::Int32(i32::try_from(self.int_arg()?).map_err(|_| bad(self))?)
            }
            "NumberLong" | "Long" => Bson::Int64(self.int_arg()?),
            "Double" => {
                let s = self.num_or_str_arg()?;
                Bson::Double(s.trim().parse::<f64>().map_err(|_| bad(self))?)
            }
            "NumberDecimal" | "Decimal128" => {
                let s = self.num_or_str_arg()?;
                Bson::Decimal128(s.trim().parse::<Decimal128>().map_err(|_| bad(self))?)
            }
            "UUID" => {
                let s = self.string()?;
                let u = bson::Uuid::parse_str(&s).map_err(|_| bad(self))?;
                Bson::Binary(Binary {
                    subtype: BinarySubtype::Uuid,
                    bytes: u.bytes().to_vec(),
                })
            }
            "BinData" => {
                let sub = u8::try_from(self.int_arg()?).map_err(|_| bad(self))?;
                self.expect(",")?;
                let b64 = self.string()?;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(b64.as_bytes())
                    .map_err(|_| bad(self))?;
                Bson::Binary(Binary {
                    subtype: BinarySubtype::from(sub),
                    bytes,
                })
            }
            "Timestamp" => {
                let time = u32::try_from(self.int_arg()?).map_err(|_| bad(self))?;
                self.expect(",")?;
                let increment = u32::try_from(self.int_arg()?).map_err(|_| bad(self))?;
                Bson::Timestamp(Timestamp { time, increment })
            }
            "RegExp" => {
                let pattern = self.string()?;
                let options = if self.eat(",") {
                    sorted_flags(&self.string()?)
                } else {
                    String::new()
                };
                Bson::RegularExpression(Regex { pattern, options })
            }
            "MinKey" => Bson::MinKey,
            "MaxKey" => Bson::MaxKey,
            _ => {
                return Err(
                    self.err_at(start, t!("core.mongo.parse.unsupported_ctor", name = word))
                );
            }
        };
        self.expect(")")?;
        Ok(v)
    }

    /// `ISODate(...)` / `new Date(...)`:无参为当前时间,字符串按 ISO-8601,数字为毫秒。
    fn date_ctor(&mut self) -> Result<Bson, ParseError> {
        self.expect("(")?;
        if self.eat(")") {
            return Ok(Bson::DateTime(bson::DateTime::now()));
        }
        self.ws();
        let pos = self.pos;
        let ms = match self.peek() {
            Some('"') | Some('\'') => {
                let s = self.string()?;
                parse_date_millis(&s)
                    .ok_or_else(|| self.err_at(pos, t!("core.mongo.parse.bad_date", text = s)))?
            }
            _ => self.int_arg()?,
        };
        self.expect(")")?;
        Ok(Bson::DateTime(bson::DateTime::from_millis(ms)))
    }
}

/// 正则 flags 排序后输出,和服务端的规范化保持一致。
fn sorted_flags(s: &str) -> String {
    let mut f: Vec<char> = s.chars().collect();
    f.sort_unstable();
    f.into_iter().collect()
}

/// 整数在 i32 范围内为 Int32、超出为 Int64;带小数点或指数为 Double。
/// 注意:mongosh 会把所有裸数字当 Double,这里保留整数类型,写入时更不容易意外变成浮点。
fn parse_number(text: &str) -> Option<Bson> {
    match text {
        "Infinity" | "+Infinity" => return Some(Bson::Double(f64::INFINITY)),
        "-Infinity" => return Some(Bson::Double(f64::NEG_INFINITY)),
        _ => {}
    }
    if !text.contains(['.', 'e', 'E']) {
        if let Ok(n) = text.parse::<i64>() {
            return Some(i32::try_from(n).map_or(Bson::Int64(n), Bson::Int32));
        }
    }
    text.parse::<f64>()
        .ok()
        .filter(|f| f.is_finite())
        .map(Bson::Double)
}

fn parse_date_millis(s: &str) -> Option<i64> {
    use chrono::{DateTime, NaiveDate, NaiveDateTime};
    let s = s.trim();
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(d.timestamp_millis());
    }
    if let Ok(d) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f") {
        return Some(d.and_utc().timestamp_millis());
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|d| d.and_utc().timestamp_millis())
}

/// 解析一个完整的值;后面不能再有多余内容。
pub fn parse_value(src: &str) -> Result<Bson, ParseError> {
    let mut p = P::new(src);
    let v = p.value()?;
    p.ws();
    if !p.at_end() {
        return Err(p.err(t!("core.mongo.parse.trailing")));
    }
    Ok(v)
}

/// 解析顶层为对象的文本(过滤栏、文档编辑器)。空串或纯空白视为 `{}`。
pub fn parse_document(src: &str) -> Result<Document, ParseError> {
    if src.trim().is_empty() {
        return Ok(Document::new());
    }
    match parse_value(src)? {
        Bson::Document(d) => Ok(d),
        _ => Err(err_at(src, 0, t!("core.mongo.parse.expected_object"))),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    Doc(Document),
    Pipeline(Vec<Document>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum MongoOp {
    ShowDbs,
    ShowCollections,
    Find {
        coll: String,
        filter: Document,
        projection: Option<Document>,
        sort: Option<Document>,
        skip: Option<u64>,
        limit: Option<i64>,
    },
    FindOne {
        coll: String,
        filter: Document,
        projection: Option<Document>,
    },
    Count {
        coll: String,
        filter: Document,
    },
    Distinct {
        coll: String,
        field: String,
        filter: Document,
    },
    Aggregate {
        coll: String,
        pipeline: Vec<Document>,
    },
    GetIndexes {
        coll: String,
    },
    InsertOne {
        coll: String,
        doc: Document,
    },
    InsertMany {
        coll: String,
        docs: Vec<Document>,
    },
    UpdateOne {
        coll: String,
        filter: Document,
        update: Update,
    },
    UpdateMany {
        coll: String,
        filter: Document,
        update: Update,
    },
    ReplaceOne {
        coll: String,
        filter: Document,
        doc: Document,
    },
    DeleteOne {
        coll: String,
        filter: Document,
    },
    DeleteMany {
        coll: String,
        filter: Document,
    },
}

/// 聚合管道里的写入阶段(`$out` 优先,其次 `$merge`),任意位置都算;没有时为 None。
/// 读写判断、风险审查与执行都用它,保证三处口径一致。
pub fn pipeline_write_stage(pipeline: &[Document]) -> Option<&'static str> {
    ["$out", "$merge"]
        .into_iter()
        .find(|stage| pipeline.iter().any(|s| s.contains_key(*stage)))
}

impl MongoOp {
    /// 写操作:7 个写方法,以及管道中任意位置含 `$out` / `$merge` 的 aggregate。
    pub fn is_write(&self) -> bool {
        match self {
            MongoOp::InsertOne { .. }
            | MongoOp::InsertMany { .. }
            | MongoOp::UpdateOne { .. }
            | MongoOp::UpdateMany { .. }
            | MongoOp::ReplaceOne { .. }
            | MongoOp::DeleteOne { .. }
            | MongoOp::DeleteMany { .. } => true,
            MongoOp::Aggregate { pipeline, .. } => pipeline_write_stage(pipeline).is_some(),
            _ => false,
        }
    }

    pub fn collection(&self) -> Option<&str> {
        match self {
            MongoOp::ShowDbs | MongoOp::ShowCollections => None,
            MongoOp::Find { coll, .. }
            | MongoOp::FindOne { coll, .. }
            | MongoOp::Count { coll, .. }
            | MongoOp::Distinct { coll, .. }
            | MongoOp::Aggregate { coll, .. }
            | MongoOp::GetIndexes { coll }
            | MongoOp::InsertOne { coll, .. }
            | MongoOp::InsertMany { coll, .. }
            | MongoOp::UpdateOne { coll, .. }
            | MongoOp::UpdateMany { coll, .. }
            | MongoOp::ReplaceOne { coll, .. }
            | MongoOp::DeleteOne { coll, .. }
            | MongoOp::DeleteMany { coll, .. } => Some(coll),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    /// 语句在原文中的字节范围(风险弹窗展示原文)
    pub range: std::ops::Range<usize>,
    pub op: MongoOp,
}

/// 集合名能作为标识符时写成 `db.name`,否则 `db.getCollection("…")`。
pub fn coll_ref(coll: &str) -> String {
    let ident = !coll.is_empty()
        && !coll.starts_with(|c: char| c.is_ascii_digit())
        && coll
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$');
    if ident {
        format!("db.{coll}")
    } else {
        format!(
            "db.getCollection({})",
            serde_json::to_string(coll).unwrap_or_default()
        )
    }
}

/// 解析脚本:语句之间用 `;` 或换行分隔。
pub fn parse_script(src: &str) -> Result<Vec<Statement>, ParseError> {
    let mut p = P::new(src);
    let mut out = vec![];
    loop {
        while p.eat(";") {}
        p.ws();
        if p.at_end() {
            return Ok(out);
        }
        let start = p.pos;
        let op = p.statement()?;
        let end = p.pos;
        out.push(Statement {
            range: start..end,
            op,
        });
        p.ws();
        if !p.at_end() && !src[p.pos..].starts_with(';') && !src[end..p.pos].contains('\n') {
            return Err(p.err(t!("core.mongo.parse.expected_separator")));
        }
    }
}

/// 方法参数:值 + 起始位置(报错定位用)。
struct Args<'s> {
    src: &'s str,
    method: &'s str,
    call_pos: usize,
    items: Vec<(usize, Bson)>,
}

impl Args<'_> {
    fn err(&self, pos: usize, msg: impl Into<String>) -> ParseError {
        err_at(self.src, pos, msg)
    }
    fn missing(&self) -> ParseError {
        self.err(
            self.call_pos,
            t!("core.mongo.parse.missing_arg", name = self.method),
        )
    }
    fn max(&self, n: usize) -> Result<(), ParseError> {
        match self.items.get(n) {
            Some((pos, _)) => Err(self.err(
                *pos,
                t!("core.mongo.parse.too_many_args", name = self.method, n = n),
            )),
            None => Ok(()),
        }
    }
    fn doc(&self, i: usize) -> Result<Document, ParseError> {
        match self.items.get(i) {
            Some((_, Bson::Document(d))) => Ok(d.clone()),
            Some((pos, _)) => Err(self.err(*pos, t!("core.mongo.parse.expected_object"))),
            None => Err(self.missing()),
        }
    }
    fn doc_or_empty(&self, i: usize) -> Result<Document, ParseError> {
        if self.items.get(i).is_none() {
            Ok(Document::new())
        } else {
            self.doc(i)
        }
    }
    fn opt_doc(&self, i: usize) -> Result<Option<Document>, ParseError> {
        if self.items.get(i).is_none() {
            Ok(None)
        } else {
            self.doc(i).map(Some)
        }
    }
    fn doc_array(&self, i: usize) -> Result<Vec<Document>, ParseError> {
        let Some((pos, v)) = self.items.get(i) else {
            return Err(self.missing());
        };
        let bad = || self.err(*pos, t!("core.mongo.parse.expected_doc_array"));
        let Bson::Array(items) = v else {
            return Err(bad());
        };
        items
            .iter()
            .map(|b| match b {
                Bson::Document(d) => Ok(d.clone()),
                _ => Err(bad()),
            })
            .collect()
    }
    fn string(&self, i: usize) -> Result<String, ParseError> {
        match self.items.get(i) {
            Some((_, Bson::String(s))) => Ok(s.clone()),
            Some((pos, _)) => Err(self.err(*pos, t!("core.mongo.parse.expected_string"))),
            None => Err(self.missing()),
        }
    }
    fn update(&self, i: usize) -> Result<Update, ParseError> {
        match self.items.get(i) {
            Some((_, Bson::Array(_))) => self.doc_array(i).map(Update::Pipeline),
            Some((pos, Bson::Document(d))) => {
                if d.is_empty() || d.keys().any(|k| !k.starts_with('$')) {
                    return Err(self.err(*pos, t!("core.mongo.parse.update_needs_operator")));
                }
                Ok(Update::Doc(d.clone()))
            }
            Some((pos, _)) => Err(self.err(*pos, t!("core.mongo.parse.expected_object"))),
            None => Err(self.missing()),
        }
    }
    fn uint(&self, i: usize) -> Result<i64, ParseError> {
        match self.items.get(i) {
            Some((_, Bson::Int32(n))) if *n >= 0 => Ok(*n as i64),
            Some((_, Bson::Int64(n))) if *n >= 0 => Ok(*n),
            Some((pos, _)) => Err(self.err(*pos, t!("core.mongo.parse.expected_uint"))),
            None => Err(self.missing()),
        }
    }
}

impl P<'_> {
    /// `(` 已吞掉;读到 `)` 为止,允许末尾逗号。
    fn call_args(&mut self) -> Result<Vec<(usize, Bson)>, ParseError> {
        let mut items = vec![];
        loop {
            if self.eat(")") {
                return Ok(items);
            }
            self.ws();
            let pos = self.pos;
            items.push((pos, self.value()?));
            if !self.eat(",") {
                self.expect(")")?;
                return Ok(items);
            }
        }
    }

    fn statement(&mut self) -> Result<MongoOp, ParseError> {
        self.ws();
        let start = self.pos;
        let head = self.ident();
        if head == Some("show") {
            self.ws();
            let what_pos = self.pos;
            return match self.ident() {
                Some("dbs") | Some("databases") => Ok(MongoOp::ShowDbs),
                Some("collections") | Some("tables") => Ok(MongoOp::ShowCollections),
                _ => Err(self.err_at(what_pos, t!("core.mongo.parse.unsupported_show"))),
            };
        }
        if head != Some("db") {
            return Err(self.err_at(start, t!("core.mongo.parse.expected_statement")));
        }
        self.expect(".")?;
        self.ws();
        let name_pos = self.pos;
        let coll = match self.ident() {
            Some("getCollection") => {
                self.expect("(")?;
                let s = self.string()?;
                self.expect(")")?;
                s
            }
            Some(name) => name.to_string(),
            None => return Err(self.err_at(name_pos, t!("core.mongo.parse.expected_collection"))),
        };
        self.expect(".")?;
        self.ws();
        let call_pos = self.pos;
        let method = self
            .ident()
            .ok_or_else(|| self.err(t!("core.mongo.parse.expected_method")))?;
        self.expect("(")?;
        let items = self.call_args()?;
        let a = Args {
            src: self.src,
            method,
            call_pos,
            items,
        };
        let mut op = build_op(&a, coll)?;
        self.chain(&mut op)?;
        Ok(op)
    }

    /// `find()` 之后的 `.sort() .skip() .limit() .projection() .pretty() .toArray()`。
    fn chain(&mut self, op: &mut MongoOp) -> Result<(), ParseError> {
        while self.eat(".") {
            self.ws();
            let call_pos = self.pos;
            let method = self
                .ident()
                .ok_or_else(|| self.err(t!("core.mongo.parse.expected_method")))?;
            self.expect("(")?;
            let items = self.call_args()?;
            let a = Args {
                src: self.src,
                method,
                call_pos,
                items,
            };
            let MongoOp::Find {
                sort,
                skip,
                limit,
                projection,
                ..
            } = op
            else {
                return Err(self.err_at(
                    call_pos,
                    t!("core.mongo.parse.chain_only_after_find", name = method),
                ));
            };
            match method {
                "sort" => {
                    a.max(1)?;
                    *sort = Some(a.doc(0)?);
                }
                "skip" => {
                    a.max(1)?;
                    *skip = Some(a.uint(0)? as u64);
                }
                "limit" => {
                    a.max(1)?;
                    *limit = Some(a.uint(0)?);
                }
                "projection" => {
                    a.max(1)?;
                    *projection = Some(a.doc(0)?);
                }
                "pretty" | "toArray" => a.max(0)?,
                _ => {
                    return Err(self.err_at(
                        call_pos,
                        t!("core.mongo.parse.unsupported_method", name = method),
                    ));
                }
            }
        }
        Ok(())
    }
}

fn build_op(a: &Args, coll: String) -> Result<MongoOp, ParseError> {
    Ok(match a.method {
        "find" => {
            a.max(2)?;
            MongoOp::Find {
                coll,
                filter: a.doc_or_empty(0)?,
                projection: a.opt_doc(1)?,
                sort: None,
                skip: None,
                limit: None,
            }
        }
        "findOne" => {
            a.max(2)?;
            MongoOp::FindOne {
                coll,
                filter: a.doc_or_empty(0)?,
                projection: a.opt_doc(1)?,
            }
        }
        "countDocuments" => {
            a.max(1)?;
            MongoOp::Count {
                coll,
                filter: a.doc_or_empty(0)?,
            }
        }
        "distinct" => {
            a.max(2)?;
            MongoOp::Distinct {
                coll,
                field: a.string(0)?,
                filter: a.doc_or_empty(1)?,
            }
        }
        "aggregate" => {
            a.max(1)?;
            MongoOp::Aggregate {
                coll,
                pipeline: a.doc_array(0)?,
            }
        }
        "getIndexes" => {
            a.max(0)?;
            MongoOp::GetIndexes { coll }
        }
        "insertOne" => {
            a.max(1)?;
            MongoOp::InsertOne {
                coll,
                doc: a.doc(0)?,
            }
        }
        "insertMany" => {
            a.max(1)?;
            MongoOp::InsertMany {
                coll,
                docs: a.doc_array(0)?,
            }
        }
        "updateOne" => {
            a.max(2)?;
            MongoOp::UpdateOne {
                coll,
                filter: a.doc(0)?,
                update: a.update(1)?,
            }
        }
        "updateMany" => {
            a.max(2)?;
            MongoOp::UpdateMany {
                coll,
                filter: a.doc(0)?,
                update: a.update(1)?,
            }
        }
        "replaceOne" => {
            a.max(2)?;
            MongoOp::ReplaceOne {
                coll,
                filter: a.doc(0)?,
                doc: a.doc(1)?,
            }
        }
        "deleteOne" => {
            a.max(1)?;
            MongoOp::DeleteOne {
                coll,
                filter: a.doc(0)?,
            }
        }
        "deleteMany" => {
            a.max(1)?;
            MongoOp::DeleteMany {
                coll,
                filter: a.doc(0)?,
            }
        }
        other => {
            return Err(a.err(
                a.call_pos,
                t!("core.mongo.parse.unsupported_method", name = other),
            ));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::{doc, oid::ObjectId, Bson, Document};

    #[test]
    fn loose_json_syntax() {
        let v =
            parse_document("{ a: 1, 'b': \"x\", c: [1, 2,], // 注释\n d: { $gt: 5 }, /* 块 */ }")
                .unwrap();
        assert_eq!(v, doc! { "a": 1, "b": "x", "c": [1, 2], "d": { "$gt": 5 } });
    }

    #[test]
    fn numbers_keep_their_types() {
        assert_eq!(parse_value("42").unwrap(), Bson::Int32(42));
        assert_eq!(
            parse_value("3000000000").unwrap(),
            Bson::Int64(3_000_000_000)
        );
        assert_eq!(parse_value("1.0").unwrap(), Bson::Double(1.0));
        assert_eq!(parse_value("-2.5e3").unwrap(), Bson::Double(-2500.0));
        assert_eq!(
            parse_value("-Infinity").unwrap(),
            Bson::Double(f64::NEG_INFINITY)
        );
    }

    #[test]
    fn constructors() {
        let oid = "64b7f0c2a1b2c3d4e5f60718";
        assert_eq!(
            parse_value(&format!("ObjectId(\"{oid}\")")).unwrap(),
            Bson::ObjectId(ObjectId::parse_str(oid).unwrap())
        );
        assert_eq!(parse_value("NumberLong(5)").unwrap(), Bson::Int64(5));
        assert_eq!(
            parse_value("NumberLong(\"9007199254740993\")").unwrap(),
            Bson::Int64(9_007_199_254_740_993)
        );
        assert_eq!(parse_value("NumberInt(7)").unwrap(), Bson::Int32(7));
        assert!(matches!(
            parse_value("NumberDecimal(\"1.10\")").unwrap(),
            Bson::Decimal128(_)
        ));
        let d = parse_value("ISODate(\"2024-01-02T03:04:05.678Z\")").unwrap();
        assert_eq!(
            d,
            Bson::DateTime(mongodb::bson::DateTime::from_millis(1_704_164_645_678))
        );
        assert_eq!(
            parse_value("new Date(0)").unwrap(),
            Bson::DateTime(mongodb::bson::DateTime::from_millis(0))
        );
        assert_eq!(
            parse_value("ISODate(\"2024-01-02\")").unwrap(),
            Bson::DateTime(mongodb::bson::DateTime::from_millis(1_704_153_600_000))
        );
        assert!(matches!(
            parse_value("UUID(\"123e4567-e89b-12d3-a456-426614174000\")").unwrap(),
            Bson::Binary(_)
        ));
        assert!(matches!(
            parse_value("BinData(0, \"AQID\")").unwrap(),
            Bson::Binary(_)
        ));
        assert!(matches!(
            parse_value("Timestamp(1, 2)").unwrap(),
            Bson::Timestamp(_)
        ));
        assert_eq!(parse_value("MinKey()").unwrap(), Bson::MinKey);
    }

    #[test]
    fn regex_literal_and_ctor() {
        let Bson::RegularExpression(r) = parse_value("/^ab\\/c[/]/im").unwrap() else {
            panic!()
        };
        assert_eq!(r.pattern, "^ab\\/c[/]");
        assert_eq!(r.options, "im");
        let Bson::RegularExpression(r) = parse_value("RegExp(\"a/b\", \"i\")").unwrap() else {
            panic!()
        };
        assert_eq!(r.pattern, "a/b");
    }

    #[test]
    fn string_escapes() {
        assert_eq!(
            parse_value(r#""a\n\t\"\u0001é""#).unwrap(),
            Bson::String("a\n\t\"\u{1}é".into())
        );
        assert_eq!(parse_value(r#""😀""#).unwrap(), Bson::String("😀".into()));
    }

    #[test]
    fn errors_carry_line_and_col() {
        let e = parse_value("{\n  a: 1,\n  b: }").unwrap_err();
        assert_eq!((e.line, e.col), (3, 6));
        let e = parse_value("{ a: Foo(1) }").unwrap_err();
        assert_eq!((e.line, e.col), (1, 6));
        assert!(parse_value("{ a: 1 } x").is_err());
        assert!(parse_value("'abc").is_err());
    }

    #[test]
    fn empty_document_source_is_empty_doc() {
        assert_eq!(parse_document("   ").unwrap(), Document::new());
        assert!(parse_document("[1]").is_err());
    }

    fn one(src: &str) -> MongoOp {
        let mut v = parse_script(src).unwrap();
        assert_eq!(v.len(), 1, "{src}");
        v.remove(0).op
    }

    #[test]
    fn find_with_chain() {
        let op = one(
            "db.orders.find({status: 'paid'}, {total: 1})\n  .sort({ts: -1}).skip(10).limit(5)",
        );
        assert_eq!(
            op,
            MongoOp::Find {
                coll: "orders".into(),
                filter: doc! { "status": "paid" },
                projection: Some(doc! { "total": 1 }),
                sort: Some(doc! { "ts": -1 }),
                skip: Some(10),
                limit: Some(5),
            }
        );
        let MongoOp::Find { projection, .. } = one("db.a.find().projection({x: 0}).pretty()")
        else {
            panic!()
        };
        assert_eq!(projection, Some(doc! { "x": 0 }));
    }

    #[test]
    fn every_supported_method() {
        assert!(matches!(
            one("db.a.findOne({_id: 1})"),
            MongoOp::FindOne { .. }
        ));
        assert!(matches!(
            one("db.a.countDocuments()"),
            MongoOp::Count { .. }
        ));
        assert!(matches!(
            one("db.a.distinct('city', {age: {$gt: 1}})"),
            MongoOp::Distinct { .. }
        ));
        assert!(matches!(
            one("db.a.aggregate([{$match: {}}, {$group: {_id: '$x'}}])"),
            MongoOp::Aggregate { .. }
        ));
        assert!(matches!(
            one("db.a.getIndexes()"),
            MongoOp::GetIndexes { .. }
        ));
        assert!(matches!(
            one("db.a.insertOne({x: 1})"),
            MongoOp::InsertOne { .. }
        ));
        assert!(matches!(
            one("db.a.insertMany([{x: 1}, {x: 2}])"),
            MongoOp::InsertMany { .. }
        ));
        assert!(matches!(
            one("db.a.updateOne({x: 1}, {$set: {y: 2}})"),
            MongoOp::UpdateOne {
                update: Update::Doc(_),
                ..
            }
        ));
        assert!(matches!(
            one("db.a.updateMany({}, [{$set: {y: 2}}])"),
            MongoOp::UpdateMany {
                update: Update::Pipeline(_),
                ..
            }
        ));
        assert!(matches!(
            one("db.a.replaceOne({x: 1}, {x: 2})"),
            MongoOp::ReplaceOne { .. }
        ));
        assert!(matches!(
            one("db.a.deleteOne({x: 1})"),
            MongoOp::DeleteOne { .. }
        ));
        assert!(matches!(
            one("db.a.deleteMany({x: 1})"),
            MongoOp::DeleteMany { .. }
        ));
        assert_eq!(one("show dbs"), MongoOp::ShowDbs);
        assert_eq!(one("show collections"), MongoOp::ShowCollections);
        assert_eq!(
            one("db.getCollection('a.b').find()").collection(),
            Some("a.b")
        );
    }

    #[test]
    fn multiple_statements_and_ranges() {
        let src = "db.a.find();db.b.find()\ndb.c.deleteOne({x: 1})";
        let v = parse_script(src).unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(&src[v[2].range.clone()], "db.c.deleteOne({x: 1})");
        assert!(
            parse_script("db.a.find() db.b.find()").is_err(),
            "同一行两条语句之间必须有 ;"
        );
        assert!(parse_script("  // 只有注释\n").unwrap().is_empty());
    }

    #[test]
    fn rejects_unsupported_syntax() {
        for src in [
            "db.a.drop()",
            "db.a.find().forEach(printjson)",
            "var x = 1",
            "db.a.updateOne({x: 1}, {y: 2})",
            "db.a.updateOne({x: 1}, {$set: {}}, {upsert: true})",
            "db.a.insertOne({x: 1}).sort({x: 1})",
            "db.a.find(",
        ] {
            assert!(parse_script(src).is_err(), "{src}");
        }
    }

    #[test]
    fn write_detection_includes_out_and_merge() {
        assert!(one("db.a.insertOne({})").is_write());
        assert!(one("db.a.deleteMany({x: 1})").is_write());
        assert!(one("db.a.aggregate([{$match: {}}, {$out: 'b'}])").is_write());
        assert!(one("db.a.aggregate([{$merge: {into: 'b'}}, {$match: {}}])").is_write());
        assert!(!one("db.a.aggregate([{$match: {}}])").is_write());
        assert!(!one("db.a.find()").is_write());
        assert!(!one("show dbs").is_write());
    }

    #[test]
    fn coll_ref_quotes_when_needed() {
        assert_eq!(coll_ref("users"), "db.users");
        assert_eq!(coll_ref("a.b"), "db.getCollection(\"a.b\")");
        assert_eq!(coll_ref("my-coll"), "db.getCollection(\"my-coll\")");
    }
}
