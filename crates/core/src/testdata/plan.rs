//! 一次造数的方案:表、行数、种子、语言,以及每列的生成器与空值比例。界面编辑的就是它。

use super::gen::{Generator, Locale};
use serde::{Deserialize, Serialize};

/// 行数上限。
pub const MAX_ROWS: u64 = 1_000_000;
/// 不超过这么多行时整个插入在一个事务里;更多时每 [`COMMIT_EVERY`] 行提交一次。
pub const SINGLE_TX_MAX: u64 = 50_000;
pub const COMMIT_EVERY: u64 = 10_000;
/// 「复制为 INSERT」「在编辑器中打开」的行数上限。
pub const SQL_MAX_ROWS: u64 = 1000;
/// 预览行数。
pub const PREVIEW_ROWS: u64 = 10;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColumnPlan {
    pub name: String,
    pub generator: Generator,
    /// 0–1;只对可空列生效
    #[serde(default)]
    pub null_ratio: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub table: String,
    pub rows: u64,
    pub seed: u64,
    #[serde(default)]
    pub locale: Locale,
    pub columns: Vec<ColumnPlan>,
}
