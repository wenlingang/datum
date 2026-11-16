//! 从一条 SELECT / UPDATE / DELETE 里摘出「索引能用上的地方」(纯函数,不连库):
//! 每个查询块的表(带别名)、等值 / 范围条件、JOIN 的连接列、ORDER BY 与 GROUP BY 的列。
//!
//! 不是完整的 SQL 解析器:只认这几种形态,其余(函数包着的列、OR、NOT、表达式排序……)跳过——
//! 跳过只会少给建议,不会给错建议;给出的候选最后还要经过执行计划验证。
//! 子查询、CTE、派生表各自是一个查询块;CTE 名与派生表别名不是真实表。

use crate::sql::Dialect;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    /// 不带引号的标识符或关键字(原样大小写)
    Word(String),
    /// 带引号的标识符(去掉引号)
    Quoted(String),
    Str,
    Num,
    /// `$1`、`?`、`:name`
    Param,
    Op(String),
    LParen,
    RParen,
    Comma,
    Dot,
    Semi,
}

impl Tok {
    fn is_kw(&self, kw: &str) -> bool {
        matches!(self, Tok::Word(w) if w.eq_ignore_ascii_case(kw))
    }
    fn is_name(&self) -> bool {
        matches!(self, Tok::Word(_) | Tok::Quoted(_))
    }
    fn name(&self) -> Option<&str> {
        match self {
            Tok::Word(w) | Tok::Quoted(w) => Some(w),
            _ => None,
        }
    }
}

const TWO_CHAR_OPS: &[&str] = &["<=", ">=", "<>", "!=", "==", "::", "||", "->", "#>", "@>", "<@", "~*", "!~", "&&"];

