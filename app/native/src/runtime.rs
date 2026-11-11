//! 全局数据层状态与 JSON 边界的小工具。
//!
//! 数据库驱动(tokio-postgres / mysql_async / redis / reqwest)需要 tokio 反应器:
//! nsh 的引擎本身跑在 tokio 运行时里,异步导出函数用 `background` 把工作丢到它的线程池上,
//! 不占引擎线程。

use datum_core::AppState;
use datum_core::error::AppError;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::future::Future;
use std::sync::{Arc, OnceLock};

static STATE: OnceLock<Arc<AppState>> = OnceLock::new();

pub fn init() {
    STATE.get_or_init(|| {
        // DATUM_CONFIG_DIR:开发/测试时指向独立的配置目录,不碰用户真实配置
        let dir = std::env::var_os("DATUM_CONFIG_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(AppState::default_config_dir);
        Arc::new(AppState::new(dir))
    });
}

/// 全局数据层状态(配置目录、连接池)。
pub fn state() -> Arc<AppState> {
    init();
    STATE.get().expect("STATE 已初始化").clone()
}

type JsResult = Result<Value, String>;

/// core 错误 → 给用户看的文案。
pub fn err(e: AppError) -> String {
    e.display()
}

/// 序列化结果;序列化失败(理论上不会发生)也以错误返回。
pub fn json<T: Serialize>(v: T) -> JsResult {
    serde_json::to_value(v).map_err(|e| e.to_string())
}

/// 反序列化 TSX 传来的参数。
pub fn from<T: DeserializeOwned>(v: Value, what: &str) -> Result<T, String> {
    serde_json::from_value(v).map_err(|e| format!("{what}: {e}"))
}

/// 在 tokio 线程池上执行(驱动需要),结果回到调用方的任务。
pub async fn spawn<F, T>(fut: F) -> Result<T, String>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    background(fut).await.map_err(|e| e.to_string())
}

use nsh_core::prelude::background;
