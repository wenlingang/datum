//! 结果网格的客户端逻辑:筛选、排序、导出、单元格分级与列宽适配。

use rust_i18n::t;
use std::cmp::Ordering;

pub type Cell = Option<String>;

// ---------------------------------------------------------------- 排序

/// 单元格比较:两边都能解析为数字时按数值,其余按字符串。NULL 由调用方单独处理。
pub fn compare_cells(a: &str, b: &str) -> Ordering {
    let (ta, tb) = (a.trim(), b.trim());
    if !ta.is_empty() && !tb.is_empty() {
        if let (Ok(na), Ok(nb)) = (ta.parse::<f64>(), tb.parse::<f64>()) {
            return na.partial_cmp(&nb).unwrap_or(Ordering::Equal);
        }
    }
    a.cmp(b)
}

/// 按某列对行排序(返回新数组)。NULL 始终排在最后,与方向无关。
pub fn sort_rows(rows: &[Vec<Cell>], col_ix: usize, descending: bool) -> Vec<Vec<Cell>> {
    let mut out = rows.to_vec();
    out.sort_by(|ra, rb| match (&ra[col_ix], &rb[col_ix]) {
        (None, None) => Ordering::Equal,
        (None, _) => Ordering::Greater,
        (_, None) => Ordering::Less,
        (Some(a), Some(b)) => {
            let o = compare_cells(a, b);
            if descending {
                o.reverse()
            } else {
                o
            }
        }
    });
    out
}

// ---------------------------------------------------------------- 筛选

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOp {
    Contains,
    Eq,
    Ne,
    Starts,
    Ends,
    Gt,
    Lt,
    Gte,
    Lte,
    Empty,
    NotEmpty,
}

impl FilterOp {
    pub const ALL: [FilterOp; 11] = [
        Self::Contains,
        Self::Eq,
        Self::Ne,
        Self::Starts,
        Self::Ends,
        Self::Gt,
        Self::Lt,
        Self::Gte,
        Self::Lte,
        Self::Empty,
        Self::NotEmpty,
    ];

    pub fn label(self) -> String {
        match self {
            Self::Contains => t!("core.filter.contains"),
            Self::Eq => t!("core.filter.eq"),
            Self::Ne => t!("core.filter.ne"),
            Self::Starts => t!("core.filter.starts"),
            Self::Ends => t!("core.filter.ends"),
            Self::Gt => t!("core.filter.gt"),
            Self::Lt => t!("core.filter.lt"),
            Self::Gte => t!("core.filter.gte"),
            Self::Lte => t!("core.filter.lte"),
            Self::Empty => t!("core.filter.empty"),
            Self::NotEmpty => t!("core.filter.not_empty"),
        }
        .to_string()
    }

    /// 为空/非空无需输入值。
    pub fn needs_value(self) -> bool {
        !matches!(self, Self::Empty | Self::NotEmpty)
    }
}

/// 条件之间的连接词(全局统一,避免 AND/OR 混用的优先级歧义)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Conj {
    #[default]
    And,
    Or,
}

impl Conj {
    pub fn toggled(self) -> Self {
        match self {
            Self::And => Self::Or,
            Self::Or => Self::And,
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::And => t!("core.filter.and"),
            Self::Or => t!("core.filter.or"),
        }
        .to_string()
    }
}

/// 单条筛选条件。`col` 为空串表示尚未选择列(无效,不参与匹配)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterRow {
    pub id: u64,
    pub col: String,
    pub op: FilterOp,
    pub value: String,
}

impl FilterRow {
    /// 有效条件:选了列,且(无需值 或 填了值)。
    pub fn is_active(&self) -> bool {
        !self.col.is_empty() && (!self.op.needs_value() || !self.value.is_empty())
    }
}

/// 单元格是否满足某条件。NULL/空串视为"空";比较类操作走数值感知比较,字符串不区分大小写。
pub fn match_cell(cell: Option<&str>, op: FilterOp, value: &str) -> bool {
    match op {
        FilterOp::Empty => return cell.is_none_or(str::is_empty),
        FilterOp::NotEmpty => return cell.is_some_and(|c| !c.is_empty()),
        _ => {}
    }
    let Some(cell) = cell else {
        return op == FilterOp::Ne; // NULL 不等于任何具体值
    };
    let (lc, lv) = (cell.to_lowercase(), value.to_lowercase());
    match op {
        FilterOp::Contains => lc.contains(&lv),
        FilterOp::Starts => lc.starts_with(&lv),
        FilterOp::Ends => lc.ends_with(&lv),
        FilterOp::Eq => compare_cells(&lc, &lv) == Ordering::Equal,
        FilterOp::Ne => compare_cells(&lc, &lv) != Ordering::Equal,
        FilterOp::Gt => compare_cells(cell, value) == Ordering::Greater,
        FilterOp::Lt => compare_cells(cell, value) == Ordering::Less,
        FilterOp::Gte => compare_cells(cell, value) != Ordering::Less,
        FilterOp::Lte => compare_cells(cell, value) != Ordering::Greater,
        FilterOp::Empty | FilterOp::NotEmpty => unreachable!(),
    }
}

