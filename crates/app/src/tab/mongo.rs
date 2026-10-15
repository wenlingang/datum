//! MongoDB 的 tab 逻辑:集合浏览(过滤栏 + 服务端分页)、查询执行与文档操作。
//! 结果仍以 `QueryResult` 形态交给网格;`docs` / `ids` 与网格行按下标一一对应。

use super::mongo_filter::FilterBar;
use super::{TabEvent, WorkTab};
use crate::dialogs::risk::{RiskGate, confirm_destructive};
use crate::runtime;
use crate::store::Toast;
use datum_core::error::AppError;
use datum_core::mongo::bson::{Bson, Document};
use datum_core::mongo::convert::{MongoResult, to_shell};
use datum_core::mongo::parse::{coll_ref, parse_document, parse_script};
use datum_core::pg::client::QueryResult;
use datum_core::service::{self, FindPage};
use datum_core::sql::risk::{RiskFinding, RiskLevel, analyze_mongo};
use gpui_kit::{AsyncWindowContext, Context, Subscription, Window};
use rust_i18n::t;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

pub const PAGE_SIZES: [usize; 3] = [50, 100, 500];

#[derive(Default)]
pub struct MongoTabState {
    /// 集合 tab 的集合名;查询 tab 为 None
    pub coll: Option<String>,
    pub bar: Option<FilterBar>,
    pub page: usize,
    pub page_size: usize,
    /// (数量, 是否精确)
    pub total: Option<(u64, bool)>,
    pub docs: Arc<Vec<String>>,
    pub ids: Arc<Vec<Option<Bson>>>,
    pub types: HashMap<String, String>,
    pub json_view: bool,
    pub truncated: bool,
    pub summary: Option<String>,
    pub bar_error: Option<String>,
}

pub fn permute<T: Clone>(v: &[T], order: &[usize]) -> Vec<T> {
    order.iter().filter_map(|&i| v.get(i).cloned()).collect()
}

/// 新建 tab 时的 Mongo 状态;集合 tab 带过滤栏。
pub(super) fn init(
    coll: Option<&str>,
    window: &mut Window,
    cx: &mut Context<WorkTab>,
) -> (MongoTabState, Vec<Subscription>) {
    let (bar, subs) = match coll {
        Some(_) => {
            let (bar, subs) = FilterBar::new(window, cx);
            (Some(bar), subs)
        }
        None => (None, vec![]),
    };
    (
        MongoTabState {
            coll: coll.map(str::to_string),
            bar,
            page_size: 100,
            ..Default::default()
        },
        subs,
    )
}

impl WorkTab {
    /// 视图行 → (文档文本, `_id`)。
    pub fn mongo_doc_at(&self, view_ix: usize) -> Option<(String, Option<Bson>)> {
        let m = self.mongo.as_ref()?;
        let ri = *self.view.get(view_ix)?;
        Some((m.docs.get(ri)?.clone(), m.ids.get(ri)?.clone()))
    }

    pub(super) fn set_mongo_result(&mut self, r: MongoResult, cx: &mut Context<Self>) {
        let MongoResult {
            columns,
            rows,
            docs,
            ids,
            types,
            truncated,
            summary,
        } = r;
        let write_only = columns.is_empty() && summary.is_some();
        if let Some(m) = self.mongo.as_mut() {
            m.types = types;
            m.docs = Arc::new(docs);
            m.ids = Arc::new(ids);
            m.truncated = truncated;
            m.summary = summary;
        }
        self.result_is_query = !write_only;
        self.set_result(
            QueryResult {
                columns,
                rows,
                affected: None,
            },
            cx,
        );
    }

