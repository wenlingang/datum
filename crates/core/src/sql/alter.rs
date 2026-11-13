//! MySQL / MariaDB 改表结构的风险评估(纯函数):`ALTER TABLE`、`CREATE INDEX`、`DROP INDEX … ON`、
//! `OPTIMIZE TABLE` 的每个子操作能否 `ALGORITHM=INSTANT` / `INPLACE`、还是只能 `COPY`,是否允许并发 DML、
//! 是否重建表;再结合表大小给出整体结论与耗时量级。
//!
//! 依据:MySQL 官方「Online DDL Operations」操作表(5.7 / 8.0 / 8.4)、MariaDB 的 InnoDB 在线 DDL 文档,
//! 并在 MySQL 8.4 上逐条实测核对(`tests/alter.rs`)。
//!
//! 只用规则 + 表元数据判断,**绝不向服务器发 ALTER 试探**:MySQL 没有 dry-run,
//! `ALTER … ALGORITHM=INSTANT` 一旦支持就会真执行。拿不准的写法、属性与版本一律判为「未知」,不去猜。

use serde::Serialize;
use std::collections::HashMap;

// ---------- 词法 ----------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    /// 未加引号的单词(关键字或标识符)
    Word(String),
    /// 反引号标识符(已去引号)
    Ident(String),
    /// 字符串(已去引号、还原转义)
    Str(String),
    Num(String),
    /// 括号:内部原文(不含两端括号)
    Group(String),
    Comma,
    Eq,
    Dot,
    Other(char),
}

#[derive(Debug, Clone)]
struct T {
    tok: Tok,
    start: usize,
    end: usize,
}

/// 读一个引号串,返回 (内容, 结束位置的下标)。`backslash`:MySQL 的字符串支持反斜杠转义,反引号标识符不支持。
fn read_quoted(cs: &[(usize, char)], i: usize, backslash: bool) -> Option<(String, usize)> {
    let q = cs[i].1;
    let mut v = String::new();
    let mut j = i + 1;
    loop {
        let c = cs.get(j)?.1;
        if backslash && c == '\\' {
            v.push(cs.get(j + 1)?.1);
            j += 2;
        } else if c == q {
            if cs.get(j + 1).map(|x| x.1) == Some(q) {
                v.push(q);
                j += 2;
            } else {
                return Some((v, j + 1));
            }
        } else {
            v.push(c);
            j += 1;
        }
    }
}

/// MySQL 词法:跳过注释,括号整体作为一个记号。可执行注释(`/*! */`、`/*M! */`)里的内容会被执行,
/// 静态分析说不清,直接放弃(返回 None)。
fn lex(s: &str) -> Option<Vec<T>> {
    let cs: Vec<(usize, char)> = s.char_indices().collect();
    let at = |k: usize| cs.get(k).map(|x| x.1);
    let pos = |k: usize| cs.get(k).map_or(s.len(), |x| x.0);
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i].1;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let dash_comment = c == '-' && at(i + 1) == Some('-') && at(i + 2).is_none_or(|x| x.is_whitespace());
        if c == '#' || dash_comment {
            while i < cs.len() && cs[i].1 != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && at(i + 1) == Some('*') {
            if at(i + 2) == Some('!') || (at(i + 2) == Some('M') && at(i + 3) == Some('!')) {
                return None;
            }
            i += 2;
            while i + 1 < cs.len() && !(cs[i].1 == '*' && cs[i + 1].1 == '/') {
                i += 1;
            }
            if i + 1 >= cs.len() {
                return None;
            }
            i += 2;
            continue;
        }
        let start = pos(i);
        let (tok, next) = match c {
            '\'' | '"' => {
                let (v, n) = read_quoted(&cs, i, true)?;
                (Tok::Str(v), n)
            }
            '`' => {
                let (v, n) = read_quoted(&cs, i, false)?;
                (Tok::Ident(v), n)
            }
            '(' => {
                let mut depth = 0;
                let mut j = i;
                loop {
                    let ch = at(j)?;
                    match ch {
                        '\'' | '"' | '`' => {
                            j = read_quoted(&cs, j, ch != '`')?.1;
                            continue;
                        }
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                (Tok::Group(s[pos(i + 1)..pos(j)].to_string()), j + 1)
            }
            ')' => return None,
            ',' => (Tok::Comma, i + 1),
            '=' => (Tok::Eq, i + 1),
            '.' if !at(i + 1).is_some_and(|x| x.is_ascii_digit()) => (Tok::Dot, i + 1),
            _ if c.is_ascii_digit() || c == '.' => {
                let mut j = i;
                while at(j).is_some_and(|x| x.is_alphanumeric() || x == '.' || x == '_') {
                    j += 1;
                }
                (Tok::Num(s[pos(i)..pos(j)].to_string()), j)
            }
            _ if c.is_alphanumeric() || c == '_' || c == '$' => {
                let mut j = i;
                while at(j).is_some_and(|x| x.is_alphanumeric() || x == '_' || x == '$') {
                    j += 1;
                }
                (Tok::Word(s[pos(i)..pos(j)].to_string()), j)
            }
            _ => (Tok::Other(c), i + 1),
        };
        out.push(T { tok, start, end: pos(next) });
        i = next;
    }
    Some(out)
}

/// 记号游标。
struct Cur<'a> {
    t: &'a [T],
    i: usize,
}

impl<'a> Cur<'a> {
    fn new(t: &'a [T]) -> Self {
        Self { t, i: 0 }
    }
    fn peek(&self) -> Option<&'a Tok> {
        self.t.get(self.i).map(|x| &x.tok)
    }
    fn peek_at(&self, k: usize) -> Option<&'a Tok> {
        self.t.get(self.i + k).map(|x| &x.tok)
    }
    fn bump(&mut self) -> Option<&'a Tok> {
        let t = self.peek();
        if t.is_some() {
            self.i += 1;
        }
        t
    }
    fn done(&self) -> bool {
        self.i >= self.t.len()
    }
    fn word(&self) -> Option<String> {
        match self.peek() {
            Some(Tok::Word(w)) => Some(w.to_ascii_uppercase()),
            _ => None,
        }
    }
    fn is(&self, k: &str) -> bool {
        matches!(self.peek(), Some(Tok::Word(w)) if w.eq_ignore_ascii_case(k))
    }
    fn is_at(&self, off: usize, k: &str) -> bool {
        matches!(self.peek_at(off), Some(Tok::Word(w)) if w.eq_ignore_ascii_case(k))
    }
    /// 是该关键字就吃掉
    fn kw(&mut self, k: &str) -> bool {
        let hit = self.is(k);
        if hit {
            self.i += 1;
        }
        hit
    }
    fn eat_eq(&mut self) {
        if self.peek() == Some(&Tok::Eq) {
            self.i += 1;
        }
    }
    fn ident(&mut self) -> Option<String> {
        match self.peek()? {
            Tok::Word(w) | Tok::Ident(w) => {
                self.i += 1;
                Some(w.clone())
            }
            _ => None,
        }
    }
    fn group(&mut self) -> Option<String> {
        match self.peek()? {
            Tok::Group(g) => {
                self.i += 1;
                Some(g.clone())
            }
            _ => None,
        }
    }
    /// 取一个选项值(`= 值`):单词、字符串、数字、标识符都行
    fn value(&mut self) -> Option<String> {
        self.eat_eq();
        match self.bump()? {
            Tok::Word(w) | Tok::Ident(w) | Tok::Str(w) | Tok::Num(w) => Some(w.clone()),
            _ => None,
        }
    }
    /// 可带库名前缀的名字 → (库, 名, 原文)
    fn qualified(&mut self, src: &str) -> Option<(Option<String>, String, String)> {
        let start = self.t.get(self.i)?.start;
        let first = self.ident()?;
        if self.peek() == Some(&Tok::Dot) {
            self.i += 1;
            let second = self.ident()?;
            let end = self.t[self.i - 1].end;
            return Some((Some(first), second, src[start..end].to_string()));
        }
        let end = self.t[self.i - 1].end;
        Some((None, first, src[start..end].to_string()))
    }
}

// ---------- 语句结构 ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StmtKind {
    AlterTable,
    CreateIndex,
    DropIndex,
    Optimize,
}

impl StmtKind {
    /// 审查弹窗里的标签
    pub fn label(self) -> &'static str {
        match self {
            Self::AlterTable => "ALTER TABLE",
            Self::CreateIndex => "CREATE INDEX",
            Self::DropIndex => "DROP INDEX",
            Self::Optimize => "OPTIMIZE TABLE",
        }
    }
}

/// 列类型(已规范化,可直接比较)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColType {
    /// 小写基础类型,别名已换成正名(integer → int、bool → tinyint 等)
    pub base: String,
    /// 括号里的参数(去空白;整数类的显示宽度已去掉)
    pub params: Option<String>,
    /// ENUM / SET 的取值
    pub values: Option<Vec<String>>,
    pub unsigned: bool,
    pub zerofill: bool,
}

/// 列定义里判定要用的部分。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColumnDef {
    pub ty: Option<ColType>,
    pub charset: Option<String>,
    pub collate: Option<String>,
    /// None:没写(默认可空)
    pub not_null: Option<bool>,
    pub auto_increment: bool,
    /// 生成列:Some(true) 为 STORED,Some(false) 为 VIRTUAL
    pub generated: Option<bool>,
    /// 默认值是括号表达式(8.0.13+ 的表达式默认值)
    pub expr_default: bool,
    pub on_update: bool,
    /// FIRST / AFTER:改列位置
    pub position: bool,
    /// 有不在规则表里的属性(PRIMARY KEY、REFERENCES、SRID、VISIBLE…):整条判为未知
    pub other: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpKind {
    AddColumn { name: String, def: ColumnDef },
    DropColumn { name: String },
    /// MODIFY,以及新旧同名的 CHANGE
    Modify { name: String, def: ColumnDef },
    /// 改名的 CHANGE
    Change { old: String, new: String, def: ColumnDef },
    RenameColumn { old: String, new: String },
    /// ALTER COLUMN … SET DEFAULT / DROP DEFAULT
    SetDefault { name: String },
    /// ALTER COLUMN … SET VISIBLE / INVISIBLE
    ColumnVisibility { name: String },
    AddIndex { name: Option<String> },
    AddFulltext { name: Option<String> },
    AddSpatial { name: Option<String> },
    DropIndex { name: String },
    RenameIndex { old: String, new: String },
    IndexVisibility { name: String },
    AddPrimaryKey,
    DropPrimaryKey,
    AddForeignKey { name: Option<String> },
    DropForeignKey { name: String },
    AddCheck,
    ConvertCharset,
    /// 表的默认字符集 / 排序规则
    TableCharset,
    Engine { engine: String },
    /// FORCE、OPTIMIZE TABLE:原样重建
    Rebuild,
    /// ROW_FORMAT / KEY_BLOCK_SIZE
    RowFormat,
    AutoIncrement,
    /// STATS_PERSISTENT 等统计信息选项
    Stats,
    TableComment,
    RenameTable,
    Partition,
    Unknown,
}

