//! SQLite:把 CSV / JSON 文件导入成新表,或按列名追加到已有表。
//!
//! 两步:[`preview`] 读文件开头若干行,给出列名、推断的列类型(INTEGER / REAL / TEXT)与样例;
//! 界面改完列名 / 类型 / 取舍后交给 [`Jobs::start`],在后台线程里一个事务内建表 + 预编译 INSERT 逐行插入,
//! 界面轮询进度,取消即回滚。
//!
//! 文件始终流式读取(CSV 逐条记录;JSON 数组逐个元素、JSON Lines 逐个对象),不整体读进内存。
//! 编码统一先经 encoding_rs 转成 UTF-8(UTF-8 自动去 BOM;GBK 可选),非法字节换成 U+FFFD,
//! 预览据此提示「可能选错了编码」。
//!
//! 标识符一律双引号转义,值全部参数绑定。

use crate::error::{AppError, AppResult, ErrorKind};
use encoding_rs_io::DecodeReaderBytesBuilder;
use rusqlite::types::Value as SqlValue;
use rusqlite::Connection;
use rust_i18n::t;
use serde::de::{DeserializeSeed, Error as _, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// 预览里展示的行数。
pub const PREVIEW_ROWS: usize = 20;
/// 推断类型(以及 JSON 收集键名)用的样本行数。
pub const INFER_ROWS: usize = 1000;
/// 探测分隔符时读取的开头字节数。
const SNIFF_BYTES: u64 = 64 * 1024;

// ---------------------------------------------------------------- 选项

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Format {
    Csv,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Delimiter {
    #[default]
    Auto,
    Comma,
    Tab,
    Semicolon,
}

impl Delimiter {
    fn byte(self) -> u8 {
        match self {
            Delimiter::Tab => b'\t',
            Delimiter::Semicolon => b';',
            Delimiter::Comma | Delimiter::Auto => b',',
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Quote {
    #[default]
    Double,
    Single,
    /// 不识别引号:引号字符按普通字符保留
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Encoding {
    /// UTF-8,带不带 BOM 都行(BOM 自动去掉)
    #[default]
    Utf8,
    /// GBK(按 WHATWG 即 GB18030 解码,兼容 GB2312)
    Gbk,
}

fn yes() -> bool {
    true
}

/// 读哪个文件、怎么解析。CSV 的选项对 JSON 无效。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceOptions {
    pub path: String,
    pub format: Format,
    #[serde(default)]
    pub delimiter: Delimiter,
    #[serde(default)]
    pub quote: Quote,
    #[serde(default)]
    pub encoding: Encoding,
    #[serde(default = "yes")]
    pub has_header: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ColType {
    Integer,
    Real,
    Text,
}

impl ColType {
    fn sql(self) -> &'static str {
        match self {
            ColType::Integer => "INTEGER",
            ColType::Real => "REAL",
            ColType::Text => "TEXT",
        }
    }
}

// ---------------------------------------------------------------- 预览

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewColumn {
    /// 来源:CSV 的表头原文(无表头为空)、JSON 的键名
    pub key: String,
    /// 建议的列名(去空、去重)
    pub name: String,
    #[serde(rename = "type")]
    pub ty: ColType,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub format: Format,
    /// CSV 实际使用的分隔符(自动探测时为探测结果);JSON 为 None
    pub delimiter: Option<Delimiter>,
    /// JSON 是否为 JSON Lines(否则为对象数组)
    pub json_lines: bool,
    pub columns: Vec<PreviewColumn>,
    /// 前 [`PREVIEW_ROWS`] 行,按 columns 的顺序;NULL / 缺失为 None
    pub rows: Vec<Vec<Option<String>>>,
    /// 参与推断的行数
    pub sampled: usize,
    /// 默认表名(由文件名清洗而来)
    pub table_name: String,
    /// 解码时遇到了非法字节(多半是编码选错了)
    pub bad_encoding: bool,
    pub file_size: u64,
}

/// 读文件开头,给出列、推断类型与样例行。
pub fn preview(src: &SourceOptions) -> AppResult<Preview> {
    let file_size = std::fs::metadata(&src.path).map_err(open_err)?.len();
    let mut cols = Columns::default();
    let mut rows: Vec<Row> = vec![];
    let info = each_record(src, None, &mut |row| {
        cols.observe(&row);
        rows.push(row);
        Ok(rows.len() < INFER_ROWS)
    })?;
    if let Some(header) = &info.header {
        cols.set_header(header);
    }
    let types = cols.infer(&rows);
    let shown = rows
        .iter()
        .take(PREVIEW_ROWS)
        .map(|r| (0..cols.len()).map(|i| cols.cell(r, i).display()).collect())
        .collect();
    let bad_encoding = rows.iter().any(Row::has_replacement)
        || info.header.iter().flatten().any(|h| h.contains('\u{FFFD}'));
    let names = cols.names();
    Ok(Preview {
        format: src.format,
        delimiter: info.delimiter,
        json_lines: info.json_lines,
        columns: cols
            .keys()
            .into_iter()
            .zip(names)
            .zip(types)
            .map(|((key, name), ty)| PreviewColumn { key, name, ty })
            .collect(),
        rows: shown,
        sampled: rows.len(),
        table_name: table_name_from_path(&src.path),
        bad_encoding,
        file_size,
    })
}

/// 文件名(去扩展名)清洗成标识符:字母数字(含中文等)与下划线以外的字符换成 `_`,
/// 合并连续的 `_`,数字开头补 `t_`,清洗后为空用 `imported`。
pub fn table_name_from_path(path: &str) -> String {
    let stem = std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut out = String::new();
    for c in stem.chars() {
        let c = if c.is_alphanumeric() || c == '_' { c } else { '_' };
        if c == '_' && out.ends_with('_') {
            continue;
        }
        out.push(c);
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() {
        "imported".into()
    } else if out.starts_with(|c: char| c.is_ascii_digit()) {
        format!("t_{out}")
    } else {
        out
    }
}

/// 双引号转义标识符。
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

// ---------------------------------------------------------------- 读取

/// 源文件里的一行:CSV 为按位置的字段,JSON 为一个对象。
#[derive(Debug, Clone)]
enum Row {
    Csv(Vec<String>),
    Json(Map<String, Value>),
}

impl Row {
    fn has_replacement(&self) -> bool {
        match self {
            Row::Csv(f) => f.iter().any(|s| s.contains('\u{FFFD}')),
            Row::Json(m) => m.iter().any(|(k, v)| k.contains('\u{FFFD}') || v.to_string().contains('\u{FFFD}')),
        }
    }
}

/// 单元格的原始值(尚未按列类型转换)。
#[derive(Debug, Clone, PartialEq)]
enum Cell {
    Missing,
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    /// JSON 的对象 / 数组:存成 JSON 文本
    Nested(String),
}

impl Cell {
    fn from_json(v: &Value) -> Cell {
        match v {
            Value::Null => Cell::Null,
            Value::Bool(b) => Cell::Int(i64::from(*b)),
            Value::Number(n) => match n.as_i64() {
                Some(i) => Cell::Int(i),
                None => n.as_f64().map(Cell::Real).unwrap_or_else(|| Cell::Text(n.to_string())),
            },
            Value::String(s) => Cell::Text(s.clone()),
            Value::Array(_) | Value::Object(_) => Cell::Nested(v.to_string()),
        }
    }

    fn display(&self) -> Option<String> {
        match self {
            Cell::Missing | Cell::Null => None,
            Cell::Int(i) => Some(i.to_string()),
            Cell::Real(f) => Some(f.to_string()),
            Cell::Text(s) | Cell::Nested(s) => Some(s.clone()),
        }
    }

    /// 按目标列类型转成绑定值。数字列里的空串当 NULL;转换不了的保留原文(SQLite 不会丢数据)。
    fn bind(&self, ty: ColType) -> SqlValue {
        match (self, ty) {
            (Cell::Missing | Cell::Null, _) => SqlValue::Null,
            (Cell::Int(i), ColType::Real) => SqlValue::Real(*i as f64),
            (Cell::Int(i), ColType::Text) => SqlValue::Text(i.to_string()),
            (Cell::Int(i), ColType::Integer) => SqlValue::Integer(*i),
            (Cell::Real(f), ColType::Text) => SqlValue::Text(f.to_string()),
            (Cell::Real(f), _) => SqlValue::Real(*f),
            (Cell::Text(s), ColType::Integer | ColType::Real) => {
                let v = s.trim();
                let int = if ty == ColType::Integer { parse_int(v) } else { None };
                if v.is_empty() {
                    SqlValue::Null
                } else if let Some(i) = int {
                    SqlValue::Integer(i)
                } else if let Some(f) = parse_real(v) {
                    SqlValue::Real(f)
                } else {
                    SqlValue::Text(s.clone())
                }
            }
            (Cell::Text(s) | Cell::Nested(s), _) => SqlValue::Text(s.clone()),
        }
    }
}

/// 整数:可带负号,不允许多余的前导零(`007` 这类编号留作文本)。
fn parse_int(s: &str) -> Option<i64> {
    let digits = s.strip_prefix('-').unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) || (digits.len() > 1 && digits.starts_with('0')) {
        return None;
    }
    s.parse().ok()
}

/// 小数:只认十进制写法(排除 `inf`、`NaN` 这类 Rust 也能解析的词);
/// 整数部分有多余前导零(`007`、`00.5`)的同样留作文本。
fn parse_real(s: &str) -> Option<f64> {
    let unsigned = s.trim_start_matches(['-', '+']).as_bytes();
    if !s.bytes().any(|b| b.is_ascii_digit())
        || !s.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+' | b'e' | b'E'))
        || (unsigned.len() > 1 && unsigned[0] == b'0' && unsigned[1].is_ascii_digit())
    {
        return None;
    }
    s.parse::<f64>().ok().filter(|f| f.is_finite())
}

/// 预览与导入共用的列集合:CSV 按位置,JSON 按首次出现的键。
#[derive(Default)]
struct Columns {
    keys: Vec<String>,
    seen: HashSet<String>,
    /// CSV:记录里出现过的最大字段数
    width: usize,
    json: bool,
}

impl Columns {
    fn observe(&mut self, row: &Row) {
        match row {
            Row::Csv(f) => self.width = self.width.max(f.len()),
            Row::Json(m) => {
                self.json = true;
                for k in m.keys() {
                    if self.seen.insert(k.clone()) {
                        self.keys.push(k.clone());
                    }
                }
            }
        }
    }

    fn set_header(&mut self, header: &[String]) {
        self.width = self.width.max(header.len());
        self.keys = header.to_vec();
    }

    fn len(&self) -> usize {
        if self.json { self.keys.len() } else { self.width }
    }

    fn keys(&self) -> Vec<String> {
        (0..self.len()).map(|i| self.keys.get(i).cloned().unwrap_or_default()).collect()
    }

    /// 列名:来源名去首尾空白;为空用 `column_N`;与前面重名(不分大小写,SQLite 列名不分)加 `_2`、`_3`……
    fn names(&self) -> Vec<String> {
        let mut used: HashSet<String> = HashSet::new();
        self.keys()
            .iter()
            .enumerate()
            .map(|(i, k)| {
                let base = match k.trim() {
                    "" => format!("column_{}", i + 1),
                    s => s.to_string(),
                };
                let mut name = base.clone();
                let mut n = 2;
                while !used.insert(name.to_lowercase()) {
                    name = format!("{base}_{n}");
                    n += 1;
                }
                name
            })
            .collect()
    }

    fn cell(&self, row: &Row, i: usize) -> Cell {
        match row {
            Row::Csv(f) => f.get(i).map(|s| Cell::Text(s.clone())).unwrap_or(Cell::Missing),
            Row::Json(m) => self
                .keys
                .get(i)
                .and_then(|k| m.get(k))
                .map(Cell::from_json)
                .unwrap_or(Cell::Missing),
        }
    }

    /// 每列取样本里所有非空值:全是整数 → INTEGER,全是数字 → REAL,否则 TEXT(日期也是 TEXT);全空为 TEXT。
    fn infer(&self, rows: &[Row]) -> Vec<ColType> {
        (0..self.len())
            .map(|i| {
                let (mut any, mut int, mut num) = (false, true, true);
                for r in rows {
                    match self.cell(r, i) {
                        Cell::Missing | Cell::Null => {}
                        Cell::Int(_) => any = true,
                        Cell::Real(_) => {
                            any = true;
                            int = false;
                        }
                        Cell::Nested(_) => {
                            any = true;
                            (int, num) = (false, false);
                        }
                        Cell::Text(s) => {
                            let v = s.trim();
                            if v.is_empty() {
                                continue;
                            }
                            any = true;
                            if parse_int(v).is_none() {
                                int = false;
                                if parse_real(v).is_none() {
                                    num = false;
                                }
                            }
                        }
                    }
                    if !num {
                        break;
                    }
                }
                match (any, int, num) {
                    (true, true, _) => ColType::Integer,
                    (true, false, true) => ColType::Real,
                    _ => ColType::Text,
                }
            })
            .collect()
    }
}

/// 读取时统计已读字节(进度条用;按原始文件字节算)。
struct Counting<R> {
    inner: R,
    n: Arc<AtomicU64>,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let k = self.inner.read(buf)?;
        self.n.fetch_add(k as u64, Ordering::Relaxed);
        Ok(k)
    }
}

fn open_err(e: std::io::Error) -> AppError {
    AppError::new(ErrorKind::Query, t!("core.err.import_open_failed")).with_detail(e.to_string())
}

fn parse_err(detail: impl ToString) -> AppError {
    AppError::new(ErrorKind::Query, t!("core.err.import_parse_failed")).with_detail(detail.to_string())
}

/// 打开文件,按所选编码转成 UTF-8 字节流。
fn open_decoded(src: &SourceOptions, counter: Option<Arc<AtomicU64>>) -> AppResult<BufReader<Box<dyn Read + Send>>> {
    let file = File::open(&src.path).map_err(open_err)?;
    let raw: Box<dyn Read + Send> = match counter {
        Some(n) => Box::new(Counting { inner: file, n }),
        None => Box::new(file),
    };
    let enc = match src.encoding {
        Encoding::Utf8 => encoding_rs::UTF_8,
        Encoding::Gbk => encoding_rs::GBK,
    };
    // GBK 文件里不会有 UTF-8 BOM;开着 BOM 嗅探的话,文件真带了 UTF-16/UTF-8 BOM 时以 BOM 为准
    let decoded = DecodeReaderBytesBuilder::new()
        .encoding(Some(enc))
        .bom_override(true)
        .build(raw);
    Ok(BufReader::with_capacity(64 * 1024, Box::new(decoded)))
}

/// 读开头一段,统计各候选分隔符在引号外的出现次数:各行次数一致且最多的胜出,都没有用逗号。
fn sniff_delimiter(src: &SourceOptions) -> AppResult<Delimiter> {
    let mut head = vec![];
    open_decoded(src, None)?
        .take(SNIFF_BYTES)
        .read_to_end(&mut head)
        .map_err(open_err)?;
    // 截在多字节字符中间时末尾会是替换字符,不影响统计
    Ok(sniff_text(&String::from_utf8_lossy(&head), src.quote))
}

fn sniff_text(head: &str, quote: Quote) -> Delimiter {
    let q = match quote {
        Quote::Double => Some('"'),
        Quote::Single => Some('\''),
        Quote::None => None,
    };
    let mut lines: Vec<&str> = head.lines().filter(|l| !l.trim().is_empty()).take(20).collect();
    // 截断处的最后一行可能不完整
    if head.len() as u64 >= SNIFF_BYTES && lines.len() > 1 {
        lines.pop();
    }
    let mut best = (Delimiter::Comma, (false, 0usize));
    for d in [Delimiter::Comma, Delimiter::Tab, Delimiter::Semicolon] {
        let ch = d.byte() as char;
        let counts: Vec<usize> = lines
            .iter()
            .map(|l| {
                let mut inside = false;
                l.chars()
                    .filter(|&c| {
                        if Some(c) == q {
                            inside = !inside;
                        }
                        !inside && c == ch
                    })
                    .count()
            })
            .collect();
        let min = counts.iter().copied().min().unwrap_or(0);
        let consistent = min > 0 && counts.iter().all(|&c| c == min);
        let score = (consistent, min);
        if score > best.1 {
            best = (d, score);
        }
    }
    best.0
}

/// 读取过程中的附带信息。
struct SourceInfo {
    header: Option<Vec<String>>,
    delimiter: Option<Delimiter>,
    json_lines: bool,
}

/// 逐行回调 `f`;`f` 返回 Ok(false) 提前停止,返回错误则中止并原样传出。
fn each_record(
    src: &SourceOptions,
    counter: Option<Arc<AtomicU64>>,
    f: &mut dyn FnMut(Row) -> AppResult<bool>,
) -> AppResult<SourceInfo> {
    match src.format {
        Format::Csv => {
            let delimiter = match src.delimiter {
                Delimiter::Auto => sniff_delimiter(src)?,
                d => d,
            };
            let mut rdr = csv::ReaderBuilder::new()
                .has_headers(false)
                .flexible(true)
                .delimiter(delimiter.byte())
                .quoting(src.quote != Quote::None)
                .quote(if src.quote == Quote::Single { b'\'' } else { b'"' })
                .from_reader(open_decoded(src, counter)?);
            let mut header = None;
            let mut rec = csv::StringRecord::new();
            loop {
                let more = rdr.read_record(&mut rec).map_err(|e| match e.position() {
                    Some(p) => parse_err(t!("core.err.import_bad_line", n = p.line(), error = e.to_string())),
                    None => parse_err(e),
                })?;
                if !more {
                    break;
                }
                let fields: Vec<String> = rec.iter().map(str::to_string).collect();
                if src.has_header && header.is_none() {
                    header = Some(fields);
                    continue;
                }
                if !f(Row::Csv(fields))? {
                    break;
                }
            }
            Ok(SourceInfo { header, delimiter: Some(delimiter), json_lines: false })
        }
        Format::Json => {
            let mut rdr = open_decoded(src, counter)?;
            // 看第一个非空白字符:`[` 是对象数组,否则按 JSON Lines(也兼容单个对象)
            let first = loop {
                let buf = rdr.fill_buf().map_err(open_err)?;
                if buf.is_empty() {
                    break None;
                }
                match buf.iter().position(|b| !b.is_ascii_whitespace()) {
                    Some(i) => {
                        let b = buf[i];
                        rdr.consume(i);
                        break Some(b);
                    }
                    None => {
                        let n = buf.len();
                        rdr.consume(n);
                    }
                }
            };
            match first {
                None => Ok(SourceInfo { header: None, delimiter: None, json_lines: true }),
                Some(b'[') => {
                    json_array(rdr, f)?;
                    Ok(SourceInfo { header: None, delimiter: None, json_lines: false })
                }
                Some(_) => {
                    let stream = serde_json::Deserializer::from_reader(rdr).into_iter::<Value>();
                    for (i, v) in stream.enumerate() {
                        let v = v.map_err(|e| parse_err(t!("core.err.import_bad_line", n = e.line(), error = e.to_string())))?;
                        if !f(Row::Json(as_object(v, i)?))? {
                            break;
                        }
                    }
                    Ok(SourceInfo { header: None, delimiter: None, json_lines: true })
                }
            }
        }
    }
}

fn as_object(v: Value, i: usize) -> AppResult<Map<String, Value>> {
    match v {
        Value::Object(m) => Ok(m),
        _ => Err(parse_err(t!("core.err.import_not_object", n = i + 1))),
    }
}

/// 流式读取对象数组:逐个元素回调,不把整个数组物化。
/// 回调的错误 / 提前停止经 `stop` 带出(serde 的错误类型装不下 AppError)。
fn json_array(rdr: impl Read, f: &mut dyn FnMut(Row) -> AppResult<bool>) -> AppResult<()> {
    struct Each<'a> {
        f: &'a mut dyn FnMut(Row) -> AppResult<bool>,
        stop: &'a mut Option<AppResult<()>>,
    }
    impl<'de> DeserializeSeed<'de> for Each<'_> {
        type Value = ();
        fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
            d.deserialize_seq(self)
        }
    }
    impl<'de> Visitor<'de> for Each<'_> {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("an array of objects")
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
            let mut i = 0;
            while let Some(v) = seq.next_element::<Value>()? {
                let r = as_object(v, i).and_then(|m| (self.f)(Row::Json(m)));
                i += 1;
                match r {
                    Ok(true) => {}
                    Ok(false) => {
                        *self.stop = Some(Ok(()));
                        return Err(A::Error::custom("stop"));
                    }
                    Err(e) => {
                        *self.stop = Some(Err(e));
                        return Err(A::Error::custom("stop"));
                    }
                }
            }
            Ok(())
        }
    }
    let mut stop = None;
    let mut de = serde_json::Deserializer::from_reader(rdr);
    let res = Each { f, stop: &mut stop }.deserialize(&mut de);
    if let Some(r) = stop {
        return r;
    }
    res.map_err(|e| parse_err(t!("core.err.import_bad_line", n = e.line(), error = e.to_string())))
}

