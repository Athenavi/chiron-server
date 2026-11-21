//! preset 目录与能力矩阵（B3b）：复用 `crate::providers::PRESETS` 的 27 个 preset，
//! 把"目录里的静态元数据"转成"可发请求的实例"，并把套餐/协议差异收敛到这里的函数上。

use std::{env, sync::Arc};

use super::{
    anthropic::AnthropicProvider, merge_models, openai::OpenAiProvider, ModelInfo, Provider,
    ProviderError,
};
use crate::providers::{self, plan_allowlist, Protocol};

/// 额外放行某个套餐白名单里的模型族（逗号分隔，如 `claude,gpt-5`）。
pub const PLAN_ALLOW_EXTRA_ENV: &str = "CHIRON_PLAN_ALLOW_EXTRA";

/// 解析后的 provider：指向真实端点所需的一切。
#[derive(Clone, Debug)]
pub struct ResolvedProvider {
    pub id: String,
    pub label: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub api_key_env: &'static str,
    pub api_key: Option<String>,
    pub model_prefixes: Vec<String>,
    pub requires_key: bool,
    pub model_discovery: bool,
}

impl ResolvedProvider {
    /// 需要 key 的 preset 缺 key 时给出可操作报错（带 env 变量名，不带 key 值）。
    pub fn require_key(&self) -> Result<&str, ProviderError> {
        match self.api_key.as_deref() {
            Some(key) => Ok(key),
            None => Err(ProviderError::config(format!(
                "{} is missing an API key (set {})",
                self.label, self.api_key_env
            ))),
        }
    }

    /// 发包前的自检：metadata-only preset 必须显式给 base_url，需要 key 的必须有 key。
    pub fn ensure_ready(&self) -> Result<(), ProviderError> {
        if self.base_url.trim().is_empty() {
            return Err(ProviderError::config(format!(
                "{} needs an explicit base_url",
                self.id
            )));
        }
        if self.requires_key {
            self.require_key()?;
        }
        Ok(())
    }
}

/// `opencode-go` → `OPENCODE_GO`（用于拼 base_url 覆盖变量名）。
pub fn env_slug(id: &str) -> String {
    id.to_ascii_uppercase().replace('-', "_")
}

/// 租户/部署级 base_url 覆盖：`CHIRON_PROVIDER_OPENCODE_GO_BASE_URL`。
pub fn base_url_override_env(id: &str) -> String {
    format!("CHIRON_PROVIDER_{}_BASE_URL", env_slug(id))
}

fn env_non_empty(name: &str) -> Option<String> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => Some(value.trim().to_string()),
        _ => None,
    }
}

pub fn resolve(id: &str) -> Result<ResolvedProvider, ProviderError> {
    resolve_with(id, None, None)
}

/// 显式覆盖优先于环境变量，环境变量优先于 preset 默认值。
pub fn resolve_with(
    id: &str,
    base_url_override: Option<&str>,
    api_key_override: Option<&str>,
) -> Result<ResolvedProvider, ProviderError> {
    let preset = providers::find(id)
        .ok_or_else(|| ProviderError::config(format!("unknown provider preset: {id}")))?;

    let base_url = base_url_override
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| env_non_empty(&base_url_override_env(preset.id)))
        .unwrap_or_else(|| preset.base_url.to_string());
    if base_url.trim().is_empty() {
        return Err(ProviderError::config(format!(
            "provider {} has no default endpoint; pass base_url explicitly",
            preset.id
        )));
    }

    let api_key = api_key_override
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| env_non_empty(preset.api_key_env));

    Ok(ResolvedProvider {
        id: preset.id.to_string(),
        label: preset.label.to_string(),
        protocol: preset.protocol,
        base_url,
        api_key_env: preset.api_key_env,
        api_key,
        model_prefixes: preset
            .model_prefixes
            .iter()
            .map(|prefix| (*prefix).to_string())
            .collect(),
        requires_key: preset.requires_key,
        model_discovery: preset.model_discovery,
    })
}

/// 套餐白名单：`None` 表示该 preset 不限模型。
pub fn plan_allowed_models(provider_id: &str) -> Option<&'static [&'static str]> {
    plan_allowlist(provider_id)
}

pub fn parse_extra_allowlist(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|item| item.trim().to_ascii_lowercase())
        .filter(|item| !item.is_empty())
        .collect()
}

