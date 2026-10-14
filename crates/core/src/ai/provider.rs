//! 把「服务商配置 + 模型」解析成一次请求所需的最终参数:协议、归一化后的地址、Key、模型。
//! 下游(client)只拿 Endpoint,不关心配置细节。

use crate::ai::settings::{AiSettings, Protocol, ProviderConfig};
use rust_i18n::t;

#[derive(Clone, Debug, PartialEq)]
pub struct Endpoint {
    pub protocol: Protocol,
    /// 已归一化:OpenAI 兼容为 API 根(含版本段),Anthropic/Gemini 为站点根
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

/// 全局当前模型 → Endpoint。
pub fn resolve(settings: &AiSettings) -> Result<Endpoint, String> {
    let current = settings
        .current
        .as_ref()
        .ok_or_else(|| t!("ai.err.no_model_selected").to_string())?;
    let provider = settings
        .provider(&current.provider_id)
        .ok_or_else(|| t!("ai.err.provider_deleted").to_string())?;
    build(provider, &current.model)
}

/// 校验并归一化;设置页「测试」和真正发起对话共用这一处,保证测试通过即可用。
pub fn build(provider: &ProviderConfig, model: &str) -> Result<Endpoint, String> {
    let base = provider.base_url.trim().trim_end_matches('/');
    let key = provider.api_key.trim();
    let model = model.trim();
    if base.is_empty() || key.is_empty() {
        return Err(t!("ai.err.missing_url_key", name = provider.name).to_string());
    }
    if model.is_empty() {
        return Err(t!("ai.err.select_model").to_string());
    }
    Ok(Endpoint {
        protocol: provider.protocol,
        base_url: normalize_base(provider.protocol, base),
        api_key: key.to_string(),
        model: model.to_string(),
    })
}

/// 列模型只需地址和 Key。
pub fn build_for_listing(provider: &ProviderConfig) -> Result<Endpoint, String> {
    build(provider, "-")
}

fn normalize_base(protocol: Protocol, base: &str) -> String {
    match protocol {
        // 用户常填站点根(如 https://gw.example.com/),OpenAI 兼容接口挂在 /v1 下,
        // 直接打根路径会拿到站点首页 HTML。只有站点根才补 /v1:
        // 智谱 /api/paas/v4、通义 /compatible-mode/v1 等自带版本段的路径保持原样。
        Protocol::OpenAi if is_site_root(base) => format!("{base}/v1"),
        Protocol::OpenAi => base.to_string(),
        // 这两家的版本段由 client 拼(/v1/messages、/v1beta/models),用户多填了就去掉
        Protocol::Anthropic => base.trim_end_matches("/v1").to_string(),
        Protocol::Gemini => base
            .trim_end_matches("/v1beta")
            .trim_end_matches("/v1")
            .to_string(),
    }
}

fn is_site_root(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    !rest.contains('/')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::settings::{ModelEntry, ModelRef};

    fn provider(protocol: Protocol, base: &str) -> ProviderConfig {
        ProviderConfig {
            id: "p".into(),
            name: "P".into(),
            protocol,
            base_url: base.into(),
            api_key: "sk-user".into(),
            enabled: true,
            models: vec![ModelEntry {
                id: "m".into(),
                enabled: true,
            }],
        }
    }

    fn base(protocol: Protocol, url: &str) -> String {
        build(&provider(protocol, url), "m").unwrap().base_url
    }

    #[test]
    fn openai_site_root_gets_v1() {
        assert_eq!(
            base(Protocol::OpenAi, "https://gw.example.com/"),
            "https://gw.example.com/v1"
        );
        assert_eq!(
            base(Protocol::OpenAi, "https://api.openai.com/v1/"),
            "https://api.openai.com/v1"
        );
    }

    /// 自带版本段/子路径的兼容端点不能被强行补 /v1。
    #[test]
    fn openai_versioned_paths_untouched() {
        assert_eq!(
            base(Protocol::OpenAi, "https://open.bigmodel.cn/api/paas/v4"),
            "https://open.bigmodel.cn/api/paas/v4"
        );
        assert_eq!(
            base(
                Protocol::OpenAi,
                "https://dashscope.aliyuncs.com/compatible-mode/v1"
            ),
            "https://dashscope.aliyuncs.com/compatible-mode/v1"
        );
    }

    #[test]
    fn anthropic_and_gemini_strip_version_suffix() {
        assert_eq!(
            base(Protocol::Anthropic, "https://api.anthropic.com/v1"),
            "https://api.anthropic.com"
        );
        assert_eq!(
            base(
                Protocol::Gemini,
                "https://generativelanguage.googleapis.com/v1beta/"
            ),
            "https://generativelanguage.googleapis.com"
        );
    }

    #[test]
    fn missing_key_or_model_errors() {
        let mut p = provider(Protocol::OpenAi, "https://x/v1");
        p.api_key = " ".into();
        // 文案随全局 locale 变化:缺 Key 的错误带服务商名,缺模型的不带
        let e = build(&p, "m").unwrap_err();
        assert!(e.contains("\"P\"") || e.contains("「P」"), "{e}");
        let p = provider(Protocol::OpenAi, "https://x/v1");
        let e = build(&p, " ").unwrap_err();
        assert!(
            !e.is_empty() && !e.contains("\"P\"") && !e.contains("「P」"),
            "{e}"
        );
    }

    #[test]
    fn resolve_uses_current_selection() {
        let s = AiSettings {
            providers: vec![provider(Protocol::Anthropic, "https://api.anthropic.com")],
            current: Some(ModelRef {
                provider_id: "p".into(),
                model: "claude".into(),
            }),
            ..Default::default()
        };
        let e = resolve(&s).unwrap();
        assert_eq!(
            (e.protocol, e.model.as_str()),
            (Protocol::Anthropic, "claude")
        );
        assert!(resolve(&AiSettings::default()).is_err());
    }
}
