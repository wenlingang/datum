//! 拼装 AI 对话的 system prompt:方言 + 库名 + 表清单 + 已加载的表结构 + 字段速览 + 知识库 + 输出规则。
//! schema 采用多级截断,控制 prompt 体积。
//!
//! 提示词随界面语言切换(en / zh-CN / zh-TW),三个版本语义保持一致。
//! 大段文字直接按语言分支写在这里,不进 locales。对外的 `build_system_prompt` 等读全局
//! `rust_i18n::locale()`;`*_in` 变体显式接收 locale,便于测试且不受全局状态干扰。

use crate::pg::browse::TableDetail;
use crate::schema::ColumnsByTable;
use crate::sql::Dialect;
use std::collections::BTreeMap;

/// 表名列表最多注入的数量;超出的以"共 N 张"折叠。
const MAX_TABLES: usize = 80;
/// 单表结构最多注入的列数。
const MAX_COLUMNS: usize = 100;
/// 字段速览:最多覆盖的表数与单表列数(一行一表)。
const MAX_BRIEF_TABLES: usize = 40;
const MAX_BRIEF_COLUMNS: usize = 24;
/// 知识库文档的字符上限;超出截断并提示。
const MAX_KNOWLEDGE_CHARS: usize = 40_000;

/// 提示词语言。未知 locale 一律按英文。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lang {
    En,
    ZhCn,
    ZhTw,
}

impl Lang {
    fn from_locale(locale: &str) -> Self {
        let l = locale.to_ascii_lowercase().replace('_', "-");
        if !l.starts_with("zh") {
            Self::En
        } else if l.contains("tw") || l.contains("hant") || l.contains("hk") || l.contains("mo") {
            Self::ZhTw
        } else {
            Self::ZhCn
        }
    }

    fn knowledge_intro(self, name: &str) -> String {
        match self {
            Self::En => format!(
                "Below is the team's \"{name}\". You must follow its guidelines when generating SQL and designing table schemas:"
            ),
            Self::ZhCn => {
                format!("以下是团队的《{name}》,生成 SQL 与设计表结构时必须遵循其中的规范:")
            }
            Self::ZhTw => {
                format!("以下是團隊的《{name}》,產生 SQL 與設計資料表結構時必須遵循其中的規範:")
            }
        }
    }

    fn knowledge_truncated(self, total: usize) -> String {
        match self {
            Self::En => format!(
                "(The document is too long and has been truncated; the full text is {total} characters)"
            ),
            Self::ZhCn => format!("(文档过长,已截断,完整内容共 {total} 字)"),
            Self::ZhTw => format!("(文件過長,已截斷,完整內容共 {total} 字)"),
        }
    }

    fn more_columns(self, n: usize) -> String {
        match self {
            Self::En => format!("…{n} columns in total"),
            Self::ZhCn => format!("…共 {n} 列"),
            Self::ZhTw => format!("…共 {n} 欄"),
        }
    }

