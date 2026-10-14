//! AI 相关命令:配置读写、状态、测试、模型列表、知识库与流式对话。
//!
//! 与 update.rs 一致用 `Result<_, String>` 返回用户可读错误;
//! 配置读写走 AppState.config_dir 下的 ai_settings.json。

use crate::ai::chat::{self, ChatEvent, ChatMessage};
use crate::ai::provider;
use crate::ai::settings::{AiSettings, AiSettingsRepo, ProviderConfig};
use crate::commands::AppState;
use rust_i18n::t;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiStatus {
    /// 当前能否发起对话;false 时 error 给出引导文案。
    pub ready: bool,
    pub model: Option<String>,
    pub error: Option<String>,
}

fn repo(state: &AppState) -> AiSettingsRepo {
    AiSettingsRepo::new(&state.config_dir)
}

pub fn ai_status(state: &AppState) -> AiStatus {
    status_of(&repo(state).load())
}

pub fn status_of(settings: &AiSettings) -> AiStatus {
    let resolved = provider::resolve(settings);
    AiStatus {
        ready: resolved.is_ok(),
        model: resolved.as_ref().ok().map(|p| p.model.clone()),
        error: resolved.err(),
    }
}

pub fn ai_get_settings(state: &AppState) -> AiSettings {
    repo(state).load()
}

pub fn ai_save_settings(state: &AppState, settings: &AiSettings) -> Result<(), String> {
    let _g = state.lock.lock().unwrap();
    repo(state).save(settings)
}

/// 设置页「测试」:用表单里当前填的服务商(未必已保存)+ 指定模型发一句话验证可用性。
pub async fn ai_test_config(provider: ProviderConfig, model: String) -> Result<(), String> {
    let ep = provider::build(&provider, &model)?;
    chat::test_connection(&ep).await
}

/// 拉取服务商的模型列表(设置页「获取模型」)。
pub async fn ai_list_models(provider: ProviderConfig) -> Result<Vec<String>, String> {
    let ep = provider::build_for_listing(&provider)?;
    chat::list_models(&ep).await
}

/// 知识库文档大小上限:超出会把整份指南塞进每次请求的 system prompt,
/// 明显失衡的量(如误选了一个大文件)先挡掉,而不是静默截断产生半份指南。
const KNOWLEDGE_MAX_BYTES: u64 = 300 * 1024;

/// 内置默认知识库:编译期打进二进制,不依赖任何本地文件路径,换机器/换用户都自带。
/// 源文件维护在 docs/database-guidelines.md,改动后重新编译即生效。
/// 英文界面用英文版 docs/database-guidelines.en.md;简体/繁体界面都用中文版。
const DEFAULT_KNOWLEDGE_ZH: &str = include_str!("../../../../docs/database-guidelines.md");
const DEFAULT_KNOWLEDGE_EN: &str = include_str!("../../../../docs/database-guidelines.en.md");

/// 按 locale 选内置知识库正文:中文(zh-CN / zh-TW / zh-*)→ 中文版,其余 → 英文版。
fn default_knowledge_content(locale: &str) -> &'static str {
    if locale.to_ascii_lowercase().starts_with("zh") {
        DEFAULT_KNOWLEDGE_ZH
    } else {
        DEFAULT_KNOWLEDGE_EN
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeDoc {
    pub name: String,
    pub content: String,
    /// 配置了自定义文件但读取失败时说明原因;此时 name/content 已回退为内置默认,
    /// 不会让对话因为一个坏路径直接不可用。
    pub warning: Option<String>,
}

fn default_knowledge() -> KnowledgeDoc {
    KnowledgeDoc {
        name: t!("ai.knowledge.default_name").to_string(),
        content: default_knowledge_content(&rust_i18n::locale()).to_string(),
        warning: None,
    }
}

fn read_custom_knowledge(path: &str) -> Result<String, String> {
    let meta =
        std::fs::metadata(path).map_err(|e| t!("ai.err.read_file", error = e).to_string())?;
    if meta.len() > KNOWLEDGE_MAX_BYTES {
        return Err(t!(
            "ai.err.file_too_large",
            size = format!("{:.1}", meta.len() as f64 / 1024.0),
            max = KNOWLEDGE_MAX_BYTES / 1024
        )
        .to_string());
    }
    std::fs::read_to_string(path).map_err(|e| t!("ai.err.read_file_utf8", error = e).to_string())
}

/// 返回当前生效的知识库文档:未配置自定义文件 → 内置默认;配置了 → 现读现取
/// (不缓存,文档更新后下一次对话立即生效);自定义文件读取失败 → 回退内置默认并带上 warning。
pub fn ai_read_knowledge(state: &AppState) -> KnowledgeDoc {
    let settings = repo(state).load();
    let path = settings.knowledge_path.trim();
    if path.is_empty() {
        return default_knowledge();
    }
    match read_custom_knowledge(path) {
        Ok(content) => {
            let name = if settings.knowledge_name.trim().is_empty() {
                path.to_string()
            } else {
                settings.knowledge_name.clone()
            };
            KnowledgeDoc {
                name,
                content,
                warning: None,
            }
        }
        Err(e) => KnowledgeDoc {
            warning: Some(t!("ai.err.knowledge_fallback", error = e).to_string()),
            ..default_knowledge()
        },
    }
}

/// 流式对话:前端传全量 messages(含 system),增量经 Channel 回推。
pub async fn ai_chat(
    state: &AppState,
    messages: Vec<ChatMessage>,
    on_event: impl Fn(ChatEvent) + Send + Sync,
) -> Result<(), String> {
    let settings = repo(state).load();
    let ep = provider::resolve(&settings)?;
    if let Err(e) = chat::stream_chat(&ep, &messages, &on_event).await {
        // 错误同时走 Channel(流中断场景前端只盯着 Channel)与返回值。
        on_event(ChatEvent::Error { message: e.clone() });
        return Err(e);
    }
    Ok(())
}
