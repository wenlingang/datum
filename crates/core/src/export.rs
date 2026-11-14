//! 整表 / 整库导出:查询结果逐行写进文件,不把全部行先收进内存、也不经过界面。
//!
//! CSV / JSON / Markdown / SQL 的输出与 [`crate::grid`] 里一次性渲染的版本逐字节一致
//! (那些用于导出当前结果、复制);Excel 需要整份生成,行收在 Rust 侧再写(上限是 Excel 的行数上限)。

use crate::error::{AppError, AppResult, ErrorKind};
use crate::grid::Cell;
use crate::sql::Dialect;
use rust_i18n::t;
use std::io::{BufWriter, Write};
use std::path::Path;

/// 导出格式(与界面的 ExportFormat 同名)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Csv,
    Json,
    Markdown,
    Sql,
    Xlsx,
}

/// 逐行接收查询结果。驱动先给列名(每条语句一次),再逐行给。
pub trait RowSink: Send {
    fn columns(&mut self, columns: &[String]) -> AppResult<()>;
    fn row(&mut self, row: Vec<Cell>) -> AppResult<()>;
}

fn io_err(e: impl std::fmt::Display) -> AppError {
    AppError::new(ErrorKind::Query, t!("core.err.export_write_failed")).with_detail(e.to_string())
}

/// 把一张表(一条查询)的结果按格式写进文件。
pub struct FileSink {
    out: BufWriter<std::fs::File>,
    format: Format,
    table: String,
    dialect: Dialect,
    columns: Vec<String>,
    json_keys: Vec<String>,
    rows: u64,
    /// Excel:行收在这里,结束时整份写出
    xlsx_rows: Vec<Vec<Cell>>,
    path: std::path::PathBuf,
}

impl FileSink {
    pub fn create(path: &Path, format: Format, table: &str, dialect: Dialect) -> AppResult<Self> {
        let file = std::fs::File::create(path).map_err(io_err)?;
        Ok(FileSink {
            out: BufWriter::with_capacity(256 * 1024, file),
            format,
            table: table.to_string(),
            dialect,
            columns: vec![],
            json_keys: vec![],
            rows: 0,
            xlsx_rows: vec![],
            path: path.to_path_buf(),
        })
    }

    pub fn rows(&self) -> u64 {
        self.rows
    }

    /// 写完结尾并落盘。
    pub fn finish(mut self) -> AppResult<u64> {
        match self.format {
            Format::Json => {
                // 列名阶段已写了 "["(没有列名时什么也没写)
                let tail: &[u8] = match (self.json_keys.is_empty() && self.columns.is_empty(), self.rows) {
                    (true, _) => b"[]",
                    (false, 0) => b"]",
                    _ => b"\n]",
                };
                self.out.write_all(tail).map_err(io_err)?;
            }
            Format::Xlsx => {
                let refs: Vec<&Vec<Cell>> = self.xlsx_rows.iter().collect();
                let bytes = crate::grid::to_xlsx(&self.table, &self.columns, &refs)
                    .map_err(|e| AppError::new(ErrorKind::Query, e))?;
                self.out.write_all(&bytes).map_err(io_err)?;
            }
            Format::Csv | Format::Markdown | Format::Sql => {}
        }
        self.out.flush().map_err(io_err)?;
        Ok(self.rows)
    }

    /// 出错时删掉写了一半的文件。
    pub fn abandon(self) {
        let path = self.path.clone();
        drop(self);
        let _ = std::fs::remove_file(path);
    }

    fn write(&mut self, s: &str) -> AppResult<()> {
        self.out.write_all(s.as_bytes()).map_err(io_err)
    }
}

fn csv_field(v: &str) -> String {
    if v.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v.to_string()
    }
}

fn md_field(v: &str) -> String {
    v.replace('|', "\\|").replace("\r\n", " ").replace('\n', " ")
}

impl RowSink for FileSink {
    fn columns(&mut self, columns: &[String]) -> AppResult<()> {
        self.columns = columns.to_vec();
        match self.format {
            Format::Csv => {
                let head = columns.iter().map(|c| csv_field(c)).collect::<Vec<_>>().join(",");
                self.write(&head)?;
            }
            Format::Markdown => {
                let head = columns.iter().map(|c| md_field(c)).collect::<Vec<_>>().join(" | ");
                let rule = columns.iter().map(|_| "---").collect::<Vec<_>>().join(" | ");
                self.write(&format!("| {head} |\n| {rule} |"))?;
            }
            Format::Json => {
                self.json_keys = crate::grid::unique_keys(columns);
                self.write("[")?;
            }
            Format::Sql | Format::Xlsx => {}
        }
        Ok(())
    }

