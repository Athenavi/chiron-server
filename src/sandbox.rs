//! 每用户 Agent 容器管理（sysbox）。
//!
//! 设计口径：容器常驻（`sleep infinity`），执行通过 `docker exec` 注入；
//! 数据目录按 scope 挂到 `/workspace`。容器只带标签与最小权限，具体运行时
//! （sysbox-runc / runc）由部署侧决定。

// B4 先把运行时 API 全部落地，真正的调用方在 B5 的工具循环里；
// 在接线之前不要让 dead_code 把 clippy 门禁打红。
#![allow(dead_code)]

use std::{
    collections::HashMap,
    env,
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::Mutex,
    time::Duration,
};

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::{io::AsyncReadExt, process::Command, sync::oneshot};

use crate::error::AppError;

/// `docker exec` 单次调用上限；溢出即截断，不把大输出带进内存/事件流。
pub const EXEC_MAX_OUTPUT_BYTES: usize = 64 * 1024;
/// `list_dir` 单次返回上限，避免被大目录拖死。
pub const LIST_DIR_MAX_ENTRIES: usize = 1000;
/// 单文件读取上限（8 MiB）；更大的内容走容器内命令处理。
pub const MAX_FILE_READ_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SandboxScope {
    User,
    Project,
}

impl SandboxScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SandboxConfig {
    pub scope: SandboxScope,
    pub image: String,
    pub runtime: String,
    pub data_root: PathBuf,
    pub network: String,
    pub memory: String,
    pub cpus: String,
    pub pids_limit: i32,
    /// 空闲多久回收容器（秒），默认 30 分钟。
    pub idle_timeout_secs: u64,
    /// 单条命令超时（秒），超时即取消并 `docker rm -f`。
    pub exec_timeout_secs: u64,
    /// 单条命令 stdout/stderr 上限（字节）。
    pub exec_max_output_bytes: usize,
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

impl SandboxConfig {
    pub fn from_env() -> Self {
        let scope = match env::var("SANDBOX_SCOPE")
            .unwrap_or_else(|_| "user".into())
            .as_str()
        {
            "project" => SandboxScope::Project,
            _ => SandboxScope::User,
        };
        Self {
            scope,
            image: env::var("SANDBOX_IMAGE").unwrap_or_else(|_| "chiron-agent:py314".into()),
            runtime: env::var("SANDBOX_DOCKER_RUNTIME").unwrap_or_else(|_| "sysbox-runc".into()),
            data_root: env::var("SANDBOX_DATA_ROOT")
                .unwrap_or_else(|_| "/var/agent-data".into())
                .into(),
            network: env::var("SANDBOX_NETWORK").unwrap_or_else(|_| "none".into()),
            memory: env::var("SANDBOX_MEMORY").unwrap_or_else(|_| "512m".into()),
            cpus: env::var("SANDBOX_CPUS").unwrap_or_else(|_| "1.0".into()),
            pids_limit: env::var("SANDBOX_PIDS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(256),
            idle_timeout_secs: env_u64("SANDBOX_IDLE_SECS", 30 * 60),
            exec_timeout_secs: env_u64("SANDBOX_EXEC_TIMEOUT_SECS", 120),
            exec_max_output_bytes: env_u64("SANDBOX_EXEC_MAX_OUTPUT", EXEC_MAX_OUTPUT_BYTES as u64)
                as usize,
        }
    }

    pub fn idle_timeout(&self) -> Duration {
        Duration::from_secs(self.idle_timeout_secs)
    }

    pub fn exec_timeout(&self) -> Duration {
        Duration::from_secs(self.exec_timeout_secs)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SandboxKey<'a> {
    pub tenant_id: i16,
    pub user_id: &'a str,
    pub project_id: Option<&'a str>,
}

impl SandboxKey<'_> {
    fn scope_id(&self) -> String {
        match (self.project_id, self.project_id) {
            (Some(p), _) if !p.is_empty() => p.to_string(),
            _ => self.user_id.to_string(),
        }
    }
}

fn sanitize_segment(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.len() > 48 {
        out.truncate(48);
    }
    if out.is_empty() {
        out.push_str("default");
    }
    // 纯点段（"." / ".."）会逃出当前目录，必须归一成安全名称。
    if out.chars().all(|c| c == '.') {
        out.clear();
        out.push_str("default");
    }
    out
}

/// 套餐 tier → Docker runtime；不认识的 tier 返回 None（沿用全局 runtime）。
///
/// 不做静默降级：映射到的 runtime 未安装时由 `docker run` 明确失败。
pub fn runtime_for_tier(tier: &str) -> Option<&'static str> {
    match tier.trim().to_ascii_lowercase().as_str() {
        "sysbox" => Some("sysbox-runc"),
        "gvisor" => Some("runsc"),
        "microvm" => Some("kata-runtime"),
        _ => None,
    }
}

/// 套餐 egress → Docker network；未知值返回 None（沿用全局 network）。
pub fn egress_network(mode: &str) -> Option<&'static str> {
    match mode.trim().to_ascii_lowercase().as_str() {
        "" => Some("none"),
        "bridge" => Some("bridge"),
        // 代理模式仍需 bridge 才能连到宿主代理端口；白名单在代理侧生效。
        "proxy" => Some("bridge"),
        "none" => Some("none"),
        _ => None,
    }
}

pub fn container_name(cfg: &SandboxConfig, key: SandboxKey<'_>) -> String {
    container_name_tiered(cfg, key, None)
}

/// 带 tier 后缀的容器名：套餐换档不会复用旧 runtime 的容器。
fn tenant_segment(key: SandboxKey<'_>) -> String {
    match crate::tenant::current_tenant() {
        Some(id) => sanitize_segment(&id.to_string()[..8]),
        None => sanitize_segment(&key.tenant_id.to_string()),
    }
}

fn tenant_label() -> String {
    crate::tenant::current_tenant()
        .map(|id| id.to_string())
        .unwrap_or_else(|| "local".to_string())
}

pub fn container_name_tiered(
    cfg: &SandboxConfig,
    key: SandboxKey<'_>,
    tier: Option<&str>,
) -> String {
    let scope = cfg.scope.as_str();
    let tenant = tenant_segment(key);
    let user = sanitize_segment(key.user_id);
    let mut name = format!("chiron-agent-{scope}-{tenant}-{user}");
    if cfg.scope == SandboxScope::Project {
        name.push('-');
        name.push_str(&sanitize_segment(&key.scope_id()));
    }
    if let Some(tier) = tier.map(str::trim).filter(|t| !t.is_empty()) {
        name.push('-');
        name.push_str(&sanitize_segment(tier));
    }
    if name.len() > 63 {
        name.truncate(63);
    }
    name
}

