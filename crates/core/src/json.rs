//! JSON 单元格:无损解析成树(界面展开、复制路径 / 值)、校验与格式化 / 压缩、
//! 按路径取值(客户端筛选)、PG 的路径表达式与包含查询(筛选下推),以及补全 JSON 键名的取样推断。
//!
//! 不用 `serde_json::Value`:它的对象按键排序,丢了原文顺序(json 列、MySQL 都保留写入时的顺序);
//! 超出 f64 的数字会丢精度,`1.10` 这类 numeric 写法也会被改写。这里数字保留原文。

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

/// 一个 JSON 值。对象按原文顺序保留键(重复键也保留,与 json 类型一致)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Object(Vec<(String, Node)>),
    Array(Vec<Node>),
    String(String),
    /// 数字的原文写法
    Number(String),
    Bool(bool),
    Null,
}

/// 值的种类(筛选条件里记录被点字段的类型,决定按数值 / 布尔 / 文本比较)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Object,
    Array,
    String,
    Number,
    Bool,
    Null,
}

impl Node {
    pub fn kind(&self) -> Kind {
        match self {
            Node::Object(_) => Kind::Object,
            Node::Array(_) => Kind::Array,
            Node::String(_) => Kind::String,
            Node::Number(_) => Kind::Number,
            Node::Bool(_) => Kind::Bool,
            Node::Null => Kind::Null,
        }
    }

    /// 界面用的形状:`{ kind, text }`(标量)、`{ kind, entries: [{ key, node }] }`、`{ kind, items }`。
    pub fn to_js(&self) -> Value {
        match self {
            Node::Object(es) => json!({
                "kind": "object",
                "entries": es.iter().map(|(k, v)| json!({ "key": k, "node": v.to_js() })).collect::<Vec<_>>(),
            }),
            Node::Array(items) => json!({
                "kind": "array",
                "items": items.iter().map(Node::to_js).collect::<Vec<_>>(),
            }),
            Node::String(s) => json!({ "kind": "string", "text": s }),
            Node::Number(n) => json!({ "kind": "number", "text": n }),
            Node::Bool(b) => json!({ "kind": "bool", "text": if *b { "true" } else { "false" } }),
            Node::Null => json!({ "kind": "null", "text": "null" }),
        }
    }

    /// 沿路径取子值;键不存在、下标越界或类型不对为 None。
    pub fn at(&self, path: &[PathSeg]) -> Option<&Node> {
        let mut cur = self;
        for seg in path {
            cur = match (cur, seg) {
                (Node::Object(es), PathSeg::Key(k)) => &es.iter().rev().find(|(name, _)| name == k)?.1,
                (Node::Array(items), PathSeg::Index(i)) => items.get(*i as usize)?,
                _ => return None,
            };
        }
        Some(cur)
    }

    /// 与 PG 的 `->>` 一致的文本:字符串去引号、数字原文、布尔 `true` / `false`,
    /// JSON null 为 None(SQL NULL),对象 / 数组为压缩写法。
    pub fn as_text(&self) -> Option<String> {
        match self {
            Node::String(s) => Some(s.clone()),
            Node::Number(n) => Some(n.clone()),
            Node::Bool(b) => Some(b.to_string()),
            Node::Null => None,
            _ => Some(to_text(self, false)),
        }
    }
}

// ---------------------------------------------------------------- 解析

/// 解析失败的原因(界面按它取多语言文案)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// 没写完就结束了
    Eof,
    /// 此处不该出现这个字符
    Unexpected,
    /// 值之后还有多余内容
    Trailing,
    /// 字符串里的转义写错了
    Escape,
    /// 数字写法不对
    Number,
    /// 字符串里有未转义的控制字符(如直接换行)
    Control,
    /// 嵌套太深
    TooDeep,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParseError {
    pub kind: ErrorKind,
    /// 1 起的行号、列号(列按字符计)
    pub line: usize,
    pub column: usize,
    /// 出错位置的字符(到末尾为 None)
    pub found: Option<String>,
}

/// 嵌套上限:防止恶意的深层嵌套把栈撑爆(PG 自己的上限也在这个量级)。
const MAX_DEPTH: usize = 512;

