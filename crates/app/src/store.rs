//! 应用级数据模型:连接与其懒加载缓存、查询历史、AI 会话、应用更新。
//!
//! 每个模型是一个 GPUI 实体,通过 [`Stores`] 全局拿到句柄;视图订阅它们的变化。
//! 后台 IO 一律经 [`crate::runtime`] 跑在 tokio 上,结果回到前台再更新实体。

use crate::runtime;
use datum_core::ai::chat::{ChatEvent, ChatMessage};
use datum_core::ai::commands::{self as ai_cmd, AiStatus, KnowledgeDoc};
use datum_core::ai::settings::{AiSettings, ModelRef, ProviderConfig};
use datum_core::commands as core_cmd;
use datum_core::core::connection::{Connection, DbKind};
use datum_core::core::history::HistoryEntry;
use datum_core::error::AppResult;
use datum_core::schema::{ColumnsByTable, ErdSchema};
use datum_core::service;
use datum_core::update::{self, Manifest, ProgressEvent};
use gpui_kit::{
    App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, Global, SharedString, Task,
    WeakEntity,
};
use rust_i18n::t;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

/// 全局模型句柄。
#[derive(Clone)]
pub struct Stores {
    pub conns: Entity<ConnectionStore>,
    pub history: Entity<HistoryStore>,
    pub ai: Entity<AiStore>,
    pub update: Entity<UpdateStore>,
}

impl Global for Stores {}

impl Stores {
    pub fn init(cx: &mut App) {
        let conns = cx.new(ConnectionStore::new);
        let history = cx.new(HistoryStore::new);
        let ai = cx.new(AiStore::new);
        let update = cx.new(UpdateStore::new);
        cx.set_global(Stores {
            conns,
            history,
            ai,
            update,
        });
    }

    pub fn global(cx: &App) -> &Stores {
        cx.global::<Stores>()
    }
}

/// 需要展示给用户的提示(由工作区订阅后推送为通知)。
#[derive(Debug, Clone)]
pub enum Toast {
    Success(SharedString),
    Error(SharedString),
    Info(SharedString),
}

// ============================================================ 连接

pub struct ConnectionStore {
    pub connections: Vec<Connection>,
    /// 当前聚焦的连接(侧栏、面包屑、新建 tab 的归属)
    pub active_id: Option<String>,
    /// 每个连接已加载的表/key 列表(懒加载缓存)
    pub tables: HashMap<String, Vec<String>>,
    /// 每个连接的表→列缓存(SQL 补全与 AI 上下文)
    pub columns: HashMap<String, ColumnsByTable>,
    /// 每个连接的外键+主键缓存(ER 图首开时拉取)
    pub erd: HashMap<String, ErdSchema>,
    /// 实例下的库列表缓存
    pub databases: HashMap<String, Vec<String>>,
    /// 已成功连接过的连接
    pub connected: HashSet<String>,
    /// 正在加载表列表的连接(防重入)
    loading: HashSet<String>,
}

impl EventEmitter<Toast> for ConnectionStore {}