impl OpKind {
    /// 跨边界的操作代码(界面按它取文案)
    pub fn code(&self) -> &'static str {
        match self {
            Self::AddColumn { .. } => "add_column",
            Self::DropColumn { .. } => "drop_column",
            Self::Modify { .. } => "modify_column",
            Self::Change { .. } => "change_column",
            Self::RenameColumn { .. } => "rename_column",
            Self::SetDefault { .. } => "set_default",
            Self::ColumnVisibility { .. } => "column_visibility",
            Self::AddIndex { .. } => "add_index",
            Self::AddFulltext { .. } => "add_fulltext",
            Self::AddSpatial { .. } => "add_spatial",
            Self::DropIndex { .. } => "drop_index",
            Self::RenameIndex { .. } => "rename_index",
            Self::IndexVisibility { .. } => "index_visibility",
            Self::AddPrimaryKey => "add_primary_key",
            Self::DropPrimaryKey => "drop_primary_key",
            Self::AddForeignKey { .. } => "add_foreign_key",
            Self::DropForeignKey { .. } => "drop_foreign_key",
            Self::AddCheck => "add_check",
            Self::ConvertCharset => "convert_charset",
            Self::TableCharset => "table_charset",
            Self::Engine { .. } => "engine",
            Self::Rebuild => "rebuild",
            Self::RowFormat => "row_format",
            Self::AutoIncrement => "auto_increment",
            Self::Stats => "stats",
            Self::TableComment => "table_comment",
            Self::RenameTable => "rename_table",
            Self::Partition => "partition",
            Self::Unknown => "unknown",
        }
    }

    /// 操作对象(列名、索引名…),界面展示用
    pub fn target(&self) -> Option<String> {
        match self {
            Self::AddColumn { name, .. }
            | Self::DropColumn { name }
            | Self::Modify { name, .. }
            | Self::SetDefault { name }
            | Self::ColumnVisibility { name }
            | Self::DropIndex { name }
            | Self::IndexVisibility { name }
            | Self::DropForeignKey { name } => Some(name.clone()),
            Self::Change { old, new, .. } | Self::RenameColumn { old, new } | Self::RenameIndex { old, new } => {
                Some(format!("{old} → {new}"))
            }
            Self::AddIndex { name } | Self::AddFulltext { name } | Self::AddSpatial { name } | Self::AddForeignKey { name } => {
                name.clone()
            }
            Self::Engine { engine } => Some(engine.clone()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    /// 子句原文
    pub clause: String,
    pub kind: OpKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub kind: StmtKind,
    pub schema: Option<String>,
    pub table: String,
    /// 表名原文(带库名与引号)
    pub table_raw: String,
    pub ops: Vec<Op>,
    /// 语句里显式写的 ALGORITHM(大写)
    pub algorithm: Option<String>,
    /// 语句里显式写的 LOCK(大写);MariaDB 的 `ALTER ONLINE TABLE` 等同 LOCK=NONE
    pub lock: Option<String>,
    /// 语句原文(去掉末尾分号)
    pub text: String,
}

/// 读列定义:类型 + 属性。读不懂的属性记为 `other`(整条判未知),不中断。
fn column_def(c: &mut Cur) -> ColumnDef {
    let mut d = ColumnDef::default();
    let Some(base) = c.word() else {
        d.other = true;
        return d;
    };
    c.bump();
    let mut base = base.to_ascii_lowercase();
    // 两个词的类型名
    if base == "double" && c.kw("PRECISION") {
        base = "double".into();
    }
    if matches!(base.as_str(), "national" | "nchar" | "nvarchar" | "long" | "serial") {
        d.other = true;
    }
    let params = c.group();
    let mut unsigned = false;
    let mut zerofill = false;
    while let Some(tok) = c.peek() {
        let Tok::Word(w) = tok else {
            d.other = true;
            c.bump();
            continue;
        };
        let w = w.to_ascii_uppercase();
        c.bump();
        match w.as_str() {
            "UNSIGNED" => unsigned = true,
            "SIGNED" => {}
            "ZEROFILL" => zerofill = true,
            "CHARACTER" | "CHAR" => {
                c.kw("SET");
                d.charset = c.value().map(|v| v.to_ascii_lowercase());
            }
            "CHARSET" => d.charset = c.value().map(|v| v.to_ascii_lowercase()),
            "COLLATE" => d.collate = c.value().map(|v| v.to_ascii_lowercase()),
            "NOT" => {
                if c.kw("NULL") {
                    d.not_null = Some(true);
                } else {
                    d.other = true;
                }
            }
            "NULL" => d.not_null = Some(false),
            "DEFAULT" => match c.bump() {
                Some(Tok::Group(_)) => d.expr_default = true,
                Some(Tok::Other('-' | '+')) => {
                    c.bump();
                }
                Some(Tok::Word(_)) => {
                    // CURRENT_TIMESTAMP(3)、b'0' 之类
                    if matches!(c.peek(), Some(Tok::Group(_) | Tok::Str(_))) {
                        c.bump();
                    }
                }
                Some(_) => {}
                None => d.other = true,
            },
            "AUTO_INCREMENT" => d.auto_increment = true,
            "COMMENT" => {
                c.value();
            }
            "ON" => {
                if c.kw("UPDATE") {
                    d.on_update = true;
                    c.bump();
                    if matches!(c.peek(), Some(Tok::Group(_))) {
                        c.bump();
                    }
                } else {
                    d.other = true;
                }
            }
            "GENERATED" | "AS" => {
                if w == "GENERATED" {
                    c.kw("ALWAYS");
                    c.kw("AS");
                }
                c.group();
                d.generated = Some(false);
            }
            "VIRTUAL" => d.generated = Some(false),
            "STORED" | "PERSISTENT" => d.generated = Some(true),
            "FIRST" => d.position = true,
            "AFTER" => {
                c.ident();
                d.position = true;
            }
            _ => d.other = true,
        }
    }
    d.ty = Some(col_type(&base, params.as_deref(), unsigned, zerofill));
    d
}

/// 规范化列类型:别名换正名,去掉不影响存储的写法差异(整数显示宽度、DECIMAL 的默认精度等)。
fn col_type(base: &str, params: Option<&str>, unsigned: bool, zerofill: bool) -> ColType {
    let base = match base {
        "integer" | "int4" => "int",
        "int1" => "tinyint",
        "int2" => "smallint",
        "int3" | "middleint" => "mediumint",
        "int8" => "bigint",
        "bool" | "boolean" => "tinyint",
        "dec" | "numeric" | "fixed" => "decimal",
        "real" | "float8" => "double",
        "float4" => "float",
        "character" => "char",
        other => other,
    }
    .to_string();
    let p: Option<String> = params.map(|p| p.chars().filter(|c| !c.is_whitespace()).collect());
    let mut values = None;
    let params = match base.as_str() {
        "tinyint" | "smallint" | "mediumint" | "int" | "bigint" | "year" => None,
        "decimal" => Some(match p.as_deref() {
            None | Some("") => "10,0".into(),
            Some(x) if !x.contains(',') => format!("{x},0"),
            Some(x) => x.to_string(),
        }),
        "char" | "binary" | "bit" => Some(p.filter(|x| !x.is_empty()).unwrap_or_else(|| "1".into())),
        "datetime" | "timestamp" | "time" => p.filter(|x| x != "0" && !x.is_empty()),
        "enum" | "set" => {
            values = params.map(|g| {
                lex(g)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|t| match t.tok {
                        Tok::Str(s) => Some(s),
                        _ => None,
                    })
                    .collect()
            });
            None
        }
        _ => p,
    };
    ColType { base, params, values, unsigned, zerofill }
}

/// 解析 information_schema.COLUMNS 的 COLUMN_TYPE(如 `varchar(20)`、`int(11) unsigned`、`enum('a','b')`)。
pub fn parse_column_type(column_type: &str) -> Option<ColType> {
    let toks = lex(column_type)?;
    let mut c = Cur::new(&toks);
    let base = c.word()?.to_ascii_lowercase();
    c.bump();
    let base = if base == "double" && c.kw("PRECISION") { "double".into() } else { base };
    let params = c.group();
    let mut unsigned = false;
    let mut zerofill = false;
    while let Some(w) = c.word() {
        c.bump();
        match w.as_str() {
            "UNSIGNED" => unsigned = true,
            "ZEROFILL" => zerofill = true,
            _ => {}
        }
    }
    Some(col_type(&base, params.as_deref(), unsigned, zerofill))
}

/// 一个 ALTER 子句 → 操作(表选项一段里可以有多个,用空白分隔)。ALGORITHM / LOCK 写进 `p`。
fn alter_spec(c: &mut Cur, src: &str, p: &mut Parsed) -> Vec<OpKind> {
    let Some(first) = c.word() else {
        return vec![OpKind::Unknown];
    };
    match first.as_str() {
        "ADD" => {
            c.bump();
            if c.kw("COLUMN") || !matches!(c.word().as_deref(), Some(
                "INDEX" | "KEY" | "UNIQUE" | "FULLTEXT" | "SPATIAL" | "PRIMARY" | "FOREIGN" | "CONSTRAINT" | "CHECK" | "PARTITION"
            )) {
                // ADD [COLUMN] (a INT, b INT) 一次加多列
                if let Some(g) = c.group() {
                    let Some(inner) = lex(&g) else { return vec![OpKind::Unknown] };
                    return inner
                        .split(|t| t.tok == Tok::Comma)
                        .map(|part| {
                            let mut cc = Cur::new(part);
                            match cc.ident() {
                                Some(name) => OpKind::AddColumn { name, def: column_def(&mut cc) },
                                None => OpKind::Unknown,
                            }
                        })
                        .collect();
                }
                return match c.ident() {
                    Some(name) => vec![OpKind::AddColumn { name, def: column_def(c) }],
                    None => vec![OpKind::Unknown],
                };
            }
            if c.kw("CONSTRAINT") && !matches!(c.word().as_deref(), Some("PRIMARY" | "UNIQUE" | "FOREIGN" | "CHECK")) {
                c.ident();
            }
            let index_name = |c: &mut Cur| {
                c.kw("INDEX");
                c.kw("KEY");
                if c.is("USING") {
                    None
                } else {
                    c.ident()
                }
            };
            match c.word().as_deref() {
                Some("INDEX" | "KEY") => vec![OpKind::AddIndex { name: index_name(c) }],
                Some("UNIQUE") => {
                    c.bump();
                    vec![OpKind::AddIndex { name: index_name(c) }]
                }
                Some("FULLTEXT") => {
                    c.bump();
                    vec![OpKind::AddFulltext { name: index_name(c) }]
                }
                Some("SPATIAL") => {
                    c.bump();
                    vec![OpKind::AddSpatial { name: index_name(c) }]
                }
                Some("PRIMARY") => vec![OpKind::AddPrimaryKey],
                Some("FOREIGN") => {
                    c.bump();
                    c.kw("KEY");
                    let name = if matches!(c.peek(), Some(Tok::Group(_))) { None } else { c.ident() };
                    vec![OpKind::AddForeignKey { name }]
                }
                Some("CHECK") => vec![OpKind::AddCheck],
                Some("PARTITION") => vec![OpKind::Partition],
                _ => vec![OpKind::Unknown],
            }
        }
        "DROP" => {
            c.bump();
            match c.word().as_deref() {
                Some("INDEX" | "KEY") => {
                    c.bump();
                    match c.ident() {
                        Some(name) if name.eq_ignore_ascii_case("PRIMARY") => vec![OpKind::DropPrimaryKey],
                        Some(name) => vec![OpKind::DropIndex { name }],
                        None => vec![OpKind::Unknown],
                    }
                }
                Some("PRIMARY") => vec![OpKind::DropPrimaryKey],
                Some("FOREIGN") => {
                    c.bump();
                    c.kw("KEY");
                    match c.ident() {
                        Some(name) => vec![OpKind::DropForeignKey { name }],
                        None => vec![OpKind::Unknown],
                    }
                }
                Some("CHECK" | "CONSTRAINT") => vec![OpKind::Unknown],
                Some("PARTITION") => vec![OpKind::Partition],
                _ => {
                    c.kw("COLUMN");
                    c.kw("IF");
                    c.kw("EXISTS");
                    match c.ident() {
                        Some(name) if c.done() || c.is("RESTRICT") || c.is("CASCADE") => vec![OpKind::DropColumn { name }],
                        _ => vec![OpKind::Unknown],
                    }
                }
            }
        }
        "MODIFY" => {
            c.bump();
            c.kw("COLUMN");
            match c.ident() {
                Some(name) => vec![OpKind::Modify { name, def: column_def(c) }],
                None => vec![OpKind::Unknown],
            }
        }
        "CHANGE" => {
            c.bump();
            c.kw("COLUMN");
            let (Some(old), Some(new)) = (c.ident(), c.ident()) else {
                return vec![OpKind::Unknown];
            };
            let def = column_def(c);
            if old.eq_ignore_ascii_case(&new) {
                vec![OpKind::Modify { name: old, def }]
            } else {
                vec![OpKind::Change { old, new, def }]
            }
        }
        "RENAME" => {
            c.bump();
            let pair = |c: &mut Cur| {
                let old = c.ident()?;
                c.kw("TO").then_some(())?;
                let new = c.ident()?;
                c.done().then_some((old, new))
            };
            if c.kw("COLUMN") {
                return match pair(c) {
                    Some((old, new)) => vec![OpKind::RenameColumn { old, new }],
                    None => vec![OpKind::Unknown],
                };
            }
            if c.kw("INDEX") || c.kw("KEY") {
                return match pair(c) {
                    Some((old, new)) => vec![OpKind::RenameIndex { old, new }],
                    None => vec![OpKind::Unknown],
                };
            }
            let _ = c.kw("TO") || c.kw("AS");
            match c.qualified(src) {
                Some(_) if c.done() => vec![OpKind::RenameTable],
                _ => vec![OpKind::Unknown],
            }
        }
        "ALTER" => {
            c.bump();
            if c.kw("INDEX") {
                let name = c.ident();
                return match (name, c.word().as_deref()) {
                    (Some(name), Some("VISIBLE" | "INVISIBLE")) => vec![OpKind::IndexVisibility { name }],
                    _ => vec![OpKind::Unknown],
                };
            }
            if c.is("CHECK") || c.is("CONSTRAINT") {
                return vec![OpKind::Unknown];
            }
            c.kw("COLUMN");
            let Some(name) = c.ident() else { return vec![OpKind::Unknown] };
            if (c.is("SET") || c.is("DROP")) && c.is_at(1, "DEFAULT") {
                // SET DEFAULT (表达式) 在 8.0.23 前不支持,且表达式默认值的规则不同:判未知
                c.bump();
                c.bump();
                if matches!(c.peek(), Some(Tok::Group(_))) {
                    return vec![OpKind::Unknown];
                }
                return vec![OpKind::SetDefault { name }];
            }
            if c.kw("SET") && (c.is("VISIBLE") || c.is("INVISIBLE")) {
                return vec![OpKind::ColumnVisibility { name }];
            }
            vec![OpKind::Unknown]
        }
        "ALGORITHM" => {
            c.bump();
            p.algorithm = c.value().map(|v| v.to_ascii_uppercase());
            vec![]
        }
        "LOCK" => {
            c.bump();
            p.lock = c.value().map(|v| v.to_ascii_uppercase());
            vec![]
        }
        "FORCE" => vec![OpKind::Rebuild],
        "CONVERT" => vec![OpKind::ConvertCharset],
        "WITH" | "WITHOUT" if c.is_at(1, "VALIDATION") => vec![],
        "COALESCE" | "REORGANIZE" | "EXCHANGE" | "ANALYZE" | "CHECK" | "OPTIMIZE" | "REBUILD" | "REPAIR" | "TRUNCATE"
        | "REMOVE" | "PARTITION" | "DISCARD" | "IMPORT" => {
            if first == "CHECK" && matches!(c.peek_at(1), Some(Tok::Group(_))) {
                return vec![OpKind::Unknown];
            }
            vec![OpKind::Partition]
        }
        _ => table_options(c),
    }
}

/// 表选项(`ENGINE=InnoDB ROW_FORMAT=DYNAMIC`,空白分隔)。认不出的选项记为未知。
fn table_options(c: &mut Cur) -> Vec<OpKind> {
    let mut out: Vec<OpKind> = Vec::new();
    let push = |k: OpKind, out: &mut Vec<OpKind>| {
        if !out.contains(&k) {
            out.push(k);
        }
    };
    while let Some(w) = c.word() {
        c.bump();
        match w.as_str() {
            "DEFAULT" => continue,
            "CHARACTER" | "CHARSET" | "COLLATE" => {
                c.kw("SET");
                c.value();
                push(OpKind::TableCharset, &mut out);
            }
            "ENGINE" | "TYPE" => match c.value() {
                Some(engine) => push(OpKind::Engine { engine }, &mut out),
                None => push(OpKind::Unknown, &mut out),
            },
            "ROW_FORMAT" | "KEY_BLOCK_SIZE" => {
                c.value();
                push(OpKind::RowFormat, &mut out);
            }
            "AUTO_INCREMENT" => {
                c.value();
                push(OpKind::AutoIncrement, &mut out);
            }
            "STATS_PERSISTENT" | "STATS_AUTO_RECALC" | "STATS_SAMPLE_PAGES" => {
                c.value();
                push(OpKind::Stats, &mut out);
            }
            "COMMENT" => {
                c.value();
                push(OpKind::TableComment, &mut out);
            }
            "PARTITION" => {
                push(OpKind::Partition, &mut out);
                break;
            }
            _ => {
                push(OpKind::Unknown, &mut out);
                break;
            }
        }
    }
    if !c.done() || out.is_empty() {
        push(OpKind::Unknown, &mut out);
    }
    out
}

/// 解析一条改表结构的语句;不是(或是多表的 OPTIMIZE 等说不清的写法)返回 None。
pub fn parse(stmt: &str) -> Option<Parsed> {
    let text = stmt.trim().trim_end_matches(';').trim_end().to_string();
    let toks = lex(&text)?;
    let mut c = Cur::new(&toks);
    let first = c.word()?;
    let mut p = Parsed {
        kind: StmtKind::AlterTable,
        schema: None,
        table: String::new(),
        table_raw: String::new(),
        ops: vec![],
        algorithm: None,
        lock: None,
        text: text.clone(),
    };
    let set_table = |p: &mut Parsed, c: &mut Cur| -> Option<()> {
        let (schema, table, raw) = c.qualified(&text)?;
        p.schema = schema;
        p.table = table;
        p.table_raw = raw;
        Some(())
    };
    match first.as_str() {
        "ALTER" => {
            c.bump();
            if c.kw("ONLINE") {
                p.lock = Some("NONE".into());
            }
            c.kw("OFFLINE");
            c.kw("IGNORE");
            if !c.kw("TABLE") {
                return None;
            }
            set_table(&mut p, &mut c)?;
            // MariaDB:WAIT n / NOWAIT
            if c.kw("WAIT") {
                c.bump();
            }
            c.kw("NOWAIT");
            let rest = &toks[c.i..];
            for seg in rest.split(|t| t.tok == Tok::Comma) {
                if seg.is_empty() {
                    continue;
                }
                let clause = text[seg[0].start..seg[seg.len() - 1].end].to_string();
                let mut sc = Cur::new(seg);
                for kind in alter_spec(&mut sc, &text, &mut p) {
                    p.ops.push(Op { clause: clause.clone(), kind });
                }
            }
            if p.ops.is_empty() && p.algorithm.is_none() && p.lock.is_none() {
                // 空 ALTER TABLE t:什么也不做,不评估
                return None;
            }
        }
        "CREATE" => {
            c.bump();
            p.kind = StmtKind::CreateIndex;
            c.kw("ONLINE");
            c.kw("OFFLINE");
            let kind = match c.word().as_deref() {
                Some("UNIQUE") => {
                    c.bump();
                    "INDEX"
                }
                Some("FULLTEXT") => {
                    c.bump();
                    "FULLTEXT"
                }
                Some("SPATIAL") => {
                    c.bump();
                    "SPATIAL"
                }
                _ => "INDEX",
            };
            if !c.kw("INDEX") {
                return None;
            }
            let name = c.ident();
            if c.kw("USING") {
                c.bump();
            }
            if !c.kw("ON") {
                return None;
            }
            set_table(&mut p, &mut c)?;
            c.group()?;
            index_tail(&mut c, &mut p);
            let op = match kind {
                "FULLTEXT" => OpKind::AddFulltext { name },
                "SPATIAL" => OpKind::AddSpatial { name },
                _ => OpKind::AddIndex { name },
            };
            p.ops.push(Op { clause: text.clone(), kind: op });
        }
        "DROP" => {
            c.bump();
            p.kind = StmtKind::DropIndex;
            c.kw("ONLINE");
            c.kw("OFFLINE");
            if !c.kw("INDEX") {
                return None;
            }
            let name = c.ident()?;
            // 没有 ON 是 PG 的写法,不是这里要评估的
            if !c.kw("ON") {
                return None;
            }
            set_table(&mut p, &mut c)?;
            index_tail(&mut c, &mut p);
            let op = if name.eq_ignore_ascii_case("PRIMARY") { OpKind::DropPrimaryKey } else { OpKind::DropIndex { name } };
            p.ops.push(Op { clause: text.clone(), kind: op });
        }
        "OPTIMIZE" => {
            c.bump();
            p.kind = StmtKind::Optimize;
            let _ = c.kw("NO_WRITE_TO_BINLOG") || c.kw("LOCAL");
            if !c.kw("TABLE") && !c.kw("TABLES") {
                return None;
            }
            set_table(&mut p, &mut c)?;
            // 一次优化多张表:逐张的大小都不同,不评估
            if !c.done() {
                return None;
            }
            p.ops.push(Op { clause: text.clone(), kind: OpKind::Rebuild });
        }
        _ => return None,
    }
    Some(p)
}

/// CREATE / DROP INDEX 末尾的 ALGORITHM / LOCK 选项(其余索引选项不影响判定)。
fn index_tail(c: &mut Cur, p: &mut Parsed) {
    while !c.done() {
        if c.kw("ALGORITHM") {
            p.algorithm = c.value().map(|v| v.to_ascii_uppercase());
        } else if c.kw("LOCK") {
            p.lock = c.value().map(|v| v.to_ascii_uppercase());
        } else {
            c.bump();
        }
    }
}

/// 是否是要做改表结构评估的语句;是则返回审查弹窗里的标签与表名。
pub fn schema_change(stmt: &str) -> Option<(&'static str, String)> {
    parse(stmt).map(|p| (p.kind.label(), p.table))
}

// ---------- 服务器版本 ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    Mysql,
    Mariadb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Server {
    pub flavor: Flavor,
    pub version: (u32, u32, u32),
}

impl Server {
    /// 解析 `SELECT VERSION()`:`8.4.3`、`8.0.35-0ubuntu0.22.04.1`、`5.7.44-log`、`10.11.6-MariaDB-0+deb12u1`、
    /// 以及经由复制协议看到的 `5.5.5-10.6.12-MariaDB`。
    pub fn parse(v: &str) -> Option<Self> {
        let mariadb = v.to_ascii_lowercase().contains("mariadb");
        let s = if mariadb { v.strip_prefix("5.5.5-").unwrap_or(v) } else { v };
        let mut nums = s.split(|c: char| !c.is_ascii_digit()).take(3).map(|x| x.parse::<u32>().ok());
        let major = nums.next()??;
        let minor = nums.next()??;
        let patch = nums.next().flatten().unwrap_or(0);
        Some(Self { flavor: if mariadb { Flavor::Mariadb } else { Flavor::Mysql }, version: (major, minor, patch) })
    }

    pub fn mysql(major: u32, minor: u32, patch: u32) -> Self {
        Self { flavor: Flavor::Mysql, version: (major, minor, patch) }
    }

    pub fn mariadb(major: u32, minor: u32, patch: u32) -> Self {
        Self { flavor: Flavor::Mariadb, version: (major, minor, patch) }
    }

    fn my(&self, v: (u32, u32, u32)) -> bool {
        self.flavor == Flavor::Mysql && self.version >= v
    }

    fn maria(&self, v: (u32, u32, u32)) -> bool {
        self.flavor == Flavor::Mariadb && self.version >= v
    }

    /// 规则表覆盖的版本:MySQL 5.7+、MariaDB 10.2+
    fn covered(&self) -> bool {
        self.my((5, 7, 0)) || self.maria((10, 2, 0))
    }

    pub fn label(&self) -> String {
        let (a, b, c) = self.version;
        let name = match self.flavor {
            Flavor::Mysql => "MySQL",
            Flavor::Mariadb => "MariaDB",
        };
        format!("{name} {a}.{b}.{c}")
    }
}

// ---------- 表元数据 ----------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColumnMeta {
    pub name: String,
    /// COLUMN_TYPE(小写)
    pub column_type: String,
    pub nullable: bool,
    pub charset: Option<String>,
    pub collation: Option<String>,
    /// CHARACTER_MAXIMUM_LENGTH(字符数)
    pub max_chars: Option<u64>,
    /// CHARACTER_OCTET_LENGTH(最大字节数)
    pub octets: Option<u64>,
    /// EXTRA(小写):auto_increment、on update …、VIRTUAL GENERATED 等
    pub extra: String,
    pub primary: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableMeta {
    pub engine: Option<String>,
    pub row_format: Option<String>,
    pub create_options: String,
    /// TABLE_ROWS(InnoDB 为估算值)
    pub rows: Option<u64>,
    /// DATA_LENGTH + INDEX_LENGTH
    pub bytes: Option<u64>,
    /// 表的默认排序规则(TABLE_COLLATION)
    pub collation: Option<String>,
    pub columns: Vec<ColumnMeta>,
    /// 出现在任一索引里的列(小写)
    pub indexed_columns: Vec<String>,
    pub has_fulltext: bool,
    /// 当前会话的 foreign_key_checks;取不到为 None
    pub foreign_key_checks: Option<bool>,
    /// MySQL 8.0.29+ 的 INSTANT 行版本数(上限 64)
    pub row_versions: Option<u32>,
    /// 字符集 → 默认排序规则(information_schema.CHARACTER_SETS)
    pub default_collations: HashMap<String, String>,
}

impl TableMeta {
    fn column(&self, name: &str) -> Option<&ColumnMeta> {
        self.columns.iter().find(|c| c.name.eq_ignore_ascii_case(name))
    }
    fn indexed(&self, name: &str) -> bool {
        self.indexed_columns.iter().any(|c| c.eq_ignore_ascii_case(name))
    }
    fn compressed(&self) -> bool {
        self.row_format.as_deref().is_some_and(|f| f.eq_ignore_ascii_case("compressed"))
            || self.create_options.to_ascii_lowercase().contains("row_format=compressed")
    }
    fn innodb(&self) -> bool {
        self.engine.as_deref().is_some_and(|e| e.eq_ignore_ascii_case("innodb"))
    }
}

/// 排序规则 → 字符集(`utf8mb4_0900_ai_ci` → `utf8mb4`,`binary` → `binary`)。
fn charset_of(collation: &str) -> String {
    collation.split('_').next().unwrap_or(collation).to_ascii_lowercase()
}

// ---------- 判定 ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Algorithm {
    Instant,
    Inplace,
    Copy,
}

