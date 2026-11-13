//! Redis 值的格式识别与解码(只读查看用):JSON、MessagePack、gzip / zlib(解压后再识别内层)、
//! Java 序列化、无 schema 的 Protobuf,认不出时按文本或十六进制展示。
//!
//! 树形格式一律转成 [`json::Node`] 再输出 JSON 文本:界面复用 JSON 树展示,键顺序保持原样。
//! 任何一步失败都不报错,回退到文本(合法 UTF-8 时)或十六进制。
//!
//! 各解码器的上界(防「小输入 → 超大输出 / 超长时间」):
//! - 压缩:每层解压后最多 [`MAX_INFLATED`] 字节,最多 [`MAX_LAYERS`] 层;
//! - JSON:先一遍扫描数节点与深度,超过 [`MAX_NODES`] / [`MAX_DEPTH`] 不建树;
//! - MessagePack:先不建值地扫描(循环次数 ≤ 节点数),超过 [`MAX_NODES`] / [`MAX_DEPTH`] 不交给 rmpv;
//! - Protobuf:所有递归层级共用 [`MAX_NODES`] 个字段的预算,同号字段 HashMap 定位,耗时 O(输入 × 深度);
//! - Java:产出节点 ≤ [`MAX_NODES`]、累计输出 ≤ [`MAX_JAVA_OUTPUT`],句柄里的名字与字符串截断到 [`MAX_REF_CHARS`];
//! - 十六进制:只转储前 [`MAX_HEX`] 字节。
//!
//! 树的格式化输出受节点数与深度共同约束。

use crate::json::{self, Node};
use serde::{Deserialize, Serialize};
use std::io::Read;

/// 解压后的大小上限:防解压炸弹(几 KB 的 gzip 可以解出几 GB)。
pub const MAX_INFLATED: usize = 10 << 20;
/// 嵌套上限(Protobuf 子消息、MessagePack / Java 对象的嵌套、压缩层数共用):防恶意深层嵌套撑爆栈。
pub const MAX_DEPTH: usize = 64;
/// 压缩最多解几层(gzip 里再套 zlib 这类):正常数据一两层就到底。
const MAX_LAYERS: usize = 4;
/// 十六进制转储最多展示的字节数:每 16 字节一行约 80 个字符,64 KB 约 33 万字符,再多界面也看不过来。
const MAX_HEX: usize = 64 << 10;
/// 树形格式最多的节点数(各层递归合计):JSON 树在界面上最多也只渲染一千多行,再多只是撑内存;
/// 配合 [`MAX_DEPTH`],格式化输出的缩进也有界(最坏约 100k × 64 层 × 2 空格 ≈ 13 MB)。
const MAX_NODES: usize = 100_000;
/// Java 解析累计输出的字节数上限:引用会复制句柄里的值,小输入可以放大出大输出,超出就停,退成摘要。
const MAX_JAVA_OUTPUT: usize = 4 << 20;
/// 句柄里存的类名 / 常量名 / 字符串的最大字符数(被引用时复制的就是它)。
const MAX_REF_CHARS: usize = 128;

/// 查看格式。`Auto` 为自动识别;压缩格式(gzip / zlib)解压后再自动识别内层。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Auto,
    Text,
    Hex,
    Json,
    Msgpack,
    Gzip,
    Zlib,
    Java,
    Protobuf,
}

/// 附注(界面按它取多语言文案)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Note {
    /// 十六进制只展示了前 [`MAX_HEX`] 字节
    HexTruncated,
    /// 解压后超过 [`MAX_INFLATED`],没有解开
    InflateLimit,
    /// Java 序列化没能完整解析(遇到自定义 writeExternal、异常对象、超出输出上限等),展示的是结构摘要
    JavaPartial,
    /// 节点数超过 [`MAX_NODES`] 或嵌套超过 [`MAX_DEPTH`],不展开成树
    TooLarge,
}

/// 解码结果。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Decoded {
    /// 最终展示的格式:text / hex / json / msgpack / java / protobuf
    pub format: Format,
    /// 依次解开的压缩层(gzip / zlib)
    pub layers: Vec<Format>,
    /// 「文本」视图的内容:文本原样、树形格式为格式化的 JSON、十六进制为转储
    pub text: String,
    /// 「树」视图的 JSON 文本;不是树形格式为 None
    pub tree: Option<String>,
    /// 手动选的格式解不出来,回退到了文本 / 十六进制
    pub fallback: bool,
    pub notes: Vec<Note>,
    /// 展示内容的字节数(解压后)
    pub size: usize,
}

/// 按 `want` 解码;解不出来回退到文本 / 十六进制(`fallback` 为 true),从不报错。
pub fn decode(bytes: &[u8], want: Format) -> Decoded {
    let mut cx = Cx::default();
    let done = match want {
        Format::Auto => Some(auto(bytes, &mut cx)),
        Format::Text => Some(text_view(bytes)),
        Format::Hex => Some(hex_view(bytes, &mut cx)),
        Format::Json => json_view(bytes, &mut cx),
        Format::Msgpack => msgpack_view(bytes, &mut cx),
        Format::Java => java_view(bytes, &mut cx),
        Format::Protobuf => protobuf_view(bytes, &mut cx),
        Format::Gzip | Format::Zlib => match inflate(bytes, want) {
            Ok(inner) => {
                cx.layers.push(want);
                Some(auto(&inner, &mut cx))
            }
            Err(limit) => {
                if limit {
                    cx.notes.push(Note::InflateLimit);
                }
                None
            }
        },
    };
    let fallback = done.is_none();
    let v = done.unwrap_or_else(|| {
        cx.layers.clear();
        plain(bytes, &mut cx)
    });
    Decoded { format: v.format, layers: cx.layers, text: v.text, tree: v.tree, fallback, notes: cx.notes, size: v.size }
}

/// 界面传来的单元格:`b64` 为原始字节(非 UTF-8 单元格,见 KeyValue 的 `raw`),没有时用 `text` 本身的字节。
pub fn decode_cell(text: &str, b64: Option<&str>, want: Format) -> Result<Decoded, String> {
    use base64::Engine;
    let bytes = match b64 {
        Some(b) => base64::engine::general_purpose::STANDARD.decode(b).map_err(|e| e.to_string())?,
        None => text.as_bytes().to_vec(),
    };
    Ok(decode(&bytes, want))
}

#[derive(Default)]
struct Cx {
    layers: Vec<Format>,
    notes: Vec<Note>,
}

struct View {
    format: Format,
    text: String,
    tree: Option<String>,
    size: usize,
}

impl View {
    fn tree(format: Format, node: &Node, size: usize) -> Self {
        let pretty = json::to_text(node, true);
        Self { format, text: pretty.clone(), tree: Some(pretty), size }
    }
}

/// 自动识别:先剥压缩层,再按「特征明确的在前、容易误判的在后」依次尝试。
/// 合法且可打印的 UTF-8 优先当文本:Protobuf / MessagePack 对普通文本也常常能「解」出东西来。
fn auto(bytes: &[u8], cx: &mut Cx) -> View {
    if cx.layers.len() < MAX_LAYERS {
        for f in [Format::Gzip, Format::Zlib] {
            if !has_magic(bytes, f) {
                continue;
            }
            match inflate(bytes, f) {
                Ok(inner) => {
                    cx.layers.push(f);
                    return auto(&inner, cx);
                }
                Err(true) => cx.notes.push(Note::InflateLimit),
                Err(false) => {}
            }
        }
    }
    if bytes.starts_with(&JAVA_MAGIC) {
        if let Some(v) = java_view(bytes, cx) {
            return v;
        }
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        if json::looks_like_document(s) {
            if let Some(v) = json_view(bytes, cx) {
                return v;
            }
        }
        if printable(s) {
            return text_view(bytes);
        }
    }
    if let Some(node) = msgpack(bytes, cx).filter(|(_, structured)| *structured).map(|(n, _)| n) {
        return View::tree(Format::Msgpack, &node, bytes.len());
    }
    if let Some(v) = protobuf_view(bytes, cx) {
        return v;
    }
    plain(bytes, cx)
}

/// 兜底:合法 UTF-8 为文本,否则十六进制。
fn plain(bytes: &[u8], cx: &mut Cx) -> View {
    if std::str::from_utf8(bytes).is_ok() {
        text_view(bytes)
    } else {
        hex_view(bytes, cx)
    }
}

/// 文本里没有控制字符(换行、制表、回车除外)。
fn printable(s: &str) -> bool {
    !s.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
}

fn text_view(bytes: &[u8]) -> View {
    View { format: Format::Text, text: String::from_utf8_lossy(bytes).into_owned(), tree: None, size: bytes.len() }
}