impl ConnectionStore {
    fn new(cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            connections: vec![],
            active_id: None,
            tables: HashMap::new(),
            columns: HashMap::new(),
            erd: HashMap::new(),
            databases: HashMap::new(),
            connected: HashSet::new(),
            loading: HashSet::new(),
        };
        this.reload(cx);
        this
    }

    pub fn get(&self, id: &str) -> Option<&Connection> {
        self.connections.iter().find(|c| c.id == id)
    }

    pub fn active(&self) -> Option<&Connection> {
        self.active_id.as_deref().and_then(|id| self.get(id))
    }

    /// 从磁盘重读连接列表(同步,文件很小)。
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        match core_cmd::list_connections(&runtime::state()) {
            Ok(list) => self.connections = list,
            Err(e) => cx.emit(Toast::Error(
                t!("store.load_connections_failed", error = e.display())
                    .to_string()
                    .into(),
            )),
        }
        cx.notify();
    }

    pub fn activate(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.active_id.as_deref() != Some(id) {
            self.active_id = Some(id.to_string());
            cx.notify();
        }
    }

    /// 加载某连接的表/key 列表(成功才标记已连接;失败下次重试)。
    /// 关系库在后台顺带拉取整库列信息,失败静默(补全降级为表名 + 关键字)。
    pub fn load_tables(&mut self, id: &str, cx: &mut Context<Self>) -> Task<Vec<String>> {
        let Some(conn) = self.get(id).cloned() else {
            return Task::ready(vec![]);
        };
        if !self.loading.insert(id.to_string()) {
            return Task::ready(self.tables.get(id).cloned().unwrap_or_default());
        }
        let id = id.to_string();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let kind = conn.kind;
            let task_id = id.clone();
            let result: AppResult<Vec<String>> = runtime::backend(move |s| async move {
                service::connect(&s, &kind, &task_id).await?;
                service::list_objects(&s, &kind, &task_id).await
            })
            .await;
            let tables = match result {
                Ok(tables) => tables,
                Err(e) => {
                    let _ = this.update(cx, |this, cx| {
                        this.loading.remove(&id);
                        cx.emit(Toast::Error(
                            t!(
                                "store.conn_load_failed",
                                name = conn.name,
                                error = e.display()
                            )
                            .to_string()
                            .into(),
                        ));
                        cx.notify();
                    });
                    return vec![];
                }
            };
            let _ = this.update(cx, |this, cx| {
                this.loading.remove(&id);
                this.tables.insert(id.clone(), tables.clone());
                this.connected.insert(id.clone());
                cx.notify();
            });
            if kind.has_columns() {
                let task_id = id.clone();
                let cols = runtime::backend(move |s| async move {
                    service::fetch_columns(&s, &kind, &task_id).await
                })
                .await;
                if let Ok(cols) = cols {
                    let _ = this.update(cx, |this, cx| {
                        this.columns.insert(id.clone(), cols);
                        cx.notify();
                    });
                }
            }
            tables
        })
    }

    /// 实例下的库列表(带缓存)。
    pub fn load_databases(
        &mut self,
        id: &str,
        cx: &mut Context<Self>,
    ) -> Task<AppResult<Vec<String>>> {
        if let Some(dbs) = self.databases.get(id) {
            return Task::ready(Ok(dbs.clone()));
        }
        let Some(kind) = self.get(id).map(|c| c.kind) else {
            return Task::ready(Ok(vec![]));
        };
        let id = id.to_string();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let task_id = id.clone();
            let dbs = runtime::backend(move |s| async move {
                service::list_databases(&s, &kind, &task_id).await
            })
            .await?;
            let _ = this.update(cx, |this, cx| {
                this.databases.insert(id, dbs.clone());
                cx.notify();
            });
            Ok(dbs)
        })
    }

    pub fn save(
        &mut self,
        conn: Connection,
        password: Option<String>,
        cx: &mut Context<Self>,
    ) -> AppResult<Connection> {
        let saved = core_cmd::save_connection(&runtime::state(), conn, password)?;
        // 配置可能变了(库名/主机),丢弃旧缓存,下次展开重新加载
        self.forget(&saved.id);
        self.reload(cx);
        Ok(saved)
    }

    pub fn delete(&mut self, id: &str, cx: &mut Context<Self>) -> AppResult<()> {
        core_cmd::delete_connection(&runtime::state(), id.to_string())?;
        self.forget(id);
        if self.active_id.as_deref() == Some(id) {
            self.active_id = None;
        }
        self.reload(cx);
        Ok(())
    }

    fn forget(&mut self, id: &str) {
        self.tables.remove(id);
        self.columns.remove(id);
        self.erd.remove(id);
        self.databases.remove(id);
        self.connected.remove(id);
    }

    /// 同实例(kind/host/port/user 一致)且指向指定库的其他连接 —— 切库克隆的产物。
    pub fn siblings(&self, conn: &Connection, db: &str) -> Vec<Connection> {
        self.connections
            .iter()
            .filter(|x| {
                x.id != conn.id
                    && x.kind == conn.kind
                    && x.host == conn.host
                    && x.port == conn.port
                    && x.user == conn.user
                    && x.database == db
            })
            .cloned()
            .collect()
    }

    /// 切库:同实例已有该库的连接则复用,否则克隆一个(密码一并复制)。
    pub fn switch_database(
        &mut self,
        id: &str,
        db: &str,
        cx: &mut Context<Self>,
    ) -> AppResult<Connection> {
        let next = core_cmd::switch_database(&runtime::state(), id.to_string(), db.to_string())?;
        self.reload(cx);
        Ok(next)
    }

    pub fn invalidate_erd(&mut self, id: &str) {
        self.erd.remove(id);
    }

    pub fn is_loading(&self, id: &str) -> bool {
        self.loading.contains(id)
    }
}

