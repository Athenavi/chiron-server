//! Provider preset 目录（迁移自旧引擎 `app/providers/catalog.py`）。
//!
//! 27 个 preset 只有两种接入协议；路由与差异由能力矩阵表达，不为每个厂商写适配器。

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Openai,
    Anthropic,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    pub protocol: Protocol,
    pub base_url: &'static str,
    pub api_key_env: &'static str,
    pub model_prefixes: &'static [&'static str],
    pub requires_key: bool,
    pub model_discovery: bool,
    pub cost: f32,
    pub quality: f32,
}

macro_rules! p {
    ($id:expr, $label:expr, $proto:ident, $base:expr, $env:expr,
     [$($prefix:expr),* $(,)?], $key:expr, $disc:expr, $cost:expr, $quality:expr) => {
        Preset {
            id: $id,
            label: $label,
            protocol: Protocol::$proto,
            base_url: $base,
            api_key_env: $env,
            model_prefixes: &[$($prefix),*],
            requires_key: $key,
            model_discovery: $disc,
            cost: $cost,
            quality: $quality,
        }
    };
}

pub static PRESETS: &[Preset] = &[
    // 国际
    p!(
        "openai",
        "OpenAI",
        Openai,
        "https://api.openai.com/v1",
        "OPENAI_API_KEY",
        ["gpt", "o1", "o3", "o4", "davinci", "text-embedding"],
        true,
        true,
        5.0,
        0.90
    ),
    p!(
        "anthropic",
        "Anthropic Claude",
        Anthropic,
        "https://api.anthropic.com",
        "ANTHROPIC_API_KEY",
        ["claude"],
        true,
        false,
        3.0,
        0.95
    ),
    p!(
        "google",
        "Google Gemini",
        Openai,
        "https://generativelanguage.googleapis.com/v1beta/openai",
        "GEMINI_API_KEY",
        ["gemini", "gemma"],
        true,
        true,
        1.5,
        0.90
    ),
    p!(
        "xai",
        "xAI Grok",
        Openai,
        "https://api.x.ai/v1",
        "XAI_API_KEY",
        ["grok"],
        true,
        true,
        3.0,
        0.88
    ),
    p!(
        "groq",
        "Groq",
        Openai,
        "https://api.groq.com/openai/v1",
        "GROQ_API_KEY",
        ["llama", "mixtral", "gemma", "whisper"],
        true,
        true,
        0.6,
        0.82
    ),
    p!(
        "mistral",
        "Mistral AI",
        Openai,
        "https://api.mistral.ai/v1",
        "MISTRAL_API_KEY",
        ["mistral", "codestral", "ministral", "pixtral"],
        true,
        true,
        1.0,
        0.84
    ),
    p!(
        "together",
        "Together AI",
        Openai,
        "https://api.together.xyz/v1",
        "TOGETHER_API_KEY",
        [],
        true,
        true,
        0.9,
        0.82
    ),
    // 国内
    p!(
        "deepseek",
        "DeepSeek",
        Openai,
        "https://api.deepseek.com",
        "DEEPSEEK_API_KEY",
        ["deepseek"],
        true,
        true,
        0.2,
        0.80
    ),
    p!(
        "moonshot",
        "月之暗面 Kimi",
        Openai,
        "https://api.moonshot.cn/v1",
        "MOONSHOT_API_KEY",
        ["moonshot", "kimi"],
        true,
        true,
        1.2,
        0.85
    ),
    p!(
        "zhipu",
        "智谱 GLM",
        Openai,
        "https://open.bigmodel.cn/api/paas/v4",
        "ZHIPU_API_KEY",
        ["glm", "chatglm"],
        true,
        true,
        0.6,
        0.82
    ),
    p!(
        "dashscope",
        "阿里云通义千问",
        Openai,
        "https://dashscope.aliyuncs.com/compatible-mode/v1",
        "DASHSCOPE_API_KEY",
        ["qwen", "qwq", "tongyi"],
        true,
        true,
        0.8,
        0.84
    ),
    p!(
        "minimax",
        "MiniMax",
        Openai,
        "https://api.minimax.chat/v1",
        "MINIMAX_API_KEY",
        ["minimax", "abab"],
        true,
        true,
        1.0,
        0.80
    ),
    p!(
        "baichuan",
        "百川智能",
        Openai,
        "https://api.baichuan-ai.com/v1",
        "BAICHUAN_API_KEY",
        ["baichuan"],
        true,
        true,
        0.6,
        0.78
    ),
    p!(
        "hunyuan",
        "腾讯混元",
        Openai,
        "https://api.hunyuan.cloud.tencent.com/v1",
        "HUNYUAN_API_KEY",
        ["hunyuan"],
        true,
        true,
        0.7,
        0.80
    ),
    p!(
        "stepfun",
        "阶跃星辰 Step",
        Openai,
        "https://api.stepfun.com/v1",
        "STEPFUN_API_KEY",
        ["step"],
        true,
        true,
        0.8,
        0.80
    ),
    // 聚合/自建
    p!(
        "openrouter",
        "OpenRouter",
        Openai,
        "https://openrouter.ai/api/v1",
        "OPENROUTER_API_KEY",
        ["openrouter/"],
        true,
        true,
        5.0,
        0.85
    ),
    p!(
        "siliconflow",
        "硅基流动 SiliconFlow",
        Openai,
        "https://api.siliconflow.cn/v1",
        "SILICONFLOW_API_KEY",
        ["Qwen/", "deepseek-ai/", "THUDM/", "Pro/"],
        true,
        true,
        0.5,
        0.80
    ),
    p!(
        "oneapi",
        "One API / New API 自建网关",
        Openai,
        "",
        "ONEAPI_API_KEY",
        [],
        true,
        true,
        5.0,
        0.80
    ),
    p!(
        "opencode",
        "OpenCode Zen",
        Openai,
        "https://opencode.ai/zen/v1",
        "OPENCODE_API_KEY",
        [],
        true,
        true,
        2.0,
        0.88
    ),
    p!(
        "opencode-anthropic",
        "OpenCode Zen（Anthropic 协议）",
        Anthropic,
        "https://opencode.ai/zen",
        "OPENCODE_API_KEY",
        [],
        true,
        false,
        2.0,
        0.88
    ),
    p!(
        "opencode-go",
        "OpenCode Go（订阅）",
        Openai,
        "https://opencode.ai/inference/go/v1",
        "OPENCODE_GO_API_KEY",
        [],
        true,
        true,
        0.4,
        0.85
    ),
    p!(
        "opencode-go-anthropic",
        "OpenCode Go（Anthropic 协议）",
        Anthropic,
        "https://opencode.ai/inference/go",
        "OPENCODE_GO_API_KEY",
        [],
        true,
        false,
        0.4,
        0.85
    ),
    // 本地/自托管
    p!(
        "ollama",
        "Ollama（本地）",
        Openai,
        "http://localhost:11434/v1",
        "OLLAMA_API_KEY",
        [],
        false,
        true,
        0.0,
        0.60
    ),
    p!(
        "vllm",
        "vLLM（自托管）",
        Openai,
        "http://localhost:8000/v1",
        "VLLM_API_KEY",
        [],
        false,
        true,
        0.0,
        0.62
    ),
    p!(
        "lmstudio",
        "LM Studio（本地）",
        Openai,
        "http://localhost:1234/v1",
        "LMSTUDIO_API_KEY",
        [],
        false,
        true,
        0.0,
        0.58
    ),
    // 自定义
    p!(
        "custom",
        "自定义（OpenAI 兼容）",
        Openai,
        "",
        "CUSTOM_API_KEY",
        [],
        true,
        true,
        5.0,
        0.80
    ),
    p!(
        "custom-anthropic",
        "自定义（Anthropic 协议）",
        Anthropic,
        "",
        "CUSTOM_ANTHROPIC_API_KEY",
        [],
        true,
        false,
        3.0,
        0.85
    ),
];

