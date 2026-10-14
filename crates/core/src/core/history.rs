//! SQL 查询历史(query_history.json)——记录执行成功的 SQL,供前端浮层展示与再次执行。
//!
//! 去重/截断都在这一层做,落盘文件是唯一权威:前端 append 后直接用返回的完整列表
//! 覆盖内存态,避免两端各自实现去重逻辑产生漂移。

use crate::error::{AppError, AppResult, ErrorKind};
use rust_i18n::t;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 历史上限:超出后丢最旧的。
pub const MAX_ENTRIES: usize = 200;

/// kind 存 String 而非 DbKind 枚举:历史文件里出现未知 kind 时不至于整个文件解析失败。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HistoryEntry {
    pub id: String,
    pub sql: String,
    pub conn_id: String,
    pub kind: String,
    /// 连接名快照:连接删除/改名后历史仍可读。
    pub conn_name: String,
    /// Unix 毫秒。
    pub executed_at: i64,
    pub elapsed_ms: u64,
    pub row_count: Option<u64>,
    pub affected: Option<u64>,
}

/// 仿 ConnectionRepo:config_dir + 固定文件名的 JSON 读写。
pub struct HistoryRepo {
    path: PathBuf,
}

impl HistoryRepo {
    pub fn new(config_dir: &Path) -> Self {
        Self {
            path: config_dir.join("query_history.json"),
        }
    }