    /// 解析过滤栏;失败时把错误显示在栏下方并返回 None。
    fn mongo_read_bar(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<(Document, Option<Document>, Option<Document>)> {
        let bar = self.mongo.as_ref()?.bar.as_ref()?;
        let read = |e: &gpui_kit::Entity<gpui_kit::component::input::InputState>| {
            e.read(cx).value().to_string()
        };
        let (f, s, p) = (read(&bar.filter), read(&bar.sort), read(&bar.projection));
        let parse = |label: &str, src: &str| {
            parse_document(src).map_err(|e| format!("{label}: {}", e.display()))
        };
        let result = parse("Filter", &f).and_then(|filter| {
            let sort = parse("Sort", &s)?;
            let proj = parse("Projection", &p)?;
            Ok((
                filter,
                (!sort.is_empty()).then_some(sort),
                (!proj.is_empty()).then_some(proj),
            ))
        });
        let m = self.mongo.as_mut()?;
        match result {
            Ok(v) => {
                m.bar_error = None;
                Some(v)
            }
            Err(e) => {
                m.bar_error = Some(e);
                cx.notify();
                None
            }
        }
    }

    /// 截图脚本用:写入 Projection 并应用。
    #[cfg(feature = "snapshot")]
    pub fn snapshot_mongo_projection(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(bar) = self.mongo.as_ref().and_then(|m| m.bar.as_ref()) {
            let text = text.to_string();
            bar.projection
                .update(cx, |s, cx| s.set_value(text, window, cx));
        }
        self.mongo_apply_filter(window, cx);
    }

    /// 截图脚本用:写入 Filter 并应用。
    #[cfg(feature = "snapshot")]
    pub fn snapshot_mongo_filter(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(bar) = self.mongo.as_ref().and_then(|m| m.bar.as_ref()) {
            let text = text.to_string();
            bar.filter.update(cx, |s, cx| s.set_value(text, window, cx));
        }
        self.mongo_apply_filter(window, cx);
    }

    pub fn mongo_apply_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(m) = self.mongo.as_mut() {
            m.total = None;
        }
        self.mongo_goto_page(0, window, cx);
    }

    pub fn mongo_reset_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(bar) = self.mongo.as_ref().and_then(|m| m.bar.as_ref()) {
            for e in [&bar.filter, &bar.sort, &bar.projection] {
                e.update(cx, |s, cx| s.set_value("", window, cx));
            }
        }
        self.sort = None;
        self.mongo_apply_filter(window, cx);
    }

    pub(super) fn mongo_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let page = self.mongo.as_ref().map_or(0, |m| m.page);
        self.mongo_goto_page(page, window, cx);
    }

    pub fn mongo_goto_page(&mut self, page: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(coll) = self.mongo.as_ref().and_then(|m| m.coll.clone()) else {
            return;
        };
        let Some((filter, sort, projection)) = self.mongo_read_bar(cx) else {
            return;
        };
        let page_size = self.mongo.as_ref().map_or(100, |m| m.page_size);
        // 无过滤条件时顺带取估算总数;有条件时由用户点「计数」
        let need_total =
            filter.is_empty() && self.mongo.as_ref().is_some_and(|m| m.total.is_none());
        let id = self.conn_id.clone();
        let req = FindPage {
            filter,
            sort,
            projection,
            skip: (page * page_size) as u64,
            limit: page_size as i64,
        };
        self.running = true;
        cx.notify();
        let started = Instant::now();
        self._task = Some(
            cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
                let result = runtime::backend(move |s| async move {
                    let r = service::mongo_find_page(&s, &id, &coll, req).await?;
                    let total = if need_total {
                        Some(service::mongo_count(&s, &id, &coll, None).await?)
                    } else {
                        None
                    };
                    Ok::<_, AppError>((r, total))
                })
                .await;
                let elapsed = started.elapsed().as_millis();
                let _ = this.update(cx, |this, cx| {
                    this.running = false;
                    match result {
                        Ok((r, total)) => {
                            this.err = None;
                            this.elapsed_ms = Some(elapsed);
                            if let Some(m) = this.mongo.as_mut() {
                                m.page = page;
                                if total.is_some() {
                                    m.total = total;
                                }
                            }
                            this.set_mongo_result(r, cx);
                        }
                        Err(e) => {
                            this.err = Some(e.display());
                            cx.notify();
                        }
                    }
                });
            }),
        );
    }

    /// 有过滤条件时手动精确计数(大集合上可能很慢,所以不自动执行)。
    pub fn mongo_count(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(coll) = self.mongo.as_ref().and_then(|m| m.coll.clone()) else {
            return;
        };
        let Some((filter, _, _)) = self.mongo_read_bar(cx) else {
            return;
        };
        let id = self.conn_id.clone();
        cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            let r = runtime::backend(move |s| async move {
                service::mongo_count(&s, &id, &coll, Some(filter)).await
            })
            .await;
            let _ = this.update(cx, |this, cx| match r {
                Ok(n) => {
                    if let Some(m) = this.mongo.as_mut() {
                        m.total = Some(n);
                    }
                    cx.notify();
                }
                Err(e) => cx.emit(TabEvent::Toast(Toast::Error(e.display().into()))),
            });
        })
        .detach();
    }

    /// 点表头:集合 tab 写入 Sort 栏并服务端重查(倒序 → 升序 → 无);查询 tab 客户端排序,
    /// 行、文档文本、`_id` 一起重排,保证编辑 / 删除作用在点中的那一篇上。
    pub(super) fn mongo_cycle_sort(
        &mut self,
        col: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(bar) = self.mongo.as_ref().and_then(|m| m.bar.as_ref()) {
            let next = match &self.sort {
                Some((c, true)) if c == col => Some((col.to_string(), false)),
                Some((c, false)) if c == col => None,
                _ => Some((col.to_string(), true)),
            };
            let text = match &next {
                // 字段名一律加引号,嵌套字段(a.b)也能正确解析
                Some((c, desc)) => format!(
                    "{{ {}: {} }}",
                    to_shell(&Bson::String(c.clone()), false),
                    if *desc { -1 } else { 1 }
                ),
                None => String::new(),
            };
            bar.sort.update(cx, |s, cx| s.set_value(text, window, cx));
            self.sort = next;
            self.mongo_goto_page(0, window, cx);
            return;
        }
        let Some(result) = self.result.clone() else {
            return;
        };
        let Some(ci) = result.columns.iter().position(|c| c == col) else {
            return;
        };
        let desc = matches!(&self.sort, Some((c, false)) if c == col);
        let order = datum_core::grid::sort_order(&result.rows, ci, desc);
        if let Some(m) = self.mongo.as_mut() {
            m.docs = Arc::new(permute(&m.docs, &order));
            m.ids = Arc::new(permute(&m.ids, &order));
        }
        self.sort = Some((col.to_string(), desc));
        self.set_result(
            QueryResult {
                columns: result.columns.clone(),
                rows: permute(&result.rows, &order),
                affected: None,
            },
            cx,
        );
    }
}

