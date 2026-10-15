//! 跨功能复用的小型展示元素:图标、环境徽章、数据库类型图标、中间省略文本。

use crate::theme::ActivePalette as _;
use datum_core::core::connection::{DbKind, Env};
use gpui_kit::component::{ActiveTheme as _, Icon, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Hsla, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _, Window, div,
    px,
};

/// 应用自带的 Lucide 图标(assets/icons/<name>.svg)。
pub fn icon(name: &str) -> Icon {
    Icon::empty().path(SharedString::from(format!("icons/{name}.svg")))
}

/// 数据库品牌图标。
pub fn db_icon(kind: DbKind) -> Icon {
    let name = match kind {
        DbKind::Pg => "db-pg",
        DbKind::Mysql => "db-mysql",
        DbKind::Redis => "db-redis",
        DbKind::Sqlite => "db-sqlite",
        DbKind::Mongo => "db-mongo",
    };
    icon(name)
}

pub fn env_color(env: Env, cx: &App) -> Hsla {
    let p = cx.palette();
    match env {
        Env::Local => p.env_local,
        Env::Staging => p.env_staging,
        Env::Prod => p.env_prod,
    }
}

pub fn env_label(env: Env) -> &'static str {
    match env {
        Env::Local => "LOCAL",
        Env::Staging => "STAGING",
        Env::Prod => "PROD",
    }
}

/// 环境徽章:描边胶囊 + 大写环境名。
#[derive(IntoElement)]
pub struct EnvBadge {
    env: Env,
}

impl EnvBadge {
    pub fn new(env: Env) -> Self {
        Self { env }
    }
}

impl RenderOnce for EnvBadge {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = env_color(self.env, cx);
        div()
            .flex_shrink_0()
            .px_1p5()
            .py(px(1.))
            .rounded_full()
            .border_1()
            .border_color(color)
            .text_color(color)
            .text_size(px(9.))
            .font_weight(gpui_kit::FontWeight::BOLD)
            .child(env_label(self.env))
    }
}

/// 中间省略:头部超宽时截断显示 …,尾部 `tail` 个字符始终可见。
/// 例如 very_long_table_name_suffix → very_long_t…suffix。
#[derive(IntoElement)]
pub struct MiddleEllipsis {
    text: SharedString,
    tail: usize,
}

impl MiddleEllipsis {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            tail: 6,
        }
    }
}

impl RenderOnce for MiddleEllipsis {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let chars: Vec<char> = self.text.chars().collect();
        if chars.len() <= self.tail + 1 {
            return h_flex()
                .min_w_0()
                .child(div().whitespace_nowrap().child(self.text))
                .into_any_element();
        }
        let split = chars.len() - self.tail;
        let head: String = chars[..split].iter().collect();
        let rest: String = chars[split..].iter().collect();
        h_flex()
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .child(
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(head),
            )
            .child(div().flex_shrink_0().whitespace_nowrap().child(rest))
            .into_any_element()
    }
}

/// 竖分隔线(工具条、顶栏内分组)。
pub fn vsep(cx: &App) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .w(px(1.))
        .h(px(16.))
        .bg(cx.palette().hairline)
}

/// 状态小圆点。
pub fn dot(color: Hsla, size: f32) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .size(px(size))
        .rounded_full()
        .bg(color)
}