struct Parser<'a> {
    s: &'a str,
    b: &'a [u8],
    i: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn err(&self, kind: ErrorKind) -> ParseError {
        let before = &self.s[..self.i.min(self.s.len())];
        let line = before.matches('\n').count() + 1;
        let column = before[before.rfind('\n').map_or(0, |p| p + 1)..].chars().count() + 1;
        let found = self.s.get(self.i..).and_then(|r| r.chars().next()).map(String::from);
        let kind = if found.is_none() && kind == ErrorKind::Unexpected { ErrorKind::Eof } else { kind };
        ParseError { kind, line, column, found }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn value(&mut self) -> Result<Node, ParseError> {
        self.ws();
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => self.string().map(Node::String),
            Some(b't') => self.word("true", Node::Bool(true)),
            Some(b'f') => self.word("false", Node::Bool(false)),
            Some(b'n') => self.word("null", Node::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(self.err(ErrorKind::Unexpected)),
        }
    }

    fn word(&mut self, w: &str, node: Node) -> Result<Node, ParseError> {
        if self.s[self.i..].starts_with(w) {
            self.i += w.len();
            Ok(node)
        } else {
            Err(self.err(ErrorKind::Unexpected))
        }
    }

    fn enter(&mut self) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.err(ErrorKind::TooDeep));
        }
        self.i += 1;
        Ok(())
    }

    fn object(&mut self) -> Result<Node, ParseError> {
        self.enter()?;
        let mut entries = Vec::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
        } else {
            loop {
                self.ws();
                if self.peek() != Some(b'"') {
                    return Err(self.err(ErrorKind::Unexpected));
                }
                let key = self.string()?;
                self.ws();
                if self.peek() != Some(b':') {
                    return Err(self.err(ErrorKind::Unexpected));
                }
                self.i += 1;
                let v = self.value()?;
                entries.push((key, v));
                self.ws();
                match self.peek() {
                    Some(b',') => self.i += 1,
                    Some(b'}') => {
                        self.i += 1;
                        break;
                    }
                    _ => return Err(self.err(ErrorKind::Unexpected)),
                }
            }
        }
        self.depth -= 1;
        Ok(Node::Object(entries))
    }

    fn array(&mut self) -> Result<Node, ParseError> {
        self.enter()?;
        let mut items = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.i += 1;
        } else {
            loop {
                items.push(self.value()?);
                self.ws();
                match self.peek() {
                    Some(b',') => self.i += 1,
                    Some(b']') => {
                        self.i += 1;
                        break;
                    }
                    _ => return Err(self.err(ErrorKind::Unexpected)),
                }
            }
        }
        self.depth -= 1;
        Ok(Node::Array(items))
    }

    fn hex4(&mut self) -> Result<u32, ParseError> {
        let h = self.s.get(self.i..self.i + 4).filter(|h| h.bytes().all(|c| c.is_ascii_hexdigit()));
        let Some(h) = h else { return Err(self.err(ErrorKind::Escape)) };
        self.i += 4;
        Ok(u32::from_str_radix(h, 16).unwrap_or(0))
    }

    fn string(&mut self) -> Result<String, ParseError> {
        self.i += 1; // 开头的引号
        let mut out = String::new();
        loop {
            let start = self.i;
            while let Some(c) = self.peek() {
                if c == b'"' || c == b'\\' || c < 0x20 {
                    break;
                }
                self.i += 1;
            }
            // 只在 ASCII 字符处停下,切片不会落在多字节字符中间
            out.push_str(&self.s[start..self.i]);
            match self.peek() {
                None => return Err(self.err(ErrorKind::Eof)),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let Some(e) = self.peek() else { return Err(self.err(ErrorKind::Eof)) };
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&hi) && self.s[self.i..].starts_with("\\u") {
                                // 代理对:紧跟的低位代理不合法时只消费高位,按孤立代理处理
                                let save = self.i;
                                self.i += 2;
                                let lo = self.hex4()?;
                                if (0xDC00..0xE000).contains(&lo) {
                                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                                } else {
                                    self.i = save;
                                    hi
                                }
                            } else {
                                hi
                            };
                            // 孤立的代理码点无法表示成 Rust 字符:换成替换字符,不让整个值打不开
                            out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                        }
                        _ => {
                            self.i -= 1;
                            return Err(self.err(ErrorKind::Escape));
                        }
                    }
                }
                Some(_) => return Err(self.err(ErrorKind::Control)),
            }
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.i;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.i += 1;
        }
        self.i - start
    }

    fn number(&mut self) -> Result<Node, ParseError> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        match self.peek() {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return Err(self.err(ErrorKind::Number)),
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            if self.digits() == 0 {
                return Err(self.err(ErrorKind::Number));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if self.digits() == 0 {
                return Err(self.err(ErrorKind::Number));
            }
        }
        // `01`、`1.5.2` 这类:数字后面紧跟数字 / 点也算写错
        if matches!(self.peek(), Some(b'0'..=b'9' | b'.')) {
            return Err(self.err(ErrorKind::Number));
        }
        Ok(Node::Number(self.s[start..self.i].to_string()))
    }
}