/// 条件集的规范化签名:仅有效条件的(列, 操作符, 去空白后的值)+ 连接词。
/// 用于比较"草稿"与"已应用"是否一致。
pub fn filters_signature(filters: &[FilterRow], conj: Conj) -> Vec<(String, FilterOp, String)> {
    let mut sig: Vec<_> = filters
        .iter()
        .filter(|f| f.is_active())
        .map(|f| {
            let v = if f.op.needs_value() {
                f.value.trim().to_string()
            } else {
                String::new()
            };
            (f.col.clone(), f.op, v)
        })
        .collect();
    // 连接词编码进签名尾部
    sig.push((String::new(), FilterOp::Contains, format!("{conj:?}")));
    sig
}

/// 按条件筛出命中行的下标(保持原顺序)。无有效条件时返回全部下标。
pub fn filter_indices(
    rows: &[Vec<Cell>],
    columns: &[String],
    filters: &[FilterRow],
    conj: Conj,
) -> Vec<usize> {
    let active: Vec<(usize, &FilterRow)> = filters
        .iter()
        .filter(|f| f.is_active())
        .map(|f| {
            (
                columns
                    .iter()
                    .position(|c| *c == f.col)
                    .unwrap_or(usize::MAX),
                f,
            )
        })
        .collect();
    if active.is_empty() {
        return (0..rows.len()).collect();
    }
    rows.iter()
        .enumerate()
        .filter(|(_, row)| {
            let mut results = active.iter().map(|(ci, f)| {
                row.get(*ci)
                    .is_some_and(|c| match_cell(c.as_deref(), f.op, &f.value))
            });
            match conj {
                Conj::And => results.all(|r| r),
                Conj::Or => results.any(|r| r),
            }
        })
        .map(|(i, _)| i)
        .collect()
}

// ---------------------------------------------------------------- 导出