fn lex(sql: &str, d: Dialect) -> Option<Vec<Tok>> {
    let s: Vec<char> = sql.chars().collect();
    let mut out = vec![];
    let mut i = 0;
    let n = s.len();
    while i < n {
        let c = s[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        // 注释
        if c == '-' && s.get(i + 1) == Some(&'-') || (c == '#' && d == Dialect::Mysql) {
            while i < n && s[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && s.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < n && !(s[i] == '*' && s[i + 1] == '/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        // 字符串
        if c == '\'' || (c == '"' && d == Dialect::Mysql) {
            let q = c;
            i += 1;
            loop {
                if i >= n {
                    return None;
                }
                if s[i] == '\\' && d == Dialect::Mysql {
                    i += 2;
                    continue;
                }
                if s[i] == q {
                    if s.get(i + 1) == Some(&q) {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push(Tok::Str);
            continue;
        }
        // 引号标识符
        if c == '"' || c == '`' || (c == '[' && d == Dialect::Sqlite) {
            let close = if c == '[' { ']' } else { c };
            i += 1;
            let mut name = String::new();
            loop {
                if i >= n {
                    return None;
                }
                if s[i] == close {
                    if close != ']' && s.get(i + 1) == Some(&close) {
                        name.push(close);
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                name.push(s[i]);
                i += 1;
            }
            out.push(Tok::Quoted(name));
            continue;
        }
        // PG 的 $tag$ … $tag$ 字符串与 $1 参数
        if c == '$' && d == Dialect::Pg {
            if s.get(i + 1).is_some_and(|x| x.is_ascii_digit()) {
                i += 1;
                while i < n && s[i].is_ascii_digit() {
                    i += 1;
                }
                out.push(Tok::Param);
                continue;
            }
            let mut j = i + 1;
            while j < n && (s[j].is_alphanumeric() || s[j] == '_') {
                j += 1;
            }
            if s.get(j) == Some(&'$') {
                let tag: String = s[i..=j].iter().collect();
                let rest: String = s[j + 1..].iter().collect();
                let end = rest.find(&tag)?;
                i = j + 1 + rest[..end].chars().count() + tag.chars().count();
                out.push(Tok::Str);
                continue;
            }
        }
        // PG 里的 ? 是 jsonb 运算符,不是占位符
        if c == '?' && d != Dialect::Pg {
            out.push(Tok::Param);
            i += 1;
            while i < n && s[i].is_ascii_digit() {
                i += 1;
            }
            continue;
        }
        if c == ':' && s.get(i + 1).is_some_and(|x| x.is_alphabetic() || *x == '_') && s.get(i.wrapping_sub(1)) != Some(&':') {
            i += 1;
            while i < n && (s[i].is_alphanumeric() || s[i] == '_') {
                i += 1;
            }
            out.push(Tok::Param);
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && s.get(i + 1).is_some_and(|x| x.is_ascii_digit())) {
            while i < n && (s[i].is_ascii_alphanumeric() || s[i] == '.') {
                // 1e-5 的指数符号
                if (s[i] == 'e' || s[i] == 'E') && matches!(s.get(i + 1), Some('+') | Some('-')) {
                    i += 1;
                }
                i += 1;
            }
            out.push(Tok::Num);
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < n && (s[i].is_alphanumeric() || s[i] == '_' || s[i] == '$') {
                i += 1;
            }
            out.push(Tok::Word(s[start..i].iter().collect()));
            continue;
        }
        match c {
            '(' => out.push(Tok::LParen),
            ')' => out.push(Tok::RParen),
            ',' => out.push(Tok::Comma),
            '.' => out.push(Tok::Dot),
            ';' => out.push(Tok::Semi),
            _ => {
                let two: String = s[i..(i + 2).min(n)].iter().collect();
                if TWO_CHAR_OPS.contains(&two.as_str()) {
                    out.push(Tok::Op(two));
                    i += 2;
                    continue;
                }
                out.push(Tok::Op(c.to_string()));
            }
        }
        i += 1;
    }
    Some(out)
}

/// 列引用:`col`、`t.col`、`schema.t.col`。`qual` 是紧挨列名的那一段(表名或别名)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColRef {
    pub qual: Option<String>,
    pub name: String,
}

/// FROM 里的一张表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRef {
    /// 表名(去掉引号与 schema)
    pub name: String,
    pub schema: Option<String>,
    /// 语句里的原文(带 schema 与引号,不含别名),生成 DDL 时照用
    pub raw: String,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pred {
    /// `=`、`IN`、`IS NULL`
    Eq(ColRef),
    /// `<`、`>`、`BETWEEN`、MySQL 的前缀 `LIKE 'x%'`
    Range(ColRef),
    /// 两张表的列相等(JOIN 条件)
    Join(ColRef, ColRef),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderItem {
    pub col: ColRef,
    pub desc: bool,
}

/// 一个查询块(SELECT / UPDATE / DELETE 的一层)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Block {
    pub tables: Vec<TableRef>,
    pub preds: Vec<Pred>,
    /// ORDER BY 的列;有一项不是纯列(表达式、序号)时为 None
    pub order: Option<Vec<OrderItem>>,
    pub group: Vec<ColRef>,
    /// 派生表 / 表函数的别名(不是真实表)
    pub derived: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Shape {
    /// 第一个是最外层(主查询)
    pub blocks: Vec<Block>,
    /// CTE 名(不是真实表)
    pub ctes: Vec<String>,
}

/// 语句里有没有占位符(`$1`、`?`、`:name`)。读不懂的语句按没有算(交给后面的解析报错)。
pub fn has_params(sql: &str, d: Dialect) -> bool {
    lex(sql, d).is_some_and(|toks| toks.contains(&Tok::Param))
}

/// 解析语句。不是 SELECT / UPDATE / DELETE(含 WITH 开头的)或读不懂时返回 None。
pub fn parse(sql: &str, d: Dialect) -> Option<Shape> {
    let mut toks = lex(sql, d)?;
    while toks.last() == Some(&Tok::Semi) {
        toks.pop();
    }
    if toks.contains(&Tok::Semi) {
        return None;
    }
    let mut p = Parser { d, shape: Shape::default() };
    p.statement(&toks)?;
    if p.shape.blocks.is_empty() {
        return None;
    }
    Some(p.shape)
}

struct Parser {
    d: Dialect,
    shape: Shape,
}

/// `toks[open]` 是 `(`,返回配对的 `)` 的下标。
fn close_of(toks: &[Tok], open: usize) -> Option<usize> {
    let mut depth = 0;
    for (i, t) in toks.iter().enumerate().skip(open) {
        match t {
            Tok::LParen => depth += 1,
            Tok::RParen => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// 顶层(括号外)满足条件的位置。
fn top_positions(toks: &[Tok], f: impl Fn(&[Tok], usize) -> bool) -> Vec<usize> {
    let mut depth = 0i32;
    let mut out = vec![];
    for i in 0..toks.len() {
        match toks[i] {
            Tok::LParen => depth += 1,
            Tok::RParen => depth -= 1,
            _ if depth == 0 && f(toks, i) => out.push(i),
            _ => {}
        }
    }
    out
}

fn split_top_commas(toks: &[Tok]) -> Vec<&[Tok]> {
    let cuts = top_positions(toks, |t, i| t[i] == Tok::Comma);
    let mut out = vec![];
    let mut from = 0;
    for c in cuts.into_iter().chain([toks.len()]) {
        out.push(&toks[from..c]);
        from = c + 1;
    }
    out
}

/// 整段被一对括号包住时去掉外层括号。
fn unwrap_parens(mut toks: &[Tok]) -> &[Tok] {
    while toks.first() == Some(&Tok::LParen) && close_of(toks, 0) == Some(toks.len() - 1) {
        toks = &toks[1..toks.len() - 1];
    }
    toks
}

fn starts_query(toks: &[Tok]) -> bool {
    toks.first().is_some_and(|t| t.is_kw("select") || t.is_kw("with"))
}

/// 纯列引用(1~3 段名字用点连接)。
fn colref(toks: &[Tok]) -> Option<ColRef> {
    if toks.is_empty() || toks.len() > 5 || toks.len().is_multiple_of(2) {
        return None;
    }
    let mut names = vec![];
    for (i, t) in toks.iter().enumerate() {
        if i % 2 == 0 {
            names.push(t.name()?.to_string());
        } else if *t != Tok::Dot {
            return None;
        }
    }
    // 不带引号的关键字不是列(NULL、TRUE、CURRENT_DATE……)
    if let [Tok::Word(w)] = toks {
        if is_const_word(w) {
            return None;
        }
    }
    let name = names.pop()?;
    Some(ColRef { qual: names.pop(), name })
}

const CONST_WORDS: &[&str] = &[
    "null", "true", "false", "current_date", "current_time", "current_timestamp", "localtime", "localtimestamp",
    "interval", "date", "time", "timestamp", "day", "days", "hour", "hours", "minute", "minutes", "second", "seconds",
    "month", "months", "year", "years", "week", "weeks", "microsecond", "unknown", "and", "array", "binary", "collate",
];

fn is_const_word(w: &str) -> bool {
    CONST_WORDS.iter().any(|k| k.eq_ignore_ascii_case(w))
}

/// 不引用任何列的表达式(字面量、参数、函数调用、不相关子查询)。
fn is_const(toks: &[Tok]) -> bool {
    if toks.is_empty() {
        return false;
    }
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            Tok::LParen => {
                let Some(close) = close_of(toks, i) else { return false };
                let inner = &toks[i + 1..close];
                if !starts_query(inner) && !inner.is_empty() && !split_top_commas(inner).iter().all(|a| is_const(a)) {
                    return false;
                }
                i = close + 1;
                continue;
            }
            Tok::Quoted(_) => return false,
            Tok::Word(w) => {
                let call = toks.get(i + 1) == Some(&Tok::LParen);
                let after_cast = i > 0 && (toks[i - 1] == Tok::Op("::".into()) || toks[i - 1].is_kw("as"));
                if !call && !after_cast && !is_const_word(w) {
                    return false;
                }
            }
            Tok::Dot | Tok::Comma | Tok::Semi | Tok::RParen => return false,
            _ => {}
        }
        i += 1;
    }
    true
}

const CMP: &[&str] = &["=", "==", "<", ">", "<=", ">=", "<>", "!="];

/// 按顶层 AND 切开条件(BETWEEN 自带的 AND 不切)。
fn conjuncts(toks: &[Tok]) -> Vec<&[Tok]> {
    let mut cuts = vec![];
    let mut depth = 0i32;
    let mut between = false;
    for (i, t) in toks.iter().enumerate() {
        match t {
            Tok::LParen => depth += 1,
            Tok::RParen => depth -= 1,
            _ if depth == 0 && t.is_kw("between") => between = true,
            _ if depth == 0 && t.is_kw("and") => {
                if between {
                    between = false;
                } else {
                    cuts.push(i);
                }
            }
            _ => {}
        }
    }
    let mut out = vec![];
    let mut from = 0;
    for c in cuts.into_iter().chain([toks.len()]) {
        if c > from {
            out.push(&toks[from..c]);
        }
        from = c + 1;
    }
    out
}

/// 关键字序列 JOIN 之前的修饰词。
const JOIN_WORDS: &[&str] = &["join", "inner", "left", "right", "full", "cross", "outer", "natural", "straight_join"];

/// 表别名不能是这些(子句开头、JOIN 修饰词等)。
const NOT_ALIAS: &[&str] = &[
    "join", "inner", "left", "right", "full", "cross", "outer", "natural", "straight_join", "on", "using", "where",
    "group", "order", "limit", "offset", "fetch", "having", "window", "union", "intersect", "except", "set", "for",
    "returning", "use", "force", "ignore", "tablesample", "lateral", "values", "partition", "indexed", "not",
];

impl Parser {
    /// 一条语句(可带 WITH)。
    fn statement(&mut self, toks: &[Tok]) -> Option<()> {
        let toks = unwrap_parens(toks);
        let first = toks.first()?;
        if first.is_kw("with") {
            let mut i = 1;
            if toks.get(i)?.is_kw("recursive") {
                i += 1;
            }
            loop {
                let name = toks.get(i)?.name()?.to_string();
                self.shape.ctes.push(name);
                i += 1;
                if toks.get(i) == Some(&Tok::LParen) {
                    i = close_of(toks, i)? + 1;
                }
                if !toks.get(i)?.is_kw("as") {
                    return None;
                }
                i += 1;
                while toks.get(i).is_some_and(|t| t.is_kw("not") || t.is_kw("materialized")) {
                    i += 1;
                }
                if toks.get(i) != Some(&Tok::LParen) {
                    return None;
                }
                let close = close_of(toks, i)?;
                // CTE 里有写语句(PG 的数据修改 CTE)不分析
                let body = &toks[i + 1..close];
                if !starts_query(body) {
                    return None;
                }
                self.statement(body)?;
                i = close + 1;
                if toks.get(i) == Some(&Tok::Comma) {
                    i += 1;
                    continue;
                }
                break;
            }
            // 主语句放在最前面
            let before = self.shape.blocks.len();
            self.statement(&toks[i..])?;
            let main: Vec<Block> = self.shape.blocks.drain(before..).collect();
            for (k, b) in main.into_iter().enumerate() {
                self.shape.blocks.insert(k, b);
            }
            return Some(());
        }
        if first.is_kw("select") {
            // UNION / INTERSECT / EXCEPT 的各段各自分析
            let cuts = top_positions(toks, |t, i| t[i].is_kw("union") || t[i].is_kw("intersect") || t[i].is_kw("except"));
            let mut from = 0;
            for c in cuts.into_iter().chain([toks.len()]) {
                let mut part = &toks[from..c];
                while part.first().is_some_and(|t| t.is_kw("all") || t.is_kw("distinct")) {
                    part = &part[1..];
                }
                let part = unwrap_parens(part);
                if part.first().is_some_and(|t| t.is_kw("select")) {
                    self.select(part)?;
                } else if !part.is_empty() {
                    self.statement(part)?;
                }
                from = c + 1;
            }
            return Some(());
        }
        if first.is_kw("update") {
            return self.update(toks);
        }
        if first.is_kw("delete") {
            return self.delete(toks);
        }
        None
    }

    /// 段里的子查询(不在 FROM 里的):各自作为查询块。
    fn subqueries(&mut self, toks: &[Tok]) {
        let mut i = 0;
        while i < toks.len() {
            if toks[i] == Tok::LParen {
                let Some(close) = close_of(toks, i) else { return };
                let inner = &toks[i + 1..close];
                if starts_query(inner) {
                    let _ = self.statement(inner);
                } else {
                    self.subqueries(inner);
                }
                i = close + 1;
                continue;
            }
            i += 1;
        }
    }

    /// 找顶层子句关键字的位置:`kws` 里的每一项是一个或两个词(`GROUP BY`)。
    fn clauses<'a>(toks: &'a [Tok], kws: &[&str]) -> Vec<(&'static str, &'a [Tok])> {
        let mut marks: Vec<(usize, usize, &'static str)> = vec![];
        let names: &[(&str, &'static str)] = &[
            ("from", "from"),
            ("where", "where"),
            ("group by", "group"),
            ("having", "having"),
            ("window", "window"),
            ("order by", "order"),
            ("limit", "limit"),
            ("offset", "offset"),
            ("fetch", "fetch"),
            ("for", "for"),
            ("set", "set"),
            ("using", "using"),
            ("returning", "returning"),
        ];
        let pos = top_positions(toks, |_, _| true);
        for &i in &pos {
            for (text, key) in names {
                if !kws.contains(key) {
                    continue;
                }
                let words: Vec<&str> = text.split(' ').collect();
                let ok = words.iter().enumerate().all(|(k, w)| toks.get(i + k).is_some_and(|t| t.is_kw(w)));
                if ok {
                    marks.push((i, i + words.len(), key));
                }
            }
        }
        marks.sort_by_key(|m| m.0);
        let mut out = vec![];
        for (k, (_, body, key)) in marks.iter().enumerate() {
            let end = marks.get(k + 1).map(|m| m.0).unwrap_or(toks.len());
            out.push((*key, &toks[*body..end]));
        }
        out
    }

    fn select(&mut self, toks: &[Tok]) -> Option<()> {
        // 先占位:外层块排在它的子查询前面
        let slot = self.shape.blocks.len();
        self.shape.blocks.push(Block::default());
        let mut block = Block::default();
        let parts = Self::clauses(toks, &["from", "where", "group", "having", "window", "order", "limit", "offset", "fetch", "for"]);
        // 选择列表里的子查询
        let list_end = top_positions(toks, |t, i| {
            ["from", "where", "group", "order", "limit", "having"].iter().any(|k| t[i].is_kw(k))
        })
        .first()
        .copied()
        .unwrap_or(toks.len());
        self.subqueries(&toks[1..list_end]);
        for (key, body) in parts {
            match key {
                "from" => self.from(body, &mut block),
                "where" => {
                    self.preds(body, &mut block.preds);
                    self.subqueries(body);
                }
                "group" => {
                    block.group = split_top_commas(body).into_iter().filter_map(colref).collect();
                }
                "having" => self.subqueries(body),
                "order" => block.order = order_items(body),
                _ => {}
            }
        }
        self.shape.blocks[slot] = block;
        Some(())
    }

    fn update(&mut self, toks: &[Tok]) -> Option<()> {
        let slot = self.shape.blocks.len();
        self.shape.blocks.push(Block::default());
        let mut block = Block::default();
        let set = top_positions(toks, |t, i| t[i].is_kw("set")).first().copied()?;
        let mut target = &toks[1..set];
        while target.first().is_some_and(|t| t.is_kw("only") || t.is_kw("low_priority") || t.is_kw("ignore")) {
            target = &target[1..];
        }
        self.from(target, &mut block);
        for (key, body) in Self::clauses(&toks[set..], &["set", "from", "where", "order", "limit", "returning"]) {
            match key {
                "set" => self.subqueries(body),
                "from" => self.from(body, &mut block),
                "where" => {
                    self.preds(body, &mut block.preds);
                    self.subqueries(body);
                }
                "order" => block.order = order_items(body),
                _ => {}
            }
        }
        self.shape.blocks[slot] = block;
        Some(())
    }

    fn delete(&mut self, toks: &[Tok]) -> Option<()> {
        let slot = self.shape.blocks.len();
        self.shape.blocks.push(Block::default());
        let mut block = Block::default();
        let parts = Self::clauses(toks, &["from", "using", "where", "order", "limit", "returning"]);
        for (key, body) in parts {
            match key {
                "from" | "using" => {
                    let mut body = body;
                    if body.first().is_some_and(|t| t.is_kw("only")) {
                        body = &body[1..];
                    }
                    self.from(body, &mut block)
                }
                "where" => {
                    self.preds(body, &mut block.preds);
                    self.subqueries(body);
                }
                "order" => block.order = order_items(body),
                _ => {}
            }
        }
        if block.tables.is_empty() {
            return None;
        }
        self.shape.blocks[slot] = block;
        Some(())
    }

    /// FROM 列表:逗号或 JOIN 连接的表、派生表,ON / USING 条件。
    fn from(&mut self, toks: &[Tok], block: &mut Block) {
        let mut i = 0;
        let mut prev: Option<String> = None;
        while i < toks.len() {
            while toks.get(i).is_some_and(|t| t.is_kw("lateral") || t.is_kw("only")) {
                i += 1;
            }
            let mut current: Option<String> = None;
            match toks.get(i) {
                Some(Tok::LParen) => {
                    let Some(close) = close_of(toks, i) else { return };
                    let inner = &toks[i + 1..close];
                    if starts_query(inner) {
                        let _ = self.statement(inner);
                        i = close + 1;
                        if let Some((alias, next)) = alias_at(toks, i) {
                            block.derived.push(alias.clone());
                            current = Some(alias);
                            i = next;
                        }
                    } else {
                        // 括号包着的 JOIN
                        self.from(inner, block);
                        i = close + 1;
                    }
                }
                Some(t) if t.is_name() => {
                    let start = i;
                    let mut parts = vec![t.name().unwrap().to_string()];
                    i += 1;
                    while toks.get(i) == Some(&Tok::Dot) && toks.get(i + 1).is_some_and(|t| t.is_name()) {
                        parts.push(toks[i + 1].name().unwrap().to_string());
                        i += 2;
                    }
                    let raw = render(&toks[start..i], self.d);
                    let function = toks.get(i) == Some(&Tok::LParen);
                    if function {
                        i = close_of(toks, i).map(|c| c + 1).unwrap_or(toks.len());
                    }
                    let alias = alias_at(toks, i);
                    if let Some((_, next)) = &alias {
                        i = *next;
                    }
                    let alias = alias.map(|a| a.0);
                    if function {
                        let a = alias.clone().unwrap_or_else(|| parts.last().cloned().unwrap_or_default());
                        block.derived.push(a.clone());
                        current = Some(a);
                    } else {
                        let name = parts.pop().unwrap_or_default();
                        current = Some(alias.clone().unwrap_or_else(|| name.clone()));
                        block.tables.push(TableRef { name, schema: parts.pop(), raw, alias });
                    }
                    // MySQL 索引提示:USE / FORCE / IGNORE INDEX (…)
                    while toks.get(i).is_some_and(|t| t.is_kw("use") || t.is_kw("force") || t.is_kw("ignore")) {
                        let Some(open) = toks[i..].iter().position(|t| *t == Tok::LParen) else { break };
                        i = close_of(toks, i + open).map(|c| c + 1).unwrap_or(toks.len());
                    }
                    // SQLite 的 INDEXED BY x / NOT INDEXED
                    if toks.get(i).is_some_and(|t| t.is_kw("indexed")) {
                        i += 3;
                    } else if toks.get(i).is_some_and(|t| t.is_kw("not")) && toks.get(i + 1).is_some_and(|t| t.is_kw("indexed")) {
                        i += 2;
                    }
                }
                _ => return,
            }
            // ON / USING
            if toks.get(i).is_some_and(|t| t.is_kw("on")) {
                let start = i + 1;
                let mut j = start;
                let mut depth = 0i32;
                while j < toks.len() {
                    match &toks[j] {
                        Tok::LParen => depth += 1,
                        Tok::RParen => depth -= 1,
                        Tok::Comma if depth == 0 => break,
                        t if depth == 0 && JOIN_WORDS.iter().any(|k| t.is_kw(k)) => break,
                        _ => {}
                    }
                    j += 1;
                }
                self.preds(&toks[start..j], &mut block.preds);
                self.subqueries(&toks[start..j]);
                i = j;
            } else if toks.get(i).is_some_and(|t| t.is_kw("using")) && toks.get(i + 1) == Some(&Tok::LParen) {
                let close = close_of(toks, i + 1).unwrap_or(toks.len() - 1);
                if let (Some(a), Some(b)) = (&prev, &current) {
                    for c in split_top_commas(&toks[i + 2..close]) {
                        if let Some(col) = colref(c) {
                            block.preds.push(Pred::Join(
                                ColRef { qual: Some(a.clone()), name: col.name.clone() },
                                ColRef { qual: Some(b.clone()), name: col.name },
                            ));
                        }
                    }
                }
                i = close + 1;
            }
            prev = current.or(prev);
            // 下一张表
            match toks.get(i) {
                Some(Tok::Comma) => i += 1,
                Some(t) if JOIN_WORDS.iter().any(|k| t.is_kw(k)) => {
                    while toks.get(i).is_some_and(|t| JOIN_WORDS.iter().any(|k| t.is_kw(k))) {
                        i += 1;
                    }
                }
                _ => return,
            }
        }
    }

    /// WHERE / ON 里能用上索引的条件。
    fn preds(&self, toks: &[Tok], out: &mut Vec<Pred>) {
        for c in conjuncts(toks) {
            let inner = unwrap_parens(c);
            if inner.len() != c.len() {
                if top_positions(inner, |t, i| t[i].is_kw("or")).is_empty() {
                    self.preds(inner, out);
                }
                continue;
            }
            if !top_positions(c, |t, i| t[i].is_kw("or")).is_empty() || c.first().is_some_and(|t| t.is_kw("not")) {
                continue;
            }
            if let Some(p) = self.pred(c) {
                out.push(p);
            }
        }
    }

    fn pred(&self, c: &[Tok]) -> Option<Pred> {
        // col IS NULL
        if c.len() >= 3 && c[c.len() - 1].is_kw("null") && c[c.len() - 2].is_kw("is") {
            return colref(&c[..c.len() - 2]).map(Pred::Eq);
        }
        if let Some(k) = top_positions(c, |t, i| t[i].is_kw("in")).first().copied() {
            if k > 0 && c[k - 1].is_kw("not") {
                return None;
            }
            return colref(&c[..k]).map(Pred::Eq);
        }
        if let Some(k) = top_positions(c, |t, i| t[i].is_kw("between")).first().copied() {
            if k > 0 && c[k - 1].is_kw("not") {
                return None;
            }
            return colref(&c[..k]).map(Pred::Range);
        }
        if let Some(k) = top_positions(c, |t, i| t[i].is_kw("like")).first().copied() {
            // 前缀匹配能走 B 树:只认 MySQL(PG 非 C 排序规则、SQLite 默认大小写不敏感的 LIKE 都用不上普通索引)
            if self.d != Dialect::Mysql || (k > 0 && c[k - 1].is_kw("not")) || c.len() != k + 2 {
                return None;
            }
            return colref(&c[..k]).map(Pred::Range);
        }
        let k = top_positions(c, |t, i| matches!(&t[i], Tok::Op(o) if CMP.contains(&o.as_str()))).first().copied()?;
        let Tok::Op(op) = &c[k] else { return None };
        if op == "<>" || op == "!=" {
            return None;
        }
        let (l, r) = (&c[..k], &c[k + 1..]);
        let eq = op == "=" || op == "==";
        match (colref(l), colref(r)) {
            (Some(a), Some(b)) => eq.then_some(Pred::Join(a, b)),
            (Some(a), None) if is_const(r) => Some(if eq { Pred::Eq(a) } else { Pred::Range(a) }),
            (None, Some(b)) if is_const(l) => Some(if eq { Pred::Eq(b) } else { Pred::Range(b) }),
            _ => None,
        }
    }
}

/// `[AS] 别名 [(列…)]`,返回 (别名, 之后的位置)。
fn alias_at(toks: &[Tok], mut i: usize) -> Option<(String, usize)> {
    let explicit = toks.get(i).is_some_and(|t| t.is_kw("as"));
    if explicit {
        i += 1;
    }
    let t = toks.get(i)?;
    let ok = match t {
        Tok::Quoted(_) => true,
        Tok::Word(w) => explicit || !NOT_ALIAS.iter().any(|k| k.eq_ignore_ascii_case(w)),
        _ => false,
    };
    if !ok {
        return None;
    }
    let alias = t.name()?.to_string();
    i += 1;
    if toks.get(i) == Some(&Tok::LParen) {
        i = close_of(toks, i).map(|c| c + 1).unwrap_or(toks.len());
    }
    Some((alias, i))
}

fn order_items(toks: &[Tok]) -> Option<Vec<OrderItem>> {
    let mut out = vec![];
    for item in split_top_commas(toks) {
        let mut end = item.len();
        let mut desc = false;
        // NULLS FIRST / LAST
        if end >= 2 && item[end - 2].is_kw("nulls") {
            end -= 2;
        }
        if end >= 1 && (item[end - 1].is_kw("asc") || item[end - 1].is_kw("desc")) {
            desc = item[end - 1].is_kw("desc");
            end -= 1;
        }
        out.push(OrderItem { col: colref(&item[..end])?, desc });
    }
    (!out.is_empty()).then_some(out)
}

/// 把名字 token 还原成 SQL 文本(引号按方言)。
fn render(toks: &[Tok], d: Dialect) -> String {
    toks.iter()
        .map(|t| match t {
            Tok::Word(w) => w.clone(),
            Tok::Quoted(q) => crate::sql::build::ident(q, d),
            Tok::Dot => ".".into(),
            _ => String::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(qual: Option<&str>, name: &str) -> ColRef {
        ColRef { qual: qual.map(str::to_string), name: name.into() }
    }

    #[test]
    fn select_with_where_order_and_alias() {
        let s = parse(
            "SELECT o.id FROM public.orders AS o WHERE o.status = 'paid' AND o.created_at >= now() - interval '7 days' ORDER BY o.created_at DESC LIMIT 10",
            Dialect::Pg,
        )
        .unwrap();
        let b = &s.blocks[0];
        assert_eq!(b.tables, vec![TableRef { name: "orders".into(), schema: Some("public".into()), raw: "public.orders".into(), alias: Some("o".into()) }]);
        assert_eq!(b.preds, vec![Pred::Eq(c(Some("o"), "status")), Pred::Range(c(Some("o"), "created_at"))]);
        assert_eq!(b.order, Some(vec![OrderItem { col: c(Some("o"), "created_at"), desc: true }]));
    }

    #[test]
    fn joins_using_between_in_and_skips_or() {
        let s = parse(
            "select * from customers c join orders o on o.customer_id = c.id and o.kind = 2 \
             left join items using (order_id) \
             where c.country in ('KE', 'PA') and o.total between 10 and 20 and (c.a = 1 or c.b = 2) \
             and lower(c.email) = 'x' and c.deleted_at is null and o.note <> 'x' and not c.vip = 1",
            Dialect::Pg,
        )
        .unwrap();
        let b = &s.blocks[0];
        assert_eq!(b.tables.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["customers", "orders", "items"]);
        assert_eq!(
            b.preds,
            vec![
                Pred::Join(c(Some("o"), "customer_id"), c(Some("c"), "id")),
                Pred::Eq(c(Some("o"), "kind")),
                Pred::Join(c(Some("o"), "order_id"), c(Some("items"), "order_id")),
                Pred::Eq(c(Some("c"), "country")),
                Pred::Range(c(Some("o"), "total")),
                Pred::Eq(c(Some("c"), "deleted_at")),
            ]
        );
    }

    #[test]
    fn quoting_and_reversed_comparison() {
        let s = parse(r#"SELECT * FROM "Order Items" oi WHERE 5 < oi."Qty" AND "Sku" = $1"#, Dialect::Pg).unwrap();
        let b = &s.blocks[0];
        assert_eq!(b.tables[0].name, "Order Items");
        assert_eq!(b.tables[0].raw, r#""Order Items""#);
        assert_eq!(b.preds, vec![Pred::Range(c(Some("oi"), "Qty")), Pred::Eq(c(None, "Sku"))]);
        let m = parse("SELECT * FROM `t` WHERE `a` = ? AND b LIKE 'ab%' ORDER BY 1", Dialect::Mysql).unwrap();
        assert_eq!(m.blocks[0].tables[0].raw, "`t`");
        assert_eq!(m.blocks[0].preds, vec![Pred::Eq(c(None, "a")), Pred::Range(c(None, "b"))]);
        assert_eq!(m.blocks[0].order, None, "按序号排序用不上");
    }

    #[test]
    fn subqueries_ctes_and_derived_tables() {
        let s = parse(
            "WITH recent AS (SELECT customer_id FROM orders WHERE created_at > '2026-01-01') \
             SELECT c.name, (SELECT count(*) FROM reviews r WHERE r.customer_id = c.id) \
             FROM customers c JOIN recent ON recent.customer_id = c.id \
             JOIN (SELECT id FROM vip WHERE level = 3) v ON v.id = c.id \
             WHERE c.id IN (SELECT customer_id FROM payments WHERE amount > 100) \
             GROUP BY c.name",
            Dialect::Pg,
        )
        .unwrap();
        assert_eq!(s.ctes, vec!["recent"]);
        // 主查询在最前
        let main = &s.blocks[0];
        assert_eq!(main.tables.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["customers", "recent"]);
        assert_eq!(main.derived, vec!["v"]);
        assert_eq!(main.group, vec![c(Some("c"), "name")]);
        let names: Vec<&str> = s.blocks.iter().flat_map(|b| b.tables.iter().map(|t| t.name.as_str())).collect();
        for t in ["orders", "reviews", "vip", "payments"] {
            assert!(names.contains(&t), "{t} 应作为子查询块: {names:?}");
        }
    }

    #[test]
    fn update_and_delete() {
        let u = parse("UPDATE orders SET status = 'x' WHERE customer_id = 3 AND created_at < '2020-01-01'", Dialect::Pg).unwrap();
        assert_eq!(u.blocks[0].tables[0].name, "orders");
        assert_eq!(u.blocks[0].preds, vec![Pred::Eq(c(None, "customer_id")), Pred::Range(c(None, "created_at"))]);
        let d = parse("DELETE FROM logs WHERE level = 'debug'", Dialect::Sqlite).unwrap();
        assert_eq!(d.blocks[0].preds, vec![Pred::Eq(c(None, "level"))]);
        let m = parse("DELETE o FROM orders o JOIN customers c ON c.id = o.customer_id WHERE c.banned = 1", Dialect::Mysql).unwrap();
        assert_eq!(m.blocks[0].tables.len(), 2);
        assert!(parse("INSERT INTO t VALUES (1)", Dialect::Pg).is_none());
        assert!(parse("SELECT 1; SELECT 2", Dialect::Pg).is_none());
        assert!(parse("WITH x AS (DELETE FROM t RETURNING *) SELECT * FROM x", Dialect::Pg).is_none());
    }

    #[test]
    fn comments_strings_and_casts() {
        let s = parse(
            "-- 注释 where a = 1\nSELECT * FROM t /* x = 1 */ WHERE t.d >= '2026-01-01'::date AND t.s = 'it''s' AND t.x::int = 1",
            Dialect::Pg,
        )
        .unwrap();
        assert_eq!(s.blocks[0].preds, vec![Pred::Range(c(Some("t"), "d")), Pred::Eq(c(Some("t"), "s"))]);
    }
}