/// 严格按 RFC 8259 解析(允许首尾空白)。
pub fn parse(text: &str) -> Result<Node, ParseError> {
    let mut p = Parser { s: text, b: text.as_bytes(), i: 0, depth: 0 };
    let v = p.value()?;
    p.ws();
    if p.i < text.len() {
        return Err(p.err(ErrorKind::Trailing));
    }
    Ok(v)
}

/// 去掉首尾空白后以 `{` / `[` 开头:可能是 JSON 文档(没有类型信息的列先用它挡掉普通文本,省得逐个解析)。
pub fn looks_like_document(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with('{') || t.starts_with('[')
}

// ---------------------------------------------------------------- 输出

fn escape_into(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// JSON 字符串字面量(带引号与转义)。
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    escape_into(s, &mut out);
    out
}

fn write(node: &Node, pretty: bool, level: usize, out: &mut String) {
    let indent = |out: &mut String, level: usize| {
        if pretty {
            out.push('\n');
            out.push_str(&"  ".repeat(level));
        }
    };
    match node {
        Node::Object(es) if es.is_empty() => out.push_str("{}"),
        Node::Array(items) if items.is_empty() => out.push_str("[]"),
        Node::Object(es) => {
            out.push('{');
            for (i, (k, v)) in es.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                indent(out, level + 1);
                escape_into(k, out);
                out.push_str(if pretty { ": " } else { ":" });
                write(v, pretty, level + 1, out);
            }
            indent(out, level);
            out.push('}');
        }
        Node::Array(items) => {
            out.push('[');
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                indent(out, level + 1);
                write(v, pretty, level + 1, out);
            }
            indent(out, level);
            out.push(']');
        }
        Node::String(s) => escape_into(s, out),
        Node::Number(n) => out.push_str(n),
        Node::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Node::Null => out.push_str("null"),
    }
}

/// 输出 JSON 文本:`pretty` 为 2 空格缩进,否则压缩成一行。键顺序与数字写法保持原样。
pub fn to_text(node: &Node, pretty: bool) -> String {
    let mut out = String::new();
    write(node, pretty, 0, &mut out);
    out
}

// ---------------------------------------------------------------- 路径

/// 路径的一段:对象键或数组下标。界面里写成 `["a", 0, "b"]`。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(untagged)]
pub enum PathSeg {
    Key(String),
    Index(u64),
}

impl<'de> Deserialize<'de> for PathSeg {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // JS 的数字过边界后可能是浮点(`0.0`),下标按非负整数收
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Key(String),
            Num(f64),
        }
        match Raw::deserialize(d)? {
            Raw::Key(k) => Ok(PathSeg::Key(k)),
            Raw::Num(n) if n >= 0.0 && n.fract() == 0.0 && n <= u32::MAX as f64 => Ok(PathSeg::Index(n as u64)),
            Raw::Num(n) => Err(serde::de::Error::custom(format!("bad array index {n}"))),
        }
    }
}

