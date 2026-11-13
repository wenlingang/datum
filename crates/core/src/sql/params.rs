//! 查询参数:SQL 里的 `:name` 占位,执行前由用户填值再代入。
//!
//! 字符串、引号标识符、注释里的不算;PG 的 `::type` 转换、数组切片 `a[1:2]` 也不算
//! (参数名必须以字母或下划线开头,且前一个字符不能是 `:` 或标识符字符)。

use super::build::literal;
use super::Dialect;
use std::collections::HashMap;
use std::ops::Range;

/// 一处参数占位:名字(不含冒号)与它在 SQL 里的字节范围(含冒号)。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Slot {
    name: String,
    range: Range<usize>,
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// 扫描出所有参数占位(按出现顺序,同名可重复)。
fn slots(sql: &str) -> Vec<Slot> {
    let chars: Vec<(usize, char)> = sql.char_indices().collect();
    let mut out = vec![];
    let mut i = 0;
    while i < chars.len() {
        let (pos, c) = chars[i];
        let next = chars.get(i + 1).map(|x| x.1);
        match c {
            // 字符串与引号标识符:跳到配对的引号(双写的引号是转义)
            '\'' | '"' | '`' => {
                i += 1;
                while i < chars.len() {
                    if chars[i].1 == c {
                        if chars.get(i + 1).map(|x| x.1) == Some(c) {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    // MySQL 字符串里的反斜杠转义
                    if chars[i].1 == '\\' && c != '"' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
            }
            '-' if next == Some('-') => {
                while i < chars.len() && chars[i].1 != '\n' {
                    i += 1;
                }
            }
            '#' => {
                // MySQL 的 # 行注释;PG 的 #> 运算符后面紧跟 > 或 -,不当注释
                if !matches!(next, Some('>') | Some('-')) {
                    while i < chars.len() && chars[i].1 != '\n' {
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }
            '/' if next == Some('*') => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i].1 == '*' && chars[i + 1].1 == '/') {
                    i += 1;
                }
                i += 2;
            }
            ':' => {
                let prev = if i > 0 { Some(chars[i - 1].1) } else { None };
                let after_colon = prev == Some(':');
                let after_ident = prev.is_some_and(is_ident_char);
                if next == Some(':') {
                    // `::type`
                    i += 2;
                    continue;
                }
                if !after_colon && !after_ident && next.is_some_and(is_ident_start) {
                    let mut j = i + 1;
                    while j < chars.len() && is_ident_char(chars[j].1) {
                        j += 1;
                    }
                    let end = chars.get(j).map_or(sql.len(), |x| x.0);
                    out.push(Slot { name: sql[chars[i + 1].0..end].to_string(), range: pos..end });
                    i = j;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    out
}

/// SQL 里的参数名(去重,按第一次出现的顺序)。
pub fn find_params(sql: &str) -> Vec<String> {
    let mut seen: Vec<String> = vec![];
    for s in slots(sql) {
        if !seen.contains(&s.name) {
            seen.push(s.name);
        }
    }
    seen
}

/// 一个参数值写进 SQL 的样子:整数 / 小数原样,`NULL`(不分大小写)为空值,其余为转义后的字符串字面量。
pub fn value_sql(v: &str, d: Dialect) -> String {
    let t = v.trim();
    if t.eq_ignore_ascii_case("null") {
        return "NULL".into();
    }
    let numeric = !t.is_empty()
        && t.trim_start_matches('-').chars().all(|c| c.is_ascii_digit() || c == '.')
        && t.matches('.').count() <= 1
        && t.trim_start_matches('-').chars().next().is_some_and(|c| c.is_ascii_digit());
    if numeric { t.to_string() } else { literal(v, d) }
}

/// 把参数值代入 SQL;没给值的参数原样保留。
pub fn bind_params(sql: &str, values: &HashMap<String, String>, d: Dialect) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut last = 0;
    for s in slots(sql) {
        let Some(v) = values.get(&s.name) else { continue };
        out.push_str(&sql[last..s.range.start]);
        out.push_str(&value_sql(v, d));
        last = s.range.end;
    }
    out.push_str(&sql[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_params_and_skips_strings_comments_casts() {
        let sql = "SELECT id::text, ':skip', \":skip\" FROM t -- :skip\n\
                   WHERE a = :status AND b > :min /* :skip */ AND arr[1:2] = x AND c = :status AND d = :_x1";
        assert_eq!(find_params(sql), vec!["status", "min", "_x1"]);
        assert!(find_params("SELECT 1").is_empty());
        // MySQL 的 # 注释、字符串里的反斜杠转义
        assert_eq!(find_params("SELECT 'it\\'s :no' # :no\nWHERE x = :yes"), vec!["yes"]);
        // 时间字面量、URL 都在字符串里
        assert!(find_params("SELECT '12:30', 'http://a:8080'").is_empty());
    }

    #[test]
    fn binds_numbers_null_and_escaped_text() {
        let values: HashMap<String, String> = [("status", "it's paid"), ("min", "100"), ("neg", "-2.5"), ("none", "null"), ("code", "007a")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let sql = "SELECT * FROM t WHERE s = :status AND n > :min AND m < :neg AND x IS :none AND c = :code AND s2 = :status AND keep = :other";
        assert_eq!(
            bind_params(sql, &values, Dialect::Pg),
            "SELECT * FROM t WHERE s = 'it''s paid' AND n > 100 AND m < -2.5 AND x IS NULL AND c = '007a' AND s2 = 'it''s paid' AND keep = :other"
        );
        // MySQL 还转义反斜杠
        let v: HashMap<String, String> = [("p".to_string(), "a\\b".to_string())].into_iter().collect();
        assert_eq!(bind_params("SELECT :p", &v, Dialect::Mysql), "SELECT 'a\\\\b'");
        assert_eq!(value_sql("1.2.3", Dialect::Pg), "'1.2.3'");
        assert_eq!(value_sql("", Dialect::Pg), "''");
    }
}