// ============================================================ 历史

pub struct HistoryStore {
    /// 内存镜像(最新在前)
    pub entries: Vec<HistoryEntry>,
}

impl HistoryStore {
    fn new(_: &mut Context<Self>) -> Self {
        Self {
            entries: core_cmd::list_history(&runtime::state()).unwrap_or_default(),
        }
    }

    /// 执行成功后记录一条;失败静默,历史记录绝不影响查询流程。
    pub fn record(&mut self, mut entry: HistoryEntry, cx: &mut Context<Self>) {
        entry.id = uuid::Uuid::new_v4().to_string();
        if let Ok(list) = core_cmd::append_history(&runtime::state(), entry) {
            self.entries = list;
            cx.notify();
        }
    }

    /// `conn_id` 为 Some 时只清该连接的历史。
    pub fn clear(&mut self, conn_id: Option<String>, cx: &mut Context<Self>) {
        if let Ok(list) = core_cmd::clear_history(&runtime::state(), conn_id) {
            self.entries = list;
            cx.notify();
        }
    }
}

// ============================================================ AI

/// 对话绑定的连接:SQL 卡片永远在此连接上执行,防止切换连接后误打新库。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundConn {
    pub conn_id: String,
    pub kind: DbKind,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct AiMessage {
    pub id: u64,
    pub role: AiRole,
    pub content: String,
    /// 合成消息(如执行结果回灌):发给 AI,但以系统条样式渲染
    pub hidden: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Default)]
pub struct Convo {
    pub messages: Vec<AiMessage>,
    pub bound: Option<BoundConn>,
}

/// 发送给 AI 的历史上限(system 每次重建,不占额度)。
const MAX_HISTORY: usize = 30;

pub struct AiStore {
    pub status: Option<AiStatus>,
    pub settings: AiSettings,
    /// key(按连接分会话)→ 会话
    pub convos: HashMap<String, Convo>,
    /// 正在流式接收的会话;同一时刻只允许一个流
    pub streaming_key: Option<String>,
    /// 编辑器选中后待发送的引用(发送时拼进消息正文)
    pub pending_ref: Option<String>,
    next_id: u64,
    _stream: Option<Task<()>>,
}

impl EventEmitter<Toast> for AiStore {}

impl AiStore {
    fn new(_: &mut Context<Self>) -> Self {
        let state = runtime::state();
        Self {
            status: Some(ai_cmd::ai_status(&state)),
            settings: ai_cmd::ai_get_settings(&state),
            convos: HashMap::new(),
            streaming_key: None,
            pending_ref: None,
            next_id: 1,
            _stream: None,
        }
    }

    fn refresh_status(&mut self, cx: &mut Context<Self>) {
        self.status = Some(ai_cmd::status_of(&self.settings));
        cx.notify();
    }

    /// 保存整份配置。当前模型失效(服务商删除/停用、模型取消勾选)时自动改选第一个可用模型。
    pub fn save_settings(
        &mut self,
        mut settings: AiSettings,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        settings.normalize_current();
        ai_cmd::ai_save_settings(&runtime::state(), &settings)?;
        self.settings = settings;
        self.refresh_status(cx);
        Ok(())
    }

    /// AI 面板切换模型(全局,写回配置,下一条消息起生效)。
    pub fn set_current(&mut self, provider_id: &str, model: &str, cx: &mut Context<Self>) {
        let mut settings = self.settings.clone();
        settings.current = Some(ModelRef {
            provider_id: provider_id.to_string(),
            model: model.to_string(),
        });
        if let Err(e) = self.save_settings(settings, cx) {
            cx.emit(Toast::Error(e.into()));
        }
    }

    pub fn knowledge(&self) -> KnowledgeDoc {
        ai_cmd::ai_read_knowledge(&runtime::state())
    }

    pub fn convo(&self, key: &str) -> Option<&Convo> {
        self.convos.get(key)
    }

    pub fn bind(&mut self, key: &str, bound: BoundConn, cx: &mut Context<Self>) {
        self.convos.entry(key.to_string()).or_default().bound = Some(bound);
        cx.notify();
    }

