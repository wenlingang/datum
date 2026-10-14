//! 结果网格:gpui-kit 的 DataTable + Datum 的单元格分级配色、暂存编辑/删除态、
//! 自定义表头(列名 + 类型 + 排序指示)与行右键菜单。

use super::WorkTab;
use crate::theme::ActivePalette as _;
use crate::ui;
use datum_core::grid::{self, Align, Cell, CellTier};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::table::{Column, TableDelegate, TableState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, ClickEvent, Context, FontWeight, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, WeakEntity,
    Window, div, px,
};
use rust_i18n::t;
use std::cell::Cell as StdCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

/// 自适应列宽的边界与采样。
const AUTO_MIN: f32 = 64.;
const AUTO_MAX: f32 = 420.;
const SAMPLE_ROWS: usize = 120;
const CELL_PAD: f32 = 32.;
const SORT_ICON_SPACE: f32 = 20.;
const DEFAULT_W: f32 = 170.;

/// 网格要展示的数据快照(由 WorkTab 在结果/筛选/暂存变化时整体替换)。
#[derive(Default, Clone)]
pub struct GridData {
    pub columns: Vec<String>,
    pub rows: Arc<Vec<Vec<Cell>>>,
    /// 当前视图行(筛选后)在 rows 中的下标
    pub view: Arc<Vec<usize>>,
    pub types: HashMap<String, String>,
    pub pk_index: Option<usize>,
    /// 已暂存编辑的单元格:(主键值, 列名)
    pub dirty: HashSet<(String, String)>,
    /// 已暂存删除的行(主键值)
    pub deleted: HashSet<String>,
    /// 当前排序:(列名, 是否倒序)
    pub sort: Option<(String, bool)>,
    /// 允许删除行(表浏览且有主键)
    pub deletable: bool,
}

pub struct ResultDelegate {
    pub data: GridData,
    widths: Vec<f32>,
    tab: WeakEntity<WorkTab>,
    /// 最近一次右键的列(行菜单需要知道点在哪一格)
    context_col: Rc<StdCell<usize>>,
}

/// 按字符估算文本宽度:CJK 等宽字符按两格计。
fn text_width(text: &str, px_per_char: f32) -> f32 {
    text.chars()
        .take(80)
        .map(|c| if c.is_ascii() { 1. } else { 1.9 })
        .sum::<f32>()
        * px_per_char
}

impl ResultDelegate {
    pub fn new(tab: WeakEntity<WorkTab>) -> Self {
        Self {
            data: GridData::default(),
            widths: vec![],
            tab,
            context_col: Rc::new(StdCell::new(0)),
        }
    }

    /// 换数据:按内容估算列宽(表头与前若干行采样取最大),再按容器宽度铺满。
    pub fn set_data(&mut self, data: GridData, container_w: f32) {
        let autos: Vec<f32> = data
            .columns
            .iter()
            .enumerate()
            .map(|(ci, col)| {
                let mut w = (text_width(col, 7.6) + SORT_ICON_SPACE)
                    .max(data.types.get(col).map_or(0., |t| text_width(t, 6.6)));
                for &ri in data.view.iter().take(SAMPLE_ROWS) {
                    let v = data.rows[ri].get(ci).cloned().flatten();
                    w = w.max(text_width(v.as_deref().unwrap_or("NULL"), 7.4));
                }
                (w + CELL_PAD).clamp(AUTO_MIN, AUTO_MAX)
            })
            .collect();
        let flexible: Vec<bool> = data
            .columns
            .iter()
            .map(|c| grid::col_align(data.types.get(c).map(String::as_str)) == Align::Left)
            .collect();
        self.widths = grid::column_widths(&autos, container_w, DEFAULT_W, &flexible);
        self.data = data;
    }

    fn row(&self, view_ix: usize) -> Option<&Vec<Cell>> {
        self.data
            .view
            .get(view_ix)
            .and_then(|ri| self.data.rows.get(*ri))
    }

    fn pk_value(&self, view_ix: usize) -> Option<String> {
        let pk = self.data.pk_index?;
        Some(
            self.row(view_ix)?
                .get(pk)
                .cloned()
                .flatten()
                .unwrap_or_default(),
        )
    }

    fn tier_color(tier: CellTier, cx: &App) -> Hsla {
        let p = cx.palette();
        match tier {
            CellTier::Strong => p.cell_strong,
            CellTier::Normal => p.cell,
            CellTier::Dim => p.cell_dim,
            CellTier::Hash => p.cell_hash,
            CellTier::Null => p.cell_null,
            CellTier::Negative => p.negative,
        }
    }
}