    fn empty_database(self) -> &'static str {
        match self {
            Self::En => "(empty database, no tables yet)",
            Self::ZhCn => "(空库,尚无表)",
            Self::ZhTw => "(空資料庫,尚無資料表)",
        }
    }

    fn more_tables(self, shown: &str, n: usize) -> String {
        match self {
            Self::En => format!("{shown} …{n} tables in total (ask about specific tables)"),
            Self::ZhCn => format!("{shown} …共 {n} 张(可询问具体表)"),
            Self::ZhTw => format!("{shown} …共 {n} 張(可詢問特定資料表)"),
        }
    }

    fn default_database(self) -> &'static str {
        match self {
            Self::En => "(default)",
            Self::ZhCn => "(默认)",
            Self::ZhTw => "(預設)",
        }
    }

    fn role(self) -> &'static str {
        match self {
            Self::En => "You are the SQL assistant embedded in Datum, a database client.",
            Self::ZhCn => "你是嵌入在数据库客户端 Datum 中的 SQL 助手。",
            Self::ZhTw => "你是內嵌在資料庫用戶端 Datum 中的 SQL 助理。",
        }
    }

    fn connection(self, dialect: &str, db: &str) -> String {
        match self {
            Self::En => format!("Current connection: {dialect}, database {db}."),
            Self::ZhCn => format!("当前连接:{dialect},数据库 {db}。"),
            Self::ZhTw => format!("目前連線:{dialect},資料庫 {db}。"),
        }
    }

    fn tables(self, table_line: &str) -> String {
        match self {
            Self::En => format!("Tables in the database: {table_line}"),
            Self::ZhCn => format!("数据库中的表:{table_line}"),
            Self::ZhTw => format!("資料庫中的資料表:{table_line}"),
        }
    }

    fn known_schemas(self, blocks: &str) -> String {
        match self {
            Self::En => format!("Known table schemas:\n{blocks}"),
            Self::ZhCn => format!("已知表结构:\n{blocks}"),
            Self::ZhTw => format!("已知資料表結構:\n{blocks}"),
        }
    }

    fn brief(self, b: &str) -> String {
        match self {
            Self::En => format!(
                "Column overview of the remaining tables (names only; ask if you need types/constraints before writing SQL):\n{b}"
            ),
            Self::ZhCn => format!(
                "其余表的字段速览(仅列名,写 SQL 前如需类型/约束可询问):\n{b}"
            ),
            Self::ZhTw => format!(
                "其餘資料表的欄位速覽(僅欄位名稱,撰寫 SQL 前如需型別/限制條件可詢問):\n{b}"
            ),
        }
    }

    /// 输出规则 1–5;第 1 条同时约定回答语言(与界面语言一致)。
    fn rules(self, dialect: &str) -> Vec<String> {
        match self {
            Self::En => vec![
                "Rules:".into(),
                "1. When generating SQL, put each executable statement in its own ```sql code block; outside the blocks, briefly explain the intent in English.".into(),
                format!("2. Strictly use the {dialect} dialect."),
                "3. Destructive operations (DROP / TRUNCATE / DELETE or UPDATE without WHERE) must be explicitly warned about in the explanation.".into(),
                "4. When information is insufficient, ask first; do not invent table or column names.".into(),
                "5. Do not add unnecessary pleasantries.".into(),
            ],
            Self::ZhCn => vec![
                "规则:".into(),
                "1. 需要生成 SQL 时,把每条可执行语句放进独立的 ```sql 代码块;块外用简短中文说明意图。".into(),
                format!("2. 严格使用 {dialect} 方言。"),
                "3. 破坏性操作(DROP / TRUNCATE / 无 WHERE 的 DELETE 或 UPDATE)必须在说明中显式警告。".into(),
                "4. 信息不足时先提问;不要臆造表名或列名。".into(),
                "5. 不要输出多余客套。".into(),
            ],
            Self::ZhTw => vec![
                "規則:".into(),
                "1. 需要產生 SQL 時,把每條可執行的陳述式放進獨立的 ```sql 程式碼區塊;區塊外以簡短的繁體中文說明意圖。".into(),
                format!("2. 嚴格使用 {dialect} 方言。"),
                "3. 破壞性操作(DROP / TRUNCATE / 沒有 WHERE 的 DELETE 或 UPDATE)必須在說明中明確警告。".into(),
                "4. 資訊不足時先提問;不要臆造資料表名稱或欄位名稱。".into(),
                "5. 不要輸出多餘的客套話。".into(),
            ],
        }
    }

    fn knowledge_rule(self, name: &str) -> String {
        match self {
            Self::En => format!(
                "6. Table and column design must follow the guidelines in \"{name}\" above (naming, required columns, comments, type choices, etc.)."
            ),
            Self::ZhCn => format!(
                "6. 建表 / 字段设计必须遵循上文《{name}》里的规范(命名、必需字段、注释、类型选择等)。"
            ),
            Self::ZhTw => format!(
                "6. 建立資料表 / 欄位設計必須遵循上文《{name}》裡的規範(命名、必要欄位、註解、型別選擇等)。"
            ),
        }
    }
}