    pub fn set_pending_ref(&mut self, sql: Option<String>, cx: &mut Context<Self>) {
        self.pending_ref = sql;
        cx.notify();
    }

    /// 向会话追加消息但不请求 AI(执行结果回灌用)。
    pub fn append(
        &mut self,
        key: &str,
        role: AiRole,
        content: String,
        hidden: bool,
        cx: &mut Context<Self>,
    ) {
        let id = self.next_id;
        self.next_id += 1;
        self.convos
            .entry(key.to_string())
            .or_default()
            .messages
            .push(AiMessage {
                id,
                role,
                content,
                hidden,
            });
        cx.notify();
    }

    pub fn clear(&mut self, key: &str, cx: &mut Context<Self>) {
        self.convos.remove(key);
        cx.notify();
    }

    fn patch_assistant(&mut self, key: &str, msg_id: u64, content: String) {
        if let Some(m) = self
            .convos
            .get_mut(key)
            .and_then(|c| c.messages.iter_mut().find(|m| m.id == msg_id))
        {
            m.content = content;
        }
    }

    /// 在会话 `key` 发送一条消息并流式接收回复。流式写入锁定在发送时的会话上。
    pub fn send(&mut self, key: &str, system_prompt: String, text: String, cx: &mut Context<Self>) {
        if self.streaming_key.is_some() || text.trim().is_empty() {
            return;
        }
        self.append(key, AiRole::User, text, false, cx);
        let assistant_id = self.next_id;
        self.append(key, AiRole::Assistant, String::new(), false, cx);
        self.streaming_key = Some(key.to_string());

        let history = &self.convos[key].messages;
        let mut wire = vec![ChatMessage {
            role: "system".into(),
            content: system_prompt,
        }];
        let history: Vec<&AiMessage> = history.iter().filter(|m| m.id != assistant_id).collect();
        let skip = history.len().saturating_sub(MAX_HISTORY);
        wire.extend(history.into_iter().skip(skip).map(|m| ChatMessage {
            role: match m.role {
                AiRole::User => "user".into(),
                AiRole::Assistant => "assistant".into(),
            },
            content: m.content.clone(),
        }));

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ChatEvent>();
        let done = runtime::backend(move |s| async move {
            let tx2 = tx.clone();
            let result = ai_cmd::ai_chat(&s, wire, move |e| {
                let _ = tx2.send(e);
            })
            .await;
            if let Err(e) = result {
                let _ = tx.send(ChatEvent::Error { message: e });
            }
        });
        let key = key.to_string();
        self._stream = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            let mut acc = String::new();
            let mut finished = false;
            while let Some(event) = rx.recv().await {
                let stop = this
                    .update(cx, |this, cx| {
                        match event {
                            ChatEvent::Delta { text } => {
                                acc.push_str(&text);
                                this.patch_assistant(&key, assistant_id, acc.clone());
                            }
                            ChatEvent::Done { full } => {
                                // 全文兜底:个别 Delta 丢失也能校正最终文本
                                let text = if !full.is_empty() {
                                    full
                                } else if acc.is_empty() {
                                    t!("store.ai_empty_reply").to_string()
                                } else {
                                    acc.clone()
                                };
                                this.patch_assistant(&key, assistant_id, text);
                                finished = true;
                            }
                            ChatEvent::Error { message } => {
                                let text = if acc.is_empty() {
                                    format!("⚠️ {message}")
                                } else {
                                    format!("{acc}\n\n⚠️ {message}")
                                };
                                this.patch_assistant(&key, assistant_id, text);
                                finished = true;
                            }
                        }
                        cx.notify();
                        finished
                    })
                    .unwrap_or(true);
                if stop {
                    break;
                }
            }
            done.await;
            let _ = this.update(cx, |this, cx| {
                this.streaming_key = None;
                cx.notify();
            });
        }));
    }
}

/// 读取知识库与 AI 测试都要走 tokio(reqwest)。
pub fn test_ai_config(
    provider: ProviderConfig,
    model: String,
) -> impl std::future::Future<Output = Result<(), String>> {
    runtime::spawn(ai_cmd::ai_test_config(provider, model))
}

/// 拉取服务商的模型列表(设置页「获取模型」)。
pub fn list_ai_models(
    provider: ProviderConfig,
) -> impl std::future::Future<Output = Result<Vec<String>, String>> {
    runtime::spawn(ai_cmd::ai_list_models(provider))
}