impl WorkTab {
    /// `view_ix` 为 None 表示新增。只有集合 tab 能编辑。
    /// 编辑时先按 `_id` 重新取完整文档:当前页可能带了 Projection,
    /// 拿页面上的文档去 replaceOne 会把没查出来的字段全部删掉。
    pub fn open_doc_editor(
        &mut self,
        view_ix: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(coll) = self.mongo.as_ref().and_then(|m| m.coll.clone()) else {
            return;
        };
        let target = crate::dialogs::doc_editor::DocTarget {
            tab: cx.entity().downgrade(),
            conn_id: self.conn_id.clone(),
            coll: coll.clone(),
        };
        let Some(ix) = view_ix else {
            crate::dialogs::doc_editor::open(target, "{\n  \n}".into(), None, window, cx);
            return;
        };
        let Some((_, Some(doc_id))) = self.mongo_doc_at(ix) else {
            cx.emit(TabEvent::Toast(Toast::Error(
                t!("mongo.need_id_hint").to_string().into(),
            )));
            return;
        };
        let id = self.conn_id.clone();
        let fetch_id = doc_id.clone();
        cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            let r = runtime::backend(move |s| async move {
                service::mongo_find_by_id(&s, &id, &coll, fetch_id).await
            })
            .await;
            let _ = this.update_in(cx, |_, window, cx| match r {
                Ok(Some(text)) => {
                    crate::dialogs::doc_editor::open(target, text, Some(doc_id), window, cx)
                }
                Ok(None) => cx.emit(TabEvent::Toast(Toast::Error(
                    t!("mongo.doc_gone").to_string().into(),
                ))),
                Err(e) => cx.emit(TabEvent::Toast(Toast::Error(e.display().into()))),
            });
        })
        .detach();
    }

    /// 删除文档:任何环境都先确认。
    pub fn confirm_delete_doc(
        &mut self,
        view_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(coll) = self.mongo.as_ref().and_then(|m| m.coll.clone()) else {
            return;
        };
        let Some((_, Some(doc_id))) = self.mongo_doc_at(view_ix) else {
            cx.emit(TabEvent::Toast(Toast::Error(
                t!("mongo.need_id_hint").to_string().into(),
            )));
            return;
        };
        let stmt = format!(
            "{}.deleteOne({{ _id: {} }})",
            coll_ref(&coll),
            to_shell(&doc_id, false)
        );
        let conn = self.connection(cx);
        let this = cx.entity().downgrade();
        confirm_destructive(
            RiskGate {
                findings: vec![RiskFinding {
                    sql: stmt,
                    level: RiskLevel::Warning,
                    kind: "deleteOne".into(),
                    object: Some(coll.clone()),
                }],
                target_label: conn.as_ref().map(|c| c.name.clone()).unwrap_or_default(),
                database: conn.map(|c| c.database).filter(|d| !d.is_empty()),
                dialect: None,
            },
            move |window, cx| {
                let (coll, doc_id) = (coll.clone(), doc_id.clone());
                let _ = this.update(cx, |t, cx| t.delete_doc(coll, doc_id, window, cx));
            },
            window,
            cx,
        );
    }

    fn delete_doc(
        &mut self,
        coll: String,
        doc_id: Bson,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.conn_id.clone();
        cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
            let r = runtime::backend(move |s| async move {
                service::mongo_delete_one(&s, &id, &coll, doc_id).await
            })
            .await;
            let _ = this.update_in(cx, |t, window, cx| match r {
                Ok(()) => {
                    cx.emit(TabEvent::Toast(Toast::Success(
                        t!("mongo.deleted").to_string().into(),
                    )));
                    t.selected_row = None;
                    t.refresh(window, cx);
                }
                Err(e) => cx.emit(TabEvent::Toast(Toast::Error(e.display().into()))),
            });
        })
        .detach();
    }
}