// ---------------------------------------------------------------- 十六进制

fn hex_view(bytes: &[u8], cx: &mut Cx) -> View {
    if bytes.len() > MAX_HEX {
        cx.notes.push(Note::HexTruncated);
    }
    View { format: Format::Hex, text: hex_dump(&bytes[..bytes.len().min(MAX_HEX)]), tree: None, size: bytes.len() }
}

/// `00000000  1f 8b 08 00 …  00 03 ab 56 …  |...V|`:偏移、16 字节十六进制(8 字节一组)、可见 ASCII。
pub fn hex_dump(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() / 16 * 78 + 78);
    for (i, line) in bytes.chunks(16).enumerate() {
        out.push_str(&format!("{:08x} ", i * 16));
        for j in 0..16 {
            if j == 8 {
                out.push(' ');
            }
            match line.get(j) {
                Some(b) => out.push_str(&format!(" {b:02x}")),
                None => out.push_str("   "),
            }
        }
        out.push_str("  |");
        out.extend(line.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }));
        out.push_str("|\n");
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------- JSON

fn json_view(bytes: &[u8], cx: &mut Cx) -> Option<View> {
    let text = std::str::from_utf8(bytes).ok()?;
    // 先粗数一遍再解析:解析器自己允许 512 层,节点也不设上限,大文档建树既占内存又让缩进输出膨胀
    let (nodes, depth) = json_shape(bytes);
    if nodes > MAX_NODES || depth > MAX_DEPTH {
        if json::parse(text).is_ok() {
            cx.notes.push(Note::TooLarge);
        }
        return None;
    }
    let node = json::parse(text).ok()?;
    Some(View::tree(Format::Json, &node, bytes.len()))
}