// ============================================================ 更新

#[derive(Debug, Clone, PartialEq)]
pub enum UpdateStatus {
    Idle,
    Checking,
    UpToDate,
    Available(Manifest),
    Downloading(Option<u8>),
    Installing,
    ReadyRestart,
    Failed(String),
}

impl UpdateStatus {
    fn is_busy(&self) -> bool {
        matches!(
            self,
            Self::Checking | Self::Downloading(_) | Self::Installing
        )
    }

    /// 更新完成(重启)前顶栏红点常亮。
    pub fn has_update(&self) -> bool {
        matches!(
            self,
            Self::Available(_) | Self::Downloading(_) | Self::Installing | Self::ReadyRestart
        )
    }
}

const FIRST_CHECK: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_secs(4 * 60 * 60);

pub struct UpdateStore {
    pub version: String,
    /// 是否支持应用内自更新(打包为 .app 运行才支持)
    pub supported: bool,
    pub status: UpdateStatus,
    /// 最近一次成功拉取的发布清单(不论新旧)
    pub latest: Option<Manifest>,
    _poll: Option<Task<()>>,
    _install: Option<Task<()>>,
}

impl UpdateStore {
    fn new(cx: &mut Context<Self>) -> Self {
        let supported = update::update_supported();
        // 打包版启动 10s 后首检,之后每 4 小时轮询一次
        let poll = supported.then(|| {
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                cx.background_executor().timer(FIRST_CHECK).await;
                loop {
                    let Ok(task) = this.update(cx, |this, cx| this.check_silently(cx)) else {
                        break;
                    };
                    task.await;
                    cx.background_executor().timer(POLL_INTERVAL).await;
                }
            })
        });
        Self {
            version: update::app_version(),
            supported,
            status: UpdateStatus::Idle,
            latest: None,
            _poll: poll,
            _install: None,
        }
    }

    pub fn check(&mut self, cx: &mut Context<Self>) {
        if self.status.is_busy() {
            return;
        }
        self.status = UpdateStatus::Checking;
        cx.notify();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let manifest = runtime::spawn(update::check_update()).await;
            let _ = this.update(cx, |this, cx| {
                this.status = match manifest {
                    None => UpdateStatus::Failed(t!("store.update_info_failed").to_string()),
                    Some(m) => {
                        this.latest = Some(m.clone());
                        if update::is_newer(&m.version, &this.version) {
                            UpdateStatus::Available(m)
                        } else {
                            UpdateStatus::UpToDate
                        }
                    }
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// 后台静默检查:失败与无更新都不改变状态。
    fn check_silently(&mut self, cx: &mut Context<Self>) -> Task<()> {
        if self.status.is_busy() || self.status == UpdateStatus::ReadyRestart {
            return Task::ready(());
        }
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let Some(m) = runtime::spawn(update::check_update()).await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                // 请求在途期间用户可能已开始手动检查/下载,落状态前复查
                if this.status.is_busy() || this.status == UpdateStatus::ReadyRestart {
                    return;
                }
                this.latest = Some(m.clone());
                if update::is_newer(&m.version, &this.version) {
                    this.status = UpdateStatus::Available(m);
                }
                cx.notify();
            });
        })
    }

    pub fn install(&mut self, cx: &mut Context<Self>) {
        let UpdateStatus::Available(m) = &self.status else {
            return;
        };
        let m = m.clone();
        self.status = UpdateStatus::Downloading(None);
        cx.notify();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ProgressEvent>();
        let done = runtime::spawn(async move {
            update::download_and_install(m, move |e| {
                let _ = tx.send(e);
            })
            .await
        });
        self._install = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            while let Some(e) = rx.recv().await {
                let _ = this.update(cx, |this, cx| {
                    this.status = match e {
                        ProgressEvent::Downloading { pct } => UpdateStatus::Downloading(pct),
                        ProgressEvent::Installing => UpdateStatus::Installing,
                    };
                    cx.notify();
                });
            }
            let result = done.await;
            let _ = this.update(cx, |this, cx| {
                this.status = match result {
                    Ok(()) => UpdateStatus::ReadyRestart,
                    Err(e) => UpdateStatus::Failed(e),
                };
                cx.notify();
            });
        }));
    }
}
