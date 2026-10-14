//! 常用服务商预设:新建时预填名称、协议与地址,用户只需贴 API Key。

use crate::ai::settings::Protocol;
use rust_i18n::t;

#[derive(Clone, Copy, Debug)]
pub struct Preset {
    /// 稳定的官方英文名(与语言无关);界面展示用 [`Preset::display_name`]。
    pub name: &'static str,
    pub protocol: Protocol,
    pub base_url: &'static str,
    /// 有本地化名称的预设对应的 i18n 键;None 表示各语言都用 `name`。
    i18n_key: Option<&'static str>,
}

impl Preset {
    /// 按当前语言的展示名(菜单项、新建服务商的默认名称)。
    pub fn display_name(&self) -> String {
        match self.i18n_key {
            Some(key) => t!(key).to_string(),
            None => self.name.to_string(),
        }
    }
}

pub const PRESETS: &[Preset] = &[
    Preset {
        name: "OpenAI",
        protocol: Protocol::OpenAi,
        base_url: "https://api.openai.com/v1",
        i18n_key: None,
    },
    Preset {
        name: "Anthropic",
        protocol: Protocol::Anthropic,
        base_url: "https://api.anthropic.com",
        i18n_key: None,
    },
    Preset {
        name: "Gemini",
        protocol: Protocol::Gemini,
        base_url: "https://generativelanguage.googleapis.com",
        i18n_key: None,
    },
    Preset {
        name: "DeepSeek",
        protocol: Protocol::OpenAi,
        base_url: "https://api.deepseek.com/v1",
        i18n_key: None,
    },
    Preset {
        name: "Qwen",
        protocol: Protocol::OpenAi,
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        i18n_key: Some("ai.preset.qwen"),
    },
    Preset {
        name: "Kimi",
        protocol: Protocol::OpenAi,
        base_url: "https://api.moonshot.cn/v1",
        i18n_key: None,
    },
    Preset {
        name: "Zhipu GLM",
        protocol: Protocol::OpenAi,
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        i18n_key: Some("ai.preset.zhipu"),
    },
    Preset {
        name: "SiliconFlow",
        protocol: Protocol::OpenAi,
        base_url: "https://api.siliconflow.cn/v1",
        i18n_key: Some("ai.preset.siliconflow"),
    },
    Preset {
        name: "OpenRouter",
        protocol: Protocol::OpenAi,
        base_url: "https://openrouter.ai/api/v1",
        i18n_key: None,
    },
    Preset {
        name: "Ollama",
        protocol: Protocol::OpenAi,
        base_url: "http://localhost:11434/v1",
        i18n_key: None,
    },
];
