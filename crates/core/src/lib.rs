//! Datum 的数据层:连接配置、凭据、查询历史、各数据库驱动、AI 与自更新。
//!
//! 与 UI 无关,所有入口都是普通(异步)函数,第一个参数是 [`AppState`]。
//! 异步函数依赖 tokio 运行时(tokio-postgres / redis / mysql_async / reqwest)。

// 界面语言由 app 层通过 rust_i18n::set_locale 设置(全局共享),这里只登记 core 自己的文案。
rust_i18n::i18n!("locales", fallback = "en");

pub mod ai;
pub mod commands;
pub mod completion;
pub mod core;
pub mod erd;
pub mod error;
pub mod grid;
pub mod my;
pub mod pg;
pub mod rds;
pub mod schema;
pub mod service;
pub mod sql;
pub mod sqlite;
pub mod update;

pub use commands::AppState;