pub fn workspace_dir(cfg: &SandboxConfig, key: SandboxKey<'_>) -> PathBuf {
    let mut p = cfg
        .data_root
        .join(tenant_segment(key))
        .join(sanitize_segment(key.user_id));
    if cfg.scope == SandboxScope::Project {
        p = p.join(sanitize_segment(&key.scope_id()));
    }
    p
}

/// 词法钳制：只接受 workspace 内相对路径，拒绝绝对路径、盘符、`..`、空段。
///
/// 这是第一道闸；符号链接逃逸由 [`resolve_in_workspace`] 在真正的
/// 文件系统上再校验一次。两者都过才允许落盘。
pub fn tenant_rel_path(rel: &str) -> Result<PathBuf, AppError> {
    let trimmed = rel.trim();
    if trimmed.is_empty() || trimmed == "." {
        return Ok(PathBuf::from("."));
    }
    if trimmed.starts_with('/') || trimmed.starts_with('\\') {
        return Err(AppError::forbidden("absolute paths are not allowed"));
    }
    // Windows 盘符/UNC 在 Linux 上也会被 `Path` 解析成 Prefix，一并拒掉。
    if trimmed.contains(':') {
        return Err(AppError::forbidden("path must stay inside workspace"));
    }
    let mut out = PathBuf::new();
    for comp in Path::new(trimmed).components() {
        match comp {
            Component::Normal(seg) => out.push(seg),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(AppError::forbidden("path must stay inside workspace"));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(AppError::forbidden("absolute paths are not allowed"));
            }
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    Ok(out)
}

/// 在真实文件系统上确认目标（含符号链接解析）仍落在 `workspace` 内。
///
/// 目标不存在时回溯到最近的已存在祖先再校验，所以"先写一个还不存在的
/// 深层文件"也能通过，而"父目录符号链接指到 workspace 外"会被拒。
pub async fn resolve_in_workspace(workspace: &Path, rel: &str) -> Result<PathBuf, AppError> {
    let rel_path = tenant_rel_path(rel)?;
    let base = tokio::fs::canonicalize(workspace).await.map_err(|e| {
        tracing::error!(error = %e, "sandbox workspace missing");
        AppError::internal("agent workspace unavailable")
    })?;
    let target = base.join(&rel_path);

    // 逐段检查已存在的组件：符号链接必须解析到 base 之内；悬空链接直接拒。
    let mut cur = base.clone();
    for comp in rel_path.components() {
        cur.push(comp);
        match tokio::fs::symlink_metadata(&cur).await {
            Ok(md) if md.file_type().is_symlink() => {
                let real = tokio::fs::canonicalize(&cur).await.map_err(|e| {
                    tracing::warn!(error = %e, path = %cur.display(), "dangling symlink");
                    AppError::forbidden("path escapes agent workspace")
                })?;
                if !real.starts_with(&base) {
                    return Err(AppError::forbidden("path escapes agent workspace"));
                }
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => {
                tracing::error!(error = %e, "stat sandbox path failed");
                return Err(AppError::internal("cannot access agent workspace"));
            }
        }
    }

    // 已存在部分再整体 canonicalize 一次，覆盖"链接 -> 链接"的链式情况。
    let mut probe = target.clone();
    loop {
        match tokio::fs::canonicalize(&probe).await {
            Ok(real) => {
                if !real.starts_with(&base) {
                    return Err(AppError::forbidden("path escapes agent workspace"));
                }
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if probe == base {
                    break;
                }
                match probe.parent() {
                    Some(p) if p.starts_with(&base) => probe = p.to_path_buf(),
                    _ => return Err(AppError::forbidden("path escapes agent workspace")),
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "resolve sandbox path failed");
                return Err(AppError::internal("cannot access agent workspace"));
            }
        }
    }
    Ok(target)
}

/// POSIX 单引号转义，用于把参数安全拼进 `sh -lc`。
pub fn shell_quote(raw: &str) -> String {
    if !raw.is_empty()
        && raw.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '@' | '%' | '_' | '+' | '=' | ':' | ',' | '.' | '/' | '-')
        })
    {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('\'');
    for c in raw.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// 按字节上限截断输出，切在 UTF-8 边界上，并标注被截掉多少字节。
pub fn truncate_output(bytes: &[u8], cap: usize) -> (String, bool) {
    if bytes.len() <= cap {
        return (String::from_utf8_lossy(bytes).into_owned(), false);
    }
    let mut end = cap;
    while end > 0 && !is_char_boundary(bytes, end) {
        end -= 1;
    }
    let mut out = String::from_utf8_lossy(&bytes[..end]).into_owned();
    out.push_str(&format!(
        "\n…[truncated {} bytes]",
        bytes.len().saturating_sub(end)
    ));
    (out, true)
}

fn is_char_boundary(bytes: &[u8], idx: usize) -> bool {
    match bytes.get(idx) {
        Some(b) => (*b & 0xC0) != 0x80,
        None => true,
    }
}

/// 空闲判定：`last_seen` 在未来（时钟回拨）视为不空闲，避免误杀。
pub fn is_idle(last_seen: DateTime<Utc>, now: DateTime<Utc>, idle: Duration) -> bool {
    let elapsed = match now.signed_duration_since(last_seen) {
        d if d.num_milliseconds() < 0 => return false,
        d => d.num_milliseconds() as u64,
    };
    elapsed >= idle.as_millis() as u64
}

/// 构造 `docker run` 参数（纯函数，便于单测；不接触 Docker）。
pub fn build_run_args(cfg: &SandboxConfig, key: SandboxKey<'_>, workspace: &Path) -> Vec<String> {
    let name = container_name(cfg, key);
    build_run_args_named(cfg, key, name, workspace, None, None)
}

fn build_run_args_named(
    cfg: &SandboxConfig,
    key: SandboxKey<'_>,
    name: String,
    workspace: &Path,
    tier: Option<&str>,
    proxy: Option<(&str, &str)>,
) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        name,
        "--runtime".to_string(),
        cfg.runtime.clone(),
        "--network".to_string(),
        cfg.network.clone(),
        "--cap-drop".to_string(),
        "ALL".to_string(),
        "--security-opt".to_string(),
        "no-new-privileges".to_string(),
        "--read-only".to_string(),
        "--tmpfs".to_string(),
        "/tmp:rw,size=64m".to_string(),
        "--user".to_string(),
        "1000:1000".to_string(),
        "--memory".to_string(),
        cfg.memory.clone(),
        "--memory-swap".to_string(),
        cfg.memory.clone(),
        "--cpus".to_string(),
        cfg.cpus.clone(),
        "--pids-limit".to_string(),
        cfg.pids_limit.to_string(),
        "--label".to_string(),
        "chiron.managed-by=agent-platform".to_string(),
        "--label".to_string(),
        format!("chiron.tenant={}", key.tenant_id),
        "--label".to_string(),
        format!("chiron.tenant-id={}", tenant_label()),
        "--label".to_string(),
        format!("chiron.user={}", sanitize_segment(key.user_id)),
        "--label".to_string(),
        format!("chiron.scope={}", cfg.scope.as_str()),
        "--label".to_string(),
        format!("chiron.scope-id={}", sanitize_segment(&key.scope_id())),
        "--label".to_string(),
        format!(
            "chiron.tier={}",
            tier.map(str::trim)
                .filter(|t| !t.is_empty())
                .unwrap_or("default")
        ),
        "-e".to_string(),
        "HOME=/workspace".to_string(),
        "-e".to_string(),
        "TMPDIR=/tmp".to_string(),
        "-v".to_string(),
        format!("{}:/workspace:rw", workspace.display()),
        "-w".to_string(),
        "/workspace".to_string(),
        cfg.image.clone(),
        "sleep".to_string(),
        "infinity".to_string(),
    ];
    if let Some((host, url)) = proxy {
        let host_only = host.split(':').next().unwrap_or(host);
        args.push("--add-host".to_string());
        args.push(format!("{host_only}:host-gateway"));
        for (key, value) in [
            ("HTTP_PROXY", url),
            ("HTTPS_PROXY", url),
            ("ALL_PROXY", url),
            ("http_proxy", url),
            ("https_proxy", url),
            ("NO_PROXY", "localhost,127.0.0.1"),
        ] {
            args.push("-e".to_string());
            args.push(format!("{key}={value}"));
        }
    }
    args.dedup();
    args
}

