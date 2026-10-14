//! GPUI 与 tokio 之间的桥:数据库驱动(tokio-postgres / mysql_async / redis / reqwest)
//! 需要 tokio 反应器,统一丢到全局 tokio 运行时执行,GPUI 侧 await 其 JoinHandle。

use datum_core::AppState;
use std::future::Future;
use std::sync::{Arc, OnceLock};

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
static STATE: OnceLock<Arc<AppState>> = OnceLock::new();

pub fn init() {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("datum-backend")
            .enable_all()
            .build()
            .expect("无法创建 tokio 运行时")
    });
    // DATUM_CONFIG_DIR:开发/截图时指向独立的配置目录,不碰用户真实配置
    let dir = std::env::var_os("DATUM_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(AppState::default_config_dir);
    STATE.get_or_init(|| Arc::new(AppState::new(dir)));
}

/// 全局数据层状态(配置目录、连接池)。
pub fn state() -> Arc<AppState> {
    STATE.get().expect("runtime::init 未调用").clone()
}

/// 在 tokio 运行时上执行一个异步任务,返回可在任意执行器上 await 的结果。
pub fn spawn<F, T>(fut: F) -> impl Future<Output = T> + Send + 'static
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let handle = RUNTIME.get().expect("runtime::init 未调用").spawn(fut);
    async move { handle.await.expect("后台任务异常退出") }
}

/// 便捷封装:把 `Arc<AppState>` 交给闭包构造的异步任务。
pub fn backend<F, Fut, T>(f: F) -> impl Future<Output = T> + Send + 'static
where
    F: FnOnce(Arc<AppState>) -> Fut,
    Fut: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    spawn(f(state()))
}