// ---------------------------------------------------------------- 导入

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    /// 建新表(已存在则报错)
    Create,
    /// 追加到已有表:按列名(不分大小写)对齐,表里有、文件里没有的列取默认值
    Append,
}

/// 界面确认后的一列:`index` / `key` 原样取自预览。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnSpec {
    pub index: usize,
    pub key: String,
    pub name: String,
    #[serde(rename = "type")]
    pub ty: ColType,
    pub include: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPlan {
    pub source: SourceOptions,
    pub table: String,
    pub mode: Mode,
    pub columns: Vec<ColumnSpec>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// 已插入行数(未提交前回滚即作废)
    pub rows: u64,
    pub bytes: u64,
    pub total_bytes: u64,
    pub done: bool,
    pub cancelled: bool,
    pub error: Option<String>,
    /// 追加模式下表里没有、因此被跳过的列
    pub skipped: Vec<String>,
}

/// 一次导入的进度与取消开关。
#[derive(Default)]
pub struct Job {
    cancel: AtomicBool,
    rows: AtomicU64,
    bytes: Arc<AtomicU64>,
    state: Mutex<Progress>,
}

impl Job {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn progress(&self) -> Progress {
        let mut p = self.state.lock().unwrap().clone();
        p.rows = self.rows.load(Ordering::Relaxed);
        p.bytes = self.bytes.load(Ordering::Relaxed).min(p.total_bytes);
        p
    }
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// 导入前的检查:表名非空、至少一列、新建时列名非空且不重名(不分大小写)。
pub fn validate(plan: &ImportPlan) -> AppResult<()> {
    let bad = |key: &str| AppError::new(ErrorKind::Query, t!(key));
    if plan.table.trim().is_empty() {
        return Err(bad("core.err.import_no_table"));
    }
    let cols: Vec<&ColumnSpec> = plan.columns.iter().filter(|c| c.include).collect();
    if cols.is_empty() {
        return Err(bad("core.err.import_no_columns"));
    }
    if plan.mode == Mode::Create {
        let mut seen = HashSet::new();
        for c in &cols {
            if c.name.trim().is_empty() {
                return Err(bad("core.err.import_empty_column"));
            }
            if !seen.insert(c.name.trim().to_lowercase()) {
                return Err(AppError::new(
                    ErrorKind::Query,
                    t!("core.err.import_duplicate_column", name = c.name.trim()),
                ));
            }
        }
    }
    Ok(())
}

fn sql_err(key: &str, e: rusqlite::Error) -> AppError {
    AppError::new(ErrorKind::Query, t!(key)).with_detail(e.to_string())
}

/// 在 `conn` 上一个事务里建表(或对齐已有表)并逐行插入;返回插入行数。
/// 出错或 `job` 被取消都在提交前返回,事务随之回滚。
pub fn run(conn: &mut Connection, plan: &ImportPlan, job: &Job) -> AppResult<u64> {
    validate(plan)?;
    let table = plan.table.trim();
    let tx = conn.transaction().map_err(|e| sql_err("core.err.begin_tx_failed", e))?;

    // (来源列, 表里的目标列名)
    let mut targets: Vec<(&ColumnSpec, String)> = vec![];
    match plan.mode {
        Mode::Create => {
            let defs: Vec<String> = plan
                .columns
                .iter()
                .filter(|c| c.include)
                .map(|c| {
                    targets.push((c, c.name.trim().to_string()));
                    format!("{} {}", quote_ident(c.name.trim()), c.ty.sql())
                })
                .collect();
            tx.execute_batch(&format!("CREATE TABLE {} ({})", quote_ident(table), defs.join(", ")))
                .map_err(|e| sql_err("core.err.import_create_failed", e))?;
        }
        Mode::Append => {
            let existing: Vec<String> = {
                let mut stmt = tx
                    .prepare("SELECT name FROM pragma_table_info(?1)")
                    .map_err(|e| sql_err("core.err.query_failed", e))?;
                let names = stmt
                    .query_map([table], |r| r.get::<_, String>(0))
                    .and_then(|it| it.collect::<Result<Vec<_>, _>>())
                    .map_err(|e| sql_err("core.err.query_failed", e))?;
                names
            };
            if existing.is_empty() {
                return Err(AppError::new(ErrorKind::NotFound, t!("core.err.import_table_missing", table = table)));
            }
            let by_lower: HashMap<String, &String> = existing.iter().map(|n| (n.to_lowercase(), n)).collect();
            let mut skipped = vec![];
            for c in plan.columns.iter().filter(|c| c.include) {
                match by_lower.get(&c.name.trim().to_lowercase()) {
                    Some(real) => targets.push((c, (*real).clone())),
                    None => skipped.push(c.name.trim().to_string()),
                }
            }
            if targets.is_empty() {
                return Err(AppError::new(ErrorKind::Query, t!("core.err.import_no_matching_columns")));
            }
            job.state.lock().unwrap().skipped = skipped;
        }
    }

    let insert = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        quote_ident(table),
        targets.iter().map(|(_, n)| quote_ident(n)).collect::<Vec<_>>().join(", "),
        vec!["?"; targets.len()].join(", ")
    );
    {
        let mut stmt = tx.prepare(&insert).map_err(|e| sql_err("core.err.import_create_failed", e))?;
        let cancelled = || AppError::new(ErrorKind::Query, t!("core.err.import_cancelled"));
        let mut n: u64 = 0;
        each_record(&plan.source, Some(job.bytes.clone()), &mut |row| {
            if job.cancelled() {
                return Err(cancelled());
            }
            let values: Vec<SqlValue> = targets
                .iter()
                .map(|(c, _)| {
                    let cell = match &row {
                        Row::Csv(f) => f.get(c.index).map(|s| Cell::Text(s.clone())).unwrap_or(Cell::Missing),
                        Row::Json(m) => m.get(&c.key).map(Cell::from_json).unwrap_or(Cell::Missing),
                    };
                    cell.bind(c.ty)
                })
                .collect();
            n += 1;
            stmt.execute(rusqlite::params_from_iter(values)).map_err(|e| {
                AppError::new(ErrorKind::Query, t!("core.err.import_row_failed", n = n)).with_detail(e.to_string())
            })?;
            job.rows.store(n, Ordering::Relaxed);
            Ok(true)
        })?;
        if job.cancelled() {
            return Err(cancelled());
        }
    }
    let n = job.rows.load(Ordering::Relaxed);
    tx.commit().map_err(|e| sql_err("core.err.commit_tx_failed", e))?;
    Ok(n)
}