    /// 文件缺失/损坏都回空列表:历史丢了不值得让整个应用报错。
    pub fn load(&self) -> Vec<HistoryEntry> {
        std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// 头插新条目;与头部条目 sql+connId 相同时原地更新(反复 ⌘↵ 同一条 SQL 不刷屏)。
    /// 返回截断后的完整列表。
    pub fn append(&self, entry: HistoryEntry) -> AppResult<Vec<HistoryEntry>> {
        let mut entries = self.load();
        match entries.first_mut() {
            Some(head) if head.sql == entry.sql && head.conn_id == entry.conn_id => {
                head.executed_at = entry.executed_at;
                head.elapsed_ms = entry.elapsed_ms;
                head.row_count = entry.row_count;
                head.affected = entry.affected;
            }
            _ => {
                entries.insert(0, entry);
                entries.truncate(MAX_ENTRIES);
            }
        }
        self.save(&entries)?;
        Ok(entries)
    }

    pub fn clear(&self) -> AppResult<()> {
        self.save(&[])
    }

    /// 只清除指定连接的历史(历史按数据库隔离展示,清空也只影响当前库)。
    /// 返回剩余的完整列表。
    pub fn clear_conn(&self, conn_id: &str) -> AppResult<Vec<HistoryEntry>> {
        let entries: Vec<HistoryEntry> = self
            .load()
            .into_iter()
            .filter(|e| e.conn_id != conn_id)
            .collect();
        self.save(&entries)?;
        Ok(entries)
    }

    fn save(&self, entries: &[HistoryEntry]) -> AppResult<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AppError::new(ErrorKind::Config, t!("core.err.create_config_dir_failed"))
                    .with_detail(e.to_string())
            })?;
        }
        let json = serde_json::to_vec_pretty(entries).map_err(|e| {
            AppError::new(ErrorKind::Config, t!("core.err.serialize_history_failed"))
                .with_detail(e.to_string())
        })?;
        std::fs::write(&self.path, json).map_err(|e| {
            AppError::new(ErrorKind::Config, t!("core.err.write_history_failed"))
                .with_detail(e.to_string())
        })?;
        // SQL 文本可能含敏感字面量,收紧到仅本用户可读。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600)).map_err(
                |e| {
                    AppError::new(ErrorKind::Config, t!("core.err.set_permissions_failed"))
                        .with_detail(e.to_string())
                },
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo(tag: &str) -> (PathBuf, HistoryRepo) {
        let dir = std::env::temp_dir().join(format!("datum-hist-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        (dir.clone(), HistoryRepo::new(&dir))
    }

    fn entry(id: &str, sql: &str, conn_id: &str) -> HistoryEntry {
        HistoryEntry {
            id: id.into(),
            sql: sql.into(),
            conn_id: conn_id.into(),
            kind: "pg".into(),
            conn_name: "local-pg".into(),
            executed_at: 1_700_000_000_000,
            elapsed_ms: 12,
            row_count: Some(3),
            affected: None,
        }
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let (_, repo) = temp_repo("missing");
        assert!(repo.load().is_empty());
    }

    #[test]
    fn load_corrupt_file_returns_empty() {
        let (dir, repo) = temp_repo("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("query_history.json"), b"{ not json").unwrap();
        assert!(repo.load().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn append_puts_newest_first() {
        let (dir, repo) = temp_repo("order");
        repo.append(entry("a", "select 1", "c1")).unwrap();
        let list = repo.append(entry("b", "select 2", "c1")).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "b");
        assert_eq!(repo.load(), list);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 连续重复(同 sql + connId):不新增,原地更新头部条目的执行信息,保留原 id。
    #[test]
    fn consecutive_duplicate_updates_head_in_place() {
        let (dir, repo) = temp_repo("dup");
        repo.append(entry("a", "select 1", "c1")).unwrap();
        let mut dup = entry("b", "select 1", "c1");
        dup.executed_at = 1_700_000_099_000;
        dup.elapsed_ms = 99;
        dup.row_count = Some(7);
        let list = repo.append(dup).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "a");
        assert_eq!(list[0].executed_at, 1_700_000_099_000);
        assert_eq!(list[0].elapsed_ms, 99);
        assert_eq!(list[0].row_count, Some(7));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 同 sql 不同连接不算重复;穿插执行别的 SQL 后同 sql 也正常新增。
    #[test]
    fn non_consecutive_or_other_conn_not_deduped() {
        let (dir, repo) = temp_repo("nodedup");
        repo.append(entry("a", "select 1", "c1")).unwrap();
        let list = repo.append(entry("b", "select 1", "c2")).unwrap();
        assert_eq!(list.len(), 2);
        repo.append(entry("c", "select 2", "c1")).unwrap();
        let list = repo.append(entry("d", "select 1", "c2")).unwrap();
        assert_eq!(list.len(), 4);
        assert_eq!(list[0].id, "d");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn truncates_to_max_entries_dropping_oldest() {
        let (dir, repo) = temp_repo("trunc");
        for i in 0..=MAX_ENTRIES {
            repo.append(entry(&format!("id{i}"), &format!("select {i}"), "c1"))
                .unwrap();
        }
        let list = repo.load();
        assert_eq!(list.len(), MAX_ENTRIES);
        assert_eq!(list[0].id, format!("id{MAX_ENTRIES}"));
        assert_eq!(list.last().unwrap().id, "id1"); // id0 最旧,被丢弃
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clear_empties_history() {
        let (dir, repo) = temp_repo("clear");
        repo.append(entry("a", "select 1", "c1")).unwrap();
        repo.clear().unwrap();
        assert!(repo.load().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 按连接清除:只删指定 connId 的条目,其他连接的历史原样保留。
    #[test]
    fn clear_conn_only_removes_matching_entries() {
        let (dir, repo) = temp_repo("clearconn");
        repo.append(entry("a", "select 1", "c1")).unwrap();
        repo.append(entry("b", "select 2", "c2")).unwrap();
        repo.append(entry("c", "select 3", "c1")).unwrap();
        let rest = repo.clear_conn("c1").unwrap();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].id, "b");
        assert_eq!(repo.load(), rest); // 落盘一致
        std::fs::remove_dir_all(&dir).ok();
    }

    /// SQL 文本可能含敏感字面量,收紧到仅本用户可读。
    #[cfg(unix)]
    #[test]
    fn saved_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, repo) = temp_repo("perm");
        repo.append(entry("a", "select 1", "c1")).unwrap();
        let mode = std::fs::metadata(dir.join("query_history.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 旧版本/手编字段缺失时不 panic,靠 default 补齐。
    #[test]
    fn partial_json_falls_back_to_defaults() {
        let e: HistoryEntry = serde_json::from_str(r#"{"sql":"select 1"}"#).unwrap();
        assert_eq!(e.sql, "select 1");
        assert!(e.conn_id.is_empty());
        assert_eq!(e.row_count, None);
    }
}