fn current_locale() -> String {
    rust_i18n::locale().to_string()
}

/// 知识库文档:每次对话都作为权威参考,置于 system prompt 前部。按当前界面语言。
pub fn describe_knowledge(name: &str, content: &str) -> String {
    describe_knowledge_in(name, content, &current_locale())
}

/// 同 [`describe_knowledge`],显式指定 locale。
pub fn describe_knowledge_in(name: &str, content: &str, locale: &str) -> String {
    let lang = Lang::from_locale(locale);
    let total = content.chars().count();
    let truncated = total > MAX_KNOWLEDGE_CHARS;
    let body: String = if truncated {
        content.chars().take(MAX_KNOWLEDGE_CHARS).collect()
    } else {
        content.to_string()
    };
    let tail = if truncated {
        format!("---\n{}", lang.knowledge_truncated(total))
    } else {
        "---".to_string()
    };
    format!("{}\n---\n{body}\n{tail}", lang.knowledge_intro(name))
}

/// 已加载的表结构 → prompt 片段。按当前界面语言。
pub fn describe_table(name: &str, detail: &TableDetail) -> String {
    describe_table_in(name, detail, &current_locale())
}

/// 同 [`describe_table`],显式指定 locale。
pub fn describe_table_in(name: &str, detail: &TableDetail, locale: &str) -> String {
    let lang = Lang::from_locale(locale);
    let mut lines = vec![format!("## {name}")];
    for c in detail.columns.iter().take(MAX_COLUMNS) {
        let marks: Vec<&str> = [c.is_pk.then_some("PK"), c.not_null.then_some("NOT NULL")]
            .into_iter()
            .flatten()
            .collect();
        let marks = if marks.is_empty() {
            String::new()
        } else {
            format!(" {}", marks.join(" "))
        };
        lines.push(format!("- {} {}{marks}", c.name, c.data_type));
    }
    if detail.columns.len() > MAX_COLUMNS {
        lines.push(format!("- {}", lang.more_columns(detail.columns.len())));
    }
    lines.join("\n")
}

pub struct PromptContext<'a> {
    pub dialect: Dialect,
    pub database: &'a str,
    pub tables: &'a [String],
    /// 已按需加载的表结构(当前浏览的表 + 用户消息里命中的表)
    pub details: &'a BTreeMap<String, TableDetail>,
    /// 全库表→列速览;为 details 未覆盖的表提供粗粒度上下文
    pub columns_brief: Option<&'a ColumnsByTable>,
    /// 知识库文档(名称, 内容)
    pub knowledge: Option<(&'a str, &'a str)>,
}

/// 按当前界面语言(全局 `rust_i18n::locale()`)拼 system prompt。
pub fn build_system_prompt(ctx: &PromptContext) -> String {
    build_system_prompt_in(ctx, &current_locale())
}

