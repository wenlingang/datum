//! Datum 的原生模块(`datum/native`):把 datum-core 的数据层暴露给 TSX 界面。
//!
//! 边界约定:跨边界的数据一律是 JSON(core 的类型本就带 camelCase 的 serde 标注,
//! 与 Tauri 时期前端看到的形状一致),TSX 侧在 `app/lib/types.ts` 里声明对应类型。
//! 失败一律以 `Err(String)` 抛给 TSX(core 错误用 `AppError::display()` 的文案)。

mod ai;
mod conn;
mod db;
mod dialogs;
mod prefs;
mod runtime;
mod shell;
mod text;

use nsh_core::prelude::*;

pub use ai::Ai;
pub use conn::Store;
pub use db::Db;
pub use dialogs::Dialogs;
pub use prefs::Prefs;
pub use shell::Shell;
pub use text::Sql;

/// Datum 的原生模块:界面通过 `datum/native` 导入。
#[native_module("datum/native")]
fn native(registry: &mut Registry) {
    runtime::init();
    registry
        .module::<Store>()
        .module::<Db>()
        .module::<Prefs>()
        .module::<Sql>()
        .module::<Ai>()
        .module::<Shell>()
        .module::<Dialogs>();
}