/// 显式放行（部署配置 `CHIRON_PLAN_ALLOW_EXTRA`）。
pub fn extra_allow_from_env() -> Vec<String> {
    env_non_empty(PLAN_ALLOW_EXTRA_ENV)
        .map(|raw| parse_extra_allowlist(&raw))
        .unwrap_or_default()
}

/// 套餐白名单检查。OpenCode Go 不含 Claude，除非显式放行。
pub fn check_plan(provider_id: &str, model: &str) -> Result<(), ProviderError> {
    let extra = extra_allow_from_env();
    check_plan_with(provider_id, model, &extra)
}

pub fn check_plan_with(
    provider_id: &str,
    model: &str,
    extra_allow: &[String],
) -> Result<(), ProviderError> {
    let Some(allowed) = plan_allowed_models(provider_id) else {
        return Ok(());
    };
    let lower = model.to_ascii_lowercase();
    let in_plan = allowed
        .iter()
        .any(|prefix| lower.contains(&prefix.to_ascii_lowercase()));
    let explicitly_allowed = extra_allow
        .iter()
        .map(|item| item.to_ascii_lowercase())
        .any(|item| !item.is_empty() && lower.contains(&item));
    if in_plan || explicitly_allowed {
        Ok(())
    } else {
        Err(ProviderError::unsupported(format!(
            "model {model} is not included in the {provider_id} subscription plan"
        )))
    }
}

/// 按协议选适配器；不做网络调用。
pub fn build(resolved: &ResolvedProvider) -> Result<Arc<dyn Provider>, ProviderError> {
    resolved.ensure_ready()?;
    let api_key = resolved.api_key.clone().unwrap_or_default();
    let provider: Arc<dyn Provider> = match resolved.protocol {
        Protocol::Openai => Arc::new(
            OpenAiProvider::new(resolved.id.clone(), resolved.base_url.clone())
                .with_api_key(api_key),
        ),
        Protocol::Anthropic => Arc::new(
            AnthropicProvider::new(resolved.id.clone(), resolved.base_url.clone())
                .with_api_key(api_key),
        ),
    };
    Ok(provider)
}

pub fn build_by_id(id: &str) -> Result<Arc<dyn Provider>, ProviderError> {
    build_by_id_with_key(id, None)
}

/// 租户 BYOK 优先：显式 key 覆盖环境变量里的平台池 key。
pub fn build_by_id_with_key(
    id: &str,
    api_key: Option<&str>,
) -> Result<Arc<dyn Provider>, ProviderError> {
    build(&resolve_with(id, None, api_key)?)
}