/// 查询结果 → CSV。含分隔符/引号/换行的字段用双引号包裹并转义。
pub fn to_csv(columns: &[String], rows: &[&Vec<Cell>]) -> String {
    fn esc(v: &str) -> String {
        if v.contains([',', '"', '\n', '\r']) {
            format!("\"{}\"", v.replace('"', "\"\""))
        } else {
            v.to_string()
        }
    }
    let mut lines = vec![columns.iter().map(|c| esc(c)).collect::<Vec<_>>().join(",")];
    for row in rows {
        lines.push(
            row.iter()
                .map(|c| esc(c.as_deref().unwrap_or("")))
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    lines.join("\r\n")
}

/// 查询结果 → GFM Markdown 表格。竖线转义,换行压成空格。
pub fn to_markdown(columns: &[String], rows: &[&Vec<Cell>]) -> String {
    fn esc(v: &str) -> String {
        v.replace('|', "\\|")
            .replace("\r\n", " ")
            .replace('\n', " ")
    }
    let mut lines = vec![
        format!(
            "| {} |",
            columns
                .iter()
                .map(|c| esc(c))
                .collect::<Vec<_>>()
                .join(" | ")
        ),
        format!(
            "| {} |",
            columns
                .iter()
                .map(|_| "---")
                .collect::<Vec<_>>()
                .join(" | ")
        ),
    ];
    for row in rows {
        lines.push(format!(
            "| {} |",
            row.iter()
                .map(|c| esc(c.as_deref().unwrap_or("")))
                .collect::<Vec<_>>()
                .join(" | ")
        ));
    }
    lines.join("\n")
}

/// 本地时间格式化成文件名友好的 `YYYYMMDD_HHmmss`。
pub fn file_timestamp() -> String {
    chrono::Local::now().format("%Y%m%d_%H%M%S").to_string()
}

// ---------------------------------------------------------------- 单元格样式

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
    Center,
}

fn is_numeric_type(t: &str) -> bool {
    [
        "int", "serial", "numeric", "decimal", "real", "double", "float", "money",
    ]
    .iter()
    .any(|k| t.contains(k))
}

/// 按类型推断列对齐(表头与单元格共用)。
pub fn col_align(data_type: Option<&str>) -> Align {
    let t = data_type.unwrap_or("").to_lowercase();
    if t.contains("bool") {
        Align::Center
    } else if is_numeric_type(&t) {
        Align::Right
    } else {
        Align::Left
    }
}

/// 单元格的灰阶档位(Datum 灰阶三档 + 单一语义色)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellTier {
    /// 主值(文本)
    Strong,
    /// 软值(enum、bool-true、默认)
    Normal,
    /// 次级(数字、时间)
    Dim,
    /// 不透明哈希块(uuid/bytea)
    Hash,
    /// 凹陷(NULL、0001- 空时间戳)
    Null,
    /// 唯一语义色:bool-false
    Negative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellStyle {
    pub tier: CellTier,
    pub mono: bool,
}

pub fn classify_cell(value: Option<&str>, data_type: Option<&str>) -> CellStyle {
    let Some(value) = value else {
        return CellStyle {
            tier: CellTier::Null,
            mono: true,
        };
    };
    let t = data_type.unwrap_or("").to_lowercase();
    if t.contains("bool") || value == "true" || value == "false" {
        let truthy = value == "true" || value == "t";
        return CellStyle {
            tier: if truthy {
                CellTier::Normal
            } else {
                CellTier::Negative
            },
            mono: true,
        };
    }
    let looks_numeric = {
        let v = value.strip_prefix('-').unwrap_or(value);
        let mut parts = v.splitn(2, '.');
        let int = parts.next().unwrap_or("");
        let frac = parts.next();
        !int.is_empty()
            && int.bytes().all(|b| b.is_ascii_digit())
            && frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
    };
    if is_numeric_type(&t) || looks_numeric {
        return CellStyle {
            tier: CellTier::Dim,
            mono: true,
        };
    }
    if t.contains("time") || t.contains("date") {
        let tier = if value.starts_with("0001-") {
            CellTier::Null
        } else {
            CellTier::Dim
        };
        return CellStyle { tier, mono: true };
    }
    if t.contains("uuid") || t.contains("bytea") {
        return CellStyle {
            tier: CellTier::Hash,
            mono: true,
        };
    }
    if ["char", "text", "citext", "name"]
        .iter()
        .any(|k| t.contains(k))
    {
        return CellStyle {
            tier: CellTier::Strong,
            mono: false,
        };
    }
    CellStyle {
        tier: CellTier::Normal,
        mono: false,
    }
}

/// 计算每列最终宽度,使结果表自适应结果区宽度。
///
/// - 内容比容器窄:多余宽度按比例分给可伸缩列(文本列),铺满容器;
/// - 内容比容器宽:保持内容宽度(横向滚动);
/// - 单列结果(如 EXPLAIN):取容器宽的一半(不小于 `default_w`)。
pub fn column_widths(
    auto_widths: &[f32],
    container_w: f32,
    default_w: f32,
    flexible: &[bool],
) -> Vec<f32> {
    let n = auto_widths.len();
    if n == 1 {
        return vec![if container_w > 0.0 {
            default_w.max((container_w * 0.5).floor())
        } else {
            auto_widths[0]
        }];
    }
    let total: f32 = auto_widths.iter().sum();
    let surplus = container_w - total;
    if container_w <= 0.0 || surplus <= 0.0 {
        return auto_widths.to_vec();
    }
    let mut targets: Vec<usize> = (0..n)
        .filter(|i| flexible.get(*i).copied().unwrap_or(true))
        .collect();
    if targets.is_empty() {
        targets = (0..n).collect();
    }
    let target_total: f32 = targets.iter().map(|i| auto_widths[*i]).sum();
    let mut out = auto_widths.to_vec();
    let mut used = 0.0;
    for (k, i) in targets.iter().enumerate() {
        let add = if k == targets.len() - 1 {
            surplus - used
        } else {
            (surplus * (auto_widths[*i] / target_total)).round()
        };
        out[*i] += add;
        used += add;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Cell {
        Some(v.to_string())
    }

    #[test]
    fn numeric_aware_sort_with_nulls_last() {
        let rows = vec![vec![s("10")], vec![None], vec![s("9")], vec![s("100")]];
        let asc = sort_rows(&rows, 0, false);
        assert_eq!(
            asc,
            vec![vec![s("9")], vec![s("10")], vec![s("100")], vec![None]]
        );
        let desc = sort_rows(&rows, 0, true);
        assert_eq!(desc.last().unwrap(), &vec![None]);
        assert_eq!(desc[0], vec![s("100")]);
    }

    #[test]
    fn filters_and_or() {
        let columns = vec!["name".to_string(), "age".to_string()];
        let rows = vec![
            vec![s("Alice"), s("30")],
            vec![s("bob"), s("25")],
            vec![None, s("40")],
        ];
        let f = |id, col: &str, op, v: &str| FilterRow {
            id,
            col: col.into(),
            op,
            value: v.into(),
        };
        let and = filter_indices(
            &rows,
            &columns,
            &[
                f(1, "name", FilterOp::Contains, "b"),
                f(2, "age", FilterOp::Gt, "26"),
            ],
            Conj::And,
        );
        assert!(and.is_empty());
        let or = filter_indices(
            &rows,
            &columns,
            &[
                f(1, "name", FilterOp::Contains, "b"),
                f(2, "age", FilterOp::Gt, "26"),
            ],
            Conj::Or,
        );
        assert_eq!(or, vec![0, 1, 2]);
        let empty = filter_indices(
            &rows,
            &columns,
            &[f(1, "name", FilterOp::Empty, "")],
            Conj::And,
        );
        assert_eq!(empty, vec![2]);
        // 无效条件(没选列 / 没填值)不参与
        assert_eq!(
            filter_indices(&rows, &columns, &[f(1, "", FilterOp::Eq, "x")], Conj::And).len(),
            3
        );
    }

    #[test]
    fn null_is_not_equal_to_values() {
        assert!(match_cell(None, FilterOp::Ne, "x"));
        assert!(!match_cell(None, FilterOp::Eq, "x"));
        assert!(match_cell(Some("ABC"), FilterOp::Eq, "abc"));
    }

    #[test]
    fn signature_ignores_ids_and_whitespace() {
        let a = vec![FilterRow {
            id: 1,
            col: "a".into(),
            op: FilterOp::Eq,
            value: " x ".into(),
        }];
        let b = vec![FilterRow {
            id: 9,
            col: "a".into(),
            op: FilterOp::Eq,
            value: "x".into(),
        }];
        assert_eq!(
            filters_signature(&a, Conj::And),
            filters_signature(&b, Conj::And)
        );
        assert_ne!(
            filters_signature(&a, Conj::And),
            filters_signature(&b, Conj::Or)
        );
    }

    #[test]
    fn csv_and_markdown_escape() {
        let cols = vec!["a".to_string(), "b".to_string()];
        let r1 = vec![s("x,y"), s("say \"hi\"")];
        let r2 = vec![None, s("a|b\nc")];
        let rows = vec![&r1, &r2];
        assert_eq!(
            to_csv(&cols, &rows),
            "a,b\r\n\"x,y\",\"say \"\"hi\"\"\"\r\n,\"a|b\nc\""
        );
        assert_eq!(
            to_markdown(&cols, &rows),
            "| a | b |\n| --- | --- |\n| x,y | say \"hi\" |\n|  | a\\|b c |"
        );
    }

    #[test]
    fn classifies_cells() {
        assert_eq!(classify_cell(None, None).tier, CellTier::Null);
        assert_eq!(
            classify_cell(Some("false"), Some("boolean")).tier,
            CellTier::Negative
        );
        assert_eq!(classify_cell(Some("-3.5"), None).tier, CellTier::Dim);
        assert_eq!(
            classify_cell(Some("0001-01-01"), Some("timestamp")).tier,
            CellTier::Null
        );
        assert_eq!(
            classify_cell(Some("abc"), Some("varchar(10)")),
            CellStyle {
                tier: CellTier::Strong,
                mono: false
            }
        );
        assert_eq!(col_align(Some("BIGINT")), Align::Right);
    }

    #[test]
    fn widths_stretch_flexible_columns() {
        assert_eq!(
            column_widths(&[100.0, 100.0], 400.0, 170.0, &[true, false]),
            vec![300.0, 100.0]
        );
        assert_eq!(
            column_widths(&[300.0, 300.0], 400.0, 170.0, &[true, true]),
            vec![300.0, 300.0]
        );
        assert_eq!(column_widths(&[80.0], 1000.0, 170.0, &[true]), vec![500.0]);
    }
}
