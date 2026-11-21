//! 运行时失败转移（failover）：按候选顺序尝试建立上游流。
//!
//! 设计口径：
//! * 候选来自租户模型路由（`tenant_model_routes`）+ key 池；没有路由时退化为
//!   现有行为（解析出的 `<provider>:<model>` + 池内加权取一条 key）；
//! * **只在首个流建立前转移**：`complete_stream` 返回 `Ok(stream)` 之后，流内
//!   再出现的错误不重试（此时用户已经看到部分增量，重放会产生重复内容）；
//! * 日志与错误信息只包含 provider/model，**不含 key**。

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use rand::Rng;

use super::{catalog, ChatRequest, DeltaStream, ModelInfo, Provider, ProviderError};
use crate::{
    providers::{self, Protocol},
    tenant::{self, PooledKey},
};

/// 一条可尝试的上游：provider + 上游模型名 + 可选取自租户池的 key。
///
/// `Debug` 手写：key 只输出是否存在的占位，避免误打日志泄露明文。
#[derive(Clone, PartialEq, Eq)]
pub struct Candidate {
    pub provider: String,
    pub upstream_model: String,
    pub key: Option<String>,
}

impl std::fmt::Debug for Candidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Candidate")
            .field("provider", &self.provider)
            .field("upstream_model", &self.upstream_model)
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// 构造候选 provider 的工厂。默认走 `catalog::build_by_id_with_key`；
/// 单测注入 mock，避免真实网络。
pub type Factory =
    Arc<dyn Fn(&str, Option<&str>) -> Result<Arc<dyn Provider>, ProviderError> + Send + Sync>;

pub struct FailoverProvider {
    requested: String,
    id: String,
    candidates: Vec<Candidate>,
    factory: Factory,
}

impl FailoverProvider {
    /// 按候选顺序构造；provider 在真正尝试时才构建（构建失败算该候选失败）。
    pub fn new(requested: impl Into<String>, candidates: Vec<Candidate>) -> Self {
        Self::with_factory(
            requested,
            candidates,
            Arc::new(catalog::build_by_id_with_key),
        )
    }

    pub fn with_factory(
        requested: impl Into<String>,
        candidates: Vec<Candidate>,
        factory: Factory,
    ) -> Self {
        let requested = requested.into();
        let id = format!("pool:{requested}");
        Self {
            requested,
            id,
            candidates,
            factory,
        }
    }

    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }

    /// 早失败：至少有一个候选的 provider 配置可构建（不做网络调用）。
    /// 保持"缺 key / 缺 base_url 在 run 启动时就明确失败"的既有行为；但只要还有
    /// 其他可构建的候选，就不因为某一个候选的配置问题挡住整次运行。
    pub fn precheck(&self) -> Result<(), ProviderError> {
        if self.candidates.is_empty() {
            return Err(ProviderError::config(format!(
                "no provider candidate for model {}",
                self.requested
            )));
        }
        let mut last_error = None;
        for candidate in &self.candidates {
            match (self.factory)(&candidate.provider, candidate.key.as_deref()) {
                Ok(_) => return Ok(()),
                Err(err) => last_error = Some(err),
            }
        }
        Err(append_context(
            last_error.expect("candidates is non-empty"),
            &self.candidates,
        ))
    }
}

#[async_trait]
impl Provider for FailoverProvider {
    fn id(&self) -> &str {
        &self.id
    }

    /// 协议取第一个候选的 preset 协议；没有候选时按 OpenAI 兼容协议。
    fn protocol(&self) -> Protocol {
        self.candidates
            .first()
            .and_then(|candidate| providers::find(&candidate.provider))
            .map(|preset| preset.protocol)
            .unwrap_or(Protocol::Openai)
    }