/// 各连接正在进行 / 最近一次的导入。
#[derive(Default)]
pub struct Jobs {
    jobs: Mutex<HashMap<String, Arc<Job>>>,
}

impl Jobs {
    /// 在后台线程里导入到 `db_path`(另开一条连接,不占用界面查询用的那条;
    /// 写锁冲突时等待 busy_timeout)。同一连接上已有进行中的导入则报错。
    pub fn start(&self, id: &str, db_path: &str, plan: ImportPlan) -> AppResult<()> {
        validate(&plan)?;
        let total = std::fs::metadata(&plan.source.path).map_err(open_err)?.len();
        let mut jobs = self.jobs.lock().unwrap();
        if jobs.get(id).is_some_and(|j| !j.state.lock().unwrap().done) {
            return Err(AppError::new(ErrorKind::Query, t!("core.err.import_running")));
        }
        let mut conn = Connection::open(db_path).map_err(|e| sql_err("core.err.sqlite_open_failed", e))?;
        let _ = conn.busy_timeout(std::time::Duration::from_secs(10));
        let job = Arc::new(Job::default());
        job.state.lock().unwrap().total_bytes = total;
        jobs.insert(id.to_string(), job.clone());
        std::thread::spawn(move || {
            let result = run(&mut conn, &plan, &job);
            let mut s = job.state.lock().unwrap();
            s.done = true;
            match result {
                Ok(_) => {}
                Err(_) if job.cancelled() => {
                    s.cancelled = true;
                    job.rows.store(0, Ordering::Relaxed);
                }
                Err(e) => s.error = Some(e.display()),
            }
        });
        Ok(())
    }

