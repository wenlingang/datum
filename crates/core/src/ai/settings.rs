//! 用户 AI 配置(ai_settings.json):多个服务商,各自的协议/地址/Key/模型列表,
//! 外加全局当前选中的(服务商, 模型)。
//!
//! api_key 明文落盘(0600)而非钥匙串:AI key 每次启动都要读,
//! dev-signed 构建下 keyring 会反复弹系统授权框,打扰远大于收益
//! (数据库密码低频读写,仍走钥匙串不变)。

use rust_i18n::t;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 服务商的接口协议。各家地址不同之外,请求/鉴权/流式格式也不同。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// OpenAI Chat Completions 及其兼容端点(DeepSeek、通义、Kimi、Ollama、各类网关…)
    #[default]
    OpenAi,
    /// Anthropic Messages API
    Anthropic,
    /// Google Gemini generateContent API
    Gemini,
}

impl Protocol {
    pub const ALL: [Protocol; 3] = [Self::OpenAi, Self::Anthropic, Self::Gemini];

    /// 展示名;调用时按当前语言求值。
    pub fn label(self) -> String {
        match self {
            Self::OpenAi => t!("ai.protocol.openai_compatible").to_string(),
            Self::Anthropic => "Anthropic".to_string(),
            Self::Gemini => "Gemini".to_string(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelEntry {
    pub id: String,
    /// 勾选后才出现在 AI 面板的模型下拉里
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProviderConfig {
    /// 稳定标识,当前选择按它引用(名称可改)
    pub id: String,
    pub name: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub api_key: String,
    /// 停用的服务商不出现在模型下拉里,配置保留
    pub enabled: bool,
    pub models: Vec<ModelEntry>,
}

impl ProviderConfig {
    pub fn is_configured(&self) -> bool {
        !self.base_url.trim().is_empty() && !self.api_key.trim().is_empty()
    }

    pub fn enabled_models(&self) -> impl Iterator<Item = &str> {
        self.models
            .iter()
            .filter(|m| m.enabled)
            .map(|m| m.id.as_str())
    }
}

/// 全局当前模型:哪个服务商的哪个模型。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelRef {
    pub provider_id: String,
    pub model: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiSettings {
    pub providers: Vec<ProviderConfig>,
    pub current: Option<ModelRef>,
    /// 知识库文档:展示用文件名(空 = 未启用)。
    pub knowledge_name: String,
    /// 知识库文档的绝对路径。每次对话发起时现读现取(不缓存内容到本文件),
    /// 保证指南更新后无需重新导入即生效。
    pub knowledge_path: String,

    // 旧版单端点配置,只读不写:加载时迁移成一个「默认」服务商。
    #[serde(skip_serializing)]
    pub(crate) base_url: String,
    #[serde(skip_serializing)]
    pub(crate) api_key: String,
    #[serde(skip_serializing)]
    pub(crate) model: String,
}

/// 迁移出来的旧配置服务商 id。
pub const LEGACY_PROVIDER_ID: &str = "default";

impl AiSettings {
    pub fn provider(&self, id: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.id == id)
    }

    pub fn provider_mut(&mut self, id: &str) -> Option<&mut ProviderConfig> {
        self.providers.iter_mut().find(|p| p.id == id)
    }

    /// 旧版 {baseUrl, apiKey, model} → providers[0] + current。已有 providers 时忽略旧字段。
    /// 返回是否发生了迁移。
    fn migrate_legacy(&mut self) -> bool {
        let legacy = (
            std::mem::take(&mut self.base_url),
            std::mem::take(&mut self.api_key),
            std::mem::take(&mut self.model),
        );
        if !self.providers.is_empty() || (legacy.0.trim().is_empty() && legacy.1.trim().is_empty())
        {
            return false;
        }
        let (base_url, api_key, model) = legacy;
        let model = model.trim().to_string();
        self.providers.push(ProviderConfig {
            id: LEGACY_PROVIDER_ID.into(),
            name: t!("ai.provider.default_name").to_string(),
            protocol: Protocol::OpenAi,
            base_url,
            api_key,
            enabled: true,
            models: if model.is_empty() {
                vec![]
            } else {
                vec![ModelEntry {
                    id: model.clone(),
                    enabled: true,
                }]
            },
        });
        if !model.is_empty() {
            self.current = Some(ModelRef {
                provider_id: LEGACY_PROVIDER_ID.into(),
                model,
            });
        }
        true
    }

    /// 当前选择失效(服务商被删/停用、模型被取消勾选)时,退到第一个可用模型。
    pub fn normalize_current(&mut self) {
        let valid = self.current.as_ref().is_some_and(|c| {
            self.provider(&c.provider_id)
                .is_some_and(|p| p.enabled && p.enabled_models().any(|m| m == c.model))
        });
        if !valid {
            self.current = self.providers.iter().filter(|p| p.enabled).find_map(|p| {
                p.enabled_models().next().map(|m| ModelRef {
                    provider_id: p.id.clone(),
                    model: m.to_string(),
                })
            });
        }
    }
}

/// 仿 ConnectionRepo:config_dir + 固定文件名的 JSON 读写。
pub struct AiSettingsRepo {
    path: PathBuf,
}

impl AiSettingsRepo {
    pub fn new(config_dir: &Path) -> Self {
        Self {
            path: config_dir.join("ai_settings.json"),
        }
    }

    /// 文件缺失/损坏都回默认值:AI 配置丢了重填即可,不值得让整个应用报错。
    /// 读到旧版格式时迁移并立即写回新格式(写失败不影响本次使用,下次再迁)。
    pub fn load(&self) -> AiSettings {
        let mut s: AiSettings = std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        if s.migrate_legacy() {
            let _ = self.save(&s);
        }
        s
    }

    pub fn save(&self, settings: &AiSettings) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| t!("ai.err.create_config_dir", error = e).to_string())?;
        }
        let json = serde_json::to_vec_pretty(settings)
            .map_err(|e| t!("ai.err.serialize", error = e).to_string())?;
        std::fs::write(&self.path, json)
            .map_err(|e| t!("ai.err.write_config", error = e).to_string())?;
        // api_key 是明文,收紧到仅本用户可读。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| t!("ai.err.set_permissions", error = e).to_string())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo(tag: &str) -> (PathBuf, AiSettingsRepo) {
        let dir = std::env::temp_dir().join(format!("datum-ai-{tag}-{}", std::process::id()));
        (dir.clone(), AiSettingsRepo::new(&dir))
    }

    fn provider(id: &str, models: &[(&str, bool)]) -> ProviderConfig {
        ProviderConfig {
            id: id.into(),
            name: id.into(),
            base_url: "https://x/v1".into(),
            api_key: "sk".into(),
            enabled: true,
            models: models
                .iter()
                .map(|(m, e)| ModelEntry {
                    id: (*m).into(),
                    enabled: *e,
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn load_missing_file_returns_default() {
        let (_, repo) = temp_repo("missing");
        assert_eq!(repo.load(), AiSettings::default());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let (dir, repo) = temp_repo("rt");
        let s = AiSettings {
            providers: vec![provider("p1", &[("gpt-4o", true)])],
            current: Some(ModelRef {
                provider_id: "p1".into(),
                model: "gpt-4o".into(),
            }),
            knowledge_name: "数据库规范.md".into(),
            knowledge_path: "/Users/x/Downloads/数据库规范.md".into(),
            ..Default::default()
        };
        repo.save(&s).unwrap();
        assert_eq!(repo.load(), s);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, repo) = temp_repo("perm");
        repo.save(&AiSettings::default()).unwrap();
        let mode = std::fs::metadata(dir.join("ai_settings.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 旧版单端点配置迁移成「默认」服务商并设为当前,旧字段不再写回。
    #[test]
    fn legacy_config_migrates_to_default_provider() {
        let (dir, repo) = temp_repo("legacy");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("ai_settings.json"),
            r#"{"baseUrl":"https://gw/","apiKey":"sk-1","model":"qwen","knowledgeName":"k"}"#,
        )
        .unwrap();
        let s = repo.load();
        assert_eq!(s.providers.len(), 1);
        let p = &s.providers[0];
        assert_eq!(
            (p.id.as_str(), p.protocol, p.base_url.as_str()),
            (LEGACY_PROVIDER_ID, Protocol::OpenAi, "https://gw/")
        );
        assert_eq!(p.enabled_models().collect::<Vec<_>>(), vec!["qwen"]);
        assert_eq!(s.current.as_ref().unwrap().model, "qwen");
        assert_eq!(s.knowledge_name, "k");
        // 加载即写回新格式,无需等用户保存
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("ai_settings.json")).unwrap()).unwrap();
        assert!(raw.get("baseUrl").is_none() && raw.get("model").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn normalize_current_falls_back_to_first_enabled_model() {
        let mut s = AiSettings {
            providers: vec![
                provider("off", &[("a", true)]),
                provider("p2", &[("b", false), ("c", true)]),
            ],
            current: Some(ModelRef {
                provider_id: "gone".into(),
                model: "x".into(),
            }),
            ..Default::default()
        };
        s.providers[0].enabled = false;
        s.normalize_current();
        assert_eq!(
            s.current,
            Some(ModelRef {
                provider_id: "p2".into(),
                model: "c".into()
            })
        );
    }

    #[test]
    fn normalize_current_keeps_valid_choice() {
        let mut s = AiSettings {
            providers: vec![provider("p", &[("a", true), ("b", true)])],
            current: Some(ModelRef {
                provider_id: "p".into(),
                model: "b".into(),
            }),
            ..Default::default()
        };
        s.normalize_current();
        assert_eq!(s.current.unwrap().model, "b");
    }
}
