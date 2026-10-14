//! 应用资源:自带的图标(Lucide 子集 + 数据库品牌图标)、主题与应用图标,
//! 找不到时回退到 gpui-kit 的默认组件图标。

use gpui_kit::{AssetSource, Result, SharedString};
use rust_embed::RustEmbed;
use std::borrow::Cow;

#[derive(RustEmbed)]
#[folder = "assets"]
#[include = "icons/**/*.svg"]
#[include = "themes/*.json"]
#[include = "*.png"]
struct Embedded;

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(file) = Embedded::get(path) {
            return Ok(Some(file.data));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(
            Embedded::iter()
                .filter(|p| p.starts_with(path))
                .map(|p| SharedString::from(p.to_string())),
        );
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

/// 读取内嵌的文本资源(主题 JSON 等)。
pub fn text(path: &str) -> Option<String> {
    Embedded::get(path).map(|f| String::from_utf8_lossy(&f.data).into_owned())
}