/// 一个操作的能力:能否 INSTANT;能否 INPLACE,INPLACE 时是否重建、是否允许并发 DML、是否要扫全表。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Caps {
    instant: bool,
    inplace: bool,
    rebuild: bool,
    dml: bool,
    scan: bool,
}

/// 只改元数据:INSTANT
const INSTANT: Caps = Caps { instant: true, inplace: true, rebuild: false, dml: true, scan: false };
/// 能 INSTANT,强制 INPLACE 时重建(MySQL 8.0 的加列 / 删列)
const INSTANT_OR_REBUILD: Caps = Caps { instant: true, inplace: true, rebuild: true, dml: true, scan: true };
/// INPLACE 只改元数据
const META: Caps = Caps { instant: false, inplace: true, rebuild: false, dml: true, scan: false };
/// INPLACE 不重建但要扫表建索引
const BUILD: Caps = Caps { instant: false, inplace: true, rebuild: false, dml: true, scan: true };
/// INPLACE 重建,允许并发 DML
const REBUILD: Caps = Caps { instant: false, inplace: true, rebuild: true, dml: true, scan: true };
/// 只能 COPY:整表复制,期间阻塞写入
const COPY: Caps = Caps { instant: false, inplace: false, rebuild: true, dml: false, scan: true };

fn combine(list: &[Caps]) -> Caps {
    Caps {
        instant: list.iter().all(|c| c.instant),
        inplace: list.iter().all(|c| c.inplace),
        rebuild: list.iter().any(|c| c.rebuild),
        dml: list.iter().all(|c| c.dml),
        scan: list.iter().any(|c| c.scan),
    }
}