/// JSON 文本节点数的上界(字符串外的 `,` `[` `{` 个数 + 1)与最大嵌套深度,一遍扫描,不建树。
fn json_shape(b: &[u8]) -> (usize, usize) {
    let (mut nodes, mut depth, mut max) = (1usize, 0usize, 0usize);
    let (mut in_str, mut esc) = (false, false);
    for &c in b {
        if in_str {
            match c {
                _ if esc => esc = false,
                b'\\' => esc = true,
                b'"' => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b',' => nodes += 1,
            b'[' | b'{' => {
                nodes += 1;
                depth += 1;
                max = max.max(depth);
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    (nodes, max)
}

// ---------------------------------------------------------------- 压缩

fn has_magic(b: &[u8], f: Format) -> bool {
    match f {
        Format::Gzip => b.starts_with(&[0x1f, 0x8b]),
        // zlib 头:CM = 8(deflate)、窗口不超过 32K,且 CMF·256 + FLG 是 31 的倍数(RFC 1950)
        Format::Zlib => b.len() >= 2 && b[0] & 0x0f == 8 && b[0] >> 4 <= 7 && (u16::from(b[0]) << 8 | u16::from(b[1])) % 31 == 0,
        _ => false,
    }
}

/// 解压;`Err(true)` 为超出 [`MAX_INFLATED`],`Err(false)` 为数据不对(截断、不是该格式)。
/// 多读一个字节判断是否超限,不会把超限的内容整个解出来。
fn inflate(bytes: &[u8], f: Format) -> Result<Vec<u8>, bool> {
    let limit = MAX_INFLATED as u64 + 1;
    let mut out = vec![];
    let r = match f {
        Format::Gzip => flate2::read::MultiGzDecoder::new(bytes).take(limit).read_to_end(&mut out),
        Format::Zlib => flate2::read::ZlibDecoder::new(bytes).take(limit).read_to_end(&mut out),
        _ => return Err(false),
    };
    match r {
        Ok(_) if out.len() > MAX_INFLATED => Err(true),
        Ok(_) => Ok(out),
        Err(_) => Err(false),
    }
}

// ---------------------------------------------------------------- MessagePack

fn msgpack_view(bytes: &[u8], cx: &mut Cx) -> Option<View> {
    msgpack(bytes, cx).map(|(node, _)| View::tree(Format::Msgpack, &node, bytes.len()))
}

/// 整段恰好是一个 MessagePack 值时返回它,以及它是否「有结构」(map / array / str / bin / ext)。
/// 自动识别只认有结构的:任意一个字节都是合法的 fixint,单个数字太容易误判。
fn msgpack(bytes: &[u8], cx: &mut Cx) -> Option<(Node, bool)> {
    if bytes.is_empty() {
        return None;
    }
    // rmpv 每个值要几十字节内存:1 字节一个的 nil 数组,10 MB 输入就是几亿字节。先不建值地数一遍,
    // 顺带校验恰好一个值、长度对得上,超出节点上限就不解
    match mp_scan(bytes) {
        Scan::Ok => {}
        Scan::TooLarge => {
            cx.notes.push(Note::TooLarge);
            return None;
        }
        Scan::Invalid => return None,
    }
    let mut rd = bytes;
    let v = rmpv::decode::read_value_with_max_depth(&mut rd, MAX_DEPTH).ok()?;
    if !rd.is_empty() {
        return None;
    }
    use rmpv::Value as V;
    let structured = matches!(v, V::Map(_) | V::Array(_) | V::String(_) | V::Binary(_) | V::Ext(..));
    Some((mp_node(&v), structured))
}

enum Scan {
    Ok,
    Invalid,
    TooLarge,
}

/// 不建值地走一遍 MessagePack:整段恰好是一个值、节点数不超过 [`MAX_NODES`]、嵌套不超过 [`MAX_DEPTH`]。
/// 循环次数不超过节点数,不递归。
fn mp_scan(b: &[u8]) -> Scan {
    // 每层还剩几个值要读(数组 n 个、map 2n 个)
    let mut stack: Vec<u64> = vec![1];
    let mut i = 0usize;
    let mut nodes = 0usize;
    let be = |b: &[u8], i: usize, n: usize| -> Option<u64> {
        let s = b.get(i..i.checked_add(n)?)?;
        Some(s.iter().fold(0u64, |a, &x| a << 8 | u64::from(x)))
    };
    while let Some(top) = stack.last_mut() {
        if *top == 0 {
            stack.pop();
            continue;
        }
        *top -= 1;
        nodes += 1;
        if nodes > MAX_NODES {
            return Scan::TooLarge;
        }
        let Some(&m) = b.get(i) else { return Scan::Invalid };
        i += 1;
        // (负载字节数, 子值个数)
        let (skip, kids): (u64, u64) = match m {
            0x00..=0x7f | 0xe0..=0xff | 0xc0 | 0xc2 | 0xc3 => (0, 0),
            0x80..=0x8f => (0, u64::from(m & 0x0f) * 2),
            0x90..=0x9f => (0, u64::from(m & 0x0f)),
            0xa0..=0xbf => (u64::from(m & 0x1f), 0),
            0xc4 | 0xd9 => match be(b, i, 1) { Some(n) => { i += 1; (n, 0) } None => return Scan::Invalid },
            0xc5 | 0xda => match be(b, i, 2) { Some(n) => { i += 2; (n, 0) } None => return Scan::Invalid },
            0xc6 | 0xdb => match be(b, i, 4) { Some(n) => { i += 4; (n, 0) } None => return Scan::Invalid },
            0xc7 => match be(b, i, 1) { Some(n) => { i += 1; (n + 1, 0) } None => return Scan::Invalid },
            0xc8 => match be(b, i, 2) { Some(n) => { i += 2; (n + 1, 0) } None => return Scan::Invalid },
            0xc9 => match be(b, i, 4) { Some(n) => { i += 4; (n + 1, 0) } None => return Scan::Invalid },
            0xca => (4, 0),
            0xcb => (8, 0),
            0xcc | 0xd0 => (1, 0),
            0xcd | 0xd1 => (2, 0),
            0xce | 0xd2 => (4, 0),
            0xcf | 0xd3 => (8, 0),
            0xd4 => (2, 0),
            0xd5 => (3, 0),
            0xd6 => (5, 0),
            0xd7 => (9, 0),
            0xd8 => (17, 0),
            0xdc => match be(b, i, 2) { Some(n) => { i += 2; (0, n) } None => return Scan::Invalid },
            0xdd => match be(b, i, 4) { Some(n) => { i += 4; (0, n) } None => return Scan::Invalid },
            0xde => match be(b, i, 2) { Some(n) => { i += 2; (0, n * 2) } None => return Scan::Invalid },
            0xdf => match be(b, i, 4) { Some(n) => { i += 4; (0, n * 2) } None => return Scan::Invalid },
            _ => return Scan::Invalid, // 0xc1 保留
        };
        i = match usize::try_from(skip).ok().and_then(|s| i.checked_add(s)) {
            Some(n) if n <= b.len() => n,
            _ => return Scan::Invalid,
        };
        if kids > 0 {
            // 声明的子值比剩余字节还多:每个值至少 1 字节,必然解不完
            if kids > (b.len() - i) as u64 {
                return Scan::Invalid;
            }
            if stack.len() >= MAX_DEPTH {
                return Scan::TooLarge;
            }
            stack.push(kids);
        }
    }
    if i == b.len() { Scan::Ok } else { Scan::Invalid }
}

fn mp_node(v: &rmpv::Value) -> Node {
    use rmpv::Value as V;
    match v {
        V::Nil => Node::Null,
        V::Boolean(b) => Node::Bool(*b),
        V::Integer(i) => Node::Number(i.to_string()),
        V::F32(f) => float(f64::from(*f)),
        V::F64(f) => float(*f),
        V::String(s) => match s.as_str() {
            Some(s) => Node::String(s.to_string()),
            None => Node::String(format!("0x{}", hex(s.as_bytes()))),
        },
        V::Binary(b) => Node::String(format!("0x{}", hex(b))),
        V::Array(items) => Node::Array(items.iter().map(mp_node).collect()),
        // 键不一定是字符串:非字符串键写成它的紧凑 JSON
        V::Map(es) => Node::Object(
            es.iter()
                .map(|(k, v)| {
                    let key = match mp_node(k) {
                        Node::String(s) => s,
                        other => json::to_text(&other, false),
                    };
                    (key, mp_node(v))
                })
                .collect(),
        ),
        V::Ext(ty, data) => Node::Object(vec![
            ("$ext".into(), Node::Number(ty.to_string())),
            ("data".into(), Node::String(format!("0x{}", hex(data)))),
        ]),
    }
}

/// JSON 没有 NaN / Infinity:写成字符串。
fn float(f: f64) -> Node {
    if f.is_finite() {
        Node::Number(format!("{f:?}"))
    } else {
        Node::String(f.to_string())
    }
}

// ---------------------------------------------------------------- Protobuf(无 schema)

fn protobuf_view(bytes: &[u8], cx: &mut Cx) -> Option<View> {
    let mut budget = MAX_NODES;
    let node = pb_message(bytes, 0, &mut budget);
    if budget == 0 {
        cx.notes.push(Note::TooLarge);
        return None;
    }
    Some(View::tree(Format::Protobuf, &node?, bytes.len()))
}

fn varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let mut v: u64 = 0;
    for shift in (0..64).step_by(7) {
        let byte = *b.get(*i)?;
        *i += 1;
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(v);
        }
    }
    None
}

/// 没有 schema 只能按 wire format 解:字段号为键(同号重复出现收成数组,位置按首次出现);
/// varint 按无符号整数(sint 的 zigzag、负数的补码看不出来),fixed32 / fixed64 按无符号整数;
/// 长度前缀的字段依次尝试:可打印文本 → 子消息(递归,最多 [`MAX_DEPTH`] 层)→ 十六进制。
/// 文本优先:短文本经常恰好也是合法的 wire format。整段必须恰好解完且至少一个字段,否则不是 Protobuf。
/// 已废弃的 group(wire type 3 / 4)不支持,遇到当作解不出。
///
/// 界:`budget` 是所有递归层级共用的字段预算(子消息尝试失败也算),用完即整体失败;每层每个字节最多扫一遍,
/// 总耗时 O(输入 × [`MAX_DEPTH`])。同号字段用 HashMap 定位,字段号很多时不退化成平方。
fn pb_message(b: &[u8], depth: usize, budget: &mut usize) -> Option<Node> {
    if b.is_empty() || depth >= MAX_DEPTH {
        return None;
    }
    let mut fields: Vec<(String, Node)> = vec![];
    let mut index: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    let mut i = 0;
    while i < b.len() {
        if *budget == 0 {
            return None;
        }
        *budget -= 1;
        let key = varint(b, &mut i)?;
        let (no, wire) = (key >> 3, key & 7);
        if no == 0 || no > 0x1fff_ffff {
            return None;
        }
        let value = match wire {
            0 => Node::Number(varint(b, &mut i)?.to_string()),
            1 => {
                let bytes: [u8; 8] = b.get(i..i + 8)?.try_into().ok()?;
                i += 8;
                Node::Number(u64::from_le_bytes(bytes).to_string())
            }
            5 => {
                let bytes: [u8; 4] = b.get(i..i + 4)?.try_into().ok()?;
                i += 4;
                Node::Number(u32::from_le_bytes(bytes).to_string())
            }
            2 => {
                let len = usize::try_from(varint(b, &mut i)?).ok()?;
                let end = i.checked_add(len)?;
                let body = b.get(i..end)?;
                i = end;
                match std::str::from_utf8(body) {
                    Ok(s) if printable(s) => Node::String(s.to_string()),
                    _ => pb_message(body, depth + 1, budget).unwrap_or_else(|| Node::String(format!("0x{}", hex(body)))),
                }
            }
            _ => return None,
        };
        match index.get(&no) {
            Some(&at) => match &mut fields[at].1 {
                Node::Array(items) => items.push(value),
                first => {
                    let prev = std::mem::replace(first, Node::Null);
                    *first = Node::Array(vec![prev, value]);
                }
            },
            None => {
                index.insert(no, fields.len());
                fields.push((no.to_string(), value));
            }
        }
    }
    Some(Node::Object(fields))
}

// ---------------------------------------------------------------- Java 序列化

const JAVA_MAGIC: [u8; 4] = [0xac, 0xed, 0x00, 0x05];

/// Java 对象序列化流(ObjectOutputStream)的解析:按协议读出类描述(类名、字段名与类型)与字段值,
/// 转成 `{ "$class": 类名, 字段: 值… }`。自定义 writeObject 写的附加数据(HashMap、ArrayList 的元素就在这里)
/// 放在 `$data` 里:块数据为十六进制,对象照常解析。
///
/// 完整还原做不到(writeExternal 的内容没有自描述、异常对象、类重置等):解析中途失败时退成结构摘要,
/// 列出已经读到的类与字段声明,文本视图给十六进制,并附 [`Note::JavaPartial`]。
fn java_view(bytes: &[u8], cx: &mut Cx) -> Option<View> {
    if !bytes.starts_with(&JAVA_MAGIC) {
        return None;
    }
    let mut p = JavaParser { b: bytes, i: 4, handles: vec![], classes: vec![], depth: 0, nodes: 0, out: 0 };
    let mut items = vec![];
    let result = loop {
        if p.i >= bytes.len() {
            break Ok(());
        }
        match p.content() {
            Ok(n) => items.push(n),
            Err(()) => break Err(()),
        }
    };
    match result {
        Ok(()) if !items.is_empty() => {
            let node = if items.len() == 1 { items.remove(0) } else { Node::Array(items) };
            Some(View::tree(Format::Java, &node, bytes.len()))
        }
        _ => {
            cx.notes.push(Note::JavaPartial);
            let classes = p
                .classes
                .iter()
                .map(|(name, fields)| {
                    Node::Object(vec![
                        ("class".into(), Node::String(name.clone())),
                        ("fields".into(), Node::Array(fields.iter().cloned().map(Node::String).collect())),
                    ])
                })
                .collect();
            let summary = Node::Object(vec![
                ("classes".into(), Node::Array(classes)),
                ("parsedBytes".into(), Node::Number(p.i.min(bytes.len()).to_string())),
                ("totalBytes".into(), Node::Number(bytes.len().to_string())),
            ]);
            let shown = &bytes[..bytes.len().min(MAX_HEX)];
            if bytes.len() > MAX_HEX {
                cx.notes.push(Note::HexTruncated);
            }
            Some(View { format: Format::Java, text: hex_dump(shown), tree: Some(json::to_text(&summary, true)), size: bytes.len() })
        }
    }
}

#[derive(Debug, Clone)]
struct JavaField {
    code: u8,
    name: String,
}

#[derive(Debug)]
struct ClassDesc {
    name: String,
    flags: u8,
    fields: Vec<JavaField>,
    sup: Option<std::rc::Rc<ClassDesc>>,
    /// 连同自己在内的继承链长度。父类可以经 TC_REFERENCE 引用已登记的类描述,不增加嵌套深度,
    /// 链能被拉得很长;每个对象都要沿链走一遍,不设上限时「对象数 × 链长」会绕过节点预算。
    chain: usize,
}

/// 继承链长度:超过 [`MAX_DEPTH`] 直接判为无法解析,保证每个对象沿链的工作量有界。
fn chain_len(sup: &Option<std::rc::Rc<ClassDesc>>) -> Result<usize, ()> {
    let n = sup.as_ref().map_or(0, |d| d.chain) + 1;
    if n > MAX_DEPTH { Err(()) } else { Ok(n) }
}

/// 句柄表里的东西:被 TC_REFERENCE 引用时,类描述要拿来解析后续数据,其余只需要一个可读的写法。
enum Handle {
    Desc(std::rc::Rc<ClassDesc>),
    Value(Node),
}

struct JavaParser<'a> {
    b: &'a [u8],
    i: usize,
    handles: Vec<Handle>,
    /// 读到过的类与字段声明(`int id`、`java.lang.String name`):解析失败时的摘要
    classes: Vec<(String, Vec<String>)>,
    depth: usize,
    /// 已产出的节点数与(粗算的)输出字节数,见 [`JavaParser::spend`]
    nodes: usize,
    out: usize,
}

type JResult<T> = Result<T, ()>;

const TC_NULL: u8 = 0x70;
const TC_REFERENCE: u8 = 0x71;
const TC_CLASSDESC: u8 = 0x72;
const TC_OBJECT: u8 = 0x73;
const TC_STRING: u8 = 0x74;
const TC_ARRAY: u8 = 0x75;
const TC_CLASS: u8 = 0x76;
const TC_BLOCKDATA: u8 = 0x77;
const TC_ENDBLOCKDATA: u8 = 0x78;
const TC_BLOCKDATALONG: u8 = 0x7a;
const TC_LONGSTRING: u8 = 0x7c;
const TC_PROXYCLASSDESC: u8 = 0x7d;
const TC_ENUM: u8 = 0x7e;
const BASE_HANDLE: u32 = 0x7e_0000;
const SC_WRITE_METHOD: u8 = 0x01;
const SC_SERIALIZABLE: u8 = 0x02;
const SC_EXTERNALIZABLE: u8 = 0x04;
const SC_BLOCK_DATA: u8 = 0x08;

impl JavaParser<'_> {
    fn take(&mut self, n: usize) -> JResult<&[u8]> {
        let end = self.i.checked_add(n).ok_or(())?;
        let s = self.b.get(self.i..end).ok_or(())?;
        self.i = end;
        Ok(s)
    }
    fn u8(&mut self) -> JResult<u8> {
        Ok(self.take(1)?[0])
    }
    fn peek(&self) -> JResult<u8> {
        self.b.get(self.i).copied().ok_or(())
    }
    fn u16(&mut self) -> JResult<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().map_err(|_| ())?))
    }
    fn u32(&mut self) -> JResult<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().map_err(|_| ())?))
    }
    fn u64(&mut self) -> JResult<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().map_err(|_| ())?))
    }
    /// modified UTF-8(与标准 UTF-8 只差 \0 与增补字符的写法),按普通 UTF-8 宽松解码。
    fn utf(&mut self) -> JResult<String> {
        let n = self.u16()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn long_utf(&mut self) -> JResult<String> {
        let n = usize::try_from(self.u64()?).map_err(|_| ())?;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }

    /// 记一笔产出:节点数超过 [`MAX_NODES`] 或输出超过 [`MAX_JAVA_OUTPUT`] 就失败(退成摘要)。
    /// 读新数据的产出不超过输入,但引用会复制句柄里的值,不记账的话小输入能放大出大输出。
    fn spend(&mut self, bytes: usize) -> JResult<()> {
        self.nodes += 1;
        self.out = self.out.saturating_add(bytes);
        if self.nodes > MAX_NODES || self.out > MAX_JAVA_OUTPUT {
            Err(())
        } else {
            Ok(())
        }
    }

    fn enter(&mut self) -> JResult<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            Err(())
        } else {
            Ok(())
        }
    }

    fn handle(&mut self, h: Handle) -> usize {
        self.handles.push(h);
        self.handles.len() - 1
    }

    fn reference(&mut self) -> JResult<&Handle> {
        let h = self.u32()?.checked_sub(BASE_HANDLE).ok_or(())? as usize;
        self.handles.get(h).ok_or(())
    }

    /// 流里的一项:对象或块数据(块数据只会出现在顶层与 writeObject 的附加数据里)。
    fn content(&mut self) -> JResult<Node> {
        match self.peek()? {
            TC_BLOCKDATA => {
                self.i += 1;
                let n = self.u8()? as usize;
                self.spend(n * 2)?;
                Ok(Node::String(format!("blockdata:{}", hex(self.take(n)?))))
            }
            TC_BLOCKDATALONG => {
                self.i += 1;
                let n = self.u32()? as usize;
                self.spend(n.saturating_mul(2))?;
                Ok(Node::String(format!("blockdata:{}", hex(self.take(n)?))))
            }
            _ => self.object(),
        }
    }

    fn object(&mut self) -> JResult<Node> {
        self.enter()?;
        let r = self.object_inner();
        self.depth -= 1;
        // 子节点在各自读出时已记过账,这里只记本节点自身(字符串内容、对象的键名)
        let n = r?;
        let own = match &n {
            Node::String(s) | Node::Number(s) => s.len(),
            Node::Object(es) => es.iter().map(|(k, _)| k.len() + 8).sum(),
            _ => 8,
        };
        self.spend(own)?;
        Ok(n)
    }

    fn object_inner(&mut self) -> JResult<Node> {
        match self.u8()? {
            TC_NULL => Ok(Node::Null),
            // 引用只给简短写法(对象 / 数组为 `<ref 类名>`,长字符串截断):整棵展开的话,
            // 层层互相引用的数据几 KB 就能展开成指数大小
            TC_REFERENCE => Ok(match self.reference()? {
                Handle::Value(n) => n.clone(),
                Handle::Desc(d) => Node::String(format!("class {}", d.name)),
            }),
            // 注:类描述的名字在登记时已截断(见 class_desc_inner),这里复制的都是短写法
            TC_STRING => {
                let s = self.utf()?;
                self.handle(Handle::Value(ref_string(&s)));
                Ok(Node::String(s))
            }
            TC_LONGSTRING => {
                let s = self.long_utf()?;
                self.handle(Handle::Value(ref_string(&s)));
                Ok(Node::String(s))
            }
            TC_CLASSDESC | TC_PROXYCLASSDESC => {
                self.i -= 1;
                let d = self.class_desc()?.ok_or(())?;
                Ok(Node::String(format!("class {}", d.name)))
            }
            TC_CLASS => {
                let d = self.class_desc()?;
                let n = ref_string(&format!("class {}", d.map(|d| d.name.clone()).unwrap_or_default()));
                self.handle(Handle::Value(n.clone()));
                Ok(n)
            }
            TC_ENUM => {
                let d = self.class_desc()?.ok_or(())?;
                let h = self.handle(Handle::Value(Node::Null));
                let constant = match self.object()? {
                    Node::String(s) => s,
                    _ => return Err(()),
                };
                let n = ref_string(&format!("{}.{}", short_name(&d.name), constant));
                self.handles[h] = Handle::Value(n.clone());
                Ok(n)
            }
            TC_ARRAY => {
                let d = self.class_desc()?.ok_or(())?;
                self.handle(Handle::Value(Node::String(format!("<ref {}>", d.name))));
                let n = self.u32()? as usize;
                // 元素类型取类名的第二个字符(`[I`、`[Ljava.lang.String;`、`[[B`)
                let code = d.name.as_bytes().get(1).copied().ok_or(())?;
                let node = if code == b'B' {
                    Node::String(format!("0x{}", hex(self.take(n)?)))
                } else {
                    // 不按声明的长度预分配:长度来自数据,可能是伪造的巨大值
                    let mut items = vec![];
                    for _ in 0..n {
                        items.push(self.value(code)?);
                    }
                    Node::Array(items)
                };
                Ok(node)
            }
            TC_OBJECT => {
                let d = self.class_desc()?.ok_or(())?;
                self.handle(Handle::Value(Node::String(format!("<ref {}>", d.name))));
                // 类层次从最上面的父类往下,各自写自己的字段
                let mut chain = vec![];
                let mut cur = Some(d.clone());
                while let Some(c) = cur {
                    cur = c.sup.clone();
                    chain.push(c);
                }
                let mut entries = vec![("$class".to_string(), Node::String(d.name.clone()))];
                for c in chain.iter().rev() {
                    if c.flags & SC_EXTERNALIZABLE != 0 {
                        // writeExternal 的内容:老协议(无块数据)没有边界,无从解析
                        if c.flags & SC_BLOCK_DATA == 0 {
                            return Err(());
                        }
                        entries.push(("$data".into(), Node::Array(self.annotation()?)));
                        continue;
                    }
                    if c.flags & SC_SERIALIZABLE == 0 {
                        continue;
                    }
                    for f in &c.fields {
                        let v = self.value(f.code)?;
                        entries.push((f.name.clone(), v));
                    }
                    if c.flags & SC_WRITE_METHOD != 0 {
                        let extra = self.annotation()?;
                        if !extra.is_empty() {
                            entries.push(("$data".into(), Node::Array(extra)));
                        }
                    }
                }
                Ok(Node::Object(entries))
            }
            // TC_EXCEPTION、TC_RESET 等:不处理
            _ => Err(()),
        }
    }

    /// 读到 TC_ENDBLOCKDATA 为止的附加内容。
    fn annotation(&mut self) -> JResult<Vec<Node>> {
        let mut out = vec![];
        while self.peek()? != TC_ENDBLOCKDATA {
            out.push(self.content()?);
        }
        self.i += 1;
        Ok(out)
    }

    /// 类描述:新的(登记句柄)、null,或引用之前的。
    fn class_desc(&mut self) -> JResult<Option<std::rc::Rc<ClassDesc>>> {
        self.enter()?;
        let r = self.class_desc_inner();
        self.depth -= 1;
        r
    }

    fn class_desc_inner(&mut self) -> JResult<Option<std::rc::Rc<ClassDesc>>> {
        match self.u8()? {
            TC_NULL => Ok(None),
            TC_REFERENCE => match self.reference()? {
                Handle::Desc(d) => Ok(Some(d.clone())),
                Handle::Value(_) => Err(()),
            },
            TC_CLASSDESC => {
                let name = self.utf()?;
                let _uid = self.u64()?;
                // 先占句柄:字段类型名里的字符串会登记在它后面
                let h = self.handle(Handle::Value(Node::Null));
                let flags = self.u8()?;
                let count = self.u16()?;
                let mut fields = vec![];
                let mut decl = vec![];
                for _ in 0..count {
                    let code = self.u8()?;
                    // 字段名每个对象都会复制一次:截断,防同一个类描述被大量对象引用时放大
                    let fname = short(&self.utf()?);
                    let ty = match code {
                        b'L' | b'[' => match self.object()? {
                            Node::String(s) => type_name(&s),
                            _ => return Err(()),
                        },
                        c => prim_name(c).ok_or(())?.to_string(),
                    };
                    decl.push(format!("{ty} {fname}"));
                    fields.push(JavaField { code, name: fname });
                }
                // 类名会出现在每个对象的 $class 与每次引用里:截断(数组元素类型只看前两个字符,不受影响)
                let name = short(&name);
                self.classes.push((name.clone(), decl));
                self.annotation()?;
                let sup = self.class_desc()?;
                let chain = chain_len(&sup)?;
                let d = std::rc::Rc::new(ClassDesc { name, flags, fields, sup, chain });
                self.handles[h] = Handle::Desc(d.clone());
                Ok(Some(d))
            }
            TC_PROXYCLASSDESC => {
                let h = self.handle(Handle::Value(Node::Null));
                let n = self.u32()?;
                let mut names = vec![];
                for _ in 0..n {
                    names.push(self.utf()?);
                }
                self.annotation()?;
                let sup = self.class_desc()?;
                let chain = chain_len(&sup)?;
                let d = std::rc::Rc::new(ClassDesc {
                    name: short(&format!("Proxy({})", names.join(", "))),
                    flags: SC_SERIALIZABLE,
                    fields: vec![],
                    sup,
                    chain,
                });
                self.handles[h] = Handle::Desc(d.clone());
                Ok(Some(d))
            }
            _ => Err(()),
        }
    }

    /// 一个字段值:基本类型按类型码读,对象 / 数组递归。
    fn value(&mut self, code: u8) -> JResult<Node> {
        if code != b'L' && code != b'[' {
            self.spend(8)?;
        }
        Ok(match code {
            b'B' => Node::Number((self.u8()? as i8).to_string()),
            b'C' => {
                let c = self.u16()?;
                Node::String(char::from_u32(u32::from(c)).map(String::from).unwrap_or_else(|| format!("\\u{c:04x}")))
            }
            b'D' => float(f64::from_bits(self.u64()?)),
            b'F' => float(f64::from(f32::from_bits(self.u32()?))),
            b'I' => Node::Number((self.u32()? as i32).to_string()),
            b'J' => Node::Number((self.u64()? as i64).to_string()),
            b'S' => Node::Number((self.u16()? as i16).to_string()),
            b'Z' => Node::Bool(self.u8()? != 0),
            b'L' | b'[' => self.object()?,
            _ => return Err(()),
        })
    }
}