/// MySQL / SQLite 通用的 `$.a[0]."b c"` 写法(`JSON_EXTRACT` / `json_extract` 的路径)。
pub fn dollar_path(path: &[PathSeg]) -> String {
    let mut out = String::from("$");
    for seg in path {
        match seg {
            PathSeg::Index(i) => out.push_str(&format!("[{i}]")),
            PathSeg::Key(k) => {
                let plain = k.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                out.push('.');
                if plain {
                    out.push_str(k);
                } else {
                    out.push_str(&quote(k));
                }
            }
        }
    }
    out
}

/// PG 的取值表达式:`col->'a'->0->>'b'`(`as_text` 时最后一段用 `->>` 取文本)。
/// 键一律写成转义后的字符串字面量,下标写成整数,不会被键名注入 SQL。
pub fn pg_path(col_sql: &str, path: &[PathSeg], as_text: bool) -> String {
    let mut out = col_sql.to_string();
    for (i, seg) in path.iter().enumerate() {
        out.push_str(if as_text && i + 1 == path.len() { "->>" } else { "->" });
        match seg {
            PathSeg::Key(k) => out.push_str(&crate::sql::build::literal(k, crate::sql::Dialect::Pg)),
            PathSeg::Index(n) => out.push_str(&n.to_string()),
        }
    }
    out
}

/// 包含查询的右边:路径全是键时,把值嵌回 `{"a":{"b":值}}`;路径里有下标则不行(数组的包含语义不同)。
pub fn containment_doc(path: &[PathSeg], value: &Node) -> Option<String> {
    let mut doc = to_text(value, false);
    for seg in path.iter().rev() {
        let PathSeg::Key(k) = seg else { return None };
        doc = format!("{{{}:{doc}}}", quote(k));
    }
    Some(doc)
}

/// 按某种类型把筛选值解释成 JSON 值:数字须是合法 JSON 数字,布尔为 true / false,null 恒为 null;
/// 对不上类型返回 None(调用方退回文本比较)。
pub fn typed_value(kind: Kind, value: &str) -> Option<Node> {
    let v = value.trim();
    match kind {
        Kind::String => Some(Node::String(value.to_string())),
        Kind::Number => match parse(v) {
            Ok(n @ Node::Number(_)) => Some(n),
            _ => None,
        },
        Kind::Bool => match v.to_ascii_lowercase().as_str() {
            "true" => Some(Node::Bool(true)),
            "false" => Some(Node::Bool(false)),
            _ => None,
        },
        Kind::Null => Some(Node::Null),
        Kind::Object | Kind::Array => None,
    }
}

// ---------------------------------------------------------------- 键名推断(补全)