/// 单个操作的判定:能力(None 为未知)+ 依据说明代码。
struct Judged {
    caps: Option<Caps>,
    note: Option<&'static str>,
}

fn known(caps: Caps, note: &'static str) -> Judged {
    Judged { caps: Some(caps), note: Some(note) }
}

fn unknown(note: &'static str) -> Judged {
    Judged { caps: None, note: Some(note) }
}

/// 语句级上下文:同一条里既删主键又加主键时,删主键按「换主键」算。
struct Ctx {
    add_pk: bool,
    drop_pk: bool,
}

/// 加列 / 删列能否 INSTANT(版本 + 表的限制),不能时给出原因。
fn instant_column(s: &Server, m: &TableMeta, position: bool, drop: bool) -> Result<(), &'static str> {
    // 删列、加到中间:MySQL 8.0.29+ / MariaDB 10.4+;加到末尾:8.0.12+ / 10.3.2+
    let version_ok = if drop || position {
        s.my((8, 0, 29)) || s.maria((10, 4, 0))
    } else {
        s.my((8, 0, 12)) || s.maria((10, 3, 2))
    };
    if !version_ok {
        return Err(match (drop, position, s.flavor) {
            (true, _, Flavor::Mysql) => "instant_drop_8029",
            (false, true, Flavor::Mysql) if s.my((8, 0, 12)) => "instant_position_8029",
            (_, _, Flavor::Mysql) => "instant_add_8012",
            (_, _, Flavor::Mariadb) => "instant_mariadb_version",
        });
    }
    if m.compressed() {
        return Err("instant_compressed");
    }
    if m.has_fulltext {
        return Err("instant_fulltext");
    }
    if s.flavor == Flavor::Mysql && m.row_versions.is_some_and(|v| v >= 64) {
        return Err("instant_row_versions");
    }
    Ok(())
}

/// MODIFY / CHANGE 的列定义与现有列比较。
fn judge_modify(s: &Server, m: &TableMeta, name: &str, def: &ColumnDef) -> Judged {
    let Some(col) = m.column(name) else {
        return unknown("column_not_found");
    };
    if def.other || def.expr_default {
        return unknown("unknown_attr");
    }
    let Some(new_ty) = &def.ty else { return unknown("unknown_attr") };
    let Some(old_ty) = parse_column_type(&col.column_type) else {
        return unknown("unknown_attr");
    };
    // 生成列、自增属性的变化:规则表没覆盖,不猜
    let old_generated = col.extra.contains("generated");
    if old_generated || def.generated.is_some() {
        return unknown("generated_change");
    }
    if col.extra.contains("auto_increment") != def.auto_increment {
        return unknown("auto_increment_change");
    }
    if col.extra.contains("on update") != def.on_update {
        return unknown("unknown_attr");
    }
    let mut parts: Vec<Judged> = Vec::new();
    // 字符集 / 排序规则:没写时跟表的默认值走(老列的字符集与表默认不同时,MODIFY 会悄悄改掉它)
    let text = |t: &ColType| {
        matches!(t.base.as_str(), "char" | "varchar" | "tinytext" | "text" | "mediumtext" | "longtext" | "enum" | "set")
    };
    if let (Some(old_cs), true) = (&col.charset, text(&old_ty) && text(new_ty)) {
        let (new_cs, new_coll) = match (&def.charset, &def.collate) {
            (_, Some(coll)) => (charset_of(coll), Some(coll.clone())),
            (Some(cs), None) => (cs.clone(), m.default_collations.get(cs).cloned()),
            (None, None) => match &m.collation {
                Some(tc) => (charset_of(tc), Some(tc.clone())),
                None => return unknown("unknown_attr"),
            },
        };
        let new_cs = if new_cs == "utf8" && s.flavor == Flavor::Mysql { "utf8mb3".to_string() } else { new_cs };
        if &new_cs != old_cs && !(new_cs == "utf8" && old_cs == "utf8mb3") {
            return known(COPY, "charset_change");
        }
        match (new_coll, &col.collation) {
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => {}
            _ => return unknown("collation_change"),
        }
    }
    if old_ty != *new_ty {
        let same_family = |b: &str| old_ty.base == b && new_ty.base == b;
        if same_family("varchar") && old_ty.unsigned == new_ty.unsigned {
            let (Some(old_len), Some(new_len)) = (
                old_ty.params.as_deref().and_then(|p| p.parse::<u64>().ok()),
                new_ty.params.as_deref().and_then(|p| p.parse::<u64>().ok()),
            ) else {
                return unknown("unknown_attr");
            };
            if new_len < old_len {
                return known(COPY, "varchar_shrink");
            }
            // 每个字符最多占几个字节:由现有列的 字节上限 / 字符上限 得出(字符集没变)
            let mb = match (col.octets, col.max_chars) {
                (Some(o), Some(c)) if c > 0 => o / c,
                _ => return unknown("unknown_attr"),
            };
            let (old_bytes, new_bytes) = (old_len * mb, new_len * mb);
            if (old_bytes <= 255) == (new_bytes <= 255) {
                parts.push(known(META, "varchar_same_tier"));
            } else if s.maria((10, 4, 0)) {
                // MariaDB 10.4 起对跨档扩展另有处理,文档说法与行格式有关:不猜
                return unknown("varchar_cross_mariadb");
            } else {
                return known(COPY, "varchar_cross_255");
            }
        } else if (same_family("enum") || same_family("set")) && old_ty.values.is_some() && new_ty.values.is_some() {
            let (old_v, new_v) = (old_ty.values.as_ref().unwrap(), new_ty.values.as_ref().unwrap());
            let size = |n: usize, set: bool| {
                if set {
                    match n.div_ceil(8) {
                        x @ 0..=4 => x,
                        _ => 8,
                    }
                } else if n <= 255 {
                    1
                } else {
                    2
                }
            };
            let set = old_ty.base == "set";
            let appended = new_v.len() > old_v.len() && new_v[..old_v.len()] == old_v[..];
            if appended && size(old_v.len(), set) == size(new_v.len(), set) {
                let caps = if s.my((8, 0, 12)) || s.maria((10, 3, 7)) { INSTANT } else { META };
                parts.push(known(caps, "enum_append"));
            } else {
                return known(COPY, "enum_reorder");
            }
        } else {
            return known(COPY, "type_change");
        }
    }
    // PRIMARY KEY 的列总是 NOT NULL
    let new_nullable = !def.not_null.unwrap_or(col.primary);
    if new_nullable != col.nullable {
        if s.maria((10, 4, 0)) {
            return unknown("nullability_mariadb");
        }
        parts.push(known(REBUILD, "nullability"));
    }
    if def.position {
        let caps = if s.maria((10, 4, 0)) && instant_column(s, m, true, false).is_ok() { INSTANT } else { REBUILD };
        parts.push(known(caps, "reorder"));
    }
    if parts.is_empty() {
        // 类型、字符集、可空都没变:只改默认值 / 注释等元数据
        let caps = if s.my((8, 0, 12)) || s.maria((10, 3, 0)) { INSTANT } else { META };
        return known(caps, "metadata_only");
    }
    merge(parts)
}

/// 合并一个操作内的几部分判定(如改名 + 改可空):能力取交集,说明取第一个「最差」的。
fn merge(parts: Vec<Judged>) -> Judged {
    if parts.iter().any(|p| p.caps.is_none()) {
        return parts.into_iter().find(|p| p.caps.is_none()).unwrap();
    }
    let caps: Vec<Caps> = parts.iter().map(|p| p.caps.unwrap()).collect();
    let combined = combine(&caps);
    let worst = parts
        .iter()
        .max_by_key(|p| {
            let c = p.caps.unwrap();
            (!c.inplace, !c.dml, c.rebuild && !c.instant, !c.instant)
        })
        .and_then(|p| p.note);
    Judged { caps: Some(combined), note: worst }
}

fn judge(op: &OpKind, s: &Server, m: &TableMeta, ctx: &Ctx) -> Judged {
    let maria = s.flavor == Flavor::Mariadb;
    let my8 = s.my((8, 0, 0));
    let mut j = match op {
        OpKind::AddColumn { def, .. } => {
            if def.other || def.expr_default {
                return unknown("unknown_attr");
            }
            if def.auto_increment {
                // 加自增列:要给每行填值,数据大幅重组;官方表:INPLACE 重建且不允许并发 DML
                return known(Caps { dml: false, ..REBUILD }, "add_auto_increment");
            }
            match def.generated {
                Some(true) => return known(COPY, "add_stored_generated"),
                Some(false) if maria => return unknown("generated_mariadb"),
                Some(false) => {
                    let caps = if s.my((8, 0, 12)) { INSTANT } else { META };
                    return known(caps, "add_virtual_generated");
                }
                None => {}
            }
            match instant_column(s, m, def.position, false) {
                Ok(()) => known(INSTANT_OR_REBUILD, "instant_add"),
                Err(why) => known(REBUILD, why),
            }
        }
        OpKind::DropColumn { name } => {
            let Some(col) = m.column(name) else { return unknown("column_not_found") };
            if col.extra.contains("virtual generated") {
                let caps = if s.my((8, 0, 12)) { INSTANT } else { META };
                return known(caps, "drop_virtual_generated");
            }
            if m.indexed(name) {
                known(REBUILD, "drop_indexed_column")
            } else {
                match instant_column(s, m, false, true) {
                    Ok(()) => known(INSTANT_OR_REBUILD, "instant_drop"),
                    Err(why) => known(REBUILD, why),
                }
            }
        }
        OpKind::Modify { name, def } => judge_modify(s, m, name, def),
        OpKind::Change { old, def, .. } => {
            // 同等程度时 merge 取后一个的说明:改名放后面
            merge(vec![judge_modify(s, m, old, def), rename_column(s)])
        }
        OpKind::RenameColumn { old, .. } => {
            if m.column(old).is_none() {
                return unknown("column_not_found");
            }
            rename_column(s)
        }
        OpKind::SetDefault { name } => {
            if m.column(name).is_none() {
                return unknown("column_not_found");
            }
            let caps = if s.my((8, 0, 12)) || s.maria((10, 3, 0)) { INSTANT } else { META };
            known(caps, "metadata_only")
        }
        OpKind::ColumnVisibility { .. } => {
            if s.my((8, 0, 23)) {
                known(INSTANT, "metadata_only")
            } else {
                unknown("unsupported_syntax")
            }
        }
        OpKind::AddIndex { .. } => known(BUILD, "add_index"),
        OpKind::AddFulltext { .. } => {
            // 首个全文索引要加隐藏的 FTS_DOC_ID 列,重建整表;已有全文索引或用户自建了 FTS_DOC_ID 列时不重建
            let has_doc_id = m.column("FTS_DOC_ID").is_some();
            if m.has_fulltext || has_doc_id {
                known(Caps { dml: false, ..BUILD }, "add_fulltext")
            } else {
                known(Caps { dml: false, ..REBUILD }, "first_fulltext")
            }
        }
        OpKind::AddSpatial { .. } => {
            if s.my((5, 7, 5)) || s.maria((10, 2, 2)) {
                known(Caps { dml: false, ..BUILD }, "add_spatial")
            } else {
                unknown("unsupported_version")
            }
        }
        OpKind::DropIndex { .. } => known(META, "metadata_only"),
        OpKind::RenameIndex { .. } => {
            if s.my((5, 7, 0)) || s.maria((10, 5, 2)) {
                known(META, "metadata_only")
            } else {
                unknown("unsupported_syntax")
            }
        }
        OpKind::IndexVisibility { .. } => {
            if my8 {
                known(META, "metadata_only")
            } else {
                unknown("unsupported_syntax")
            }
        }
        OpKind::AddPrimaryKey => known(REBUILD, "add_primary_key"),
        OpKind::DropPrimaryKey => {
            if ctx.add_pk {
                known(REBUILD, "replace_primary_key")
            } else {
                known(COPY, "drop_primary_key")
            }
        }
        OpKind::AddForeignKey { .. } => match m.foreign_key_checks {
            // foreign_key_checks 开着时要逐行校验已有数据,只能 COPY
            Some(true) => known(COPY, "fk_checks_on"),
            Some(false) => known(META, "fk_checks_off"),
            None => unknown("fk_checks_unknown"),
        },
        OpKind::DropForeignKey { .. } => known(META, "metadata_only"),
        OpKind::AddCheck => {
            if s.my((8, 0, 16)) {
                known(COPY, "add_check")
            } else {
                unknown("unknown_op")
            }
        }
        OpKind::ConvertCharset => known(COPY, "convert_charset"),
        OpKind::TableCharset => {
            if my8 {
                known(META, "table_charset")
            } else if s.my((5, 7, 0)) {
                known(REBUILD, "table_charset_57")
            } else {
                unknown("unknown_op")
            }
        }
        OpKind::Engine { engine } => {
            if m.engine.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(engine)) {
                known(REBUILD, "null_rebuild")
            } else {
                known(COPY, "engine_change")
            }
        }
        OpKind::Rebuild => known(REBUILD, "null_rebuild"),
        OpKind::RowFormat => known(REBUILD, "row_format"),
        OpKind::AutoIncrement => known(BUILD_NO_SCAN, "auto_increment_value"),
        OpKind::Stats => known(META, "metadata_only"),
        OpKind::TableComment => {
            if my8 {
                known(META, "metadata_only")
            } else {
                unknown("unknown_op")
            }
        }
        OpKind::RenameTable => {
            let caps = if my8 { INSTANT } else { META };
            known(caps, "metadata_only")
        }
        OpKind::Partition => unknown("partition"),
        OpKind::Unknown => unknown("unknown_op"),
    };
    // 表上有全文索引时,需要重建的操作不能 INPLACE(8.4 实测加列 / 删列 / FORCE 都退成 COPY)
    if let Some(c) = j.caps {
        if m.has_fulltext && c.rebuild && !c.instant && c.inplace && !matches!(op, OpKind::AddFulltext { .. }) {
            j = if maria { unknown("fulltext_rebuild_mariadb") } else { known(COPY, "fulltext_rebuild_copy") };
        }
    }
    j
}