#[derive(Debug, Serialize)]
pub struct ContainerInfo {
    pub name: String,
    pub status: String,
    pub labels: String,
}

/// 单次 `docker exec` 的约束。
#[derive(Clone, Debug)]
pub struct ExecOptions {
    pub timeout: Duration,
    pub max_output_bytes: usize,
    /// 容器内工作目录，相对 `/workspace`；非空且非 `.` 时会先过路径钳制。
    pub workdir: Option<String>,
}

impl Default for ExecOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(120),
            max_output_bytes: EXEC_MAX_OUTPUT_BYTES,
            workdir: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ExecOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub timed_out: bool,
    pub cancelled: bool,
    pub duration_ms: u64,
}

impl ExecOutput {
    pub fn success(&self) -> bool {
        !self.timed_out && !self.cancelled && self.exit_code == 0
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DirEntryInfo {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

/// 容器内路径：`/workspace` + 已钳制的相对路径。
pub fn container_path(rel: &str) -> Result<String, AppError> {
    let rel_path = tenant_rel_path(rel)?;
    if rel_path == Path::new(".") {
        return Ok("/workspace".to_string());
    }
    // 容器内路径一律 POSIX：Windows 宿主上 `Path::display()` 会给出反斜杠。
    let rel = rel_path.display().to_string().replace('\\', "/");
    Ok(format!("/workspace/{rel}"))
}

/// 构造 `docker exec` 参数（纯函数；单测不接触 Docker）。
pub fn build_exec_args(
    container: &str,
    command: &str,
    opts: &ExecOptions,
) -> Result<Vec<String>, AppError> {
    let workdir = match opts.workdir.as_deref() {
        Some(w) if !w.trim().is_empty() => container_path(w)?,
        _ => "/workspace".to_string(),
    };
    Ok(vec![
        "exec".to_string(),
        "-i".to_string(),
        "-w".to_string(),
        workdir,
        container.to_string(),
        "sh".to_string(),
        "-lc".to_string(),
        command.to_string(),
    ])
}

/// 构造 `git -C <repo> --no-pager <sub> ...` 命令串（纯函数）。
pub fn build_git_command(repo_rel: &str, sub: &str, args: &[String]) -> Result<String, AppError> {
    let repo = container_path(repo_rel)?;
    let mut out = format!(
        "git -C {} --no-pager {}",
        shell_quote(&repo),
        shell_quote(sub)
    );
    for a in args {
        out.push(' ');
        out.push_str(&shell_quote(a));
    }
    Ok(out)
}

pub struct SandboxManager {
    cfg: SandboxConfig,
    /// BYOK/代理令牌密钥（来自控制库 SECRETS_KEY）；空 = 不启用代理。
    secrets_key: String,
    /// 沙箱内可见的代理地址（host:port）。
    egress_proxy_addr: String,
    /// 最近一次活动时间（容器名 -> 时间），供空闲回收使用。
    last_seen: Mutex<HashMap<String, DateTime<Utc>>>,
}

impl SandboxManager {
    pub fn new(cfg: SandboxConfig) -> Self {
        Self {
            cfg,
            secrets_key: String::new(),
            egress_proxy_addr: String::new(),
            last_seen: Mutex::new(HashMap::new()),
        }
    }

    /// 启用 egress 代理：`egress_mode=proxy` 时给容器注入 HTTP(S)_PROXY 与租户令牌。
    pub fn with_egress(mut self, secrets_key: String, proxy_addr: String) -> Self {
        self.secrets_key = secrets_key;
        self.egress_proxy_addr = proxy_addr;
        self
    }

    pub fn cfg(&self) -> &SandboxConfig {
        &self.cfg
    }

    /// 记录一次活动；B5 的工具循环在每次工具调用后调用。
    pub fn touch(&self, name: &str) {
        if let Ok(mut guard) = self.last_seen.lock() {
            guard.insert(name.to_string(), Utc::now());
        }
    }

    pub fn last_seen(&self, name: &str) -> Option<DateTime<Utc>> {
        self.last_seen
            .lock()
            .ok()
            .and_then(|g| g.get(name).copied())
    }

    /// 判定并回收空闲容器（按 `last_seen`，未知容器以当前时间登记）。
    pub async fn reclaim_idle(&self, idle: Duration) -> Result<Vec<String>, AppError> {
        let now = Utc::now();
        let mut reclaimed = Vec::new();
        for c in self.list_containers().await? {
            let seen = self.last_seen(&c.name).unwrap_or_else(|| {
                self.touch(&c.name);
                now
            });
            if !is_idle(seen, now, idle) {
                continue;
            }
            if let Err(e) = self.stop_container(&c.name).await {
                tracing::warn!(error = %e, name = %c.name, "stop idle container failed");
            }
            self.remove_container(&c.name).await?;
            if let Ok(mut guard) = self.last_seen.lock() {
                guard.remove(&c.name);
            }
            reclaimed.push(c.name);
        }
        Ok(reclaimed)
    }

    /// 用户删除时的清理：按标签清掉该用户的全部容器，再删工作区目录。
    pub async fn cleanup_user(&self, tenant_id: i16, user_id: &str) -> Result<(), AppError> {
        let removed = self.remove_user_containers(tenant_id, user_id).await?;
        tracing::info!(tenant_id, removed, "cleaned user containers");

        // 只删 data_root/{tenant}/{user}，不允许越出数据根。
        let user_dir = self
            .cfg
            .data_root
            .join(sanitize_segment(&tenant_id.to_string()))
            .join(sanitize_segment(user_id));
        let root = &self.cfg.data_root;
        if user_dir.starts_with(root) && user_dir != *root {
            match tokio::fs::remove_dir_all(&user_dir).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::error!(error = %e, path = %user_dir.display(), "remove workspace failed");
                    return Err(AppError::internal("cannot clean agent workspace"));
                }
            }
        }
        Ok(())
    }

    pub async fn ensure_container(&self, key: SandboxKey<'_>) -> Result<String, AppError> {
        let workspace = workspace_dir(&self.cfg, key);
        tokio::fs::create_dir_all(&workspace).await.map_err(|e| {
            tracing::error!(error = %e, "create sandbox workspace failed");
            AppError::internal("cannot create agent workspace")
        })?;
        // 套餐 tier 决定 runtime 与容器名后缀；未知值明确失败，不静默降级。
        let tier = crate::tenant::current_tier().filter(|t| !t.trim().is_empty());
        let mut cfg = self.cfg.clone();
        if let Some(tier) = tier.as_deref() {
            match runtime_for_tier(tier) {
                Some(runtime) => {
                    if runtime != cfg.runtime {
                        tracing::info!(
                            tier = %tier, runtime = %runtime,
                            default = %cfg.runtime,
                            "sandbox runtime selected by plan tier"
                        );
                    }
                    cfg.runtime = runtime.to_string();
                }
                None => {
                    return Err(AppError::bad_request(format!(
                        "unknown isolation tier: {tier}"
                    )))
                }
            }
        }
        // egress 策略：none = 完全断网；proxy = 注入租户代理；未知值明确失败。
        let mut proxy: Option<(String, String)> = None;
        if let Some(mode) = crate::tenant::current_egress().filter(|m| !m.trim().is_empty()) {
            match egress_network(&mode) {
                Some(network) => cfg.network = network.to_string(),
                None => {
                    return Err(AppError::bad_request(format!(
                        "unknown egress mode: {mode}"
                    )))
                }
            }
            if mode.eq_ignore_ascii_case("proxy") {
                let addr = self.egress_proxy_addr.trim();
                if addr.is_empty() || self.secrets_key.trim().is_empty() {
                    return Err(AppError::new(
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        "egress_unavailable",
                        "egress_mode=proxy but proxy is not configured",
                    ));
                }
                let tenant = crate::tenant::current_tenant()
                    .ok_or_else(|| AppError::internal("tenant context missing for egress proxy"))?;
                let token = crate::egress::proxy_token(&self.secrets_key, tenant);
                let host = addr.split(':').next().unwrap_or(addr).to_string();
                proxy = Some((host, format!("http://{tenant}:{token}@{addr}")));
            }
        }
        let name = container_name_tiered(&cfg, key, tier.as_deref());

        let existing = Command::new("docker")
            .args(["inspect", "--type", "container", &name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        if matches!(existing, Ok(s) if s.success()) {
            self.touch(&name);
            return Ok(name);
        }

        let proxy_arg = proxy
            .as_ref()
            .map(|(host, url)| (host.as_str(), url.as_str()));
        let args = build_run_args_named(
            &cfg,
            key,
            name.clone(),
            &workspace,
            tier.as_deref(),
            proxy_arg,
        );
        let output = Command::new("docker")
            .args(&args)
            .output()
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "docker run failed to spawn");
                AppError::internal("docker is not available")
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::error!(%stderr, "docker run failed");
            return Err(AppError::internal(format!(
                "agent container start failed: {}",
                stderr.trim()
            )));
        }
        self.touch(&name);
        Ok(name)
    }

    /// 按标签批量删除某用户的容器（含 project scope），返回删除个数。
    pub async fn remove_user_containers(
        &self,
        tenant_id: i16,
        user_id: &str,
    ) -> Result<usize, AppError> {
        let tenant_label = format!("label=chiron.tenant={tenant_id}");
        let user_label = format!("label=chiron.user={}", sanitize_segment(user_id));
        let args = vec![
            "ps".to_string(),
            "-aq".to_string(),
            "--filter".to_string(),
            "label=chiron.managed-by=agent-platform".to_string(),
            "--filter".to_string(),
            tenant_label,
            "--filter".to_string(),
            user_label,
        ];
        let output = Command::new("docker")
            .args(&args)
            .output()
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "docker ps failed to spawn");
                AppError::internal("docker is not available")
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(%stderr, "docker ps by label failed");
            return Ok(0);
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let ids: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        let mut removed = 0;
        for id in ids {
            let rm = Command::new("docker").args(["rm", "-f", id]).output().await;
            match rm {
                Ok(o) if o.status.success() => removed += 1,
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    tracing::warn!(%stderr, %id, "docker rm failed during cleanup");
                }
                Err(e) => tracing::warn!(error = %e, %id, "docker rm failed to spawn"),
            }
        }
        Ok(removed)
    }

    /// 按控制库租户 UUID 删除全部容器（暂停/下线时使用）。
    pub async fn remove_tenant_containers(&self, tenant_id: &str) -> Result<usize, AppError> {
        let tenant_label = format!("label=chiron.tenant-id={}", sanitize_segment(tenant_id));
        let args = vec![
            "ps".to_string(),
            "-aq".to_string(),
            "--filter".to_string(),
            "label=chiron.managed-by=agent-platform".to_string(),
            "--filter".to_string(),
            tenant_label,
        ];
        let output = Command::new("docker")
            .args(&args)
            .output()
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "docker ps failed to spawn");
                AppError::internal("docker is not available")
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(%stderr, "docker ps by tenant label failed");
            return Ok(0);
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut removed = 0;
        for id in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            match Command::new("docker")
                .args(["rm", "-f", id])
                .output()
                .await
            {
                Ok(o) if o.status.success() => removed += 1,
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    tracing::warn!(%stderr, %id, "docker rm failed during tenant cleanup");
                }
                Err(e) => tracing::warn!(error = %e, %id, "docker rm failed to spawn"),
            }
        }
        Ok(removed)
    }

    pub async fn remove_container(&self, name: &str) -> Result<(), AppError> {
        let output = Command::new("docker")
            .args(["rm", "-f", name])
            .output()
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "docker rm failed to spawn");
                AppError::internal("docker is not available")
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(%stderr, %name, "docker rm failed");
        }
        Ok(())
    }

    pub async fn list_containers(&self) -> Result<Vec<ContainerInfo>, AppError> {
        let output = Command::new("docker")
            .args([
                "ps",
                "-a",
                "--filter",
                "label=chiron.managed-by=agent-platform",
                "--format",
                "{{.Names}}\t{{.Status}}\t{{.Labels}}",
            ])
            .output()
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "docker ps failed to spawn");
                AppError::internal("docker is not available")
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AppError::internal(format!(
                "docker ps failed: {}",
                stderr.trim()
            )));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut out = Vec::new();
        for line in text.lines() {
            let mut parts = line.splitn(3, '\t');
            let Some(name) = parts.next() else { continue };
            let status = parts.next().unwrap_or("").to_string();
            let labels = parts.next().unwrap_or("").to_string();
            if !name.is_empty() {
                out.push(ContainerInfo {
                    name: name.to_string(),
                    status,
                    labels,
                });
            }
        }
        Ok(out)
    }

    fn default_exec_opts(&self) -> ExecOptions {
        ExecOptions {
            timeout: self.cfg.exec_timeout(),
            max_output_bytes: self.cfg.exec_max_output_bytes,
            workdir: None,
        }
    }

    /// 停止容器（保留容器本身，`docker rm -f` 走 [`Self::remove_container`]）。
    pub async fn stop_container(&self, name: &str) -> Result<(), AppError> {
        let output = Command::new("docker")
            .args(["stop", "-t", "5", name])
            .output()
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, %name, "docker stop failed to spawn");
                AppError::internal("docker is not available")
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(%stderr, %name, "docker stop failed");
        }
        Ok(())
    }

    pub async fn exec(
        &self,
        container: &str,
        command: &str,
        opts: &ExecOptions,
    ) -> Result<ExecOutput, AppError> {
        self.exec_cancellable(container, command, opts, None).await
    }

    /// 带上限执行命令。超时或外部取消都会 `docker rm -f` 容器，
    /// 因为 `docker exec` 客户端被杀不会带走容器内进程。
    pub async fn exec_cancellable(
        &self,
        container: &str,
        command: &str,
        opts: &ExecOptions,
        cancel: Option<oneshot::Receiver<()>>,
    ) -> Result<ExecOutput, AppError> {
        let args = build_exec_args(container, command, opts)?;
        let timeout = if opts.timeout.is_zero() {
            self.cfg.exec_timeout()
        } else {
            opts.timeout
        };
        let cap = if opts.max_output_bytes == 0 {
            self.cfg.exec_max_output_bytes
        } else {
            opts.max_output_bytes
        };

        let started = std::time::Instant::now();
        let mut child = Command::new("docker")
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                tracing::error!(error = %e, "docker exec failed to spawn");
                AppError::internal("docker is not available")
            })?;

        // 多读 1 字节，好让 truncate_output 知道确实溢出了。
        let read_cap = cap.saturating_add(1);
        let stdout_task = child
            .stdout
            .take()
            .map(|s| tokio::spawn(read_capped(s, read_cap)));
        let stderr_task = child
            .stderr
            .take()
            .map(|s| tokio::spawn(read_capped(s, read_cap)));

        let mut cancel = cancel;
        let mut timed_out = false;
        let mut cancelled = false;
        // 作用域单独包一层，确保 wait 借用到 child 的借用先结束，
        // 后面超时分支才能 `start_kill`。
        let outcome = {
            let wait_fut = async {
                match cancel.take() {
                    Some(rx) => tokio::select! {
                        res = child.wait() => WaitOutcome::Exited(res),
                        _ = rx => WaitOutcome::Cancelled,
                    },
                    None => WaitOutcome::Exited(child.wait().await),
                }
            };
            tokio::time::timeout(timeout, wait_fut).await
        };

        let exit_code = match outcome {
            Ok(WaitOutcome::Exited(Ok(st))) => st.code().unwrap_or(-1),
            Ok(WaitOutcome::Exited(Err(e))) => {
                tracing::error!(error = %e, "docker exec wait failed");
                return Err(AppError::internal("agent command failed"));
            }
            Ok(WaitOutcome::Cancelled) => {
                cancelled = true;
                let _ = child.start_kill();
                let _ = child.wait().await;
                -1
            }
            Err(_) => {
                timed_out = true;
                let _ = child.start_kill();
                let _ = child.wait().await;
                -1
            }
        };

        if timed_out || cancelled {
            tracing::warn!(%container, timed_out, cancelled, "agent command cancelled");
            if let Err(e) = self.remove_container(container).await {
                tracing::warn!(error = %e, %container, "remove container after cancel failed");
            }
        }

        let (stdout, out_truncated) = truncate_output(&finish_capture(stdout_task).await, cap);
        let (stderr, err_truncated) = truncate_output(&finish_capture(stderr_task).await, cap);

        Ok(ExecOutput {
            exit_code,
            stdout,
            stderr,
            truncated: out_truncated || err_truncated,
            timed_out,
            cancelled,
            duration_ms: started.elapsed().as_millis() as u64,
        })
    }

    // ---- Git：一律经 `docker exec` 在 `/workspace` 内执行 ----

    pub async fn git(
        &self,
        container: &str,
        repo_rel: &str,
        sub: &str,
        args: &[String],
    ) -> Result<ExecOutput, AppError> {
        let cmd = build_git_command(repo_rel, sub, args)?;
        self.exec(container, &cmd, &self.default_exec_opts()).await
    }

    pub async fn git_init(&self, container: &str, repo_rel: &str) -> Result<ExecOutput, AppError> {
        let repo = shell_quote(&container_path(repo_rel)?);
        let cmd = format!("set -e; mkdir -p {repo}; git -C {repo} init -q");
        self.exec(container, &cmd, &self.default_exec_opts()).await
    }

    pub async fn git_status(
        &self,
        container: &str,
        repo_rel: &str,
    ) -> Result<ExecOutput, AppError> {
        self.git(
            container,
            repo_rel,
            "status",
            &["--porcelain=v1".to_string(), "--branch".to_string()],
        )
        .await
    }

    pub async fn git_diff(
        &self,
        container: &str,
        repo_rel: &str,
        pathspec: Option<&str>,
    ) -> Result<ExecOutput, AppError> {
        let mut args = vec!["--no-color".to_string()];
        if let Some(p) = pathspec {
            // pathspec 同样只允许相对路径，避免 `-- /etc/passwd` 这类越界。
            let rel = tenant_rel_path(p)?;
            args.push("--".to_string());
            args.push(rel.display().to_string());
        }
        self.git(container, repo_rel, "diff", &args).await
    }

    /// 工作区快照的最小入口：`add -A` → `write-tree` → `commit-tree`。
    /// stdout 最后一行是 commit sha；parent 由调用方后续 `git update-ref` 接上。
    pub async fn git_commit_tree(
        &self,
        container: &str,
        repo_rel: &str,
        message: &str,
    ) -> Result<ExecOutput, AppError> {
        let repo = shell_quote(&container_path(repo_rel)?);
        let msg = shell_quote(message);
        let cmd = format!(
            "set -e; git -C {repo} add -A; tree=$(git -C {repo} write-tree); \
             git -C {repo} commit-tree \"$tree\" -m {msg}"
        );
        self.exec(container, &cmd, &self.default_exec_opts()).await
    }

    // ---- 文件：宿主侧直接读写已挂载的 workspace 卷 ----

    pub async fn read_file(&self, key: SandboxKey<'_>, rel: &str) -> Result<Vec<u8>, AppError> {
        let ws = self.ensure_workspace_dir(key).await?;
        let path = resolve_in_workspace(&ws, rel).await?;
        let md = tokio::fs::symlink_metadata(&path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AppError::not_found("file not found")
            } else {
                tracing::error!(error = %e, "stat sandbox file failed");
                AppError::internal("cannot read agent workspace")
            }
        })?;
        if !md.is_file() {
            return Err(AppError::bad_request("not a regular file"));
        }
        if md.len() > MAX_FILE_READ_BYTES {
            return Err(AppError::bad_request("file too large to read"));
        }
        tokio::fs::read(&path).await.map_err(|e| {
            tracing::error!(error = %e, "read sandbox file failed");
            AppError::internal("cannot read agent workspace")
        })
    }

    pub async fn write_file(
        &self,
        key: SandboxKey<'_>,
        rel: &str,
        contents: &[u8],
    ) -> Result<(), AppError> {
        let ws = self.ensure_workspace_dir(key).await?;
        let path = resolve_in_workspace(&ws, rel).await?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                tracing::error!(error = %e, "create sandbox parent dir failed");
                AppError::internal("cannot write agent workspace")
            })?;
        }
        // 目标本身若是符号链接，拒绝覆盖（可能指向 workspace 外）。
        if let Ok(md) = tokio::fs::symlink_metadata(&path).await {
            if md.file_type().is_symlink() {
                return Err(AppError::forbidden("path escapes agent workspace"));
            }
        }
        tokio::fs::write(&path, contents).await.map_err(|e| {
            tracing::error!(error = %e, "write sandbox file failed");
            AppError::internal("cannot write agent workspace")
        })
    }

    pub async fn list_dir(
        &self,
        key: SandboxKey<'_>,
        rel: &str,
    ) -> Result<Vec<DirEntryInfo>, AppError> {
        let ws = self.ensure_workspace_dir(key).await?;
        let path = resolve_in_workspace(&ws, rel).await?;
        let mut rd = tokio::fs::read_dir(&path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AppError::not_found("directory not found")
            } else {
                tracing::error!(error = %e, "read sandbox dir failed");
                AppError::internal("cannot list agent workspace")
            }
        })?;
        let mut out = Vec::new();
        while let Ok(Some(entry)) = rd.next_entry().await {
            if out.len() >= LIST_DIR_MAX_ENTRIES {
                break;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            // 不跟随符号链接：链接只当普通项列出来。
            let (is_dir, size) = match entry.metadata().await {
                Ok(md) => (md.is_dir(), md.len()),
                Err(_) => (false, 0),
            };
            out.push(DirEntryInfo { name, is_dir, size });
        }
        sort_dir_entries(&mut out);
        Ok(out)
    }

    async fn ensure_workspace_dir(&self, key: SandboxKey<'_>) -> Result<PathBuf, AppError> {
        let ws = workspace_dir(&self.cfg, key);
        tokio::fs::create_dir_all(&ws).await.map_err(|e| {
            tracing::error!(error = %e, "create sandbox workspace failed");
            AppError::internal("cannot create agent workspace")
        })?;
        Ok(ws)
    }
}

