//! MongoDB:连接串处理、mongosh 子集解析、BSON 展示转换与驱动交互。

pub mod client;
pub mod commands;
pub mod convert;
pub mod parse;
pub mod uri;

/// app 层通过这里使用 BSON 类型,不必单独依赖 mongodb。
pub use mongodb::bson;