    fn row(&mut self, row: Vec<Cell>) -> AppResult<()> {
        let first = self.rows == 0;
        self.rows += 1;
        match self.format {
            Format::Csv => {
                let line = row.iter().map(|c| csv_field(c.as_deref().unwrap_or(""))).collect::<Vec<_>>().join(",");
                self.write("\r\n")?;
                self.write(&line)
            }
            Format::Markdown => {
                let line = row.iter().map(|c| md_field(c.as_deref().unwrap_or(""))).collect::<Vec<_>>().join(" | ");
                self.write(&format!("\n| {line} |"))
            }
            Format::Json => {
                let obj = crate::grid::json_object(&self.json_keys, &row);
                let pretty = serde_json::to_string_pretty(&obj).map_err(io_err)?;
                // 与 to_string_pretty(数组) 的缩进一致:元素整体再缩进两格
                let indented = pretty.lines().map(|l| format!("  {l}")).collect::<Vec<_>>().join("\n");
                self.write(if first { "\n" } else { ",\n" })?;
                self.write(&indented)
            }
            Format::Sql => {
                let stmt = crate::sql::build::build_insert_row(&self.table, &self.columns, &row, self.dialect);
                if !first {
                    self.write("\n")?;
                }
                self.write(&stmt)
            }
            Format::Xlsx => {
                if self.xlsx_rows.len() >= crate::grid::XLSX_MAX_ROWS {
                    return Err(AppError::new(
                        ErrorKind::Query,
                        t!("core.err.xlsx_too_many_rows", max = crate::grid::XLSX_MAX_ROWS),
                    ));
                }
                self.xlsx_rows.push(row);
                Ok(())
            }
        }
    }
}

/// 整库导出成一个 SQL 文件:每张表一段注释(行数或失败原因)+ 逐行 INSERT,表之间空一行。
/// 与以前整份拼字符串的版本输出一致。
pub struct DatabaseSink {
    out: BufWriter<std::fs::File>,
    dialect: Dialect,
    table: String,
    columns: Vec<String>,
    /// 当前表的 INSERT 先写进临时文件:注释里的行数要等整张表读完才知道
    body: BufWriter<std::fs::File>,
    body_path: std::path::PathBuf,
    rows: u64,
    tables_written: usize,
}

impl DatabaseSink {
    pub fn create(path: &Path, dialect: Dialect) -> AppResult<Self> {
        let out = BufWriter::with_capacity(256 * 1024, std::fs::File::create(path).map_err(io_err)?);
        let body_path = path.with_extension(format!("{}.part", std::process::id()));
        let body = BufWriter::with_capacity(256 * 1024, std::fs::File::create(&body_path).map_err(io_err)?);
        Ok(DatabaseSink { out, dialect, table: String::new(), columns: vec![], body, body_path, rows: 0, tables_written: 0 })
    }

    /// 开始一张表。
    pub fn begin_table(&mut self, table: &str) -> AppResult<()> {
        self.table = table.to_string();
        self.columns.clear();
        self.rows = 0;
        let f = std::fs::File::create(&self.body_path).map_err(io_err)?;
        self.body = BufWriter::with_capacity(256 * 1024, f);
        Ok(())
    }

    fn separator(&mut self) -> AppResult<()> {
        if self.tables_written > 0 {
            self.out.write_all(b"\n\n").map_err(io_err)?;
        }
        self.tables_written += 1;
        Ok(())
    }

    /// 表读完:写注释(行数)与暂存的 INSERT。
    pub fn end_table(&mut self) -> AppResult<()> {
        self.body.flush().map_err(io_err)?;
        self.separator()?;
        let head = t!("core.export.table_rows", table = self.table, count = self.rows).to_string();
        self.out.write_all(head.as_bytes()).map_err(io_err)?;
        if self.rows > 0 {
            self.out.write_all(b"\n\n").map_err(io_err)?;
            let mut f = std::fs::File::open(&self.body_path).map_err(io_err)?;
            std::io::copy(&mut f, &mut self.out).map_err(io_err)?;
        }
        Ok(())
    }

    /// 表读失败:写一段失败说明(已暂存的行丢弃)。
    pub fn failed_table(&mut self, error: &str) -> AppResult<()> {
        self.separator()?;
        let line = t!("core.export.table_failed", table = self.table, error = error).to_string();
        self.out.write_all(line.as_bytes()).map_err(io_err)
    }

    pub fn finish(mut self) -> AppResult<()> {
        self.out.flush().map_err(io_err)?;
        let _ = std::fs::remove_file(&self.body_path);
        Ok(())
    }
}

impl RowSink for DatabaseSink {
    fn columns(&mut self, columns: &[String]) -> AppResult<()> {
        self.columns = columns.to_vec();
        Ok(())
    }

    fn row(&mut self, row: Vec<Cell>) -> AppResult<()> {
        if self.rows > 0 {
            self.body.write_all(b"\n").map_err(io_err)?;
        }
        self.rows += 1;
        let stmt = crate::sql::build::build_insert_row(&self.table, &self.columns, &row, self.dialect);
        self.body.write_all(stmt.as_bytes()).map_err(io_err)
    }
}

impl Drop for DatabaseSink {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.body_path);
    }
}
