//! SQL 美化:统一缩进、关键字大写、逗号换行。

use sqlformat::{FormatOptions, Indent, QueryParams};

/// 按统一风格格式化 SQL。格式化器对残缺文本也会尽力输出,不会失败。
pub fn format_sql(sql: &str) -> String {
    let options = FormatOptions {
        indent: Indent::Spaces(2),
        uppercase: Some(true),
        lines_between_queries: 1,
        ..Default::default()
    };
    sqlformat::format(sql, &QueryParams::None, &options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uppercases_keywords_and_breaks_lines() {
        let out = format_sql("select id, name from users where id = 1");
        assert!(out.starts_with("SELECT"));
        assert!(out.contains("FROM"));
        assert!(out.contains('\n'));
    }
}
