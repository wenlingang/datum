//! 关系图导出:按节点包围盒(与当前缩放无关)生成 SVG,再用 resvg 栅格化为 PNG。

use crate::theme::ActivePalette as _;
use datum_core::erd::{self, ErdGraph, NODE_HEADER_H, NODE_PAD_Y, NODE_ROW_H, NodeRect};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{App, Hsla, Rgba};
use rust_i18n::t;
use std::collections::HashMap;
use std::fmt::Write as _;

/// 导出四周留白。
const PAD: f32 = 40.;
/// 像素密度。
const PIXEL_RATIO: f32 = 2.;
/// 单边上限(物理像素),超限时两边等比缩小。
const MAX_SIDE: f32 = 8192.;

fn hex(c: Hsla) -> String {
    let rgba = Rgba::from(c);
    format!(
        "#{:02x}{:02x}{:02x}",
        (rgba.r * 255.).round() as u8,
        (rgba.g * 255.).round() as u8,
        (rgba.b * 255.).round() as u8
    )
}

fn opacity(c: Hsla) -> f32 {
    Rgba::from(c).a
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 生成整图 SVG(颜色取当前主题)。
pub fn to_svg(graph: &ErdGraph, pos: &HashMap<String, NodeRect>, cx: &App) -> String {
    let palette = *cx.palette();
    let theme = cx.theme();
    let (x0, y0) = pos
        .values()
        .fold((f32::MAX, f32::MAX), |(x, y), r| (x.min(r.x), y.min(r.y)));
    let (x1, y1) = pos.values().fold((f32::MIN, f32::MIN), |(x, y), r| {
        (x.max(r.x + r.w), y.max(r.y + r.h))
    });
    let (w, h) = (x1 - x0 + PAD * 2., y1 - y0 + PAD * 2.);
    let (ox, oy) = (PAD - x0, PAD - y0);
    let font = "PingFang SC, Helvetica Neue, Arial, sans-serif";
    let mono = "SF Mono, Menlo, Monaco, monospace";

    let mut svg = String::new();
    let _ = write!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}"><rect width="100%" height="100%" fill="{}"/>"#,
        hex(palette.editor)
    );
    let edge_color = hex(palette.fg_faint);
    for e in &graph.edges {
        let (Some(s), Some(t)) = (pos.get(&e.source), pos.get(&e.target)) else {
            continue;
        };
        let pts = erd::route_edge(s, t);
        let d = pts
            .iter()
            .enumerate()
            .map(|(i, (x, y))| format!("{}{} {}", if i == 0 { "M" } else { "L" }, x + ox, y + oy))
            .collect::<Vec<_>>()
            .join(" ");
        let dash = if e.inferred {
            r#" stroke-dasharray="6 4""#
        } else {
            ""
        };
        let _ = write!(
            svg,
            r#"<path d="{d}" fill="none" stroke="{edge_color}" stroke-width="1.4"{dash}/>"#
        );
        let (tx, ty) = pts[pts.len() - 1];
        let (tx, ty) = (tx + ox, ty + oy);
        let _ = write!(
            svg,
            r#"<path d="M{tx} {ty} L{} {} L{} {} Z" fill="{edge_color}"/>"#,
            tx - 7.,
            ty - 4.2,
            tx - 7.,
            ty + 4.2
        );
    }
    for n in &graph.nodes {
        let Some(r) = pos.get(&n.table) else { continue };
        let (x, y) = (r.x + ox, r.y + oy);
        let _ = write!(
            svg,
            r#"<g><rect x="{x}" y="{y}" width="{}" height="{}" rx="8" fill="{}" stroke="{}" stroke-opacity="{}"/>"#,
            r.w,
            r.h,
            hex(palette.panel),
            hex(theme.border),
            opacity(theme.border).max(0.3)
        );
        let _ = write!(
            svg,
            r#"<path d="M{x} {} a8 8 0 0 1 8 -8 h{} a8 8 0 0 1 8 8 v{} h-{} z" fill="{}"/>"#,
            y + 8.,
            r.w - 16.,
            NODE_HEADER_H - 8.,
            r.w,
            hex(palette.raised)
        );
        let _ = write!(
            svg,
            r#"<text x="{}" y="{}" font-family="{mono}" font-size="13" font-weight="600" fill="{}">{}</text>"#,
            x + 10.,
            y + NODE_HEADER_H / 2. + 4.5,
            hex(theme.foreground),
            esc(&n.table)
        );
        let mut ry = y + NODE_HEADER_H + NODE_PAD_Y;
        for c in &n.columns {
            let mark = if c.is_pk {
                "🔑"
            } else if c.is_fk {
                "↗"
            } else {
                ""
            };
            let mark_color = if c.is_pk {
                hex(palette.data)
            } else {
                hex(palette.fg_faint)
            };
            let base = ry + NODE_ROW_H / 2. + 4.;
            if !mark.is_empty() {
                let _ = write!(
                    svg,
                    r#"<text x="{}" y="{base}" font-family="{font}" font-size="10" fill="{mark_color}">{mark}</text>"#,
                    x + 10.
                );
            }
            let _ = write!(
                svg,
                r#"<text x="{}" y="{base}" font-family="{mono}" font-size="12" fill="{}">{}</text>"#,
                x + 28.,
                hex(theme.foreground),
                esc(&c.name)
            );
            let _ = write!(
                svg,
                r#"<text x="{}" y="{base}" text-anchor="end" font-family="{font}" font-size="11" fill="{}">{}</text>"#,
                x + r.w - 10.,
                hex(theme.muted_foreground),
                esc(&c.data_type)
            );
            ry += NODE_ROW_H;
        }
        if n.hidden_count > 0 {
            let _ = write!(
                svg,
                r#"<text x="{}" y="{}" font-family="{font}" font-size="11" fill="{}">{}</text>"#,
                x + 10.,
                ry + NODE_ROW_H / 2. + 4.,
                hex(palette.fg_faint),
                esc(&t!("erd.more_columns", n = n.hidden_count))
            );
        }
        svg.push_str("</g>");
    }
    svg.push_str("</svg>");
    svg
}

/// SVG → PNG(2x;超限时等比缩小)。在后台线程调用。
pub fn rasterize(svg: &str) -> Result<Vec<u8>, String> {
    let mut opt = resvg::usvg::Options::default();
    opt.fontdb_mut().load_system_fonts();
    let tree = resvg::usvg::Tree::from_str(svg, &opt).map_err(|e| e.to_string())?;
    let size = tree.size();
    let scale = PIXEL_RATIO
        .min(MAX_SIDE / size.width())
        .min(MAX_SIDE / size.height());
    let (w, h) = (
        (size.width() * scale).ceil() as u32,
        (size.height() * scale).ceil() as u32,
    );
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w.max(1), h.max(1))
        .ok_or_else(|| t!("erd.canvas_failed").to_string())?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().map_err(|e| e.to_string())
}