/// 段落式的提示文本(空态、说明)。
pub fn hint(text: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

/// 小号大写分节标题(设置页、详情栏)。
pub fn section_label(text: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    div()
        .text_xs()
        .font_weight(gpui_kit::FontWeight::BOLD)
        .text_color(cx.palette().fg_faint)
        .child(text.into())
}

/// 列表/导航行:左对齐、悬停高亮、选中态加粗加底色。点击行为由调用方挂 `on_click`。
pub fn nav_row(
    id: impl Into<gpui_kit::ElementId>,
    label: impl Into<SharedString>,
    icon_name: Option<&str>,
    selected: bool,
    cx: &App,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    use gpui_kit::InteractiveElement as _;
    let palette = cx.palette();
    h_flex()
        .id(id.into())
        .h(px(30.))
        .px_2p5()
        .gap_2()
        .rounded(cx.theme().radius)
        .cursor_pointer()
        .text_sm()
        .when(selected, |d| {
            d.bg(palette.raised)
                .font_weight(gpui_kit::FontWeight::BOLD)
                .text_color(cx.theme().foreground)
        })
        .when(!selected, |d| {
            d.text_color(cx.theme().muted_foreground)
                .hover(|s| s.bg(cx.theme().list_hover))
        })
        .when_some(icon_name.map(|n| icon(n)), |d, i| d.child(i.size(px(15.))))
        .child(label.into())
}

/// 品牌三色(靛 → 紫 → 青)的 RGB,与应用图标闭环;亮色主题换深一档保证白底可读。
fn brand_rgb(dark: bool) -> [u32; 3] {
    if dark {
        [0x6366f1, 0xc084fc, 0x22d3ee]
    } else {
        [0x4f46e5, 0x9333ea, 0x0891b2]
    }
}

fn rgba_of(rgb: u32, alpha: f32) -> Hsla {
    let mut c: Hsla = gpui_kit::rgb(rgb).into();
    c.a = alpha;
    c
}

/// 用若干段两色线性渐变拼出多色标渐变(gpui 只支持两个色标)。
/// `stops`:(位置 0..1, 颜色),`mirror` 为 true 时从右往左铺。
fn gradient_strip(stops: &[(f32, Hsla)], mirror: bool) -> gpui_kit::Div {
    div()
        .absolute()
        .inset_0()
        .children(stops.windows(2).map(|w| {
            let ((p0, c0), (p1, c1)) = (w[0], w[1]);
            let left = if mirror { 1. - p1 } else { p0 };
            let (from, to) = if mirror { (c1, c0) } else { (c0, c1) };
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(gpui_kit::relative(left))
                .w(gpui_kit::relative(p1 - p0))
                .bg(gpui_kit::linear_gradient(
                    90.,
                    gpui_kit::linear_color_stop(from, 0.),
                    gpui_kit::linear_color_stop(to, 1.),
                ))
        }))
}

fn brand_stops(cx: &App, line: bool) -> [(f32, Hsla); 4] {
    let dark = cx.theme().is_dark();
    let [indigo, purple, cyan] = brand_rgb(dark);
    if line {
        let a = if dark {
            [0.9, 0.8, 0.7]
        } else {
            [0.55, 0.48, 0.42]
        };
        [
            (0., rgba_of(indigo, a[0])),
            (0.38, rgba_of(purple, a[1])),
            (0.68, rgba_of(cyan, a[2])),
            (0.92, rgba_of(cyan, 0.)),
        ]
    } else {
        let a = if dark {
            [0.14, 0.07, 0.04]
        } else {
            [0.10, 0.06, 0.03]
        };
        [
            (0., rgba_of(indigo, a[0])),
            (0.40, rgba_of(purple, a[1])),
            (0.62, rgba_of(cyan, a[2])),
            (0.78, rgba_of(cyan, 0.)),
        ]
    }
}

/// 顶栏/状态栏的品牌洗色:一侧起 靛→紫→青 渐隐。放在容器(需 `.relative()`)的
/// 第一个子元素,画在内容之下。`mirror`:从右侧起(状态栏)。
pub fn brand_wash(cx: &App, mirror: bool) -> impl IntoElement {
    gradient_strip(&brand_stops(cx, false), mirror)
}

/// 霓虹渐变发丝线,压在顶栏下缘/状态栏上缘的边框上。放在容器的最后一个子元素。
pub fn brand_line(cx: &App, mirror: bool, top: bool) -> impl IntoElement {
    div()
        .absolute()
        .left_0()
        .right_0()
        .h(px(1.))
        .map(|d| if top { d.top_0() } else { d.bottom_0() })
        .child(gradient_strip(&brand_stops(cx, true), mirror))
}
