//! 破坏性语句执行前的审查弹窗:逐条列出命中(风险等级 + 语句),标明目标连接与数据库,
//! 点「确认执行」放行。所有编辑器执行入口共用。

use crate::theme::ActivePalette as _;
use crate::ui;
use datum_core::sql::Dialect;
use datum_core::sql::risk::{RiskFinding, RiskLevel};
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    App, AppContext as _, Entity, FontWeight, ParentElement as _, Styled as _, Window, div, px,
};
use rust_i18n::t;
use std::rc::Rc;

pub struct RiskGate {
    pub findings: Vec<RiskFinding>,
    /// 目标连接展示名
    pub target_label: String,
    /// 当前数据库(SQLite 为文件路径)
    pub database: Option<String>,
    pub dialect: Option<Dialect>,
}

/// 只读的 SQL 展示(带语法高亮)。必须在打开弹窗之前创建,弹窗构建器可能被多次调用。
pub fn sql_view(
    sql: &str,
    dialect: Option<Dialect>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<EditorState> {
    let sql = sql.to_string();
    let lang = if dialect.is_some() { "sql" } else { "text" };
    cx.new(|cx| {
        EditorState::new(window, cx)
            .language(lang)
            .line_number(false)
            .soft_wrap(true)
            .default_value(sql)
    })
}

pub fn confirm_destructive(
    gate: RiskGate,
    on_confirm: impl Fn(&mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let views: Vec<(RiskLevel, Entity<EditorState>, usize)> = gate
        .findings
        .iter()
        .map(|f| {
            (
                f.level,
                sql_view(&f.sql, gate.dialect, window, cx),
                f.sql.lines().count().clamp(1, 8),
            )
        })
        .collect();
    let target = match &gate.database {
        Some(db) => t!("risk.target_db", db = db, count = gate.findings.len()).to_string(),
        None => t!(
            "risk.target_conn",
            target = gate.target_label,
            count = gate.findings.len()
        )
        .to_string(),
    };
    let on_confirm = Rc::new(on_confirm);
    window.open_alert_dialog(cx, move |alert, _, cx| {
        let on_confirm = on_confirm.clone();
        let palette = *cx.palette();
        let body = v_flex()
            .gap_3()
            .child(div().text_sm().child(target.clone()))
            .child(v_flex().gap_2().max_h(px(360.)).children(views.iter().map(
                |(level, view, lines)| {
                    let (label, color) = match level {
                        RiskLevel::Danger => (t!("risk.danger"), cx.theme().danger),
                        RiskLevel::Warning => (t!("risk.warning"), cx.theme().warning),
                    };
                    div()
                        .relative()
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(cx.theme().border)
                        .overflow_hidden()
                        .child(
                            Editor::new(view)
                                .readonly(true)
                                .bordered(false)
                                .h(px(*lines as f32 * 20. + 16.)),
                        )
                        .child(
                            div()
                                .absolute()
                                .top_1()
                                .right_1p5()
                                .px_2()
                                .rounded_full()
                                .bg(color)
                                .text_size(px(10.5))
                                .font_weight(FontWeight::BOLD)
                                .text_color(cx.theme().danger_foreground)
                                .child(label),
                        )
                },
            )))
            .child(
                h_flex()
                    .gap_1p5()
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .text_color(palette.negative)
                    .child(ui::icon("triangle-alert").small())
                    .child(t!("risk.irreversible")),
            );
        alert
            .width(px(560.))
            .icon(ui::icon("triangle-alert").text_color(cx.theme().danger))
            .title(t!("risk.title").to_string())
            .description(body)
            .show_cancel(true)
            .cancel_text(t!("risk.cancel").to_string())
            .ok_text(t!("risk.confirm").to_string())
            .ok_variant(ButtonVariant::Danger)
            .on_ok(move |_, window, cx| {
                on_confirm(window, cx);
                true
            })
    });
}