/// INPLACE 不重建也不扫表(改自增值)
const BUILD_NO_SCAN: Caps = META;

fn rename_column(s: &Server) -> Judged {
    let caps = if s.my((8, 0, 28)) || s.maria((10, 3, 0)) { INSTANT } else { META };
    known(caps, "rename_column")
}

// ---------- 结论 ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Low,
    Medium,
    High,
    Unknown,
}

/// 单个子操作的判定(按服务器默认算法)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpVerdict {
    pub clause: String,
    /// 操作代码(add_column、modify_column…)
    pub op: &'static str,
    pub target: Option<String>,
    /// None:未知
    pub algorithm: Option<Algorithm>,
    pub rebuild: Option<bool>,
    pub concurrent_dml: Option<bool>,
    /// 依据说明代码
    pub note: Option<&'static str>,
}

/// 一条改表结构语句的评估。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlterAssessment {
    pub statement: &'static str,
    /// 表名(不带库名)
    pub table: String,
    /// 「MySQL 8.4.9」
    pub server: String,
    pub ops: Vec<OpVerdict>,
    /// 整条语句实际会用的算法;None 为未知
    pub algorithm: Option<Algorithm>,
    pub rebuild: Option<bool>,
    /// 执行期间是否阻塞写入
    pub blocks_writes: Option<bool>,
    /// 语句里显式写的 ALGORITHM / LOCK(原样展示)
    pub requested: Option<String>,
    /// 显式子句满足不了:服务器会直接报错、不执行(原因代码)
    pub fails: Option<&'static str>,
    pub risk: Risk,
    pub rows: Option<u64>,
    pub bytes: Option<u64>,
    /// 耗时量级:metadata | seconds | minutes | hours | days
    pub duration: Option<&'static str>,
    /// 加上保护子句的写法:不满足时服务器直接报错,而不是悄悄锁表
    pub guarded: Option<String>,
    /// 建议改用 gh-ost / pt-online-schema-change
    pub suggest_osc: bool,
    /// 表级说明代码(table_not_found、non_innodb、unsupported_version…)
    pub notes: Vec<&'static str>,
}

/// 大表:行数或体积超过这个量级,重建 / 建索引就是分钟级以上
pub const BIG_ROWS: u64 = 1_000_000;
pub const BIG_BYTES: u64 = 1 << 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cost {
    Metadata,
    Scan,
    Rebuild,
    Copy,
}

struct Outcome {
    algorithm: Option<Algorithm>,
    rebuild: Option<bool>,
    blocks_writes: Option<bool>,
    fails: Option<&'static str>,
    cost: Option<Cost>,
}

const UNKNOWN_OUTCOME: Outcome = Outcome { algorithm: None, rebuild: None, blocks_writes: None, fails: None, cost: None };

fn fails(why: &'static str) -> Outcome {
    Outcome { fails: Some(why), ..UNKNOWN_OUTCOME }
}

/// 按显式的 ALGORITHM / LOCK 求整条语句的结果。`list` 里 None 为未知的操作。
fn resolve(list: &[Option<Caps>], alg: Option<&str>, lock: Option<&str>, s: &Server) -> Outcome {
    let alg = alg.filter(|a| *a != "DEFAULT");
    let lock = lock.filter(|l| *l != "DEFAULT");
    let copy = |lock: Option<&str>| {
        if lock == Some("NONE") {
            fails("lock_none_unsupported")
        } else {
            Outcome {
                algorithm: Some(Algorithm::Copy),
                rebuild: Some(true),
                blocks_writes: Some(true),
                fails: None,
                cost: Some(Cost::Copy),
            }
        }
    };
    // MySQL:ALGORITHM=INSTANT 不能再带 LOCK 子句(报 1221)
    if alg == Some("INSTANT") && lock.is_some() && s.flavor == Flavor::Mysql {
        return fails("instant_with_lock");
    }
    if alg == Some("COPY") {
        return copy(lock);
    }
    let known: Vec<Caps> = list.iter().flatten().copied().collect();
    if known.len() < list.len() {
        // 有未知的操作:只有能确定的结论才给出
        return match alg {
            None if known.iter().any(|c| !c.inplace) => copy(lock),
            Some("INSTANT") if known.iter().any(|c| !c.instant) => fails("instant_unsupported"),
            Some("INPLACE" | "NOCOPY") if known.iter().any(|c| !c.inplace) => fails("inplace_unsupported"),
            _ => UNKNOWN_OUTCOME,
        };
    }
    let k = combine(&known);
    let instant = Outcome {
        algorithm: Some(Algorithm::Instant),
        rebuild: Some(false),
        blocks_writes: Some(false),
        fails: None,
        cost: Some(Cost::Metadata),
    };
    let inplace = |rebuild: bool| {
        if lock == Some("NONE") && !k.dml {
            return fails("lock_none_unsupported");
        }
        let cost = if rebuild {
            Cost::Rebuild
        } else if k.scan {
            Cost::Scan
        } else {
            Cost::Metadata
        };
        Outcome {
            algorithm: Some(Algorithm::Inplace),
            rebuild: Some(rebuild),
            blocks_writes: Some(!k.dml || matches!(lock, Some("SHARED" | "EXCLUSIVE"))),
            fails: None,
            cost: Some(cost),
        }
    };
    match alg {
        Some("INSTANT") => {
            if k.instant {
                instant
            } else {
                fails("instant_unsupported")
            }
        }
        Some("INPLACE") => {
            if !k.inplace {
                fails("inplace_unsupported")
            } else if k.instant && s.flavor == Flavor::Mariadb {
                // MariaDB 的 ALGORITHM 是「至少这么好」:能 INSTANT 就 INSTANT
                instant
            } else {
                // MySQL 指定 INPLACE 时不会走 INSTANT:8.0 的加列会因此重建整表
                inplace(k.rebuild)
            }
        }
        Some("NOCOPY") => {
            if !k.inplace || (k.rebuild && !k.instant) {
                fails("nocopy_unsupported")
            } else if k.instant {
                instant
            } else {
                inplace(false)
            }
        }
        Some(_) => UNKNOWN_OUTCOME,
        None => {
            if k.instant {
                instant
            } else if k.inplace {
                inplace(k.rebuild)
            } else {
                copy(lock)
            }
        }
    }
}

/// 耗时量级(粗估):按体积与吞吐估秒数再分档。吞吐取保守值:扫表建索引 ~50MB/s、INPLACE 重建 ~25MB/s、
/// COPY ~10MB/s(还要写 undo / binlog、重建全部二级索引)。只用来给出量级,不是精确预测。
fn duration(cost: Cost, rows: Option<u64>, bytes: Option<u64>) -> Option<&'static str> {
    if cost == Cost::Metadata {
        return Some("metadata");
    }
    if rows.is_none() && bytes.is_none() {
        return None;
    }
    let size = bytes.unwrap_or(0).max(rows.unwrap_or(0) * 100);
    let rate: u64 = match cost {
        Cost::Scan => 50 << 20,
        Cost::Rebuild => 25 << 20,
        _ => 10 << 20,
    };
    let secs = size / rate;
    Some(match secs {
        0..60 => "seconds",
        60..3600 => "minutes",
        3600..86400 => "hours",
        _ => "days",
    })
}

/// 评估一条已解析的语句。`meta` 为 None 表示没取到表信息(表不存在等),按列判定的规则都给不出结论。
pub fn assess(p: &Parsed, s: &Server, meta: Option<&TableMeta>) -> AlterAssessment {
    let mut notes: Vec<&'static str> = Vec::new();
    let ctx = Ctx {
        add_pk: p.ops.iter().any(|o| o.kind == OpKind::AddPrimaryKey),
        drop_pk: p.ops.iter().any(|o| o.kind == OpKind::DropPrimaryKey),
    };
    let blocked: Option<&'static str> = if !s.covered() {
        Some("unsupported_version")
    } else if meta.is_none() {
        Some("table_not_found")
    } else if !meta.is_some_and(|m| m.innodb()) {
        // MyISAM 等:Online DDL 规则表只适用于 InnoDB
        Some("non_innodb")
    } else {
        None
    };
    if let Some(b) = blocked {
        notes.push(b);
    }
    let judged: Vec<Judged> = p
        .ops
        .iter()
        .map(|o| match (blocked, meta) {
            (None, Some(m)) => {
                let mut j = judge(&o.kind, s, m, &ctx);
                // 换主键时加主键那一侧也已按「重建」算,不用重复说明
                if ctx.drop_pk && o.kind == OpKind::AddPrimaryKey {
                    j.note = Some("replace_primary_key");
                }
                j
            }
            (b, _) => unknown(b.unwrap_or("unknown_op")),
        })
        .collect();
    let ops: Vec<OpVerdict> = p
        .ops
        .iter()
        .zip(&judged)
        .map(|(o, j)| {
            let r = resolve(&[j.caps], None, None, s);
            OpVerdict {
                clause: o.clause.clone(),
                op: o.kind.code(),
                target: o.kind.target(),
                algorithm: r.algorithm,
                rebuild: r.rebuild,
                concurrent_dml: r.blocks_writes.map(|b| !b),
                note: j.note,
            }
        })
        .collect();
    let caps: Vec<Option<Caps>> = judged.iter().map(|j| j.caps).collect();
    let out = resolve(&caps, p.algorithm.as_deref(), p.lock.as_deref(), s);
    let (rows, bytes) = meta.map_or((None, None), |m| (m.rows, m.bytes));
    let big = rows.is_some_and(|r| r >= BIG_ROWS) || bytes.is_some_and(|b| b >= BIG_BYTES);
    let risk = if out.fails.is_some() {
        // 显式子句满足不了:服务器报错、什么也不改
        Risk::Low
    } else {
        match (out.algorithm, out.blocks_writes, out.rebuild, out.cost) {
            (None, ..) => Risk::Unknown,
            (Some(Algorithm::Copy), ..) | (_, Some(true), ..) => Risk::High,
            (_, _, Some(true), _) if big => Risk::High,
            (_, _, Some(true), _) => Risk::Medium,
            (_, _, _, Some(Cost::Scan)) if big => Risk::Medium,
            _ => Risk::Low,
        }
    };
    let requested = match (&p.algorithm, &p.lock) {
        (None, None) => None,
        (a, l) => Some(
            [a.as_ref().map(|a| format!("ALGORITHM={a}")), l.as_ref().map(|l| format!("LOCK={l}"))]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", "),
        ),
    };
    AlterAssessment {
        statement: p.kind.label(),
        table: p.table.clone(),
        server: s.label(),
        ops,
        algorithm: out.algorithm,
        rebuild: out.rebuild,
        blocks_writes: out.blocks_writes,
        requested,
        fails: out.fails,
        risk,
        rows,
        bytes,
        duration: if out.fails.is_some() { None } else { out.cost.and_then(|c| duration(c, rows, bytes)) },
        guarded: guarded(p, out.algorithm),
        suggest_osc: risk == Risk::High,
        notes,
    }
}