pub fn find(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// 订阅套餐默认允许的模型族（OpenCode Go）。显式配置可覆盖。
pub fn plan_allowlist(provider: &str) -> Option<&'static [&'static str]> {
    match provider {
        "opencode-go" | "opencode-go-anthropic" => Some(&[
            "glm", "kimi", "qwen", "minimax", "deepseek", "grok", "gpt-oss", "gpt-5", "gpt-6",
            "gemini", "longcat", "mimo", "muse", "hy", "step", "beam", "iceman",
        ]),
        _ => None,
    }
}

/// 模型名 → 直连模型族（仅用于提示/默认路由）。
#[allow(dead_code)]
pub fn model_family(model: &str) -> &'static str {
    let name = model.to_lowercase();
    if name.contains("claude") {
        "anthropic"
    } else if name.contains("gemini") || name.starts_with("gemma") {
        "google"
    } else if name.contains("deepseek") {
        "deepseek"
    } else if name.starts_with("gpt")
        || name.starts_with("o1")
        || name.starts_with("o3")
        || name.starts_with("o4")
        || name.starts_with("text-embedding")
        || name.starts_with("davinci")
    {
        "openai"
    } else if name.contains("kimi") || name.contains("moonshot") {
        "moonshot"
    } else if name.contains("glm") || name.contains("chatglm") {
        "zhipu"
    } else if name.contains("qwen") || name.contains("qwq") {
        "dashscope"
    } else if name.starts_with("minimax") || name.starts_with("abab") {
        "minimax"
    } else if name.contains("grok") {
        "xai"
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn catalog_has_all_presets_and_unique_ids() {
        assert_eq!(PRESETS.len(), 27, "旧引擎共有 27 个 preset");
        let mut ids = HashSet::new();
        for p in PRESETS {
            assert!(ids.insert(p.id), "duplicate preset id: {}", p.id);
            assert!(!p.label.is_empty());
            assert!(matches!(p.protocol, Protocol::Openai | Protocol::Anthropic));
        }
    }

    #[test]
    fn metadata_only_providers_have_no_base_url() {
        for p in PRESETS {
            match p.id {
                "oneapi" | "custom" | "custom-anthropic" => assert!(p.base_url.is_empty()),
                _ => assert!(!p.base_url.is_empty(), "{} missing base_url", p.id),
            }
        }
    }

    #[test]
    fn opencode_go_allowlist_excludes_claude() {
        let list = plan_allowlist("opencode-go").expect("allowlist");
        assert!(!list.contains(&"claude"));
        assert!(list.contains(&"glm"));
        assert!(plan_allowlist("openai").is_none());
    }

    #[test]
    fn model_family_matches_known_prefixes() {
        assert_eq!(model_family("claude-sonnet-4"), "anthropic");
        assert_eq!(model_family("gpt-5"), "openai");
        assert_eq!(model_family("deepseek-chat"), "deepseek");
        assert_eq!(model_family("qwen3"), "dashscope");
        assert_eq!(model_family("grok-4"), "xai");
        assert_eq!(model_family("unknown-model"), "");
    }
}