    pub fn progress(&self, id: &str) -> Option<Progress> {
        self.jobs.lock().unwrap().get(id).map(|j| j.progress())
    }

    pub fn cancel(&self, id: &str) {
        if let Some(j) = self.jobs.lock().unwrap().get(id) {
            j.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    fn tmp(name: &str, bytes: &[u8]) -> Tmp {
        let p = std::env::temp_dir().join(format!("datum-import-{}-{name}", uuid::Uuid::new_v4()));
        File::create(&p).unwrap().write_all(bytes).unwrap();
        Tmp(p)
    }
    fn csv_src(t: &Tmp) -> SourceOptions {
        SourceOptions {
            path: t.0.to_string_lossy().into_owned(),
            format: Format::Csv,
            delimiter: Delimiter::Auto,
            quote: Quote::Double,
            encoding: Encoding::Utf8,
            has_header: true,
        }
    }
    fn json_src(t: &Tmp) -> SourceOptions {
        SourceOptions { format: Format::Json, ..csv_src(t) }
    }
    fn specs(p: &Preview) -> Vec<ColumnSpec> {
        p.columns
            .iter()
            .enumerate()
            .map(|(i, c)| ColumnSpec { index: i, key: c.key.clone(), name: c.name.clone(), ty: c.ty, include: true })
            .collect()
    }
    fn plan(src: SourceOptions, table: &str, mode: Mode) -> ImportPlan {
        let p = preview(&src).unwrap();
        ImportPlan { columns: specs(&p), source: src, table: table.into(), mode }
    }
    fn rows(conn: &Connection, sql: &str) -> Vec<Vec<String>> {
        let mut stmt = conn.prepare(sql).unwrap();
        let n = stmt.column_count();
        stmt.query_map([], |r| {
            Ok((0..n)
                .map(|i| match r.get_ref(i).unwrap() {
                    rusqlite::types::ValueRef::Null => "NULL".into(),
                    rusqlite::types::ValueRef::Integer(i) => format!("i:{i}"),
                    rusqlite::types::ValueRef::Real(f) => format!("r:{f}"),
                    rusqlite::types::ValueRef::Text(t) => format!("t:{}", String::from_utf8_lossy(t)),
                    rusqlite::types::ValueRef::Blob(_) => "blob".into(),
                })
                .collect())
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    #[test]
    fn infers_types_from_csv_and_strips_bom() {
        let f = tmp("people.csv", "\u{FEFF}id,name,score,born,zip\n1,Ann,3.5,2001-02-03,007\n2,\"Bo, Jr\",4,,010\n".as_bytes());
        let p = preview(&csv_src(&f)).unwrap();
        let cols: Vec<(&str, ColType)> = p.columns.iter().map(|c| (c.name.as_str(), c.ty)).collect();
        assert_eq!(
            cols,
            vec![
                ("id", ColType::Integer),
                ("name", ColType::Text),
                ("score", ColType::Real),
                ("born", ColType::Text),
                ("zip", ColType::Text),
            ]
        );
        assert_eq!(p.delimiter, Some(Delimiter::Comma));
        assert_eq!(p.rows[1][1].as_deref(), Some("Bo, Jr"));
        assert_eq!(p.rows.len(), 2);
        assert!(!p.bad_encoding);
    }

    #[test]
    fn sniffs_tab_and_semicolon() {
        assert_eq!(sniff_text("a\tb\tc\n1\t2\t3\n", Quote::Double), Delimiter::Tab);
        assert_eq!(sniff_text("a;b\n\"x;y\";2\n", Quote::Double), Delimiter::Semicolon);
        // 逗号在引号里不算
        assert_eq!(sniff_text("a;b\n\"1,5\";2\n", Quote::Double), Delimiter::Semicolon);
        assert_eq!(sniff_text("single\n", Quote::Double), Delimiter::Comma);
    }

    #[test]
    fn headerless_and_duplicate_names() {
        let f = tmp("x.csv", b"1,2\n3,4,5\n");
        let mut src = csv_src(&f);
        src.has_header = false;
        let p = preview(&src).unwrap();
        let names: Vec<_> = p.columns.iter().map(|c| c.name.clone()).collect();
        assert_eq!(names, vec!["column_1", "column_2", "column_3"]);
        assert_eq!(p.rows[0][2], None);

        let f = tmp("y.csv", b"Id,id,,\n1,2,3,4\n");
        let p = preview(&csv_src(&f)).unwrap();
        let names: Vec<_> = p.columns.iter().map(|c| c.name.clone()).collect();
        assert_eq!(names, vec!["Id", "id_2", "column_3", "column_4"]);
    }

    #[test]
    fn decodes_gbk_and_flags_wrong_encoding() {
        let (bytes, _, _) = encoding_rs::GBK.encode("名字,城市\n张三,北京\n");
        let f = tmp("gbk.csv", &bytes);
        let mut src = csv_src(&f);
        let p = preview(&src).unwrap();
        assert!(p.bad_encoding);
        src.encoding = Encoding::Gbk;
        let p = preview(&src).unwrap();
        assert!(!p.bad_encoding);
        assert_eq!(p.columns[0].name, "名字");
        assert_eq!(p.rows[0][1].as_deref(), Some("北京"));
    }

    #[test]
    fn json_array_and_lines_with_nested_values() {
        let f = tmp("a.json", br#"[{"id":1,"tags":["x","y"],"meta":{"k":1}},{"id":2,"ok":true,"v":1.5}]"#);
        let p = preview(&json_src(&f)).unwrap();
        assert!(!p.json_lines);
        let cols: Vec<(&str, ColType)> = p.columns.iter().map(|c| (c.name.as_str(), c.ty)).collect();
        assert_eq!(
            cols,
            vec![
                ("id", ColType::Integer),
                ("tags", ColType::Text),
                ("meta", ColType::Text),
                ("ok", ColType::Integer),
                ("v", ColType::Real),
            ]
        );
        assert_eq!(p.rows[0][1].as_deref(), Some(r#"["x","y"]"#));
        assert_eq!(p.rows[0][3], None);

        let f = tmp("b.jsonl", b"{\"a\":1}\n\n{\"a\":2,\"b\":\"x\"}\n");
        let p = preview(&json_src(&f)).unwrap();
        assert!(p.json_lines);
        assert_eq!(p.columns.len(), 2);
        assert_eq!(p.rows.len(), 2);

        let f = tmp("c.json", b"[1,2]");
        assert!(preview(&json_src(&f)).is_err());
    }

    #[test]
    fn table_name_is_sanitized() {
        assert_eq!(table_name_from_path("/x/My Sales-2024.csv"), "My_Sales_2024");
        assert_eq!(table_name_from_path("/x/2024 data.csv"), "t_2024_data");
        assert_eq!(table_name_from_path("/x/订单 明细.json"), "订单_明细");
        assert_eq!(table_name_from_path("/x/---.csv"), "imported");
    }

    #[test]
    fn creates_table_and_inserts_typed_values() {
        let f = tmp("t.csv", b"id,name,score,note\n1,Ann,3.5,\n2,Bob,,x\n");
        let mut conn = Connection::open_in_memory().unwrap();
        let mut pl = plan(csv_src(&f), "weird \"name\"", Mode::Create);
        pl.columns[3].include = false;
        pl.columns[1].name = "who".into();
        let job = Job::default();
        assert_eq!(run(&mut conn, &pl, &job).unwrap(), 2);
        assert_eq!(
            rows(&conn, "SELECT * FROM \"weird \"\"name\"\"\" ORDER BY id"),
            vec![vec!["i:1", "t:Ann", "r:3.5"], vec!["i:2", "t:Bob", "NULL"]]
        );
        // 已存在 → 建表失败,不会插入
        assert!(run(&mut conn, &pl, &Job::default()).is_err());
    }

    #[test]
    fn json_import_stores_nested_as_text() {
        let f = tmp("n.json", br#"[{"id":1,"meta":{"k":[1,2]},"flag":false}]"#);
        let mut conn = Connection::open_in_memory().unwrap();
        run(&mut conn, &plan(json_src(&f), "j", Mode::Create), &Job::default()).unwrap();
        assert_eq!(rows(&conn, "SELECT * FROM j"), vec![vec!["i:1", r#"t:{"k":[1,2]}"#, "i:0"]]);
    }

    #[test]
    fn append_aligns_by_name_and_skips_unknown() {
        let f = tmp("a.csv", b"NAME,extra,id\nAnn,zzz,7\n");
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE p(id INTEGER, name TEXT, city TEXT DEFAULT 'n/a')").unwrap();
        let job = Job::default();
        run(&mut conn, &plan(csv_src(&f), "p", Mode::Append), &job).unwrap();
        assert_eq!(rows(&conn, "SELECT id, name, city FROM p"), vec![vec!["i:7", "t:Ann", "t:n/a"]]);
        assert_eq!(job.progress().skipped, vec!["extra".to_string()]);
        // 表不存在
        assert!(run(&mut conn, &plan(csv_src(&f), "nope", Mode::Append), &Job::default()).is_err());
    }

    #[test]
    fn failure_and_cancel_roll_back_everything() {
        let f = tmp("r.csv", b"id\n1\n2\n2\n");
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE u(id INTEGER PRIMARY KEY)").unwrap();
        let err = run(&mut conn, &plan(csv_src(&f), "u", Mode::Append), &Job::default()).unwrap_err();
        assert!(err.display().contains('3'), "{}", err.display());
        assert_eq!(rows(&conn, "SELECT count(*) FROM u"), vec![vec!["i:0"]]);

        let job = Job::default();
        job.cancel();
        assert!(run(&mut conn, &plan(csv_src(&f), "c", Mode::Create), &job).is_err());
        // 取消后建表也一起回滚
        assert_eq!(rows(&conn, "SELECT count(*) FROM sqlite_master WHERE name = 'c'"), vec![vec!["i:0"]]);
    }

    #[test]
    fn validate_rejects_bad_plans() {
        let f = tmp("v.csv", b"a,b\n1,2\n");
        let mut pl = plan(csv_src(&f), "t", Mode::Create);
        pl.columns[1].name = "A".into();
        assert!(validate(&pl).is_err());
        pl.columns[1].include = false;
        assert!(validate(&pl).is_ok());
        pl.columns[0].include = false;
        assert!(validate(&pl).is_err());
        pl.columns[0].include = true;
        pl.table = "  ".into();
        assert!(validate(&pl).is_err());
    }

    #[test]
    fn background_job_reports_progress_and_cancel_rolls_back() {
        let db = tmp("db.sqlite", b"");
        let mut csv = String::from("id,v\n");
        for i in 0..200_000 {
            csv.push_str(&format!("{i},value {i}\n"));
        }
        let f = tmp("big.csv", csv.as_bytes());
        let jobs = Jobs::default();
        let db_path = db.0.to_string_lossy().into_owned();

        jobs.start("c", &db_path, plan(csv_src(&f), "big", Mode::Create)).unwrap();
        let p = loop {
            let p = jobs.progress("c").unwrap();
            if p.done {
                break p;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(p.error, None);
        assert_eq!(p.rows, 200_000);
        assert_eq!(p.bytes, p.total_bytes);
        let conn = Connection::open(&db_path).unwrap();
        assert_eq!(rows(&conn, "SELECT count(*) FROM big"), vec![vec!["i:200000"]]);

        jobs.start("c", &db_path, plan(csv_src(&f), "big2", Mode::Create)).unwrap();
        jobs.cancel("c");
        let p = loop {
            let p = jobs.progress("c").unwrap();
            if p.done {
                break p;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(p.cancelled);
        assert_eq!(p.error, None);
        assert_eq!(rows(&conn, "SELECT count(*) FROM sqlite_master WHERE name = 'big2'"), vec![vec!["i:0"]]);
    }
}