fn prim_name(code: u8) -> Option<&'static str> {
    Some(match code {
        b'B' => "byte",
        b'C' => "char",
        b'D' => "double",
        b'F' => "float",
        b'I' => "int",
        b'J' => "long",
        b'S' => "short",
        b'Z' => "boolean",
        _ => return None,
    })
}

/// 字段的 JVM 类型签名转成源码写法:`Ljava/lang/String;` → `java.lang.String`,`[I` → `int[]`。
fn type_name(sig: &str) -> String {
    let dims = sig.chars().take_while(|&c| c == '[').count();
    let base = &sig[dims..];
    let name = match base.as_bytes().first() {
        Some(b'L') => base.trim_start_matches('L').trim_end_matches(';').replace('/', "."),
        Some(&c) => prim_name(c).map(str::to_string).unwrap_or_else(|| base.to_string()),
        None => String::new(),
    };
    format!("{name}{}", "[]".repeat(dims))
}

/// 截断到 [`MAX_REF_CHARS`] 个字符(超出的加 `…`)。
fn short(s: &str) -> String {
    match s.char_indices().nth(MAX_REF_CHARS) {
        None => s.to_string(),
        Some((at, _)) => format!("{}…", &s[..at]),
    }
}

/// 存进句柄表的值(被引用时会被复制):一律截断(见 TC_REFERENCE 处的说明)。
fn ref_string(s: &str) -> Node {
    Node::String(short(s))
}