/// 静态目录 + 远端发现合并（v1 必须：前端选模型不能只靠硬编码）。
pub async fn discover_models(
    provider: &dyn Provider,
    static_ids: &[&str],
) -> Result<Vec<ModelInfo>, ProviderError> {
    let remote = provider.list_models().await?;
    Ok(merge_models(static_ids, remote))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON_BASE: &str = "http://127.0.0.1:9/v1";

    #[test]
    fn all_27_presets_resolve_to_a_concrete_endpoint() {
        assert_eq!(providers::PRESETS.len(), 27);
        for preset in providers::PRESETS {
            let resolved = resolve_with(preset.id, Some(EPSILON_BASE), Some("test-key"))
                .unwrap_or_else(|e| panic!("{} should resolve: {e}", preset.id));
            assert_eq!(resolved.id, preset.id);
            assert_eq!(resolved.label, preset.label);
            assert_eq!(resolved.protocol, preset.protocol);
            assert_eq!(resolved.base_url, EPSILON_BASE);
            assert_eq!(resolved.api_key_env, preset.api_key_env);
            assert_eq!(resolved.model_prefixes.len(), preset.model_prefixes.len());
        }
    }

    #[test]
    fn default_base_urls_come_from_presets_for_every_self_hosted_provider() {
        for id in [
            "openai",
            "anthropic",
            "ollama",
            "vllm",
            "lmstudio",
            "opencode-go",
        ] {
            let resolved = resolve_with(id, None, Some("k")).expect("preset has a default url");
            assert!(!resolved.base_url.is_empty(), "{id} base_url");
        }
    }

    #[test]
    fn metadata_only_presets_require_explicit_base_url() {
        for id in ["oneapi", "custom", "custom-anthropic"] {
            let error = resolve_with(id, None, Some("k")).expect_err("needs explicit url");
            assert!(matches!(error, ProviderError::Config(_)), "{id}");
        }
        let resolved = resolve_with("custom", Some("https://gw.example/v1"), Some("k"))
            .expect("explicit url resolves");
        assert_eq!(resolved.base_url, "https://gw.example/v1");
    }

    #[test]
    fn unknown_preset_is_a_config_error() {
        assert!(matches!(resolve("nope"), Err(ProviderError::Config(_))));
    }

    #[test]
    fn protocol_matches_preset_for_anthropic_and_openai_endpoints() {
        assert_eq!(
            resolve_with("anthropic", Some("https://api.anthropic.com"), Some("k"))
                .expect("anthropic")
                .protocol,
            Protocol::Anthropic
        );
        assert_eq!(
            resolve_with("opencode-go-anthropic", None, Some("k"))
                .expect("opencode go anthropic")
                .protocol,
            Protocol::Anthropic
        );
        assert_eq!(
            resolve_with("opencode-go", None, Some("k"))
                .expect("opencode go")
                .protocol,
            Protocol::Openai
        );
        assert_eq!(
            resolve_with("deepseek", None, Some("k"))
                .expect("deepseek")
                .protocol,
            Protocol::Openai
        );
    }

    #[test]
    fn base_url_override_env_name_is_derived_from_preset_id() {
        assert_eq!(
            base_url_override_env("opencode-go"),
            "CHIRON_PROVIDER_OPENCODE_GO_BASE_URL"
        );
        assert_eq!(env_slug("custom-anthropic"), "CUSTOM_ANTHROPIC");
    }

    #[test]
    fn require_key_reports_missing_key_without_leaking_it() {
        let resolved =
            resolve_with("openai", Some(EPSILON_BASE), Some("sk-secret")).expect("openai");
        assert_eq!(resolved.require_key().expect("key"), "sk-secret");

        let mut without_key = resolved.clone();
        without_key.api_key = None;
        let error = without_key.require_key().expect_err("missing key");
        assert!(error.to_string().contains("OPENAI_API_KEY"));
        assert!(!error.to_string().contains("sk-secret"));
        assert!(without_key.ensure_ready().is_err());
    }

    #[test]
    fn local_presets_do_not_require_a_key() {
        let mut resolved =
            resolve_with("ollama", Some("http://localhost:11434/v1"), None).expect("ollama");
        resolved.api_key = None;
        resolved.ensure_ready().expect("ollama is keyless");
    }

    #[test]
    fn opencode_go_plan_excludes_claude_but_can_be_overridden() {
        let error = check_plan_with("opencode-go", "claude-sonnet-4", &[]).expect_err("excluded");
        assert!(matches!(error, ProviderError::Unsupported(_)));
        assert!(check_plan_with("opencode-go", "claude-sonnet-4", &["claude".to_string()]).is_ok());
        assert!(check_plan_with("opencode-go", "glm-4.6", &[]).is_ok());
        assert!(check_plan_with("opencode-go", "kimi-k2", &[]).is_ok());
        assert!(check_plan_with("opencode-go-anthropic", "deepseek-v3", &[]).is_ok());
        // 非订阅套餐不限模型
        assert!(check_plan_with("openai", "claude-sonnet-4", &[]).is_ok());
    }

    #[test]
    fn extra_allowlist_parsing_is_trimmed_and_lowercased() {
        assert_eq!(
            parse_extra_allowlist(" Claude , GPT-5 ,,"),
            vec!["claude".to_string(), "gpt-5".to_string()]
        );
        assert!(parse_extra_allowlist("  ").is_empty());
    }

    #[test]
    fn build_picks_adapter_by_protocol() {
        let resolved = resolve_with("anthropic", Some("https://api.anthropic.com"), Some("k"))
            .expect("anthropic resolved");
        let provider = build(&resolved).expect("build anthropic");
        assert_eq!(provider.protocol(), Protocol::Anthropic);
        assert_eq!(provider.id(), "anthropic");

        let resolved =
            resolve_with("deepseek", Some(EPSILON_BASE), Some("k")).expect("deepseek resolved");
        let provider = build(&resolved).expect("build openai-compatible");
        assert_eq!(provider.protocol(), Protocol::Openai);
        assert_eq!(provider.id(), "deepseek");
    }

    #[test]
    fn build_rejects_missing_key_for_keyed_presets() {
        let mut resolved = resolve_with("openai", Some(EPSILON_BASE), Some("k")).expect("openai");
        resolved.api_key = None;
        let error = match build(&resolved) {
            Ok(_) => panic!("missing key must fail"),
            Err(e) => e,
        };
        assert!(matches!(error, ProviderError::Config(_)));
    }
}