impl WorkTab {
    /// 执行查询 tab 里的 mongosh 脚本:解析 → 风险审查 → 只读拦截 → 依次执行。
    pub(super) fn run_mongo(
        &mut self,
        src: String,
        skip_history: bool,
        skip_risk: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stmts = match parse_script(&src) {
            Ok(v) if v.is_empty() => {
                self.err = Some(t!("tab.empty_command").to_string());
                cx.notify();
                return;
            }
            Ok(v) => v,
            Err(e) => {
                self.err = Some(e.display());
                self.result = None;
                self.recompute_view(cx);
                return;
            }
        };
        if !skip_risk {
            let findings = analyze_mongo(&src, &stmts);
            if !findings.is_empty() {
                // 交给用户在确认框里决定,不沿用免确认的只读标记
                self.read_only_next = false;
                let conn = self.connection(cx);
                let this = cx.entity().downgrade();
                let again = src.clone();
                confirm_destructive(
                    RiskGate {
                        findings,
                        target_label: conn.as_ref().map(|c| c.name.clone()).unwrap_or_default(),
                        database: conn.map(|c| c.database).filter(|d| !d.is_empty()),
                        dialect: None,
                    },
                    move |window, cx| {
                        let src = again.clone();
                        let _ = this
                            .update(cx, |t, cx| t.run_mongo(src, skip_history, true, window, cx));
                    },
                    window,
                    cx,
                );
                return;
            }
        }
        let ops: Vec<_> = stmts.into_iter().map(|st| st.op).collect();
        let wrote = ops.iter().any(|o| o.is_write());
        let read_only = std::mem::take(&mut self.read_only_next);
        let (id, max_rows) = (self.conn_id.clone(), Self::max_rows(cx));
        self.running = true;
        cx.notify();
        let started = Instant::now();
        self._task = Some(
            cx.spawn_in(window, async move |this, cx: &mut AsyncWindowContext| {
                let r = runtime::backend(move |s| async move {
                    service::mongo_execute(&s, &id, ops, max_rows, read_only).await
                })
                .await;
                let elapsed = started.elapsed().as_millis();
                let _ = this.update(cx, |this, cx| {
                    this.running = false;
                    match r {
                        Ok(res) => {
                            this.err = None;
                            this.elapsed_ms = Some(elapsed);
                            this.sort = None;
                            this.filter.reset_all();
                            let has_rows = !res.columns.is_empty();
                            let rows = res.rows.len() as u64;
                            this.set_mongo_result(res, cx);
                            // 写操作可能新建集合:刷新侧栏集合列表
                            if wrote {
                                cx.emit(TabEvent::SchemaChanged);
                            }
                            if !skip_history {
                                this.record_history(
                                    &src,
                                    elapsed,
                                    has_rows.then_some(rows),
                                    None,
                                    cx,
                                );
                            }
                        }
                        Err(e) => {
                            this.err = Some(e.display());
                            this.result = None;
                            this.recompute_view(cx);
                        }
                    }
                    cx.notify();
                });
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::permute;

    #[test]
    fn permute_keeps_rows_docs_and_ids_aligned() {
        let rows = vec!["r0", "r1", "r2"];
        let docs = vec!["d0", "d1", "d2"];
        let order = vec![2, 0, 1];
        assert_eq!(permute(&rows, &order), vec!["r2", "r0", "r1"]);
        assert_eq!(permute(&docs, &order), vec!["d2", "d0", "d1"]);
    }
}