impl TableDelegate for ResultDelegate {
    fn columns_count(&self, _: &App) -> usize {
        self.data.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.data.view.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let name = self.data.columns[col_ix].clone();
        let col = Column::new(SharedString::from(format!("{col_ix}:{name}")), name)
            .width(px(self.widths.get(col_ix).copied().unwrap_or(DEFAULT_W)))
            .min_width(px(48.))
            .max_width(px(2400.))
            .movable(false)
            .p_0();
        match grid::col_align(
            self.data
                .types
                .get(&self.data.columns[col_ix])
                .map(String::as_str),
        ) {
            Align::Right => col.text_right(),
            Align::Center => col.text_center(),
            Align::Left => col,
        }
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let name = self.data.columns[col_ix].clone();
        let ty = self.data.types.get(&name).cloned();
        let align = grid::col_align(ty.as_deref());
        let sorted = self
            .data
            .sort
            .as_ref()
            .filter(|(c, _)| *c == name)
            .map(|(_, desc)| *desc);
        let tab = self.tab.clone();
        let col_name = name.clone();
        h_flex()
            .id(("th", col_ix))
            .size_full()
            .px_3()
            .gap_1p5()
            .justify_between()
            .cursor_pointer()
            .on_click(move |_, window, cx| {
                let col = col_name.clone();
                let _ = tab.update(cx, |tab, cx| tab.cycle_sort(&col, window, cx));
            })
            .child(
                v_flex()
                    .min_w_0()
                    .flex_1()
                    .gap_0p5()
                    .map(|this| match align {
                        Align::Right => this.items_end(),
                        Align::Center => this.items_center(),
                        Align::Left => this.items_start(),
                    })
                    .child(
                        div()
                            .max_w_full()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_sm()
                            .font_weight(FontWeight::BOLD)
                            .text_color(cx.theme().foreground)
                            .child(name),
                    )
                    .when_some(ty, |this, ty| {
                        this.child(
                            div()
                                .max_w_full()
                                .overflow_hidden()
                                .text_ellipsis()
                                .text_size(px(9.5))
                                .font_family(cx.theme().mono_font_family.clone())
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(cx.palette().fg_faint)
                                .child(ty.to_uppercase()),
                        )
                    }),
            )
            .when_some(sorted, |this, desc| {
                this.child(
                    ui::icon(if desc { "chevron-down" } else { "chevron-up" })
                        .small()
                        .text_color(cx.palette().data),
                )
            })
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let value: Option<String> = self
            .row(row_ix)
            .and_then(|r| r.get(col_ix).cloned().flatten());
        let col = &self.data.columns[col_ix];
        let ty = self.data.types.get(col).map(String::as_str);
        let style = grid::classify_cell(value.as_deref(), ty);
        let align = grid::col_align(ty);
        let pk = self.pk_value(row_ix);
        let deleted = pk.as_ref().is_some_and(|pk| self.data.deleted.contains(pk));
        let dirty = !deleted
            && pk
                .as_ref()
                .is_some_and(|pk| self.data.dirty.contains(&(pk.clone(), col.clone())));
        let single = self.data.columns.len() == 1;
        let palette = *cx.palette();
        let is_null = value.is_none();
        let text: SharedString = value
            .map(|v| {
                // 多行值在网格里压成一行(双击看全文)
                if single {
                    v
                } else {
                    v.replace(['\n', '\r'], " ")
                }
            })
            .unwrap_or_else(|| "NULL".into())
            .into();

        let tab = self.tab.clone();
        let context_col = self.context_col.clone();
        div()
            .id(("td", row_ix * 4096 + col_ix))
            .size_full()
            .px_3()
            .flex()
            .items_center()
            .overflow_hidden()
            .map(|this| match align {
                Align::Right => this.justify_end(),
                Align::Center => this.justify_center(),
                Align::Left => this,
            })
            .when(deleted, |this| {
                this.bg(palette.delete_bg).line_through().opacity(0.55)
            })
            .when(dirty, |this| {
                this.bg(palette.dirty_bg)
                    .border_1()
                    .border_color(cx.theme().primary)
            })
            .text_color(Self::tier_color(style.tier, cx))
            .when(style.mono, |this| {
                this.font_family(cx.theme().mono_font_family.clone())
            })
            .when(is_null, |this| {
                this.text_size(px(10.5)).font_weight(FontWeight::SEMIBOLD)
            })
            .when(!is_null, |this| this.text_sm())
            .on_mouse_down(MouseButton::Right, move |_, _, _| context_col.set(col_ix))
            .on_click(move |e: &ClickEvent, window, cx| {
                if e.click_count() >= 2 {
                    let _ = tab.update(cx, |tab, cx| tab.open_cell(row_ix, col_ix, window, cx));
                }
            })
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(text),
            )
    }

    fn context_menu(
        &mut self,
        row_ix: usize,
        menu: PopupMenu,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let col_ix = self.context_col.get();
        let editable = self.data.pk_index.is_some();
        let tab = self.tab.clone();
        let item =
            |label: String, f: Rc<dyn Fn(&mut WorkTab, &mut Window, &mut Context<WorkTab>)>| {
                let tab = tab.clone();
                PopupMenuItem::new(label).on_click(move |_, window, cx| {
                    let f = f.clone();
                    let _ = tab.update(cx, |tab, cx| f(tab, window, cx));
                })
            };
        let mut menu = menu
            .item(item(
                t!("grid.view_detail").to_string(),
                Rc::new(move |t, w, cx| t.open_row_detail(row_ix, w, cx)),
            ))
            .item(item(
                if editable {
                    t!("grid.edit").to_string()
                } else {
                    t!("grid.view").to_string()
                },
                Rc::new(move |t, w, cx| t.open_cell(row_ix, col_ix, w, cx)),
            ))
            .item(item(
                t!("grid.copy_insert").to_string(),
                Rc::new(move |t, w, cx| t.copy_insert_row(row_ix, w, cx)),
            ));
        if editable && self.data.deletable {
            let deleted = self
                .pk_value(row_ix)
                .is_some_and(|pk| self.data.deleted.contains(&pk));
            menu = menu.separator().item(item(
                if deleted {
                    t!("grid.undo_delete").to_string()
                } else {
                    t!("grid.delete_row").to_string()
                },
                Rc::new(move |t, _, cx| t.toggle_delete(row_ix, cx)),
            ));
        }
        menu
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        h_flex()
            .size_full()
            .justify_center()
            .text_sm()
            .text_color(cx.palette().fg_faint)
            .child(t!("grid.no_rows"))
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        self.row(row_ix)
            .and_then(|r| r.get(col_ix).cloned().flatten())
            .unwrap_or_default()
    }
}