fn short_name(class: &str) -> &str {
    class.rsplit('.').next().unwrap_or(class)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn gzip(b: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
        e.write_all(b).unwrap();
        e.finish().unwrap()
    }

    fn zlib(b: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::ZlibEncoder::new(vec![], flate2::Compression::default());
        e.write_all(b).unwrap();
        e.finish().unwrap()
    }

    fn tree(d: &Decoded) -> Node {
        json::parse(d.tree.as_deref().expect("应有树")).unwrap()
    }

    /// 路径:`#3` 为数组下标,其余为对象键
    fn at<'a>(n: &'a Node, path: &[&str]) -> &'a Node {
        let p: Vec<json::PathSeg> = path
            .iter()
            .map(|s| match s.strip_prefix('#') {
                Some(i) => json::PathSeg::Index(i.parse().unwrap()),
                None => json::PathSeg::Key(s.to_string()),
            })
            .collect();
        n.at(&p).unwrap_or_else(|| panic!("{path:?} 不存在:{}", json::to_text(n, false)))
    }

    fn s(v: &str) -> Node {
        Node::String(v.into())
    }

    fn num(v: &str) -> Node {
        Node::Number(v.into())
    }

    /// 手写的 MessagePack:{"name":"Ann","age":30,"tags":["a",true],"bin":<01 02>}
    fn mp_sample() -> Vec<u8> {
        let mut b = vec![0x84];
        b.extend([0xa4, b'n', b'a', b'm', b'e', 0xa3, b'A', b'n', b'n']);
        b.extend([0xa3, b'a', b'g', b'e', 30]);
        b.extend([0xa4, b't', b'a', b'g', b's', 0x92, 0xa1, b'a', 0xc3]);
        b.extend([0xa3, b'b', b'i', b'n', 0xc4, 2, 1, 2]);
        b
    }

    /// 手写的 Protobuf:1: 150、2: "hello"、3: { 1: 7 }、4: 1、4: 2(重复字段)、5: fixed32 1
    fn pb_sample() -> Vec<u8> {
        let mut b = vec![0x08, 0x96, 0x01];
        b.extend([0x12, 5]);
        b.extend(b"hello");
        b.extend([0x1a, 2, 0x08, 7]);
        b.extend([0x20, 1, 0x20, 2]);
        b.extend([0x2d, 1, 0, 0, 0]);
        b
    }

    #[test]
    fn plain_text_stays_text() {
        let d = decode("hello world\n第二行".as_bytes(), Format::Auto);
        assert_eq!(d.format, Format::Text);
        assert_eq!(d.text, "hello world\n第二行");
        assert!(d.tree.is_none() && !d.fallback && d.layers.is_empty());
        // 普通短文本也是合法的 wire format("hi" = 字段 13 的 varint),不能当成 Protobuf
        assert_eq!(decode(b"hi", Format::Auto).format, Format::Text);
        assert_eq!(decode(b"", Format::Auto).format, Format::Text);
    }

    #[test]
    fn json_keeps_key_order_and_pretty_prints() {
        let d = decode(br#"{"b":1,"a":[1.10,{"c":null}]}"#, Format::Auto);
        assert_eq!(d.format, Format::Json);
        assert_eq!(d.text, "{\n  \"b\": 1,\n  \"a\": [\n    1.10,\n    {\n      \"c\": null\n    }\n  ]\n}");
        assert_eq!(d.tree.as_deref(), Some(d.text.as_str()));
        // 像 JSON 但不合法:按文本
        let d = decode(b"{not json", Format::Auto);
        assert_eq!(d.format, Format::Text);
    }

    #[test]
    fn msgpack_decodes_to_ordered_tree() {
        let d = decode(&mp_sample(), Format::Auto);
        assert_eq!(d.format, Format::Msgpack);
        let t = tree(&d);
        assert_eq!(at(&t, &["name"]), &s("Ann"));
        assert_eq!(at(&t, &["age"]), &num("30"));
        assert_eq!(at(&t, &["tags", "#1"]), &Node::Bool(true));
        assert_eq!(at(&t, &["bin"]), &s("0x0102"));
        let Node::Object(es) = &t else { panic!() };
        assert_eq!(es.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["name", "age", "tags", "bin"]);
        // 非字符串键、浮点、扩展类型
        let d = decode(&[0x81, 0x01, 0xcb, 0x3f, 0xf8, 0, 0, 0, 0, 0, 0], Format::Msgpack);
        assert_eq!(at(&tree(&d), &["1"]), &num("1.5"));
        let d = decode(&[0xd4, 0x05, 0xff], Format::Auto);
        assert_eq!(d.format, Format::Msgpack);
        assert_eq!(at(&tree(&d), &["$ext"]), &num("5"));
    }

    #[test]
    fn msgpack_requires_exact_length_and_structure() {
        // 多出尾巴:不是一个完整的值
        let mut b = mp_sample();
        b.push(0x00);
        assert_ne!(decode(&b, Format::Auto).format, Format::Msgpack);
        // 单个数字手动选可以解,自动识别不认
        let d = decode(&[0xcc, 0xff], Format::Msgpack);
        assert_eq!(d.text, "255");
        assert_ne!(decode(&[0xcc, 0xff], Format::Auto).format, Format::Msgpack);
        // 截断:手动选回退十六进制
        let d = decode(&mp_sample()[..10], Format::Msgpack);
        assert!(d.fallback);
        assert_eq!(d.format, Format::Hex);
    }

    #[test]
    fn protobuf_wire_format_decodes_with_nesting_and_repeats() {
        let d = decode(&pb_sample(), Format::Auto);
        assert_eq!(d.format, Format::Protobuf);
        let t = tree(&d);
        assert_eq!(at(&t, &["1"]), &num("150"));
        assert_eq!(at(&t, &["2"]), &s("hello"));
        assert_eq!(at(&t, &["3", "1"]), &num("7"));
        assert_eq!(at(&t, &["4"]), &Node::Array(vec![num("1"), num("2")]));
        assert_eq!(at(&t, &["5"]), &num("1"));
        // 不可打印、也不是子消息的字节串:十六进制
        let d = decode(&[0x0a, 2, 0xff, 0xfe], Format::Protobuf);
        assert_eq!(at(&tree(&d), &["1"]), &s("0xfffe"));
        // 负数(10 字节 varint)按无符号显示
        let mut b = vec![0x08];
        b.extend([0xff; 9]);
        b.push(0x01);
        assert_eq!(at(&tree(&decode(&b, Format::Protobuf)), &["1"]), &num("18446744073709551615"));
    }

    #[test]
    fn protobuf_rejects_invalid_wire_format() {
        for bad in [&[0x08][..], &[0x00, 0x01], &[0x0b, 0x01], &[0x12, 0x05, b'a'], &[0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01]] {
            let d = decode(bad, Format::Protobuf);
            assert!(d.fallback, "{bad:?}");
        }
    }

    #[test]
    fn protobuf_nesting_is_capped() {
        // 每层:字段 1 的子消息,最里层 1: 1。超过上限的那层按十六进制显示,不会无限递归
        let mut b = vec![0x08, 0x01];
        for _ in 0..(MAX_DEPTH + 10) {
            let mut outer = vec![0x0a];
            let mut len = b.len();
            while len >= 0x80 {
                outer.push((len as u8 & 0x7f) | 0x80);
                len >>= 7;
            }
            outer.push(len as u8);
            outer.extend(&b);
            b = outer;
        }
        let d = decode(&b, Format::Protobuf);
        assert_eq!(d.format, Format::Protobuf);
        assert!(d.text.contains("\"0x"), "最深处应退成十六进制");
    }

    #[test]
    fn compressed_values_are_unwrapped_then_detected() {
        let d = decode(&gzip(br#"{"a":1}"#), Format::Auto);
        assert_eq!(d.layers, vec![Format::Gzip]);
        assert_eq!(d.format, Format::Json);
        assert_eq!(d.size, 7);
        let d = decode(&zlib(&mp_sample()), Format::Auto);
        assert_eq!(d.layers, vec![Format::Zlib]);
        assert_eq!(d.format, Format::Msgpack);
        // 两层
        let d = decode(&gzip(&zlib(b"plain text")), Format::Auto);
        assert_eq!(d.layers, vec![Format::Gzip, Format::Zlib]);
        assert_eq!(d.text, "plain text");
        // 手动选 gzip:解压后自动识别内层
        let d = decode(&gzip(&pb_sample()), Format::Gzip);
        assert_eq!((d.format, d.fallback), (Format::Protobuf, false));
        // 手动选 zlib 但其实是 gzip:回退(不是文本,给十六进制)
        let d = decode(&gzip(b"x"), Format::Zlib);
        assert!(d.fallback);
        assert_eq!(d.format, Format::Hex);
        assert!(d.layers.is_empty());
    }

    #[test]
    fn truncated_gzip_falls_back_quietly() {
        let g = gzip(&b"abc".repeat(1000));
        let d = decode(&g[..g.len() / 2], Format::Auto);
        assert!(d.layers.is_empty());
        assert_eq!(d.format, Format::Hex);
        assert!(!d.fallback, "自动识别认不出不算回退");
    }

    #[test]
    fn decompression_bomb_is_refused() {
        let bomb = gzip(&vec![0u8; MAX_INFLATED + 1]);
        assert!(bomb.len() < 64 << 10, "压缩后应很小:{}", bomb.len());
        let d = decode(&bomb, Format::Auto);
        assert!(d.layers.is_empty());
        assert!(d.notes.contains(&Note::InflateLimit));
        assert_eq!(d.format, Format::Hex);
        let d = decode(&bomb, Format::Gzip);
        assert!(d.fallback && d.notes.contains(&Note::InflateLimit));
        // 刚好在上限内的可以解开
        let ok = zlib(&vec![b'a'; MAX_INFLATED]);
        let d = decode(&ok, Format::Auto);
        assert_eq!((d.layers.clone(), d.size), (vec![Format::Zlib], MAX_INFLATED));
    }

    #[test]
    fn nested_compression_layers_are_capped() {
        let mut b = b"inner".to_vec();
        for _ in 0..(MAX_LAYERS + 2) {
            b = gzip(&b);
        }
        let d = decode(&b, Format::Auto);
        assert_eq!(d.layers.len(), MAX_LAYERS);
        assert_ne!(d.text, "inner");
    }

    #[test]
    fn msgpack_depth_is_capped() {
        // 很深的嵌套数组:超过上限不解(不爆栈),回退
        let mut b = vec![0x91; 100_000];
        b.push(0xc0);
        let d = decode(&b, Format::Msgpack);
        assert!(d.fallback);
    }

    #[test]
    fn hex_view_and_fallbacks() {
        let d = decode(&[0x00, 0xff, b'A'], Format::Auto);
        assert_eq!(d.format, Format::Hex);
        assert_eq!(d.text, "00000000  00 ff 41                                          |..A|\n");
        let d = decode(b"hello", Format::Hex);
        assert!(d.text.starts_with("00000000  68 65 6c 6c 6f"));
        // 手动选 JSON 但不是 JSON:文本回退
        let d = decode(b"hello", Format::Json);
        assert!(d.fallback);
        assert_eq!((d.format, d.text.as_str()), (Format::Text, "hello"));
        // 手动选文本:非 UTF-8 用替换符
        let d = decode(&[b'a', 0xff], Format::Text);
        assert_eq!(d.text, "a\u{fffd}");
        // 很长的二进制只转储前一部分
        let d = decode(&vec![0xffu8; MAX_HEX + 1], Format::Auto);
        assert!(d.notes.contains(&Note::HexTruncated));
        assert_eq!(d.text.lines().count(), MAX_HEX / 16);
        // 序列化成界面要的形状
        let v = serde_json::to_value(decode(&gzip(b"{}"), Format::Auto)).unwrap();
        assert_eq!(v["format"], "json");
        assert_eq!(v["layers"], serde_json::json!(["gzip"]));
        assert_eq!(serde_json::to_value(Note::InflateLimit).unwrap(), "inflate_limit");
        let f: Format = serde_json::from_str("\"protobuf\"").unwrap();
        assert_eq!(f, Format::Protobuf);
    }

    // ------------------------------------------------------------ Java

    struct J(Vec<u8>);
    impl J {
        fn new() -> Self {
            J(JAVA_MAGIC.to_vec())
        }
        fn b(mut self, v: &[u8]) -> Self {
            self.0.extend_from_slice(v);
            self
        }
        fn utf(self, s: &str) -> Self {
            let n = (s.len() as u16).to_be_bytes();
            self.b(&n).b(s.as_bytes())
        }
    }

    /// 等价于 `new User(7, "Ann", true)`,`class User implements Serializable { int id; String name; boolean admin; }`
    fn java_user() -> Vec<u8> {
        J::new()
            .b(&[TC_OBJECT, TC_CLASSDESC])
            .utf("com.example.User")
            .b(&[0, 0, 0, 0, 0, 0, 0, 1]) // serialVersionUID
            .b(&[SC_SERIALIZABLE, 0, 3])
            .b(b"Z")
            .utf("admin")
            .b(b"I")
            .utf("id")
            .b(b"L")
            .utf("name")
            .b(&[TC_STRING])
            .utf("Ljava/lang/String;")
            .b(&[TC_ENDBLOCKDATA, TC_NULL]) // 类注解结束、没有父类
            .b(&[1]) // admin
            .b(&7i32.to_be_bytes())
            .b(&[TC_STRING])
            .utf("Ann")
            .0
    }

    #[test]
    fn java_object_shows_class_and_fields() {
        let d = decode(&java_user(), Format::Auto);
        assert_eq!(d.format, Format::Java);
        assert!(d.notes.is_empty(), "{:?}", d.notes);
        let t = tree(&d);
        assert_eq!(at(&t, &["$class"]), &s("com.example.User"));
        assert_eq!(at(&t, &["id"]), &num("7"));
        assert_eq!(at(&t, &["name"]), &s("Ann"));
        assert_eq!(at(&t, &["admin"]), &Node::Bool(true));
    }

    #[test]
    fn java_write_object_data_and_references() {
        // 两个对象:第二个引用第一个的类描述(句柄 0x7e0000)与字符串;带 writeObject 附加数据
        let b = J::new()
            .b(&[TC_OBJECT, TC_CLASSDESC])
            .utf("a.B")
            .b(&[0; 8])
            .b(&[SC_SERIALIZABLE | SC_WRITE_METHOD, 0, 1])
            .b(b"J")
            .utf("n")
            .b(&[TC_ENDBLOCKDATA, TC_NULL])
            .b(&5i64.to_be_bytes())
            .b(&[TC_BLOCKDATA, 2, 0xab, 0xcd])
            .b(&[TC_STRING])
            .utf("x")
            .b(&[TC_ENDBLOCKDATA])
            .b(&[TC_OBJECT, TC_REFERENCE, 0x00, 0x7e, 0x00, 0x00])
            .b(&(-1i64).to_be_bytes())
            .b(&[TC_REFERENCE, 0x00, 0x7e, 0x00, 0x02]) // 句柄 0 类描述、1 对象、2 字符串 "x"
            .b(&[TC_ENDBLOCKDATA])
            .0;
        let d = decode(&b, Format::Java);
        assert!(!d.fallback && d.notes.is_empty(), "{d:?}");
        let t = tree(&d);
        assert_eq!(at(&t, &["#0", "n"]), &num("5"));
        assert_eq!(at(&t, &["#0", "$data", "#0"]), &s("blockdata:abcd"));
        assert_eq!(at(&t, &["#0", "$data", "#1"]), &s("x"));
        assert_eq!(at(&t, &["#1", "$class"]), &s("a.B"));
        assert_eq!(at(&t, &["#1", "n"]), &num("-1"));
        assert_eq!(at(&t, &["#1", "$data", "#0"]), &s("x"));
    }

    #[test]
    fn java_arrays_and_enums() {
        let b = J::new()
            .b(&[TC_ARRAY, TC_CLASSDESC])
            .utf("[I")
            .b(&[0; 8])
            .b(&[SC_SERIALIZABLE, 0, 0, TC_ENDBLOCKDATA, TC_NULL])
            .b(&2u32.to_be_bytes())
            .b(&1i32.to_be_bytes())
            .b(&(-2i32).to_be_bytes())
            .b(&[TC_ENUM, TC_CLASSDESC])
            .utf("a.Color")
            .b(&[0; 8])
            .b(&[SC_SERIALIZABLE | 0x10, 0, 0, TC_ENDBLOCKDATA, TC_NULL])
            .b(&[TC_STRING])
            .utf("RED")
            .0;
        let t = tree(&decode(&b, Format::Auto));
        assert_eq!(at(&t, &["#0"]), &Node::Array(vec![num("1"), num("-2")]));
        assert_eq!(at(&t, &["#1"]), &s("Color.RED"));
    }

    #[test]
    fn java_partial_parse_gives_summary_and_hex() {
        // 截断在字段值中间:列出已读到的类与字段声明,文本视图为十六进制
        let full = java_user();
        let d = decode(&full[..full.len() - 3], Format::Auto);
        assert_eq!(d.format, Format::Java);
        assert!(d.notes.contains(&Note::JavaPartial));
        let t = tree(&d);
        assert_eq!(at(&t, &["classes", "#0", "class"]), &s("com.example.User"));
        assert_eq!(
            at(&t, &["classes", "#0", "fields"]),
            &Node::Array(vec![s("boolean admin"), s("int id"), s("java.lang.String name")])
        );
        assert!(d.text.starts_with("00000000  ac ed 00 05"));
        // 只有魔数:摘要为空,不崩
        let d = decode(&JAVA_MAGIC, Format::Java);
        assert!(d.notes.contains(&Note::JavaPartial));
        // 不是 Java 序列化:手动选回退
        assert!(decode(b"abc", Format::Java).fallback);
    }

    #[test]
    fn java_deep_nesting_is_capped() {
        // 自引用的对象数组层层嵌套:超过深度上限退成摘要,不爆栈
        let mut b = J::new().b(&[TC_ARRAY, TC_CLASSDESC]).utf("[Ljava.lang.Object;").b(&[0; 8]).b(&[SC_SERIALIZABLE, 0, 0, TC_ENDBLOCKDATA, TC_NULL]).b(&1u32.to_be_bytes()).0;
        for _ in 0..10_000 {
            b.extend([TC_ARRAY, TC_REFERENCE, 0, 0x7e, 0, 0]);
            b.extend(1u32.to_be_bytes());
        }
        b.push(TC_NULL);
        let d = decode(&b, Format::Auto);
        assert!(d.notes.contains(&Note::JavaPartial));
    }

    #[test]
    fn java_references_do_not_expand() {
        // 数组里放 2 万个对同一数组的引用:引用只给简短写法,结果大小与输入同一量级
        let mut b = J::new().b(&[TC_ARRAY, TC_CLASSDESC]).utf("[Ljava.lang.Object;").b(&[0; 8]).b(&[SC_SERIALIZABLE, 0, 0, TC_ENDBLOCKDATA, TC_NULL]).b(&20_000u32.to_be_bytes()).0;
        for _ in 0..20_000 {
            b.extend([TC_REFERENCE, 0, 0x7e, 0, 1]);
        }
        let d = decode(&b, Format::Auto);
        assert!(d.notes.is_empty());
        assert_eq!(at(&tree(&d), &["#0"]), &s("<ref [Ljava.lang.Object;>"));
        assert!(d.text.len() < b.len() * 20);
    }

    /// 数组里放 `n` 个 Class 对象,第 i 个的类描述以 TC_REFERENCE 引用第 i-1 个作父类:
    /// 不增加嵌套深度,继承链却长到 `n`。句柄:数组描述 0x7e0000、数组 0x7e0001,之后每个元素占两个(类描述、Class)。
    fn java_reference_chain(n: u32) -> Vec<u8> {
        let mut b = J::new().b(&[TC_ARRAY, TC_CLASSDESC]).utf("[Ljava.lang.Class;").b(&[0; 8]).b(&[SC_SERIALIZABLE, 0, 0, TC_ENDBLOCKDATA, TC_NULL]).b(&n.to_be_bytes()).0;
        for i in 0..n {
            b = J(b).b(&[TC_CLASS, TC_CLASSDESC]).utf(&format!("C{i}")).b(&[0; 8]).b(&[SC_SERIALIZABLE, 0, 0, TC_ENDBLOCKDATA]).0;
            if i == 0 {
                b.push(TC_NULL);
            } else {
                b.push(TC_REFERENCE);
                b.extend((0x7e0002 + 2 * (i - 1)).to_be_bytes());
            }
        }
        b
    }

    #[test]
    fn java_superclass_chain_is_capped() {
        // 链长在上限内照常解析;超过 MAX_DEPTH 判为无法解析,退成摘要
        let ok = decode(&java_reference_chain(MAX_DEPTH as u32), Format::Auto);
        assert!(ok.notes.is_empty(), "{:?}", ok.notes);
        let long = decode(&java_reference_chain(MAX_DEPTH as u32 + 1), Format::Auto);
        assert!(long.notes.contains(&Note::JavaPartial), "{:?}", long.notes);
    }

    #[test]
    fn java_type_names() {
        assert_eq!(type_name("Ljava/lang/String;"), "java.lang.String");
        assert_eq!(type_name("[I"), "int[]");
        assert_eq!(type_name("[[Ljava/util/List;"), "java.util.List[][]");
    }

    // ------------------------------------------------------------ 总量上界(DoS 防护)

    /// Protobuf varint 编码(测试里拼数据用)。
    fn pb_varint(mut v: u64, out: &mut Vec<u8>) {
        while v >= 0x80 {
            out.push((v as u8 & 0x7f) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    #[test]
    fn protobuf_many_distinct_fields_stay_linear() {
        // 5 万个不同的字段号:原来逐个线性查找同号字段是平方级(约 12 亿次比较),现在用 HashMap
        let mut b = vec![];
        for no in 1..=50_000u64 {
            pb_varint(no << 3, &mut b);
            b.push(1);
        }
        let t = std::time::Instant::now();
        let d = decode(&b, Format::Protobuf);
        assert!(!d.fallback);
        assert_eq!(at(&tree(&d), &["50000"]), &num("1"));
        assert!(t.elapsed() < std::time::Duration::from_secs(10), "{:?}", t.elapsed());
    }

    #[test]
    fn protobuf_field_budget_spans_all_levels() {
        // 顶层只有 1 个字段,子消息里塞超过预算的字段:预算跨层共用,整体回退并附注
        let mut inner = vec![];
        for _ in 0..(MAX_NODES + 10) {
            inner.extend([0x08, 0x01]);
        }
        let mut b = vec![0x0a];
        pb_varint(inner.len() as u64, &mut b);
        b.extend(&inner);
        let d = decode(&b, Format::Protobuf);
        assert!(d.fallback);
        assert!(d.notes.contains(&Note::TooLarge));
        // 顶层直接超预算同样
        let d = decode(&inner, Format::Protobuf);
        assert!(d.fallback && d.notes.contains(&Note::TooLarge));
        // 预算内的照常解
        let d = decode(&inner[..2 * 1000], Format::Protobuf);
        assert!(!d.fallback);
    }

    #[test]
    fn msgpack_node_count_is_capped_before_building_values() {
        // 1 字节一个 nil 的大数组:不交给 rmpv 建值,直接判为太大
        let n = (MAX_NODES + 1) as u32;
        let mut b = vec![0xdd];
        b.extend(n.to_be_bytes());
        b.extend(std::iter::repeat_n(0xc0u8, n as usize));
        let d = decode(&b, Format::Msgpack);
        assert!(d.fallback && d.notes.contains(&Note::TooLarge));
        assert!(!decode(&b, Format::Auto).notes.is_empty());
        // 声明 40 亿个元素、实际只有几个字节:扫描立即判无效,不循环 40 亿次
        let t = std::time::Instant::now();
        let d = decode(&[0xdd, 0xff, 0xff, 0xff, 0xff, 0xc0], Format::Msgpack);
        assert!(d.fallback && !d.notes.contains(&Note::TooLarge));
        assert!(t.elapsed() < std::time::Duration::from_secs(1));
        // 扫描与 rmpv 对合法数据结论一致
        assert!(matches!(mp_scan(&mp_sample()), Scan::Ok));
        assert!(matches!(mp_scan(&mp_sample()[..5]), Scan::Invalid));
    }

    #[test]
    fn json_tree_is_capped_by_nodes_and_depth() {
        let big = format!("[{}0]", "0,".repeat(MAX_NODES + 1));
        let d = decode(big.as_bytes(), Format::Json);
        assert!(d.fallback && d.notes.contains(&Note::TooLarge));
        assert_eq!(d.format, Format::Text);
        // 自动识别:按文本显示(仍可编辑)
        let d = decode(big.as_bytes(), Format::Auto);
        assert_eq!((d.format, d.tree.is_none()), (Format::Text, true));
        // 深度超过上限(JSON 解析器自己允许 512 层)
        let deep = format!("{}{}", "[".repeat(MAX_DEPTH + 1), "]".repeat(MAX_DEPTH + 1));
        let d = decode(deep.as_bytes(), Format::Json);
        assert!(d.notes.contains(&Note::TooLarge));
        // 字符串里的逗号、括号不算节点
        assert_eq!(json_shape(br#"{"a":"x,[{,\"]"}"#), (2, 1));
    }

    #[test]
    fn java_handle_names_are_truncated() {
        // 6 万字符的类名,被 2 万个对象经引用复用:$class 与引用都只有截断后的短名
        let long = format!("a.{}", "X".repeat(60_000));
        let mut b = J::new()
            .b(&[TC_OBJECT, TC_CLASSDESC])
            .utf(&long)
            .b(&[0; 8])
            .b(&[SC_SERIALIZABLE, 0, 1])
            .b(b"I")
            .utf(&"f".repeat(60_000))
            .b(&[TC_ENDBLOCKDATA, TC_NULL])
            .b(&1i32.to_be_bytes())
            .0;
        for _ in 0..20_000 {
            b.extend([TC_OBJECT, TC_REFERENCE, 0, 0x7e, 0, 0]);
            b.extend(2i32.to_be_bytes());
        }
        let d = decode(&b, Format::Java);
        assert!(d.notes.is_empty(), "{:?}", d.notes);
        assert!(d.text.len() < 10 << 20, "输出 {} 字节", d.text.len());
        let t = tree(&d);
        let Node::String(name) = at(&t, &["#5", "$class"]) else { panic!() };
        assert_eq!(name.chars().count(), MAX_REF_CHARS + 1);
        // 枚举常量名同样截断后才进句柄表
        assert!(matches!(ref_string(&"E".repeat(1000)), Node::String(s) if s.chars().count() == MAX_REF_CHARS + 1));
    }

    #[test]
    fn java_output_budget_stops_amplification() {
        // 128 字符的字符串被引用 20 万次:每次引用 5 字节输入、复制 128 字节,超出输出上限即停,退成摘要
        let mut b = J::new()
            .b(&[TC_ARRAY, TC_CLASSDESC])
            .utf("[Ljava.lang.String;")
            .b(&[0; 8])
            .b(&[SC_SERIALIZABLE, 0, 0, TC_ENDBLOCKDATA, TC_NULL])
            .b(&200_001u32.to_be_bytes())
            .b(&[TC_STRING])
            .utf(&"s".repeat(MAX_REF_CHARS))
            .0;
        for _ in 0..200_000 {
            b.extend([TC_REFERENCE, 0, 0x7e, 0, 2]);
        }
        assert!(b.len() < (2 << 20));
        let d = decode(&b, Format::Auto);
        assert_eq!(d.format, Format::Java);
        assert!(d.notes.contains(&Note::JavaPartial));
        assert!(d.tree.unwrap().len() < 1 << 20);
    }
}