    async fn complete_stream(&self, request: ChatRequest) -> Result<DeltaStream, ProviderError> {
        if self.candidates.is_empty() {
            return Err(ProviderError::config(format!(
                "no provider candidate for model {}",
                self.requested
            )));
        }

        let mut last_error: Option<ProviderError> = None;
        for (idx, candidate) in self.candidates.iter().enumerate() {
            // 每个候选都用**自己的** upstream_model 覆盖请求模型；provider 按候选构建。
            let built = (self.factory)(&candidate.provider, candidate.key.as_deref());
            match built {
                Err(err) => last_error = Some(err),
                Ok(provider) => {
                    let mut attempt = request.clone();
                    attempt.model = candidate.upstream_model.clone();
                    match provider.complete_stream(attempt).await {
                        Ok(stream) => return Ok(stream),
                        Err(err) => {
                            // 取消是控制面信号，不视为上游故障，也不再尝试下一条。
                            if matches!(err, ProviderError::Cancelled) {
                                return Err(err);
                            }
                            last_error = Some(err);
                        }
                    }
                }
            }

            // 还有下一条候选才叫"转移"：日志只记 provider/model，不含 key。
            if let Some(next) = self.candidates.get(idx + 1) {
                tracing::warn!(
                    from = %candidate.provider,
                    to = %next.provider,
                    model = %next.upstream_model,
                    "provider failover"
                );
            }
        }

        let error = last_error.expect("candidates is non-empty");
        Err(append_context(error, &self.candidates))
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let Some(first) = self.candidates.first() else {
            return Err(ProviderError::config(format!(
                "no provider candidate for model {}",
                self.requested
            )));
        };
        let provider = (self.factory)(&first.provider, first.key.as_deref())?;
        provider.list_models().await
    }
}

/// 给最终错误补一句"尝试过哪些 provider"（key 不在其中）。
fn append_context(error: ProviderError, tried: &[Candidate]) -> ProviderError {
    let names: Vec<&str> = tried.iter().map(|c| c.provider.as_str()).collect();
    let suffix = format!("; tried providers: {}", names.join(", "));
    match error {
        ProviderError::Config(message) => ProviderError::Config(format!("{message}{suffix}")),
        ProviderError::Unsupported(message) => {
            ProviderError::Unsupported(format!("{message}{suffix}"))
        }
        ProviderError::Unauthorized(message) => {
            ProviderError::Unauthorized(format!("{message}{suffix}"))
        }
        ProviderError::Request { status, message } => ProviderError::Request {
            status,
            message: format!("{message}{suffix}"),
        },
        ProviderError::Upstream { status, message } => ProviderError::Upstream {
            status,
            message: format!("{message}{suffix}"),
        },
        ProviderError::RateLimited {
            retry_after_ms,
            message,
        } => ProviderError::RateLimited {
            retry_after_ms,
            message: format!("{message}{suffix}"),
        },
        ProviderError::Timeout(message) => ProviderError::Timeout(format!("{message}{suffix}")),
        ProviderError::Network(message) => ProviderError::Network(format!("{message}{suffix}")),
        ProviderError::Decode(message) => ProviderError::Decode(format!("{message}{suffix}")),
        ProviderError::StreamError { kind, message } => ProviderError::StreamError {
            kind,
            message: format!("{message}{suffix}"),
        },
        // `Cancelled` 没有可拼接的消息，保持原样。
        ProviderError::Cancelled => ProviderError::Cancelled,
    }
}

/// 按权重给出 key 的尝试顺序（不放回抽样）。
///
/// 至少覆盖池内每一条 key 一次；权重越大越可能排在前面。权重全 0 时保留原顺序。
fn ordered_keys(pool: &[PooledKey]) -> Vec<PooledKey> {
    let mut remaining: Vec<PooledKey> = pool.to_vec();
    let mut ordered: Vec<PooledKey> = Vec::with_capacity(remaining.len());
    while !remaining.is_empty() {
        let weights: Vec<u32> = remaining.iter().map(|key| key.weight).collect();
        let total: u64 = weights.iter().map(|weight| *weight as u64).sum();
        if total == 0 {
            ordered.append(&mut remaining);
            break;
        }
        let roll = rand::thread_rng().gen_range(0..total);
        match tenant::weighted_index(&weights, roll) {
            Some(idx) => ordered.push(remaining.remove(idx)),
            None => {
                ordered.append(&mut remaining);
                break;
            }
        }
    }
    ordered
}