/// 取样推断出的键结构:每层对象出现过的键(按出现次数降序),数组元素的结构合并在 `items` 里。
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct KeyTree {
    pub keys: Vec<KeyEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Box<KeyTree>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeyEntry {
    pub name: String,
    pub count: u32,
    pub tree: KeyTree,
}

impl KeyTree {
    /// 沿路径往下:键找同名项,下标进数组元素的合并结构。
    pub fn at(&self, path: &[PathSeg]) -> Option<&KeyTree> {
        let mut cur = self;
        for seg in path {
            cur = match seg {
                PathSeg::Key(k) => &cur.keys.iter().find(|e| e.name == *k)?.tree,
                PathSeg::Index(_) => cur.items.as_deref()?,
            };
        }
        Some(cur)
    }
}

/// 推断的深度、每层键数与每个数组取样的元素数上限(取样只为补全提示,不求全)。
const KEY_DEPTH: usize = 8;
const KEY_MAX: usize = 200;
const ARRAY_SAMPLE: usize = 20;

#[derive(Default)]
struct Acc {
    keys: Vec<(String, u32, Acc)>,
    index: HashMap<String, usize>,
    items: Option<Box<Acc>>,
}

impl Acc {
    fn add(&mut self, node: &Node, depth: usize) {
        if depth >= KEY_DEPTH {
            return;
        }
        match node {
            Node::Object(es) => {
                for (k, v) in es {
                    let ix = match self.index.get(k) {
                        Some(ix) => *ix,
                        None if self.keys.len() >= KEY_MAX => continue,
                        None => {
                            self.index.insert(k.clone(), self.keys.len());
                            self.keys.push((k.clone(), 0, Acc::default()));
                            self.keys.len() - 1
                        }
                    };
                    let entry = &mut self.keys[ix];
                    entry.1 += 1;
                    entry.2.add(v, depth + 1);
                }
            }
            Node::Array(items) => {
                let acc = self.items.get_or_insert_with(Default::default);
                for it in items.iter().take(ARRAY_SAMPLE) {
                    acc.add(it, depth + 1);
                }
            }
            _ => {}
        }
    }

    fn finish(self) -> KeyTree {
        let mut keys: Vec<KeyEntry> = self
            .keys
            .into_iter()
            .map(|(name, count, acc)| KeyEntry { name, count, tree: acc.finish() })
            .collect();
        keys.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
        KeyTree { keys, items: self.items.map(|a| Box::new(a.finish())) }
    }
}

/// 从若干行样本(JSON 文本)推断键结构;解析不了的样本跳过。
pub fn infer_keys<'a>(samples: impl IntoIterator<Item = &'a str>) -> KeyTree {
    let mut acc = Acc::default();
    for s in samples {
        if let Ok(node) = parse(s) {
            acc.add(&node, 0);
        }
    }
    acc.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> PathSeg {
        PathSeg::Key(s.into())
    }

    #[test]
    fn parses_losslessly_in_order() {
        let n = parse(r#" {"b": 1.10, "a": [true, null, "x\"y\u00e9\ud83d\ude00"], "big": 123456789012345678901234567890} "#).unwrap();
        let Node::Object(es) = &n else { panic!() };
        assert_eq!(es.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), vec!["b", "a", "big"]);
        assert_eq!(n.at(&[k("b")]), Some(&Node::Number("1.10".into())));
        assert_eq!(n.at(&[k("a"), PathSeg::Index(2)]), Some(&Node::String("x\"yé😀".into())));
        assert_eq!(to_text(&n, false), r#"{"b":1.10,"a":[true,null,"x\"yé😀"],"big":123456789012345678901234567890}"#);
        assert_eq!(to_text(&parse("[]").unwrap(), true), "[]");
        assert_eq!(
            to_text(&parse(r#"{"a":{"b":[1,{}]},"c":"\n"}"#).unwrap(), true),
            "{\n  \"a\": {\n    \"b\": [\n      1,\n      {}\n    ]\n  },\n  \"c\": \"\\n\"\n}"
        );
        // 孤立代理换成替换字符,不报错
        assert_eq!(parse(r#""\ud800x""#).unwrap(), Node::String("\u{FFFD}x".into()));
    }

    #[test]
    fn reports_error_position() {
        let e = parse("{\n  \"a\": 1,\n  \"b\" 2\n}").unwrap_err();
        assert_eq!((e.kind, e.line, e.column, e.found.as_deref()), (ErrorKind::Unexpected, 3, 7, Some("2")));
        assert_eq!(parse("[1, 2").unwrap_err().kind, ErrorKind::Eof);
        assert_eq!(parse("{} x").unwrap_err().kind, ErrorKind::Trailing);
        assert_eq!(parse(r#""a\q""#).unwrap_err().kind, ErrorKind::Escape);
        assert_eq!(parse("01").unwrap_err().kind, ErrorKind::Number);
        assert_eq!(parse("1.").unwrap_err().kind, ErrorKind::Number);
        assert_eq!(parse("\"a\nb\"").unwrap_err().kind, ErrorKind::Control);
        assert_eq!(parse("{'a': 1}").unwrap_err().kind, ErrorKind::Unexpected);
        assert_eq!(parse(&"[".repeat(MAX_DEPTH + 1)).unwrap_err().kind, ErrorKind::TooDeep);
        // 列号按字符计(中文不按字节)
        assert_eq!(parse("[\"中文\" x]").unwrap_err().column, 7);
    }

    #[test]
    fn text_like_pg_arrow_arrow() {
        let n = parse(r#"{"s":"a","n":3,"b":false,"z":null,"o":{"x":[1, 2]}}"#).unwrap();
        assert_eq!(n.at(&[k("s")]).unwrap().as_text().as_deref(), Some("a"));
        assert_eq!(n.at(&[k("n")]).unwrap().as_text().as_deref(), Some("3"));
        assert_eq!(n.at(&[k("b")]).unwrap().as_text().as_deref(), Some("false"));
        assert_eq!(n.at(&[k("z")]).unwrap().as_text(), None);
        assert_eq!(n.at(&[k("o")]).unwrap().as_text().as_deref(), Some(r#"{"x":[1,2]}"#));
        assert!(n.at(&[k("s"), k("x")]).is_none());
        assert!(n.at(&[k("o"), k("x"), PathSeg::Index(5)]).is_none());
    }

    #[test]
    fn paths_escape_keys() {
        let path = vec![k("a"), PathSeg::Index(0), k("it's"), k("x y")];
        assert_eq!(pg_path("\"doc\"", &path, true), "\"doc\"->'a'->0->'it''s'->>'x y'");
        assert_eq!(pg_path("\"doc\"", &path[..1], false), "\"doc\"->'a'");
        assert_eq!(dollar_path(&path), "$.a[0].\"it's\".\"x y\"");
        assert_eq!(dollar_path(&[k("a\"b")]), "$.\"a\\\"b\"");
        let doc = containment_doc(&[k("a"), k("b'\"")], &Node::Number("3".into())).unwrap();
        assert_eq!(doc, r#"{"a":{"b'\"":3}}"#);
        assert!(containment_doc(&[k("a"), PathSeg::Index(1)], &Node::Null).is_none());
    }

    #[test]
    fn typed_values() {
        assert_eq!(typed_value(Kind::Number, " -1.5e3 "), Some(Node::Number("-1.5e3".into())));
        assert_eq!(typed_value(Kind::Number, "1."), None);
        assert_eq!(typed_value(Kind::Number, "abc"), None);
        assert_eq!(typed_value(Kind::Bool, "TRUE"), Some(Node::Bool(true)));
        assert_eq!(typed_value(Kind::Bool, "yes"), None);
        assert_eq!(typed_value(Kind::String, " a "), Some(Node::String(" a ".into())));
    }

    #[test]
    fn path_seg_from_js_numbers() {
        let p: Vec<PathSeg> = serde_json::from_str(r#"["a", 0, 2.0, "3"]"#).unwrap();
        assert_eq!(p, vec![k("a"), PathSeg::Index(0), PathSeg::Index(2), k("3")]);
        assert!(serde_json::from_str::<Vec<PathSeg>>("[-1]").is_err());
        assert!(serde_json::from_str::<Vec<PathSeg>>("[1.5]").is_err());
    }

    #[test]
    fn infers_keys_by_frequency_and_nesting() {
        let samples = [
            r#"{"user": {"name": "a", "tags": ["x"]}, "items": [{"sku": 1}, {"sku": 2, "qty": 1}]}"#,
            r#"{"user": {"name": "b", "age": 3}, "status": "ok"}"#,
            "not json",
            r#"[{"only": 1}]"#,
        ];
        let tree = infer_keys(samples);
        let names = |t: &KeyTree| t.keys.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&tree), vec!["user", "items", "status"]);
        assert_eq!(tree.keys[0].count, 2);
        assert_eq!(names(tree.at(&[k("user")]).unwrap()), vec!["name", "age", "tags"]);
        assert_eq!(names(tree.at(&[k("items"), PathSeg::Index(0)]).unwrap()), vec!["sku", "qty"]);
        // 顶层是数组的样本:合并进 items
        assert_eq!(names(tree.at(&[PathSeg::Index(3)]).unwrap()), vec!["only"]);
        assert!(tree.at(&[k("nope")]).is_none());
        assert!(tree.at(&[k("status")]).unwrap().keys.is_empty());
        let js = serde_json::to_value(&tree).unwrap();
        assert_eq!(js["keys"][0]["name"], "user");
        assert!(js["keys"][0]["tree"].get("items").is_none());
    }
}