/// 同 [`build_system_prompt`],显式指定 locale(en / zh-CN / zh-TW;其它按英文)。
pub fn build_system_prompt_in(ctx: &PromptContext, locale: &str) -> String {
    let lang = Lang::from_locale(locale);
    let dialect = ctx.dialect.display_name();
    let table_line = if ctx.tables.is_empty() {
        lang.empty_database().to_string()
    } else {
        let shown = ctx
            .tables
            .iter()
            .take(MAX_TABLES)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if ctx.tables.len() > MAX_TABLES {
            lang.more_tables(&shown, ctx.tables.len())
        } else {
            shown
        }
    };
    let detail_blocks = ctx
        .details
        .iter()
        .map(|(n, d)| describe_table_in(n, d, locale))
        .collect::<Vec<_>>()
        .join("\n\n");

    let brief_tables: Vec<&String> = ctx
        .tables
        .iter()
        .filter(|t| {
            !ctx.details.contains_key(*t)
                && ctx
                    .columns_brief
                    .is_some_and(|b| b.get(*t).is_some_and(|c| !c.is_empty()))
        })
        .collect();
    let brief_block =
        (!brief_tables.is_empty() && brief_tables.len() <= MAX_BRIEF_TABLES).then(|| {
            brief_tables
                .iter()
                .map(|t| {
                    let cols = &ctx.columns_brief.unwrap()[*t];
                    let names = cols
                        .iter()
                        .take(MAX_BRIEF_COLUMNS)
                        .map(|c| c.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let more = if cols.len() > MAX_BRIEF_COLUMNS {
                        format!(", {}", lang.more_columns(cols.len()))
                    } else {
                        String::new()
                    };
                    format!("- {t}({names}{more})")
                })
                .collect::<Vec<_>>()
                .join("\n")
        });

    let mut lines: Vec<String> = vec![lang.role().into()];
    if let Some((name, content)) = ctx.knowledge {
        lines.push(describe_knowledge_in(name, content, locale));
    }
    let db = if ctx.database.is_empty() {
        lang.default_database()
    } else {
        ctx.database
    };
    lines.push(lang.connection(dialect, db));
    lines.push(lang.tables(&table_line));
    if !detail_blocks.is_empty() {
        lines.push(lang.known_schemas(&detail_blocks));
    }
    if let Some(b) = brief_block {
        lines.push(lang.brief(&b));
    }
    lines.push(String::new());
    lines.extend(lang.rules(dialect));
    if let Some((name, _)) = ctx.knowledge {
        lines.push(lang.knowledge_rule(name));
    }
    lines.join("\n")
}

/// 用户消息中命中的已存在表名(用于按需加载结构),最多 10 张。
pub fn mentioned_tables(text: &str, tables: &[String]) -> Vec<String> {
    let lower = text.to_lowercase();
    tables
        .iter()
        .filter(|t| lower.contains(&t.to_lowercase()))
        .take(10)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pg::browse::ColumnInfo;

    struct Fixture {
        tables: Vec<String>,
        details: BTreeMap<String, TableDetail>,
        brief: ColumnsByTable,
    }

    fn fixture() -> Fixture {
        let tables = vec!["users".to_string(), "orders".to_string()];
        let mut details = BTreeMap::new();
        details.insert(
            "users".to_string(),
            TableDetail {
                columns: vec![ColumnInfo {
                    name: "id".into(),
                    data_type: "int".into(),
                    default: None,
                    comment: None,
                    not_null: true,
                    is_pk: true,
                }],
                indexes: vec![],
            },
        );
        let mut brief = ColumnsByTable::new();
        brief.insert(
            "orders".into(),
            vec![crate::schema::ColumnBrief {
                name: "total".into(),
                data_type: "numeric".into(),
            }],
        );
        Fixture {
            tables,
            details,
            brief,
        }
    }

    fn build(f: &Fixture, knowledge: Option<(&str, &str)>, locale: &str) -> String {
        build_system_prompt_in(
            &PromptContext {
                dialect: Dialect::Mysql,
                database: "shop",
                tables: &f.tables,
                details: &f.details,
                columns_brief: Some(&f.brief),
                knowledge,
            },
            locale,
        )
    }

    #[test]
    fn prompt_contains_schema_and_rules_zh_cn() {
        let f = fixture();
        let p = build(&f, Some(("规范", "表名用复数")), "zh-CN");
        assert!(p.starts_with("你是嵌入在数据库客户端 Datum 中的 SQL 助手。"));
        assert!(p.contains("当前连接:MySQL,数据库 shop。"));
        assert!(p.contains("- id int PK NOT NULL"));
        assert!(p.contains("- orders(total)"));
        assert!(p.contains("《规范》"));
        assert!(p.contains("块外用简短中文说明意图"));
        assert!(p.contains("2. 严格使用 MySQL 方言。"));
        assert!(p.contains("6. 建表"));
    }

    #[test]
    fn prompt_contains_schema_and_rules_en() {
        let f = fixture();
        let p = build(&f, Some(("Guide", "Use plural table names")), "en");
        assert!(p.starts_with("You are the SQL assistant embedded in Datum"));
        assert!(p.contains("Current connection: MySQL, database shop."));
        assert!(p.contains("Tables in the database: users, orders"));
        assert!(p.contains("- id int PK NOT NULL"));
        assert!(p.contains("- orders(total)"));
        assert!(p.contains("\"Guide\""));
        assert!(p.contains("explain the intent in English"));
        assert!(p.contains("2. Strictly use the MySQL dialect."));
        assert!(p.contains("6. Table and column design"));
        // 英文提示词里不应残留中文
        assert!(
            !p.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "{p}"
        );
    }

    #[test]
    fn prompt_contains_schema_and_rules_zh_tw() {
        let f = fixture();
        let p = build(&f, Some(("規範", "資料表名稱用複數")), "zh-TW");
        assert!(p.starts_with("你是內嵌在資料庫用戶端 Datum 中的 SQL 助理。"));
        assert!(p.contains("目前連線:MySQL,資料庫 shop。"));
        assert!(p.contains("- id int PK NOT NULL"));
        assert!(p.contains("- orders(total)"));
        assert!(p.contains("《規範》"));
        assert!(p.contains("繁體中文說明意圖"));
        assert!(p.contains("2. 嚴格使用 MySQL 方言。"));
        assert!(p.contains("6. 建立資料表"));
    }

    /// 三种语言的规则条数一致(语义对齐),无知识库时都没有第 6 条。
    #[test]
    fn rules_aligned_across_locales() {
        let f = fixture();
        for locale in ["en", "zh-CN", "zh-TW"] {
            let p = build(&f, None, locale);
            for n in 1..=5 {
                assert!(p.contains(&format!("\n{n}. ")), "{locale} missing rule {n}");
            }
            assert!(!p.contains("\n6. "), "{locale}");
        }
    }

    #[test]
    fn locale_mapping() {
        assert_eq!(Lang::from_locale("en"), Lang::En);
        assert_eq!(Lang::from_locale("fr"), Lang::En);
        assert_eq!(Lang::from_locale("zh-CN"), Lang::ZhCn);
        assert_eq!(Lang::from_locale("zh"), Lang::ZhCn);
        assert_eq!(Lang::from_locale("zh-TW"), Lang::ZhTw);
        assert_eq!(Lang::from_locale("zh-Hant"), Lang::ZhTw);
    }

    #[test]
    fn empty_database_and_truncated_knowledge() {
        let tables: Vec<String> = vec![];
        let details = BTreeMap::new();
        let ctx = PromptContext {
            dialect: Dialect::Pg,
            database: "",
            tables: &tables,
            details: &details,
            columns_brief: None,
            knowledge: None,
        };
        assert!(build_system_prompt_in(&ctx, "en").contains("(empty database, no tables yet)"));
        assert!(build_system_prompt_in(&ctx, "en").contains("database (default)."));
        assert!(build_system_prompt_in(&ctx, "zh-TW").contains("(空資料庫,尚無資料表)"));
        let long = "x".repeat(MAX_KNOWLEDGE_CHARS + 1);
        assert!(describe_knowledge_in("K", &long, "en").contains("has been truncated"));
        assert!(describe_knowledge_in("K", &long, "zh-CN").contains("文档过长"));
    }

    #[test]
    fn mentioned_tables_case_insensitive() {
        let tables = vec!["Users".to_string(), "orders".to_string()];
        assert_eq!(mentioned_tables("给 users 加一列", &tables), vec!["Users"]);
    }
}
