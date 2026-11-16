//! 测试数据生成:按列的类型、约束与列名语义自动挑生成器,逐行造出「看起来真实」的数据,
//! 遵守非空、长度 / 精度、唯一、外键(取父表已有的键)、枚举与 CHECK 清单;
//! 同一个种子造出同样的数据。
//!
//! - [`meta`]——表结构里与造数有关的部分(只读查询 + 纯函数解析)
//! - [`infer`]——自动方案;[`gen`]——各生成器;[`engine`]——逐行生成与约束处理
//! - [`job`]——后台插入(批量、事务、进度、取消);[`api`]——界面调用的入口;[`ai`]——让 AI 按表结构建议生成器

pub mod ai;
pub mod api;
pub mod engine;
pub mod gen;
pub mod infer;
pub mod job;
pub mod meta;
pub mod plan;
pub mod rng;
pub mod words;

pub use engine::{Context, Engine};
pub use gen::{Generator, Locale, Value};
pub use meta::{BaseType, ColumnMeta, TableMeta};
pub use plan::{ColumnPlan, Plan};