/// 解析后的模型 → 候选列表。
///
/// 顺序：
/// 1. `lookup_model` 命中的全部启用路由，按 priority 升序 / weight 降序；
/// 2. 每条路由展开为该 provider 池内所有 key（加权顺序）；池为空则回落到平台 env（key=None）；
/// 3. 没有任何路由时退回 `resolved`（保持既有行为：解析出的 provider+model + 池内加权一条 key）；
/// 4. `resolved` 为空且无路由 → 空列表（调用方把解析错误抛给用户）。
pub fn plan_candidates(
    lookup_model: &str,
    resolved: Option<(String, String)>,
    routes: &[tenant::Route],
    pools: &HashMap<String, Vec<PooledKey>>,
) -> Vec<Candidate> {
    let hits = tenant::matching_routes(routes, lookup_model);
    if !hits.is_empty() {
        let mut candidates = Vec::new();
        for route in hits {
            match pools.get(&route.provider) {
                Some(pool) if !pool.is_empty() => {
                    for key in ordered_keys(pool) {
                        candidates.push(Candidate {
                            provider: route.provider.clone(),
                            upstream_model: route.upstream_model.clone(),
                            key: Some(key.key),
                        });
                    }
                }
                _ => candidates.push(Candidate {
                    provider: route.provider.clone(),
                    upstream_model: route.upstream_model.clone(),
                    key: None,
                }),
            }
        }
        return candidates;
    }

    let Some((provider, upstream_model)) = resolved else {
        return Vec::new();
    };
    let key = pools
        .get(&provider)
        .and_then(|pool| tenant::pick_key(pool))
        .map(|key| key.key.clone());
    vec![Candidate {
        provider,
        upstream_model,
        key,
    }]
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use futures_util::StreamExt;

    use async_trait::async_trait;

    use super::*;
    use crate::provider::{ChatMessage, StreamDelta};

    /// 记录每次请求的 model；`fail` 时返回 503。
    struct Mock {
        id: String,
        fail: bool,
        seen: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Provider for Mock {
        fn id(&self) -> &str {
            &self.id
        }

        fn protocol(&self) -> Protocol {
            Protocol::Openai
        }

        async fn complete_stream(
            &self,
            request: ChatRequest,
        ) -> Result<DeltaStream, ProviderError> {
            self.seen.lock().expect("seen lock").push(request.model);
            if self.fail {
                return Err(ProviderError::Upstream {
                    status: 503,
                    message: format!("{} is down", self.id),
                });
            }
            let deltas: Vec<Result<StreamDelta, ProviderError>> =
                vec![Ok(StreamDelta::Text("ok".into()))];
            Ok(Box::pin(futures_util::stream::iter(deltas)))
        }

        async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
            Ok(vec![ModelInfo::new("mock-model")])
        }
    }

    fn factory_for(failing: &'static [&'static str], seen: Arc<Mutex<Vec<String>>>) -> Factory {
        Arc::new(move |provider: &str, _key: Option<&str>| {
            Ok(Arc::new(Mock {
                id: provider.to_string(),
                fail: failing.contains(&provider),
                seen: seen.clone(),
            }) as Arc<dyn Provider>)
        })
    }

    fn candidate(provider: &str, model: &str) -> Candidate {
        Candidate {
            provider: provider.into(),
            upstream_model: model.into(),
            key: Some("sk-secret".into()),
        }
    }

    fn request() -> ChatRequest {
        ChatRequest::new("requested").with_messages(vec![ChatMessage::user("hi")])
    }

    #[tokio::test]
    async fn precheck_skips_unbuildable_candidates_but_fails_when_none_build() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_inner = seen.clone();
        let factory: Factory = Arc::new(move |provider: &str, _key: Option<&str>| {
            if provider == "missing" {
                return Err(ProviderError::config("missing key"));
            }
            Ok(Arc::new(Mock {
                id: provider.to_string(),
                fail: false,
                seen: seen_inner.clone(),
            }) as Arc<dyn Provider>)
        });
        let provider = FailoverProvider::with_factory(
            "requested",
            vec![candidate("missing", "m1"), candidate("ok", "m2")],
            factory.clone(),
        );
        provider.precheck().expect("second candidate builds");

        let provider =
            FailoverProvider::with_factory("requested", vec![candidate("missing", "m1")], factory);
        let error = provider.precheck().expect_err("nothing builds");
        assert!(error.to_string().contains("missing"), "{error}");
    }

    #[tokio::test]
    async fn falls_through_to_the_next_candidate_when_first_fails() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let provider = FailoverProvider::with_factory(
            "requested",
            vec![candidate("a", "model-a"), candidate("b", "model-b")],
            factory_for(&["a"], seen.clone()),
        );
        let mut stream = provider
            .complete_stream(request())
            .await
            .expect("second candidate should succeed");
        let first = stream.next().await.expect("one delta").expect("ok delta");
        assert_eq!(first, StreamDelta::Text("ok".into()));
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            ["model-a".to_string(), "model-b".to_string()],
            "每次尝试都把自己的 upstream_model 写进请求"
        );
    }

    #[tokio::test]
    async fn all_candidates_failing_reports_every_provider_and_no_key() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let provider = FailoverProvider::with_factory(
            "requested",
            vec![candidate("a", "model-a"), candidate("b", "model-b")],
            factory_for(&["a", "b"], seen),
        );
        let result = provider.complete_stream(request()).await;
        let error = match result {
            Ok(_) => panic!("all candidates must fail"),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(message.contains("a"), "{message}");
        assert!(message.contains("b"), "{message}");
        assert!(message.contains("tried providers"), "{message}");
        assert!(!message.contains("sk-secret"), "{message}");
    }

    #[tokio::test]
    async fn build_failure_counts_as_a_failed_candidate() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_inner = seen.clone();
        let factory: Factory = Arc::new(move |provider: &str, _key: Option<&str>| {
            if provider == "missing" {
                return Err(ProviderError::config("missing key"));
            }
            Ok(Arc::new(Mock {
                id: provider.to_string(),
                fail: false,
                seen: seen_inner.clone(),
            }) as Arc<dyn Provider>)
        });
        let provider = FailoverProvider::with_factory(
            "requested",
            vec![candidate("missing", "m1"), candidate("ok", "m2")],
            factory,
        );
        assert!(provider.complete_stream(request()).await.is_ok());
        assert_eq!(seen.lock().unwrap().as_slice(), ["m2".to_string()]);
    }

    #[tokio::test]
    async fn cancellation_is_not_a_failover_trigger() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_inner = seen.clone();
        let factory: Factory = Arc::new(move |provider: &str, _key: Option<&str>| {
            let _ = provider;
            Ok(Arc::new(CancelOnly {
                seen: seen_inner.clone(),
            }) as Arc<dyn Provider>)
        });
        let provider = FailoverProvider::with_factory(
            "requested",
            vec![candidate("a", "m1"), candidate("b", "m2")],
            factory,
        );
        assert!(matches!(
            provider.complete_stream(request()).await,
            Err(ProviderError::Cancelled)
        ));
        assert_eq!(seen.lock().unwrap().len(), 1, "取消后不再尝试下一条");
    }

    struct CancelOnly {
        seen: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait]
    impl Provider for CancelOnly {
        fn id(&self) -> &str {
            "cancel"
        }

        fn protocol(&self) -> Protocol {
            Protocol::Openai
        }

        async fn complete_stream(
            &self,
            request: ChatRequest,
        ) -> Result<DeltaStream, ProviderError> {
            self.seen.lock().expect("seen lock").push(request.model);
            Err(ProviderError::Cancelled)
        }

        async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn list_models_and_id_delegate_to_the_first_candidate() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let provider = FailoverProvider::with_factory(
            "openai:gpt-5",
            vec![candidate("a", "model-a")],
            factory_for(&[], seen),
        );
        assert_eq!(provider.id(), "pool:openai:gpt-5");
        assert_eq!(provider.protocol(), Protocol::Openai);
        let models = provider.list_models().await.expect("delegated");
        assert_eq!(models[0].id, "mock-model");
    }

    fn route(
        model: &str,
        provider: &str,
        upstream: &str,
        priority: i32,
        weight: i32,
    ) -> tenant::Route {
        tenant::Route {
            model: model.into(),
            provider: provider.into(),
            upstream_model: upstream.into(),
            priority,
            weight,
            enabled: true,
        }
    }

    fn pool(key: &str, weight: u32) -> Vec<PooledKey> {
        vec![PooledKey {
            key: key.into(),
            weight,
        }]
    }

    #[test]
    fn plan_orders_routes_then_expands_every_pool_key() {
        let routes = vec![
            route("gpt-5", "openai", "gpt-5", 200, 1),
            route("gpt-5", "openrouter", "gpt-5-openrouter", 100, 5),
        ];
        let mut pools: HashMap<String, Vec<PooledKey>> = HashMap::new();
        pools.insert(
            "openrouter".into(),
            vec![
                PooledKey {
                    key: "r1".into(),
                    weight: 1,
                },
                PooledKey {
                    key: "r2".into(),
                    weight: 1,
                },
            ],
        );
        pools.insert("openai".into(), pool("o1", 1));

        let candidates = plan_candidates(
            "openai:gpt-5",
            Some(("openai".into(), "gpt-5".into())),
            &routes,
            &pools,
        );
        assert_eq!(candidates.len(), 3, "优先路由 2 条 key + 次优 1 条");
        assert_eq!(candidates[0].provider, "openrouter");
        assert_eq!(candidates[0].upstream_model, "gpt-5-openrouter");
        assert_eq!(candidates[1].provider, "openrouter");
        assert_eq!(candidates[2].provider, "openai");
        let mut keys: Vec<&str> = candidates[..2]
            .iter()
            .map(|c| c.key.as_deref().expect("pool key"))
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["r1", "r2"], "池内 key 各尝试一次");
    }

    #[test]
    fn plan_falls_back_to_platform_env_when_provider_has_no_pool() {
        let routes = vec![route("gpt-5", "openai", "gpt-5", 100, 1)];
        let candidates = plan_candidates(
            "gpt-5",
            Some(("openai".into(), "gpt-5".into())),
            &routes,
            &HashMap::new(),
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].provider, "openai");
        assert!(candidates[0].key.is_none(), "无租户 key 走 env");
    }

    #[test]
    fn plan_without_routes_keeps_the_resolved_provider_and_model() {
        let mut pools: HashMap<String, Vec<PooledKey>> = HashMap::new();
        pools.insert("openai".into(), pool("sk-pool", 1));
        let candidates = plan_candidates(
            "openai:gpt-5",
            Some(("openai".into(), "gpt-5".into())),
            &[],
            &pools,
        );
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].provider, "openai");
        assert_eq!(candidates[0].upstream_model, "gpt-5");
        assert_eq!(candidates[0].key.as_deref(), Some("sk-pool"));
    }

    #[test]
    fn plan_yields_nothing_when_there_is_no_route_and_no_resolution() {
        assert!(plan_candidates("nope", None, &[], &HashMap::new()).is_empty());
    }

    #[test]
    fn candidate_debug_never_leaks_the_key() {
        let debug = format!("{:?}", candidate("a", "m"));
        assert!(!debug.contains("sk-secret"), "{debug}");
        assert!(debug.contains("redacted"), "{debug}");
    }
}