enum WaitOutcome {
    Exited(std::io::Result<std::process::ExitStatus>),
    Cancelled,
}

async fn read_capped<R>(mut reader: R, cap: usize) -> Vec<u8>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut out = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                let room = cap.saturating_sub(out.len());
                if room == 0 {
                    break;
                }
                if n > room {
                    out.extend_from_slice(&chunk[..room]);
                    break;
                }
                out.extend_from_slice(&chunk[..n]);
            }
            Err(_) => break,
        }
    }
    out
}

async fn finish_capture(handle: Option<tokio::task::JoinHandle<Vec<u8>>>) -> Vec<u8> {
    let Some(handle) = handle else {
        return Vec::new();
    };
    match tokio::time::timeout(Duration::from_secs(5), handle).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "capture sandbox output failed");
            Vec::new()
        }
        Err(_) => {
            tracing::warn!("capture sandbox output timed out");
            Vec::new()
        }
    }
}

/// 目录项按名字排序，保证 API 输出稳定（纯逻辑，便于单测）。
pub fn sort_dir_entries(entries: &mut [DirEntryInfo]) {
    entries.sort_by(|a, b| a.name.cmp(&b.name));
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn cfg_at(scope: SandboxScope, data_root: PathBuf) -> SandboxConfig {
        SandboxConfig {
            scope,
            image: "chiron-agent:py314".into(),
            runtime: "sysbox-runc".into(),
            data_root,
            network: "bridge".into(),
            memory: "512m".into(),
            cpus: "1.0".into(),
            pids_limit: 256,
            idle_timeout_secs: 1800,
            exec_timeout_secs: 120,
            exec_max_output_bytes: EXEC_MAX_OUTPUT_BYTES,
        }
    }

    fn cfg(scope: SandboxScope) -> SandboxConfig {
        cfg_at(scope, PathBuf::from("/var/agent-data"))
    }

    #[test]
    fn user_scope_name_and_mount() {
        let c = cfg(SandboxScope::User);
        let key = SandboxKey {
            tenant_id: 1,
            user_id: "u-1",
            project_id: None,
        };
        assert_eq!(container_name(&c, key), "chiron-agent-user-1-u-1");
        assert_eq!(
            workspace_dir(&c, key),
            PathBuf::from("/var/agent-data/1/u-1")
        );
    }

    #[test]
    fn project_scope_includes_project() {
        let c = cfg(SandboxScope::Project);
        let key = SandboxKey {
            tenant_id: 1,
            user_id: "u1",
            project_id: Some("p1"),
        };
        assert_eq!(container_name(&c, key), "chiron-agent-project-1-u1-p1");
        assert_eq!(
            workspace_dir(&c, key),
            PathBuf::from("/var/agent-data/1/u1/p1")
        );
    }

    #[test]
    fn run_args_are_hardened() {
        let c = cfg(SandboxScope::User);
        let key = SandboxKey {
            tenant_id: 1,
            user_id: "u1",
            project_id: None,
        };
        let args = build_run_args(&c, key, &workspace_dir(&c, key));
        let joined = args.join(" ").replace('\\', "/");
        for needle in [
            "--runtime sysbox-runc",
            "--cap-drop ALL",
            "--security-opt no-new-privileges",
            "--read-only",
            "--memory 512m",
            "--cpus 1.0",
            "--pids-limit 256",
            "--tmpfs /tmp:rw,size=64m",
            "--user 1000:1000",
            "--network bridge",
            "chiron.managed-by=agent-platform",
            "chiron.tenant=1",
            "chiron.user=u1",
            "chiron.scope=user",
            "chiron.scope-id=u1",
            "/var/agent-data/1/u1:/workspace:rw",
            "sleep infinity",
            "chiron-agent:py314",
        ] {
            assert!(joined.contains(needle), "missing {needle} in {joined}");
        }
    }

    #[tokio::test]
    async fn sandbox_identity_uses_control_tenant() {
        let c = cfg(SandboxScope::User);
        let key = SandboxKey {
            tenant_id: 1,
            user_id: "u1",
            project_id: None,
        };
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgresql://x@127.0.0.1:1/x")
            .expect("lazy pool");
        let tenant = uuid::Uuid::parse_str("12345678-0000-0000-0000-000000000000").unwrap();
        let (name, ws, args) = crate::tenant::scope_with(
            pool,
            std::sync::Arc::new(std::collections::HashMap::new()),
            std::sync::Arc::new(Vec::new()),
            String::new(),
            String::new(),
            tenant,
            async {
                (
                    container_name(&c, key),
                    workspace_dir(&c, key),
                    build_run_args(&c, key, std::path::Path::new("/tmp/ws")),
                )
            },
        )
        .await;
        assert!(name.contains("12345678"), "name={name}");
        assert!(ws.to_string_lossy().contains("12345678"));
        assert!(args
            .iter()
            .any(|a| a == &format!("chiron.tenant-id={tenant}")));
    }

    #[test]
    fn plan_egress_maps_to_network_without_unknown_fallback() {
        assert_eq!(egress_network("bridge"), Some("bridge"));
        assert_eq!(egress_network("none"), Some("none"));
        assert_eq!(egress_network(""), Some("none"));
        assert_eq!(egress_network("allowlist"), None);
    }

    #[test]
    fn plan_tier_selects_runtime_without_silent_downgrade() {
        assert_eq!(runtime_for_tier("sysbox"), Some("sysbox-runc"));
        assert_eq!(runtime_for_tier("gVisor"), Some("runsc"));
        assert_eq!(runtime_for_tier("microvm"), Some("kata-runtime"));
        assert_eq!(runtime_for_tier(""), None);
        assert_eq!(runtime_for_tier("docker"), None);
    }

    #[test]
    fn tier_suffix_and_label_are_rendered_in_run_args() {
        let c = cfg(SandboxScope::User);
        let key = SandboxKey {
            tenant_id: 3,
            user_id: "u1",
            project_id: None,
        };
        let name = container_name_tiered(&c, key, Some("gvisor"));
        assert_eq!(name, "chiron-agent-user-3-u1-gvisor");
        let mut tiered = c.clone();
        tiered.runtime = "runsc".into();
        let args = build_run_args_named(
            &tiered,
            key,
            name.clone(),
            std::path::Path::new("/tmp/ws"),
            Some("gvisor"),
            Some((
                "host.docker.internal",
                "http://tenant:token@host.docker.internal:3128",
            )),
        );
        assert!(args
            .windows(2)
            .any(|w| w == ["--runtime".to_string(), "runsc".to_string()]));
        assert!(args.iter().any(|a| a == "chiron.tier=gvisor"));
        assert!(args.iter().any(|a| a == &name));
        assert!(args
            .iter()
            .any(|a| a == "HTTP_PROXY=http://tenant:token@host.docker.internal:3128"));
        assert!(args.windows(2).any(|w| w
            == [
                "--add-host".to_string(),
                "host.docker.internal:host-gateway".to_string()
            ]));
    }

    #[test]
    fn container_name_is_sanitized_and_truncated() {
        let c = cfg(SandboxScope::User);
        let key = SandboxKey {
            tenant_id: 7,
            user_id: "a/b..c*d",
            project_id: None,
        };
        let name = container_name(&c, key);
        assert_eq!(name, "chiron-agent-user-7-a_b..c_d");

        let long = "x".repeat(120);
        let key = SandboxKey {
            tenant_id: 7,
            user_id: &long,
            project_id: None,
        };
        assert_eq!(container_name(&c, key).len(), 63);
    }

    #[test]
    fn project_scope_labels_carry_project_id() {
        let c = cfg(SandboxScope::Project);
        let key = SandboxKey {
            tenant_id: 2,
            user_id: "u1",
            project_id: Some("p1"),
        };
        let joined = build_run_args(&c, key, &workspace_dir(&c, key))
            .join(" ")
            .replace('\\', "/");
        assert!(joined.contains("chiron.scope=project"));
        assert!(joined.contains("chiron.scope-id=p1"));
        assert!(joined.contains("/var/agent-data/2/u1/p1:/workspace:rw"));
    }

    #[test]
    fn rel_path_rejects_escapes() {
        for bad in [
            "../etc/passwd",
            "a/../../b",
            "/etc/passwd",
            "\\windows\\system32",
            "C:\\Windows",
            "a:b",
        ] {
            assert!(tenant_rel_path(bad).is_err(), "should reject {bad}");
        }
        assert_eq!(
            tenant_rel_path("a/b.txt").unwrap(),
            PathBuf::from("a/b.txt")
        );
        assert_eq!(tenant_rel_path("./a/./b").unwrap(), PathBuf::from("a/b"));
        assert_eq!(tenant_rel_path("  ").unwrap(), PathBuf::from("."));
        assert_eq!(tenant_rel_path("a//b").unwrap(), PathBuf::from("a/b"));
    }

    #[test]
    fn container_paths_are_anchored_at_workspace() {
        assert_eq!(
            container_path("src/main.py").unwrap(),
            "/workspace/src/main.py"
        );
        assert_eq!(container_path(".").unwrap(), "/workspace");
        assert_eq!(container_path("").unwrap(), "/workspace");
        assert!(container_path("../secret").is_err());
    }

    #[test]
    fn exec_args_shape() {
        let opts = ExecOptions {
            timeout: Duration::from_secs(5),
            max_output_bytes: 128,
            workdir: Some("pkg/sub".into()),
        };
        let args = build_exec_args("chiron-agent-user-1-u1", "echo hi", &opts).unwrap();
        assert_eq!(args[0], "exec");
        assert_eq!(args[1], "-i");
        assert_eq!(args[2], "-w");
        assert_eq!(args[3], "/workspace/pkg/sub");
        assert_eq!(args[4], "chiron-agent-user-1-u1");
        assert_eq!(args[5], "sh");
        assert_eq!(args[6], "-lc");
        assert_eq!(args[7], "echo hi");

        let args = build_exec_args("c", "pwd", &ExecOptions::default()).unwrap();
        assert_eq!(args[3], "/workspace");
        assert_eq!(ExecOptions::default().timeout, Duration::from_secs(120));

        let escaped = ExecOptions {
            workdir: Some("../etc".into()),
            ..Default::default()
        };
        assert!(build_exec_args("c", "id", &escaped).is_err());
    }

    #[test]
    fn shell_quote_escapes() {
        assert_eq!(shell_quote("plain-1.0/x"), "plain-1.0/x");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn output_truncation_marks_and_respects_utf8() {
        let (s, cut) = truncate_output(b"short", 16);
        assert_eq!(s, "short");
        assert!(!cut);

        let (s, cut) = truncate_output(&b"x".repeat(32), 8);
        assert!(cut);
        assert!(s.starts_with("xxxxxxxx"));
        assert!(s.contains("truncated 24 bytes"));

        let bytes = "中中中".as_bytes();
        let (s, cut) = truncate_output(bytes, 4);
        assert!(cut);
        assert!(s.starts_with('中'));
        assert!(!s.contains('\u{FFFD}'));
    }

    #[test]
    fn idle_uses_last_seen_and_ignores_future() {
        let now = Utc::now();
        let idle = Duration::from_secs(30 * 60);
        assert!(is_idle(now - chrono::Duration::minutes(31), now, idle));
        assert!(!is_idle(now - chrono::Duration::minutes(29), now, idle));
        assert!(is_idle(now - chrono::Duration::seconds(1800), now, idle));
        assert!(!is_idle(now + chrono::Duration::minutes(5), now, idle));
    }

    #[test]
    fn manager_tracks_last_seen() {
        let mgr = SandboxManager::new(cfg(SandboxScope::User));
        assert!(mgr.last_seen("missing").is_none());
        mgr.touch("c1");
        assert!(mgr.last_seen("c1").is_some());
    }

    #[test]
    fn git_commands_are_quoted_and_clamped() {
        let cmd = build_git_command(
            "repo dir",
            "diff",
            &["--no-color".to_string(), "src".to_string()],
        )
        .unwrap();
        assert_eq!(
            cmd,
            "git -C '/workspace/repo dir' --no-pager diff --no-color src"
        );
        assert!(build_git_command("../escape", "status", &[]).is_err());
        assert!(build_git_command("/etc", "status", &[]).is_err());
        let quoted = build_git_command(".", "status; id", &[]).unwrap();
        assert!(quoted.contains("'status; id'"), "{quoted}");
    }

    #[test]
    fn dir_entries_are_sorted() {
        let mut v = vec![
            DirEntryInfo {
                name: "b".into(),
                is_dir: false,
                size: 1,
            },
            DirEntryInfo {
                name: "a".into(),
                is_dir: true,
                size: 0,
            },
        ];
        sort_dir_entries(&mut v);
        assert_eq!(v[0].name, "a");
        assert_eq!(v[1].name, "b");
    }

    #[tokio::test]
    async fn file_roundtrip_and_traversal_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SandboxManager::new(cfg_at(SandboxScope::User, dir.path().to_path_buf()));
        let key = SandboxKey {
            tenant_id: 1,
            user_id: "u1",
            project_id: None,
        };

        mgr.write_file(key, "notes/a.txt", b"hello").await.unwrap();
        assert_eq!(
            mgr.read_file(key, "notes/a.txt").await.unwrap(),
            b"hello".to_vec()
        );

        let entries = mgr.list_dir(key, "notes").await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "a.txt");
        assert!(!entries[0].is_dir);
        assert_eq!(entries[0].size, 5);

        assert!(mgr.write_file(key, "../escape.txt", b"x").await.is_err());
        assert!(mgr.read_file(key, "/etc/passwd").await.is_err());
        assert!(mgr.read_file(key, "missing.txt").await.is_err());
        assert!(!dir.path().join("escape.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_escape_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = SandboxManager::new(cfg_at(SandboxScope::User, dir.path().to_path_buf()));
        let key = SandboxKey {
            tenant_id: 1,
            user_id: "u1",
            project_id: None,
        };
        mgr.write_file(key, "ok.txt", b"hi").await.unwrap();

        let ws = workspace_dir(mgr.cfg(), key);
        std::os::unix::fs::symlink("/etc/passwd", ws.join("leak")).unwrap();
        assert!(mgr.read_file(key, "leak").await.is_err());
        assert!(mgr.write_file(key, "leak", b"x").await.is_err());

        // 悬空链接同样不能当作写入入口。
        std::os::unix::fs::symlink("/tmp/chiron-no-such-target", ws.join("dangling")).unwrap();
        assert!(mgr.write_file(key, "dangling", b"x").await.is_err());
    }
}