/// 加上保护子句的写法:预计 INSTANT 的写 `ALGORITHM=INSTANT`(MySQL 下写 INPLACE 反而会让加列重建整表),
/// 其余写 `ALGORITHM=INPLACE, LOCK=NONE`——做不到时服务器直接报错,而不是悄悄 COPY 或锁表。
/// 语句已经显式写了 ALGORITHM / LOCK,或带分区子句(语法位置不同)时不给。
fn guarded(p: &Parsed, alg: Option<Algorithm>) -> Option<String> {
    if p.algorithm.is_some() || p.lock.is_some() || p.ops.iter().any(|o| o.kind == OpKind::Partition) {
        return None;
    }
    let instant = alg == Some(Algorithm::Instant);
    // 原文末尾可能是行注释:另起一行再接子句
    let sep = if p.text.contains("--") || p.text.contains('#') { "\n" } else { "" };
    Some(match p.kind {
        StmtKind::AlterTable => {
            let clause = if instant { "ALGORITHM=INSTANT" } else { "ALGORITHM=INPLACE, LOCK=NONE" };
            format!("{}{sep}, {clause}", p.text)
        }
        StmtKind::CreateIndex | StmtKind::DropIndex => {
            let clause = if instant { "ALGORITHM=INSTANT" } else { "ALGORITHM=INPLACE LOCK=NONE" };
            format!("{}{sep} {clause}", p.text)
        }
        // OPTIMIZE TABLE 没有算法子句:InnoDB 上它就是 ALTER TABLE … FORCE
        StmtKind::Optimize => format!("ALTER TABLE {} FORCE, ALGORITHM=INPLACE, LOCK=NONE", p.table_raw),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, ty: &str) -> ColumnMeta {
        let lower = ty.to_ascii_lowercase();
        let string = lower.starts_with("varchar") || lower.starts_with("char") || lower.starts_with("text") || lower.starts_with("enum");
        let max = parse_column_type(&lower).and_then(|t| t.params).and_then(|p| p.parse::<u64>().ok());
        ColumnMeta {
            name: name.into(),
            column_type: lower,
            nullable: true,
            charset: string.then(|| "utf8mb4".into()),
            collation: string.then(|| "utf8mb4_0900_ai_ci".into()),
            max_chars: if string { max } else { None },
            octets: if string { max.map(|m| m * 4) } else { None },
            extra: String::new(),
            primary: false,
        }
    }

    /// 与 tests/alter.rs 实测用的表结构一致
    fn meta() -> TableMeta {
        let mut id = col("id", "int");
        id.nullable = false;
        id.primary = true;
        let mut n = col("n", "int");
        n.nullable = false;
        let mut l = col("l", "varchar(100)");
        l.charset = Some("latin1".into());
        l.collation = Some("latin1_swedish_ci".into());
        l.octets = Some(100);
        TableMeta {
            engine: Some("InnoDB".into()),
            row_format: Some("Dynamic".into()),
            rows: Some(1000),
            bytes: Some(1 << 20),
            collation: Some("utf8mb4_0900_ai_ci".into()),
            columns: vec![
                id,
                col("a", "int"),
                col("b", "varchar(20)"),
                col("v60", "varchar(60)"),
                l,
                n,
                col("e", "enum('x','y')"),
                col("txt", "text"),
            ],
            indexed_columns: vec!["id".into(), "a".into()],
            has_fulltext: false,
            foreign_key_checks: Some(true),
            row_versions: Some(0),
            default_collations: HashMap::from([
                ("utf8mb4".to_string(), "utf8mb4_0900_ai_ci".to_string()),
                ("latin1".to_string(), "latin1_swedish_ci".to_string()),
            ]),
            create_options: String::new(),
        }
    }

    fn run(sql: &str, s: Server, m: &TableMeta) -> AlterAssessment {
        assess(&parse(sql).unwrap_or_else(|| panic!("parse {sql}")), &s, Some(m))
    }

    /// (算法, 是否重建, 是否允许并发 DML)
    fn alg(sql: &str, s: Server) -> (Option<Algorithm>, Option<bool>, Option<bool>) {
        let a = run(sql, s, &meta());
        (a.algorithm, a.rebuild, a.blocks_writes.map(|b| !b))
    }

    use Algorithm::*;
    const M84: Server = Server { flavor: Flavor::Mysql, version: (8, 4, 9) };
    const M8029: Server = Server { flavor: Flavor::Mysql, version: (8, 0, 29) };
    const M8020: Server = Server { flavor: Flavor::Mysql, version: (8, 0, 20) };
    const M8011: Server = Server { flavor: Flavor::Mysql, version: (8, 0, 11) };
    const M57: Server = Server { flavor: Flavor::Mysql, version: (5, 7, 44) };
    const MA1011: Server = Server { flavor: Flavor::Mariadb, version: (10, 11, 6) };
    const MA103: Server = Server { flavor: Flavor::Mariadb, version: (10, 3, 30) };
    const MA102: Server = Server { flavor: Flavor::Mariadb, version: (10, 2, 44) };

    const I: (Option<Algorithm>, Option<bool>, Option<bool>) = (Some(Instant), Some(false), Some(true));
    const META_ONLY: (Option<Algorithm>, Option<bool>, Option<bool>) = (Some(Inplace), Some(false), Some(true));
    const REBUILD_ONLINE: (Option<Algorithm>, Option<bool>, Option<bool>) = (Some(Inplace), Some(true), Some(true));
    const C: (Option<Algorithm>, Option<bool>, Option<bool>) = (Some(Copy), Some(true), Some(false));
    const UNKNOWN: (Option<Algorithm>, Option<bool>, Option<bool>) = (None, None, None);

    #[test]
    fn server_versions() {
        assert_eq!(Server::parse("8.4.3"), Some(Server::mysql(8, 4, 3)));
        assert_eq!(Server::parse("8.0.35-0ubuntu0.22.04.1"), Some(Server::mysql(8, 0, 35)));
        assert_eq!(Server::parse("5.7.44-log"), Some(Server::mysql(5, 7, 44)));
        assert_eq!(Server::parse("10.11.6-MariaDB-0+deb12u1"), Some(Server::mariadb(10, 11, 6)));
        assert_eq!(Server::parse("5.5.5-10.6.12-MariaDB"), Some(Server::mariadb(10, 6, 12)));
        assert_eq!(Server::parse("9.1"), Some(Server::mysql(9, 1, 0)));
        assert_eq!(Server::parse("garbage"), None);
        assert_eq!(Server::mariadb(10, 6, 12).label(), "MariaDB 10.6.12");
    }

    #[test]
    fn parses_clauses() {
        let p = parse("ALTER TABLE `shop`.`orders` ADD COLUMN c INT NOT NULL DEFAULT 0 AFTER a, DROP INDEX idx_x, ALGORITHM = INPLACE, LOCK=NONE;").unwrap();
        assert_eq!((p.schema.as_deref(), p.table.as_str(), p.table_raw.as_str()), (Some("shop"), "orders", "`shop`.`orders`"));
        assert_eq!(p.algorithm.as_deref(), Some("INPLACE"));
        assert_eq!(p.lock.as_deref(), Some("NONE"));
        assert_eq!(p.ops.len(), 2);
        assert_eq!(p.ops[0].clause, "ADD COLUMN c INT NOT NULL DEFAULT 0 AFTER a");
        let OpKind::AddColumn { name, def } = &p.ops[0].kind else { panic!() };
        assert_eq!(name, "c");
        assert!(def.position && def.not_null == Some(true) && !def.other);
        assert_eq!(p.ops[1].kind, OpKind::DropIndex { name: "idx_x".into() });

        let kinds = |sql: &str| parse(sql).unwrap().ops.into_iter().map(|o| o.kind.code()).collect::<Vec<_>>();
        assert_eq!(kinds("alter table t add (x int, y varchar(3))"), ["add_column", "add_column"]);
        assert_eq!(kinds("ALTER TABLE t ADD UNIQUE KEY uk (a), ADD FULLTEXT ft (b), ADD SPATIAL INDEX (g)"), ["add_index", "add_fulltext", "add_spatial"]);
        assert_eq!(kinds("ALTER TABLE t ADD CONSTRAINT pk PRIMARY KEY (id), DROP PRIMARY KEY"), ["add_primary_key", "drop_primary_key"]);
        assert_eq!(kinds("ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY (a) REFERENCES p(id), DROP FOREIGN KEY fk2"), ["add_foreign_key", "drop_foreign_key"]);
        assert_eq!(kinds("ALTER TABLE t CHANGE a a INT, CHANGE COLUMN b c VARCHAR(5), RENAME COLUMN x TO y"), ["modify_column", "change_column", "rename_column"]);
        assert_eq!(kinds("ALTER TABLE t ALTER COLUMN a SET DEFAULT 1, ALTER b DROP DEFAULT, ALTER COLUMN c SET INVISIBLE"), ["set_default", "set_default", "column_visibility"]);
        assert_eq!(kinds("ALTER TABLE t RENAME INDEX a TO b, ALTER INDEX c INVISIBLE, RENAME TO t2"), ["rename_index", "index_visibility", "rename_table"]);
        assert_eq!(kinds("ALTER TABLE t ENGINE=InnoDB ROW_FORMAT=DYNAMIC, DEFAULT CHARSET=utf8mb4 COLLATE utf8mb4_bin"), ["engine", "row_format", "table_charset"]);
        assert_eq!(kinds("ALTER TABLE t CONVERT TO CHARACTER SET utf8mb4, FORCE, AUTO_INCREMENT=5, STATS_PERSISTENT=0, COMMENT='x'"), ["convert_charset", "rebuild", "auto_increment", "stats", "table_comment"]);
        assert_eq!(kinds("ALTER TABLE t ADD PARTITION (PARTITION p3 VALUES LESS THAN (2030))"), ["partition"]);
        assert_eq!(kinds("ALTER TABLE t ORDER BY a"), ["unknown"]);
        assert_eq!(kinds("ALTER TABLE t ADD CHECK (a > 0), DROP CHECK c1"), ["add_check", "unknown"]);
        assert_eq!(kinds("ALTER TABLE t DROP a, DROP COLUMN `b`"), ["drop_column", "drop_column"]);
        // 字符串、注释里的逗号与关键字不切分
        let p = parse("ALTER TABLE t -- 加一列, DROP x\n ADD COLUMN s VARCHAR(10) DEFAULT 'a,b' COMMENT 'DROP COLUMN y'").unwrap();
        assert_eq!(p.ops.len(), 1);
        // MariaDB:ALTER ONLINE TABLE = LOCK=NONE;WAIT n
        let p = parse("ALTER ONLINE TABLE t WAIT 5 ADD INDEX (a)").unwrap();
        assert_eq!(p.lock.as_deref(), Some("NONE"));
        assert_eq!(p.ops[0].kind, OpKind::AddIndex { name: None });

        let p = parse("CREATE UNIQUE INDEX uk ON shop.t (a, b) USING BTREE ALGORITHM=INPLACE LOCK=NONE").unwrap();
        assert_eq!((p.kind, p.table.as_str(), p.algorithm.as_deref()), (StmtKind::CreateIndex, "t", Some("INPLACE")));
        assert_eq!(p.ops[0].kind, OpKind::AddIndex { name: Some("uk".into()) });
        assert_eq!(parse("CREATE FULLTEXT INDEX ft ON t (b)").unwrap().ops[0].kind.code(), "add_fulltext");
        assert_eq!(parse("DROP INDEX `PRIMARY` ON t").unwrap().ops[0].kind, OpKind::DropPrimaryKey);
        assert_eq!(parse("OPTIMIZE NO_WRITE_TO_BINLOG TABLE t").unwrap().ops[0].kind, OpKind::Rebuild);

        // 不评估的
        assert!(parse("ALTER TABLE t").is_none());
        assert!(parse("DROP INDEX idx").is_none(), "PG 写法");
        assert!(parse("OPTIMIZE TABLE a, b").is_none());
        assert!(parse("ALTER TABLE t /*! ADD c INT */").is_none());
        assert!(parse("ALTER VIEW v AS SELECT 1").is_none());
        assert!(parse("CREATE TABLE t (a INT)").is_none());
        assert!(parse("SELECT 1").is_none());
        assert_eq!(schema_change("alter table t add c int"), Some(("ALTER TABLE", "t".into())));
    }

    #[test]
    fn column_types_normalize() {
        let t = |s: &str| parse_column_type(s).unwrap();
        assert_eq!(t("int(11)"), t("int"));
        assert_eq!(t("INTEGER"), t("int"));
        assert_eq!(t("bigint(20) unsigned"), t("BIGINT UNSIGNED"));
        assert_ne!(t("bigint unsigned"), t("bigint"));
        assert_eq!(t("decimal"), t("decimal(10,0)"));
        assert_eq!(t("numeric(8)"), t("decimal(8, 0)"));
        assert_eq!(t("char"), t("char(1)"));
        assert_eq!(t("datetime(0)"), t("datetime"));
        assert_ne!(t("datetime(3)"), t("datetime"));
        assert_eq!(t("enum('a','b')").values, Some(vec!["a".into(), "b".into()]));
        assert_eq!(t("bool"), t("tinyint(1)"));
    }

    #[test]
    fn add_and_drop_column_by_version() {
        // 加列到末尾:8.0.12+ INSTANT;更早 INPLACE 重建
        for s in [M84, M8029, M8020] {
            assert_eq!(alg("ALTER TABLE t ADD COLUMN c INT", s), I, "{s:?}");
        }
        assert_eq!(alg("ALTER TABLE t ADD COLUMN c INT", M8011), REBUILD_ONLINE);
        assert_eq!(alg("ALTER TABLE t ADD COLUMN c INT", M57), REBUILD_ONLINE);
        // 加到中间:8.0.29+ 才 INSTANT
        assert_eq!(alg("ALTER TABLE t ADD COLUMN c INT AFTER a", M84), I);
        assert_eq!(alg("ALTER TABLE t ADD COLUMN c INT FIRST", M8029), I);
        assert_eq!(alg("ALTER TABLE t ADD COLUMN c INT AFTER a", M8020), REBUILD_ONLINE);
        assert_eq!(run("ALTER TABLE t ADD COLUMN c INT AFTER a", M8020, &meta()).ops[0].note, Some("instant_position_8029"));
        // 删列:8.0.29+ INSTANT;列在索引里时不能 INSTANT
        assert_eq!(alg("ALTER TABLE t DROP COLUMN txt", M84), I);
        assert_eq!(alg("ALTER TABLE t DROP COLUMN txt", M8020), REBUILD_ONLINE);
        assert_eq!(alg("ALTER TABLE t DROP COLUMN txt", M57), REBUILD_ONLINE);
        assert_eq!(alg("ALTER TABLE t DROP COLUMN a", M84), REBUILD_ONLINE);
        assert_eq!(alg("ALTER TABLE t DROP COLUMN nope", M84), UNKNOWN);
        // MariaDB:10.3.2+ 末尾 INSTANT,10.4+ 任意位置与删列
        assert_eq!(alg("ALTER TABLE t ADD COLUMN c INT", MA103), I);
        assert_eq!(alg("ALTER TABLE t ADD COLUMN c INT FIRST", MA103), REBUILD_ONLINE);
        assert_eq!(alg("ALTER TABLE t ADD COLUMN c INT FIRST", MA1011), I);
        assert_eq!(alg("ALTER TABLE t DROP COLUMN txt", MA1011), I);
        assert_eq!(alg("ALTER TABLE t ADD COLUMN c INT", MA102), REBUILD_ONLINE);
        // 自增列:INPLACE 重建且阻塞写入;STORED 生成列 COPY;VIRTUAL 生成列 8.0 INSTANT、5.7 只改元数据
        assert_eq!(alg("ALTER TABLE t ADD COLUMN c INT NOT NULL AUTO_INCREMENT", M84), (Some(Inplace), Some(true), Some(false)));
        assert_eq!(alg("ALTER TABLE t ADD COLUMN c INT AS (a + 1) STORED", M84), C);
        assert_eq!(alg("ALTER TABLE t ADD COLUMN c INT GENERATED ALWAYS AS (a + 1) VIRTUAL", M84), I);
        assert_eq!(alg("ALTER TABLE t ADD COLUMN c INT AS (a + 1)", M57), META_ONLY);
        assert_eq!(alg("ALTER TABLE t ADD COLUMN c INT AS (a + 1)", MA1011), UNKNOWN);
        // 表达式默认值、带主键 / 外键之类的列属性:不猜
        assert_eq!(alg("ALTER TABLE t ADD COLUMN c INT DEFAULT (rand())", M84), UNKNOWN);
        assert_eq!(alg("ALTER TABLE t ADD COLUMN c INT PRIMARY KEY", M84), UNKNOWN);
    }

    #[test]
    fn instant_blockers() {
        // 压缩行格式、已有全文索引、行版本到上限:不能 INSTANT
        let mut m = meta();
        m.row_format = Some("Compressed".into());
        assert_eq!(run("ALTER TABLE t ADD COLUMN c INT", M84, &m).algorithm, Some(Inplace));
        assert_eq!(run("ALTER TABLE t ADD COLUMN c INT", M84, &m).ops[0].note, Some("instant_compressed"));
        let mut m = meta();
        m.row_versions = Some(64);
        assert_eq!(run("ALTER TABLE t DROP COLUMN txt", M84, &m).ops[0].note, Some("instant_row_versions"));
        // 全文索引:需要重建的操作只能 COPY(8.4 实测)
        let mut m = meta();
        m.has_fulltext = true;
        let a = run("ALTER TABLE t ADD COLUMN c INT", M84, &m);
        assert_eq!((a.algorithm, a.ops[0].note), (Some(Copy), Some("fulltext_rebuild_copy")));
        assert_eq!(run("ALTER TABLE t FORCE", M84, &m).algorithm, Some(Copy));
        assert_eq!(run("OPTIMIZE TABLE t", M84, &m).algorithm, Some(Copy));
        assert_eq!(run("ALTER TABLE t ADD INDEX (b)", M84, &m).algorithm, Some(Inplace));
        assert_eq!(run("ALTER TABLE t FORCE", MA1011, &m).algorithm, None);
    }

    #[test]
    fn modify_column() {
        for s in [M84, M8029] {
            // 同类型:只改默认值 / 注释 / 显示宽度 → INSTANT
            assert_eq!(alg("ALTER TABLE t MODIFY a INT DEFAULT 5", s), I);
            assert_eq!(alg("ALTER TABLE t MODIFY a INT(11) COMMENT 'x'", s), I);
            assert_eq!(alg("ALTER TABLE t MODIFY b VARCHAR(20)", s), I);
            // 改类型:COPY
            assert_eq!(alg("ALTER TABLE t MODIFY a BIGINT", s), C);
            assert_eq!(alg("ALTER TABLE t CHANGE a a2 BIGINT", s), C);
            // NULL ↔ NOT NULL:INPLACE 重建
            assert_eq!(alg("ALTER TABLE t MODIFY a INT NOT NULL", s), REBUILD_ONLINE);
            assert_eq!(alg("ALTER TABLE t MODIFY n INT NULL DEFAULT 0", s), REBUILD_ONLINE);
            // 调位置:INPLACE 重建
            assert_eq!(alg("ALTER TABLE t MODIFY a INT AFTER txt", s), REBUILD_ONLINE);
            // ENUM 末尾追加 INSTANT,改顺序 COPY
            assert_eq!(alg("ALTER TABLE t MODIFY e ENUM('x','y','z')", s), I);
            assert_eq!(alg("ALTER TABLE t MODIFY e ENUM('z','x','y')", s), C);
        }
        assert_eq!(alg("ALTER TABLE t MODIFY a INT DEFAULT 5", M57), META_ONLY);
        assert_eq!(alg("ALTER TABLE t MODIFY e ENUM('x','y','z')", M57), META_ONLY);
        assert_eq!(alg("ALTER TABLE t MODIFY a BIGINT", M57), C);
        assert_eq!(alg("ALTER TABLE t MODIFY a INT NOT NULL", M57), REBUILD_ONLINE);
        assert_eq!(alg("ALTER TABLE t MODIFY a INT NOT NULL", MA1011), UNKNOWN);
        assert_eq!(alg("ALTER TABLE t MODIFY nope INT", M84), UNKNOWN);
        // 改字符集 COPY;只换排序规则说不准
        assert_eq!(alg("ALTER TABLE t MODIFY b VARCHAR(20) CHARACTER SET latin1", M84), C);
        assert_eq!(alg("ALTER TABLE t MODIFY b VARCHAR(20) COLLATE utf8mb4_bin", M84), UNKNOWN);
        // latin1 列不写字符集:会被改成表默认的 utf8mb4 → COPY
        assert_eq!(alg("ALTER TABLE t MODIFY l VARCHAR(100)", M84), C);
        assert_eq!(run("ALTER TABLE t MODIFY l VARCHAR(100)", M84, &meta()).ops[0].note, Some("charset_change"));
    }

    #[test]
    fn varchar_widening_byte_tiers() {
        // utf8mb4:20 → 63 字符(252 字节)同在 1 字节长度档,INPLACE 不重建;→ 64(256 字节)跨档 COPY
        for s in [M84, M57, MA103] {
            assert_eq!(alg("ALTER TABLE t MODIFY b VARCHAR(63) CHARACTER SET utf8mb4", s), META_ONLY, "{s:?}");
            assert_eq!(alg("ALTER TABLE t MODIFY b VARCHAR(64) CHARACTER SET utf8mb4", s), C, "{s:?}");
        }
        assert_eq!(alg("ALTER TABLE t MODIFY v60 VARCHAR(100)", M84), C);
        // latin1:100 → 255 同档,→ 256 跨档
        assert_eq!(alg("ALTER TABLE t MODIFY l VARCHAR(255) CHARACTER SET latin1", M84), META_ONLY);
        assert_eq!(alg("ALTER TABLE t MODIFY l VARCHAR(256) CHARACTER SET latin1", M84), C);
        assert_eq!(run("ALTER TABLE t MODIFY l VARCHAR(256) CHARACTER SET latin1", M84, &meta()).ops[0].note, Some("varchar_cross_255"));
        // 缩短 COPY
        assert_eq!(alg("ALTER TABLE t MODIFY b VARCHAR(10)", M84), C);
        // MariaDB 10.4+ 跨档:不猜
        assert_eq!(alg("ALTER TABLE t MODIFY b VARCHAR(64)", MA1011), UNKNOWN);
        // 同档扩展 + 改可空:取最差,INPLACE 重建
        assert_eq!(alg("ALTER TABLE t MODIFY b VARCHAR(30) NOT NULL", M84), REBUILD_ONLINE);
    }

    #[test]
    fn rename_and_defaults() {
        assert_eq!(alg("ALTER TABLE t RENAME COLUMN a TO a2", M84), I);
        assert_eq!(alg("ALTER TABLE t CHANGE a a2 INT", M84), I);
        assert_eq!(alg("ALTER TABLE t RENAME COLUMN a TO a2", M8020), META_ONLY);
        assert_eq!(alg("ALTER TABLE t RENAME COLUMN a TO a2", M57), META_ONLY);
        assert_eq!(alg("ALTER TABLE t ALTER COLUMN a SET DEFAULT 3", M84), I);
        assert_eq!(alg("ALTER TABLE t ALTER COLUMN n DROP DEFAULT", M8020), I);
        assert_eq!(alg("ALTER TABLE t ALTER COLUMN n DROP DEFAULT", M8011), META_ONLY);
        assert_eq!(alg("ALTER TABLE t ALTER COLUMN a SET DEFAULT (rand())", M84), UNKNOWN);
        assert_eq!(alg("ALTER TABLE t ALTER COLUMN a SET INVISIBLE", M84), I);
        assert_eq!(alg("ALTER TABLE t ALTER COLUMN a SET INVISIBLE", M57), UNKNOWN);
        assert_eq!(alg("ALTER TABLE t RENAME TO t2", M84), I);
        assert_eq!(alg("ALTER TABLE t RENAME TO t2", M57), META_ONLY);
    }

    #[test]
    fn indexes_and_keys() {
        for s in [M84, M57, MA1011] {
            assert_eq!(alg("ALTER TABLE t ADD INDEX kb (b)", s), META_ONLY, "{s:?}");
            assert_eq!(alg("CREATE UNIQUE INDEX kb ON t (b)", s), META_ONLY);
            assert_eq!(alg("ALTER TABLE t DROP INDEX ka", s), META_ONLY);
            assert_eq!(alg("DROP INDEX ka ON t", s), META_ONLY);
            // 首个全文索引:INPLACE 重建且阻塞写入;空间索引:不重建但阻塞写入
            assert_eq!(alg("ALTER TABLE t ADD FULLTEXT INDEX ft (txt)", s), (Some(Inplace), Some(true), Some(false)));
            assert_eq!(alg("ALTER TABLE t ADD SPATIAL INDEX sp (g)", s), (Some(Inplace), Some(false), Some(false)));
            // 删主键 COPY;删了再加 INPLACE 重建;加主键 INPLACE 重建
            assert_eq!(alg("ALTER TABLE t DROP PRIMARY KEY", s), C);
            assert_eq!(alg("ALTER TABLE t DROP PRIMARY KEY, ADD PRIMARY KEY (id, n)", s), REBUILD_ONLINE);
            assert_eq!(alg("ALTER TABLE t ADD PRIMARY KEY (n)", s), REBUILD_ONLINE);
            // 外键:foreign_key_checks 开着 COPY;删外键只改元数据
            assert_eq!(alg("ALTER TABLE t ADD CONSTRAINT fk2 FOREIGN KEY (a) REFERENCES p(id)", s), C);
            assert_eq!(alg("ALTER TABLE t DROP FOREIGN KEY fk_p", s), META_ONLY);
        }
        // 已有全文索引时再加一个:不重建
        let mut m = meta();
        m.has_fulltext = true;
        let a = run("ALTER TABLE t ADD FULLTEXT INDEX ft2 (b)", M84, &m);
        assert_eq!((a.algorithm, a.rebuild, a.blocks_writes), (Some(Inplace), Some(false), Some(true)));
        let mut m = meta();
        m.foreign_key_checks = Some(false);
        assert_eq!(run("ALTER TABLE t ADD FOREIGN KEY (a) REFERENCES p(id)", M84, &m).algorithm, Some(Inplace));
        m.foreign_key_checks = None;
        assert_eq!(run("ALTER TABLE t ADD FOREIGN KEY (a) REFERENCES p(id)", M84, &m).algorithm, None);
        assert_eq!(alg("ALTER TABLE t RENAME INDEX ka TO kz", M84), META_ONLY);
        assert_eq!(alg("ALTER TABLE t RENAME INDEX ka TO kz", MA103), UNKNOWN);
        assert_eq!(alg("ALTER TABLE t ALTER INDEX ka INVISIBLE", M84), META_ONLY);
        assert_eq!(alg("ALTER TABLE t ALTER INDEX ka INVISIBLE", M57), UNKNOWN);
    }

    #[test]
    fn table_level_operations() {
        for s in [M84, M57] {
            assert_eq!(alg("ALTER TABLE t CONVERT TO CHARACTER SET utf8mb4", s), C);
            assert_eq!(alg("ALTER TABLE t ENGINE=InnoDB", s), REBUILD_ONLINE);
            assert_eq!(alg("ALTER TABLE t FORCE", s), REBUILD_ONLINE);
            assert_eq!(alg("OPTIMIZE TABLE t", s), REBUILD_ONLINE);
            assert_eq!(alg("ALTER TABLE t ENGINE=MyISAM", s), C);
            assert_eq!(alg("ALTER TABLE t ROW_FORMAT=COMPACT", s), REBUILD_ONLINE);
            assert_eq!(alg("ALTER TABLE t KEY_BLOCK_SIZE=8", s), REBUILD_ONLINE);
            assert_eq!(alg("ALTER TABLE t AUTO_INCREMENT=100", s), META_ONLY);
            assert_eq!(alg("ALTER TABLE t STATS_PERSISTENT=0", s), META_ONLY);
        }
        assert_eq!(alg("ALTER TABLE t DEFAULT CHARACTER SET latin1", M84), META_ONLY);
        assert_eq!(alg("ALTER TABLE t DEFAULT CHARACTER SET latin1", M57), REBUILD_ONLINE);
        assert_eq!(alg("ALTER TABLE t DEFAULT CHARACTER SET latin1", MA1011), UNKNOWN);
        assert_eq!(alg("ALTER TABLE t COMMENT='hello'", M84), META_ONLY);
        assert_eq!(alg("ALTER TABLE t COMMENT='hello'", M57), UNKNOWN);
        assert_eq!(alg("ALTER TABLE t ADD CONSTRAINT ck CHECK (a > 0)", M84), C);
        assert_eq!(alg("ALTER TABLE t ADD PARTITION (PARTITION p1 VALUES LESS THAN (10))", M84), UNKNOWN);
        assert_eq!(alg("ALTER TABLE t ORDER BY a", M84), UNKNOWN);
    }

    #[test]
    fn statement_combines_ops() {
        // 能 INSTANT 的加列 + 要 INPLACE 的加索引:整条 INPLACE,且加列因此重建
        assert_eq!(alg("ALTER TABLE t ADD COLUMN c INT, ADD INDEX kb (b)", M84), REBUILD_ONLINE);
        assert_eq!(alg("ALTER TABLE t ADD COLUMN c INT, RENAME COLUMN b TO b2", M84), I);
        // 只要一个 COPY,整条 COPY(即使其他操作未知)
        assert_eq!(alg("ALTER TABLE t ADD COLUMN c INT, MODIFY a BIGINT", M84), C);
        assert_eq!(alg("ALTER TABLE t ORDER BY a, MODIFY a BIGINT", M84), C);
        assert_eq!(alg("ALTER TABLE t ORDER BY a, ADD COLUMN c INT", M84), UNKNOWN);
        let a = run("ALTER TABLE t ADD COLUMN c INT, MODIFY a BIGINT", M84, &meta());
        assert_eq!(a.ops.iter().map(|o| o.algorithm).collect::<Vec<_>>(), [Some(Instant), Some(Copy)]);
    }

    #[test]
    fn explicit_algorithm_and_lock() {
        let m = meta();
        // 满足不了:服务器报错不执行,风险低
        let a = run("ALTER TABLE t MODIFY a BIGINT, ALGORITHM=INPLACE, LOCK=NONE", M84, &m);
        assert_eq!((a.fails, a.risk, a.requested.as_deref()), (Some("inplace_unsupported"), Risk::Low, Some("ALGORITHM=INPLACE, LOCK=NONE")));
        assert!(a.guarded.is_none() && !a.suggest_osc);
        assert_eq!(run("ALTER TABLE t ADD FULLTEXT ft (txt), LOCK=NONE", M84, &m).fails, Some("lock_none_unsupported"));
        assert_eq!(run("ALTER TABLE t DROP COLUMN a, ALGORITHM=INSTANT", M84, &m).fails, Some("instant_unsupported"));
        assert_eq!(run("ALTER TABLE t ADD COLUMN c INT, ALGORITHM=INSTANT, LOCK=NONE", M84, &m).fails, Some("instant_with_lock"));
        assert_eq!(run("ALTER TABLE t ADD COLUMN c INT, ALGORITHM=COPY, LOCK=NONE", M84, &m).fails, Some("lock_none_unsupported"));
        // MySQL 显式 INPLACE:能 INSTANT 的加列也会重建;MariaDB 是「至少」语义,仍 INSTANT
        let a = run("ALTER TABLE t ADD COLUMN c INT, ALGORITHM=INPLACE", M84, &m);
        assert_eq!((a.algorithm, a.rebuild), (Some(Inplace), Some(true)));
        assert_eq!(run("ALTER TABLE t ADD COLUMN c INT, ALGORITHM=INPLACE", MA1011, &m).algorithm, Some(Instant));
        // 默认算法 + LOCK=NONE:能 INSTANT 就 INSTANT(8.4 实测)
        assert_eq!(run("ALTER TABLE t ADD COLUMN c INT, LOCK=NONE", M84, &m).algorithm, Some(Instant));
        // 显式 COPY:锁写
        let a = run("ALTER TABLE t ADD COLUMN c INT, ALGORITHM=COPY", M84, &m);
        assert_eq!((a.algorithm, a.blocks_writes, a.risk), (Some(Copy), Some(true), Risk::High));
        // 显式 LOCK=SHARED:INPLACE 期间也阻塞写入
        let a = run("ALTER TABLE t ADD INDEX (b), LOCK=SHARED", M84, &m);
        assert_eq!((a.algorithm, a.blocks_writes, a.risk), (Some(Inplace), Some(true), Risk::High));
        // MariaDB NOCOPY:要重建的不行
        assert_eq!(run("ALTER TABLE t FORCE, ALGORITHM=NOCOPY", MA1011, &m).fails, Some("nocopy_unsupported"));
        assert_eq!(run("ALTER TABLE t ADD INDEX (b), ALGORITHM=NOCOPY", MA1011, &m).algorithm, Some(Inplace));
    }

    #[test]
    fn risk_size_and_duration() {
        let small = meta();
        let mut big = meta();
        big.rows = Some(3_000_000);
        big.bytes = Some(2 << 30);
        let r = |sql: &str, m: &TableMeta| {
            let a = run(sql, M84, m);
            (a.risk, a.duration)
        };
        assert_eq!(r("ALTER TABLE t ADD COLUMN c INT", &big), (Risk::Low, Some("metadata")));
        assert_eq!(r("ALTER TABLE t ADD INDEX (b)", &small), (Risk::Low, Some("seconds")));
        assert_eq!(r("ALTER TABLE t ADD INDEX (b)", &big), (Risk::Medium, Some("seconds")));
        assert_eq!(r("ALTER TABLE t MODIFY a INT NOT NULL", &small), (Risk::Medium, Some("seconds")));
        assert_eq!(r("ALTER TABLE t MODIFY a INT NOT NULL", &big), (Risk::High, Some("minutes")));
        assert_eq!(r("ALTER TABLE t MODIFY a BIGINT", &small), (Risk::High, Some("seconds")));
        assert_eq!(r("ALTER TABLE t MODIFY a BIGINT", &big), (Risk::High, Some("minutes")));
        let mut huge = meta();
        huge.rows = Some(2_000_000_000);
        huge.bytes = Some(2000 << 30);
        assert_eq!(r("ALTER TABLE t MODIFY a BIGINT", &huge), (Risk::High, Some("days")));
        assert_eq!(r("ALTER TABLE t FORCE", &huge), (Risk::High, Some("hours")));
        assert_eq!(r("ALTER TABLE t ORDER BY a", &big), (Risk::Unknown, None));
        // 大小未知:没有耗时量级
        let mut m = meta();
        m.rows = None;
        m.bytes = None;
        assert_eq!(r("ALTER TABLE t FORCE", &m), (Risk::Medium, None));
        assert!(run("ALTER TABLE t MODIFY a BIGINT", M84, &big).suggest_osc);
        assert!(!run("ALTER TABLE t ADD COLUMN c INT", M84, &big).suggest_osc);
    }

    #[test]
    fn guarded_statement() {
        let m = meta();
        let g = |sql: &str| run(sql, M84, &m).guarded;
        assert_eq!(g("ALTER TABLE t MODIFY a BIGINT;").as_deref(), Some("ALTER TABLE t MODIFY a BIGINT, ALGORITHM=INPLACE, LOCK=NONE"));
        // 预计 INSTANT:写 ALGORITHM=INSTANT(MySQL 下写 INPLACE 会让加列重建)
        assert_eq!(g("ALTER TABLE t ADD COLUMN c INT").as_deref(), Some("ALTER TABLE t ADD COLUMN c INT, ALGORITHM=INSTANT"));
        assert_eq!(g("CREATE INDEX kb ON t (b)").as_deref(), Some("CREATE INDEX kb ON t (b) ALGORITHM=INPLACE LOCK=NONE"));
        assert_eq!(g("OPTIMIZE TABLE `t`").as_deref(), Some("ALTER TABLE `t` FORCE, ALGORITHM=INPLACE, LOCK=NONE"));
        assert_eq!(g("ALTER TABLE t MODIFY a BIGINT -- 扩容").as_deref(), Some("ALTER TABLE t MODIFY a BIGINT -- 扩容\n, ALGORITHM=INPLACE, LOCK=NONE"));
        assert_eq!(g("ALTER TABLE t MODIFY a BIGINT, LOCK=SHARED"), None);
        assert_eq!(g("ALTER TABLE t ADD PARTITION (PARTITION p1 VALUES LESS THAN (10))"), None);
    }

    #[test]
    fn table_state_blocks_judgement() {
        let p = parse("ALTER TABLE t ADD COLUMN c INT").unwrap();
        let a = assess(&p, &M84, None);
        assert_eq!((a.algorithm, a.risk, a.notes.as_slice()), (None, Risk::Unknown, ["table_not_found"].as_slice()));
        let mut m = meta();
        m.engine = Some("MyISAM".into());
        let a = assess(&p, &M84, Some(&m));
        assert_eq!((a.algorithm, a.notes.as_slice()), (None, ["non_innodb"].as_slice()));
        let a = assess(&p, &Server::mysql(5, 6, 51), Some(&meta()));
        assert_eq!((a.algorithm, a.notes.as_slice()), (None, ["unsupported_version"].as_slice()));
        assert_eq!(a.ops[0].note, Some("unsupported_version"));
    }
}
