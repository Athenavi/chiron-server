//! 媒体/文件存储后端抽象。
//!
//! 已实现 local（宿主目录）、webdav（远端 WebDAV 集合）与 s3（SigV4 签名对象存储，
//! 兼容 MinIO 等）；`s3` 缺少环境变量时构造失败（fail closed），不会静默退回本地。
//! key 由服务端生成（`<tenant>/<user>/<id>/blob`），实现不得接受用户输入拼接路径。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::config::Config;

/// WebDAV 单次请求上限：既要覆盖 50 MiB 上传，又不能在远端卡死时长期挂起。
const WEBDAV_TIMEOUT_SECS: u64 = 20;
/// 建连上限：远端不可达时尽快失败，不拖住上传/下载请求。
const WEBDAV_CONNECT_TIMEOUT_SECS: u64 = 5;

#[async_trait]
pub trait MediaStorage: Send + Sync {
    async fn put(&self, key: &str, bytes: &[u8]) -> Result<(), String>;
    async fn get(&self, key: &str) -> Result<Vec<u8>, String>;
    async fn delete(&self, key: &str) -> Result<(), String>;
}

pub struct LocalStorage {
    root: PathBuf,
}

impl LocalStorage {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn path(&self, key: &str) -> Result<PathBuf, String> {
        if key
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err("invalid storage key".into());
        }
        Ok(self.root.join(key))
    }

    /// 拒绝路径上任何符号链接，避免租户侧（或上次部署遗留）用链接把读写带出媒体根。
    async fn checked(&self, key: &str) -> Result<PathBuf, String> {
        let path = self.path(key)?;
        let rel = path
            .strip_prefix(&self.root)
            .map_err(|_| "storage key escaped root".to_string())?;
        let mut cur = self.root.clone();
        for component in rel.components() {
            let std::path::Component::Normal(part) = component else {
                return Err("invalid storage key".into());
            };
            cur.push(part);
            match tokio::fs::symlink_metadata(&cur).await {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err("refusing symlinked storage path".into())
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
                Err(e) => return Err(format!("stat: {e}")),
            }
        }
        Ok(path)
    }
}

#[async_trait]
impl MediaStorage for LocalStorage {
    async fn put(&self, key: &str, bytes: &[u8]) -> Result<(), String> {
        let path = self.checked(key).await?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("create dir: {e}"))?;
        }
        tokio::fs::write(&path, bytes)
            .await
            .map_err(|e| format!("write: {e}"))
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, String> {
        let path = self.checked(key).await?;
        tokio::fs::read(&path)
            .await
            .map_err(|e| format!("read: {e}"))
    }

    async fn delete(&self, key: &str) -> Result<(), String> {
        // 媒体对象固定为 <tenant>/<user>/<asset>/blob；其它形状不许删父目录。
        if key.split('/').count() != 4 {
            return Err("invalid storage key".into());
        }
        let path = self.checked(key).await?;
        let dir = path.parent().ok_or_else(|| "invalid storage key".to_string())?;
        match tokio::fs::symlink_metadata(dir).await {
            Ok(meta) if meta.is_dir() && !meta.is_symlink() => {}
            Ok(_) => return Err("refusing to delete non-directory storage path".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("stat: {e}")),
        }
        match tokio::fs::remove_dir_all(dir).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("delete: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// WebDAV
// ---------------------------------------------------------------------------

/// 远端 WebDAV 存储：`PUT/GET/DELETE {base}/{prefix}/{key}`。
///
/// - 连接参数只从环境变量读取（[`WebDavStorage::from_env`]）：URL 必填；
///   用户名/密码可选（HTTP Basic）；`prefix` 可选，相当于远端集合根；
/// - key 逐段百分号编码，空段 / `.` / `..` / `?` / `#` / 控制字符一律拒绝，
///   避免路径穿越与查询串注入；
/// - 删除时 404 视为成功（幂等）；其余非 2xx 直接报错，不做静默降级。
pub struct WebDavStorage {
    client: reqwest::Client,
    /// 已校验的 base：仅 http/https、无内嵌凭据、无 query/fragment。
    base: url::Url,
    /// 已归一化并编码的前缀（无首尾 `/`；空 = 直接挂在 base 下）。
    prefix: String,
    /// HTTP Basic 凭据；`None` = 匿名访问。
    auth: Option<(String, String)>,
}

impl std::fmt::Debug for WebDavStorage {
    /// 手写 Debug：绝不把密码打进日志。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebDavStorage")
            .field("base", &self.base.as_str())
            .field("prefix", &self.prefix)
            .field("auth", &self.auth.as_ref().map(|(user, _)| user))
            .finish()
    }
}

impl WebDavStorage {
    /// 从环境变量构造：
    /// `CHIRON_MEDIA_WEBDAV_URL`（必填）、`CHIRON_MEDIA_WEBDAV_USERNAME`、
    /// `CHIRON_MEDIA_WEBDAV_PASSWORD`、`CHIRON_MEDIA_WEBDAV_PREFIX`。
    pub fn from_env() -> Result<Self, String> {
        let url = env_trimmed("CHIRON_MEDIA_WEBDAV_URL").ok_or_else(|| {
            "CHIRON_MEDIA_WEBDAV_URL is required for the `webdav` media backend".to_string()
        })?;
        let username = env_trimmed("CHIRON_MEDIA_WEBDAV_USERNAME");
        let password = std::env::var("CHIRON_MEDIA_WEBDAV_PASSWORD")
            .ok()
            .filter(|value| !value.is_empty());
        let prefix = env_trimmed("CHIRON_MEDIA_WEBDAV_PREFIX");
        Self::new(
            &url,
            username.as_deref(),
            password.as_deref(),
            prefix.as_deref(),
        )
    }

    /// 显式构造（不做任何网络访问）。只配密码不配用户名直接报错（fail closed）。
    pub fn new(
        base_url: &str,
        username: Option<&str>,
        password: Option<&str>,
        prefix: Option<&str>,
    ) -> Result<Self, String> {
        let base = parse_base_url(base_url)?;
        let prefix = normalize_prefix(prefix.unwrap_or(""), "CHIRON_MEDIA_WEBDAV_PREFIX")?;
        let username = username.map(str::trim).filter(|value| !value.is_empty());
        let password = password.filter(|value| !value.is_empty());
        let auth = match (username, password) {
            (Some(user), pass) => Some((user.to_string(), pass.unwrap_or_default().to_string())),
            (None, Some(_)) => {
                return Err(
                    "CHIRON_MEDIA_WEBDAV_PASSWORD is set but CHIRON_MEDIA_WEBDAV_USERNAME is empty"
                        .to_string(),
                )
            }
            (None, None) => None,
        };
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(WEBDAV_CONNECT_TIMEOUT_SECS))
            .timeout(Duration::from_secs(WEBDAV_TIMEOUT_SECS))
            .redirect(reqwest::redirect::Policy::limited(3))
            .user_agent("chiron-server/media")
            .build()
            .map_err(|e| format!("cannot build WebDAV http client: {e}"))?;
        Ok(Self {
            client,
            base,
            prefix,
            auth,
        })
    }

    /// 校验 key 并返回 (对象 URL, 已编码 key)。
    fn target(&self, key: &str) -> Result<(url::Url, String), String> {
        let encoded = encode_key(key)?;
        let url = self.object_url_encoded(&encoded)?;
        Ok((url, encoded))
    }

    /// 把已编码的 key 拼到 `{base}/{prefix}` 之后。
    fn object_url_encoded(&self, encoded_key: &str) -> Result<url::Url, String> {
        debug_assert!(!encoded_key.starts_with('/'));
        let mut url = String::with_capacity(
            self.base.as_str().len() + self.prefix.len() + encoded_key.len() + 2,
        );
        url.push_str(self.base.as_str().trim_end_matches('/'));
        if !self.prefix.is_empty() {
            url.push('/');
            url.push_str(&self.prefix);
        }
        url.push('/');
        url.push_str(encoded_key);
        url::Url::parse(&url).map_err(|e| format!("cannot build WebDAV object URL: {e}"))
    }

    fn request(&self, method: reqwest::Method, url: url::Url) -> reqwest::RequestBuilder {
        let builder = self.client.request(method, url);
        match &self.auth {
            // 预置 Basic 头：WebDAV 服务端普遍不回 401 挑战之外的提示。
            Some((user, pass)) => builder.basic_auth(user, Some(pass)),
            None => builder,
        }
    }

    async fn put_once(&self, url: &url::Url, bytes: &[u8]) -> Result<reqwest::StatusCode, String> {
        self.request(reqwest::Method::PUT, url.clone())
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(bytes.to_vec())
            .send()
            .await
            .map(|response| response.status())
            .map_err(|e| format!("webdav PUT `{url}`: {e}"))
    }

    /// best-effort 逐级 MKCOL：部分服务端（nginx-dav/Nextcloud 严格模式）要求父集合先存在，
    /// 否则 PUT 返回 409。失败一律忽略，由随后的 PUT 重试给出真实错误。
    async fn ensure_parents(&self, encoded_key: &str) {
        let mut segments: Vec<&str> = encoded_key.split('/').collect();
        segments.pop(); // 去掉对象名本身
        let Some(method) = reqwest::Method::from_bytes(b"MKCOL").ok() else {
            return;
        };
        let mut joined = String::new();
        for segment in segments {
            if !joined.is_empty() {
                joined.push('/');
            }
            joined.push_str(segment);
            let Ok(url) = self.object_url_encoded(&joined) else {
                return;
            };
            // 405 = 集合已存在，其余失败也不阻断。
            let _ = self.request(method.clone(), url).send().await;
        }
    }
}

#[async_trait]
impl MediaStorage for WebDavStorage {
    async fn put(&self, key: &str, bytes: &[u8]) -> Result<(), String> {
        let (url, encoded) = self.target(key)?;
        let mut status = self.put_once(&url, bytes).await?;
        if status == reqwest::StatusCode::CONFLICT {
            self.ensure_parents(&encoded).await;
            status = self.put_once(&url, bytes).await?;
        }
        if status.is_success() {
            Ok(())
        } else {
            Err(format!("webdav PUT `{key}` failed: {status}"))
        }
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, String> {
        let (url, _) = self.target(key)?;
        let response = self
            .request(reqwest::Method::GET, url)
            .send()
            .await
            .map_err(|e| format!("webdav GET `{key}`: {e}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("webdav GET `{key}` failed: {status}"));
        }
        let body = response
            .bytes()
            .await
            .map_err(|e| format!("webdav GET `{key}` body: {e}"))?;
        Ok(body.to_vec())
    }

    async fn delete(&self, key: &str) -> Result<(), String> {
        let (url, _) = self.target(key)?;
        let response = self
            .request(reqwest::Method::DELETE, url)
            .send()
            .await
            .map_err(|e| format!("webdav DELETE `{key}`: {e}"))?;
        let status = response.status();
        if status.is_success() || status == reqwest::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(format!("webdav DELETE `{key}` failed: {status}"))
        }
    }
}

// ---------------------------------------------------------------------------
// S3（AWS Signature Version 4）
// ---------------------------------------------------------------------------

/// S3 单次请求上限：对象最大 50 MiB，留足慢链路余量，但不无限挂起。
const S3_TIMEOUT_SECS: u64 = 60;
/// 建连上限：端点不可达时尽快失败。
const S3_CONNECT_TIMEOUT_SECS: u64 = 10;
/// 空负载的 SHA-256（GET/DELETE 用；PUT 用真实 body 哈希，绝不用 UNSIGNED-PAYLOAD）。
const S3_EMPTY_PAYLOAD_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// S3 构造参数（公开给后台配置构造器）。
#[derive(Clone)]
pub struct S3Options {
    pub bucket: String,
    pub region: String,
    pub endpoint: Option<String>,
    pub access_key: String,
    pub secret_key: String,
    pub session_token: Option<String>,
    pub path_style: Option<bool>,
    pub prefix: Option<String>,
}

#[cfg(test)]
impl S3Options {
    fn new(bucket: &str, access_key: &str, secret_key: &str) -> Self {
        Self {
            bucket: bucket.to_string(),
            region: "us-east-1".to_string(),
            endpoint: None,
            access_key: access_key.to_string(),
            secret_key: secret_key.to_string(),
            session_token: None,
            path_style: None,
            prefix: None,
        }
    }
}

/// S3 / S3 兼容（MinIO、R2、OSS 网关）存储：`PUT/GET/DELETE {bucket}/{prefix}/{key}`。
///
/// - 参数只从环境变量读取（[`S3Storage::from_env`]）：bucket 与凭证必填，其余可选；
///   缺失时构造失败（fail closed），不会退回本地盘；
/// - 请求用 SigV4 签名：`host`、`x-amz-content-sha256`、`x-amz-date`
///   （有临时凭证时再加 `x-amz-security-token`）全部进入 SignedHeaders，
///   body 参与哈希，不使用 `UNSIGNED-PAYLOAD`；
/// - 默认 virtual-hosted style；配置自定义 endpoint（MinIO 等）时默认 path-style，
///   可用 `CHIRON_MEDIA_S3_PATH_STYLE` 显式覆盖；
/// - key 逐段百分号编码（SigV4 规范），空段 / `.` / `..` 等一律拒绝；
/// - 删除时 404 视为成功（幂等）；其余非 2xx 带状态码与截断后的响应体报错。
///   Authorization 头与密钥绝不进入错误信息或日志。
pub struct S3Storage {
    client: reqwest::Client,
    bucket: String,
    region: String,
    /// 主机（含非默认端口）；virtual-hosted style 下已带 `<bucket>.` 前缀。
    host: String,
    scheme: String,
    /// endpoint 自带的路径前缀（无尾部 `/`；通常为空）。
    base_path: String,
    /// 已编码的前缀（无首尾 `/`；空 = 直接挂在 bucket 根）。
    prefix: String,
    /// path-style 时 canonical URI 需要带上 bucket 段。
    path_style: bool,
    access_key: String,
    secret_key: String,
    session_token: Option<String>,
}

impl std::fmt::Debug for S3Storage {
    /// 手写 Debug：只暴露 bucket/region/host，密钥与 token 一律隐去。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Storage")
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("host", &self.host)
            .field("scheme", &self.scheme)
            .field("base_path", &self.base_path)
            .field("prefix", &self.prefix)
            .field("path_style", &self.path_style)
            .field("access_key", &"<redacted>")
            .field("secret_key", &"<redacted>")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl S3Storage {
    /// 从环境变量构造：
    /// `CHIRON_MEDIA_S3_BUCKET`（必填）、`CHIRON_MEDIA_S3_REGION`（默认 us-east-1）、
    /// `CHIRON_MEDIA_S3_ENDPOINT`（可选，自定义端点如 MinIO）、
    /// `CHIRON_MEDIA_S3_ACCESS_KEY` / `CHIRON_MEDIA_S3_SECRET_KEY`（必填）、
    /// `CHIRON_MEDIA_S3_SESSION_TOKEN`、`CHIRON_MEDIA_S3_PATH_STYLE`、
    /// `CHIRON_MEDIA_S3_PREFIX`（后三者可选）。
    pub fn from_env() -> Result<Self, String> {
        let bucket = env_trimmed("CHIRON_MEDIA_S3_BUCKET").ok_or_else(|| {
            "CHIRON_MEDIA_S3_BUCKET is required for the `s3` media backend".to_string()
        })?;
        let access_key = env_trimmed("CHIRON_MEDIA_S3_ACCESS_KEY").ok_or_else(|| {
            "CHIRON_MEDIA_S3_ACCESS_KEY is required for the `s3` media backend".to_string()
        })?;
        let secret_key = std::env::var("CHIRON_MEDIA_S3_SECRET_KEY")
            .map(|value| value.trim().to_string())
            .ok()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                "CHIRON_MEDIA_S3_SECRET_KEY is required for the `s3` media backend".to_string()
            })?;
        let path_style = match env_trimmed("CHIRON_MEDIA_S3_PATH_STYLE") {
            Some(raw) => Some(parse_bool(&raw).ok_or_else(|| {
                format!("CHIRON_MEDIA_S3_PATH_STYLE must be true/false, got `{raw}`")
            })?),
            None => None,
        };
        Self::new(S3Options {
            bucket,
            region: env_trimmed("CHIRON_MEDIA_S3_REGION").unwrap_or_else(|| "us-east-1".into()),
            endpoint: env_trimmed("CHIRON_MEDIA_S3_ENDPOINT"),
            access_key,
            secret_key,
            session_token: env_trimmed("CHIRON_MEDIA_S3_SESSION_TOKEN"),
            path_style,
            prefix: env_trimmed("CHIRON_MEDIA_S3_PREFIX"),
        })
    }

    /// 显式构造（不做任何网络访问）。
    pub fn from_options(opts: S3Options) -> Result<Self, String> {
        Self::new(opts)
    }

    /// 显式构造（不做任何网络访问）。
    fn new(opts: S3Options) -> Result<Self, String> {
        validate_bucket(&opts.bucket)?;
        let region = opts.region.trim().to_string();
        if region.is_empty() {
            return Err("CHIRON_MEDIA_S3_REGION must not be empty".to_string());
        }
        if opts.access_key.trim().is_empty() || opts.secret_key.is_empty() {
            return Err(
                "CHIRON_MEDIA_S3_ACCESS_KEY and CHIRON_MEDIA_S3_SECRET_KEY must be non-empty"
                    .to_string(),
            );
        }
        let custom_endpoint = opts.endpoint.is_some();
        let endpoint = match opts.endpoint.as_deref() {
            Some(raw) => parse_endpoint(raw)?,
            None => url::Url::parse(&format!("https://s3.{region}.amazonaws.com"))
                .map_err(|e| format!("cannot build default S3 endpoint: {e}"))?,
        };
        let host_base = match endpoint.port() {
            Some(port) => format!("{}:{port}", endpoint.host_str().unwrap_or_default()),
            None => endpoint.host_str().unwrap_or_default().to_string(),
        };
        if host_base.is_empty() {
            return Err("S3 endpoint has no host".to_string());
        }
        // 自定义端点（MinIO 等）默认 path-style；AWS 默认 virtual-hosted style。
        let path_style = opts.path_style.unwrap_or(custom_endpoint);
        let host = if path_style {
            host_base
        } else {
            format!("{}.{host_base}", opts.bucket)
        };
        let prefix = normalize_prefix(
            opts.prefix.as_deref().unwrap_or(""),
            "CHIRON_MEDIA_S3_PREFIX",
        )?;
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(S3_CONNECT_TIMEOUT_SECS))
            .timeout(Duration::from_secs(S3_TIMEOUT_SECS))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("chiron-server/media")
            .build()
            .map_err(|e| format!("cannot build S3 http client: {e}"))?;
        Ok(Self {
            client,
            bucket: opts.bucket,
            region,
            host,
            scheme: endpoint.scheme().to_string(),
            base_path: endpoint.path().trim_end_matches('/').to_string(),
            prefix,
            path_style,
            access_key: opts.access_key,
            secret_key: opts.secret_key,
            session_token: opts.session_token,
        })
    }

    /// 已编码 key -> 请求路径（canonical URI 与实际 URL 共用同一字符串）。
    fn object_path(&self, encoded_key: &str) -> String {
        let mut path = String::new();
        path.push_str(&self.base_path);
        if self.path_style {
            path.push('/');
            path.push_str(&self.bucket);
        }
        if !self.prefix.is_empty() {
            path.push('/');
            path.push_str(&self.prefix);
        }
        path.push('/');
        path.push_str(encoded_key);
        path
    }

    fn object_url(&self, path: &str) -> Result<url::Url, String> {
        let raw = format!("{}://{}{}", self.scheme, self.host, path);
        url::Url::parse(&raw).map_err(|e| format!("cannot build S3 object URL: {e}"))
    }

    /// 校验 key 并返回 (URL, canonical URI)。
    fn target(&self, key: &str) -> Result<(url::Url, String), String> {
        let encoded = encode_key(key)?;
        let path = self.object_path(&encoded);
        Ok((self.object_url(&path)?, path))
    }

    /// 计算 `Authorization` 头值（调用方另行写入 x-amz-* 头与 Host）。
    fn authorization(&self, method: &str, canonical_uri: &str, payload_hash: &str, amz_date: &str) -> String {
        let date_stamp = amz_date.get(..8).unwrap_or(amz_date);
        let mut canonical_headers = format!(
            "host:{}\nx-amz-content-sha256:{}\nx-amz-date:{}\n",
            self.host, payload_hash, amz_date
        );
        let mut signed_headers = "host;x-amz-content-sha256;x-amz-date".to_string();
        if let Some(token) = &self.session_token {
            canonical_headers.push_str(&format!("x-amz-security-token:{token}\n"));
            signed_headers.push_str(";x-amz-security-token");
        }
        let canonical_request = canonical_request(
            method,
            canonical_uri,
            "",
            &canonical_headers,
            &signed_headers,
            payload_hash,
        );
        let scope = format!("{date_stamp}/{}/s3/aws4_request", self.region);
        let string_to_sign = string_to_sign(amz_date, &scope, &canonical_request);
        let key = signing_key(&self.secret_key, date_stamp, &self.region, "s3");
        let signature = hex_lower(&hmac_sha256(&key, string_to_sign.as_bytes()));
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            self.access_key
        )
    }

    /// 响应体截断后，再把凭证串替换掉（S3 错误信息有时会回显 AccessKeyId）。
    fn sanitize_error(&self, body: &str) -> String {
        let mut out = sanitize_body(body);
        for needle in [&self.access_key, &self.secret_key] {
            if !needle.is_empty() {
                out = out.replace(needle.as_str(), "<redacted>");
            }
        }
        if let Some(token) = &self.session_token {
            if !token.is_empty() {
                out = out.replace(token.as_str(), "<redacted>");
            }
        }
        out
    }

    /// 发一次已签名请求；`body` 为 `None` 时用空负载哈希。
    async fn send(
        &self,
        method: reqwest::Method,
        key: &str,
        body: Option<&[u8]>,
    ) -> Result<reqwest::Response, String> {
        let (url, canonical_uri) = self.target(key)?;
        let payload = body.unwrap_or_default();
        // 空负载直接用常量，避免每次 GET/DELETE 重算。
        let payload_hash = if payload.is_empty() {
            S3_EMPTY_PAYLOAD_SHA256.to_string()
        } else {
            sha256_hex(payload)
        };
        let now = chrono::Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let auth = self.authorization(method.as_str(), &canonical_uri, &payload_hash, &amz_date);

        let mut request = self
            .client
            .request(method.clone(), url)
            .header(reqwest::header::HOST, &self.host)
            .header("x-amz-content-sha256", &payload_hash)
            .header("x-amz-date", &amz_date)
            .header(reqwest::header::AUTHORIZATION, auth);
        if let Some(token) = &self.session_token {
            request = request.header("x-amz-security-token", token);
        }
        if let Some(bytes) = body {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                .body(bytes.to_vec());
        }
        request.send().await.map_err(|e| {
            // 只带方法与 key：URL 与头里没有任何密钥，但仍避免把完整 URL 打进日志。
            format!("s3 {} `{key}` request failed: {e}", method.as_str())
        })
    }
}

#[async_trait]
impl MediaStorage for S3Storage {
    async fn put(&self, key: &str, bytes: &[u8]) -> Result<(), String> {
        let response = self.send(reqwest::Method::PUT, key, Some(bytes)).await?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        let body = response.text().await.unwrap_or_default();
        Err(format!(
            "s3 PUT `{key}` failed: {status}: {}",
            self.sanitize_error(&body)
        ))
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, String> {
        let response = self.send(reqwest::Method::GET, key, None).await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(format!(
                "s3 GET `{key}` failed: {status}: {}",
                self.sanitize_error(&body)
            ));
        }
        let body = response
            .bytes()
            .await
            .map_err(|e| format!("s3 GET `{key}` body: {e}"))?;
        Ok(body.to_vec())
    }

    async fn delete(&self, key: &str) -> Result<(), String> {
        let response = self.send(reqwest::Method::DELETE, key, None).await?;
        let status = response.status();
        if status.is_success() || status == reqwest::StatusCode::NOT_FOUND {
            Ok(())
        } else {
            let body = response.text().await.unwrap_or_default();
            Err(format!(
                "s3 DELETE `{key}` failed: {status}: {}",
                self.sanitize_error(&body)
            ))
        }
    }
}

fn env_trimmed(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// bucket 名最小校验：非空、无 `/`、无空白/控制字符；不做 DNS 级严格校验，
/// 兼容 MinIO 与各类 S3 兼容服务。
fn validate_bucket(bucket: &str) -> Result<(), String> {
    if bucket.is_empty() {
        return Err("CHIRON_MEDIA_S3_BUCKET is empty".to_string());
    }
    if bucket.len() > 255 {
        return Err("CHIRON_MEDIA_S3_BUCKET is too long".to_string());
    }
    if let Some(bad) = bucket
        .chars()
        .find(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '\\' | '?' | '#'))
    {
        return Err(format!(
            "CHIRON_MEDIA_S3_BUCKET contains forbidden character `{bad:?}`"
        ));
    }
    Ok(())
}

/// 自定义端点：只接受 http/https，拒绝内嵌凭据与 query/fragment。
fn parse_endpoint(raw: &str) -> Result<url::Url, String> {
    let raw = raw.trim();
    let endpoint = url::Url::parse(raw)
        .map_err(|e| format!("CHIRON_MEDIA_S3_ENDPOINT `{raw}` is not a valid URL: {e}"))?;
    if !matches!(endpoint.scheme(), "http" | "https") {
        return Err(format!(
            "CHIRON_MEDIA_S3_ENDPOINT must be http or https, got `{}`",
            endpoint.scheme()
        ));
    }
    if !endpoint.username().is_empty() || endpoint.password().is_some() {
        return Err("CHIRON_MEDIA_S3_ENDPOINT must not embed credentials".to_string());
    }
    if endpoint.query().is_some() || endpoint.fragment().is_some() {
        return Err("CHIRON_MEDIA_S3_ENDPOINT must not contain a query or fragment".to_string());
    }
    Ok(endpoint)
}

/// 错误体截断：去掉控制字符，最长 400 字符，避免把整页 XML/HTML 塞进日志。
fn sanitize_body(body: &str) -> String {
    let cleaned: String = body
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    let mut out: String = trimmed.chars().take(400).collect();
    if trimmed.chars().count() > 400 {
        out.push_str("...");
    }
    out
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_lower(&hasher.finalize())
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// SigV4 签名密钥：`HMAC(HMAC(HMAC(HMAC("AWS4"+secret, date), region), service), "aws4_request")`。
fn signing_key(secret: &str, date_stamp: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), date_stamp.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}

/// CanonicalRequest：method / URI / query / 头 / SignedHeaders / 负载哈希。
fn canonical_request(
    method: &str,
    canonical_uri: &str,
    canonical_query: &str,
    canonical_headers: &str,
    signed_headers: &str,
    payload_hash: &str,
) -> String {
    format!(
        "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    )
}

/// StringToSign + 哈希 CanonicalRequest。
fn string_to_sign(amz_date: &str, scope: &str, canonical_request: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    )
}

/// 只接受 http/https 的绝对 URL；拒绝内嵌凭据与 query/fragment（避免拼出意外目标）。
fn parse_base_url(raw: &str) -> Result<url::Url, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("CHIRON_MEDIA_WEBDAV_URL is empty".to_string());
    }
    let base = url::Url::parse(raw)
        .map_err(|e| format!("CHIRON_MEDIA_WEBDAV_URL `{raw}` is not a valid URL: {e}"))?;
    if !matches!(base.scheme(), "http" | "https") {
        return Err(format!(
            "CHIRON_MEDIA_WEBDAV_URL must be http or https, got `{}`",
            base.scheme()
        ));
    }
    if !base.username().is_empty() || base.password().is_some() {
        return Err(
            "CHIRON_MEDIA_WEBDAV_URL must not embed credentials; use \
             CHIRON_MEDIA_WEBDAV_USERNAME / CHIRON_MEDIA_WEBDAV_PASSWORD"
                .to_string(),
        );
    }
    if base.query().is_some() || base.fragment().is_some() {
        return Err("CHIRON_MEDIA_WEBDAV_URL must not contain a query or fragment".to_string());
    }
    Ok(base)
}

/// 前缀归一化：去掉首尾 `/`，逐段校验并编码；空串表示没有前缀。
fn normalize_prefix(raw: &str, label: &str) -> Result<String, String> {
    let raw = raw.trim().trim_matches('/');
    if raw.is_empty() {
        return Ok(String::new());
    }
    let mut parts = Vec::new();
    for part in raw.split('/') {
        check_segment(part).map_err(|e| format!("invalid {label} `{raw}`: {e}"))?;
        parts.push(encode_segment(part));
    }
    Ok(parts.join("/"))
}

/// key 校验 + 逐段编码；不合法直接拒绝（调用方已由服务端生成，仍需防御）。
fn encode_key(key: &str) -> Result<String, String> {
    if key.trim().is_empty() {
        return Err("invalid storage key: empty".to_string());
    }
    if key.trim() != key {
        return Err(format!("invalid storage key `{key}`: surrounding whitespace"));
    }
    let mut parts = Vec::new();
    for part in key.split('/') {
        check_segment(part).map_err(|e| format!("invalid storage key `{key}`: {e}"))?;
        parts.push(encode_segment(part));
    }
    Ok(parts.join("/"))
}

fn check_segment(part: &str) -> Result<(), String> {
    if part.is_empty() {
        return Err("empty path segment".to_string());
    }
    if part == "." || part == ".." {
        return Err("traversal path segment".to_string());
    }
    if let Some(bad) = part
        .chars()
        .find(|c| c.is_control() || matches!(c, '?' | '#' | '\\'))
    {
        return Err(format!("forbidden character `{bad:?}`"));
    }
    Ok(())
}

/// RFC 3986 非保留字符原样保留，其余按 UTF-8 字节百分号编码。
fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// 后台/环境变量合并后的媒体后端配置。
#[derive(Clone)]
pub struct MediaStorageConfig {
    pub backend: String,
    pub local_root: PathBuf,
    pub webdav_url: String,
    pub webdav_username: String,
    pub webdav_password: String,
    pub webdav_prefix: String,
    pub s3: Option<S3Options>,
}

pub fn from_values(cfg: &MediaStorageConfig) -> Result<Arc<dyn MediaStorage>, String> {
    match cfg.backend.trim().to_ascii_lowercase().as_str() {
        "" | "local" => Ok(Arc::new(LocalStorage::new(cfg.local_root.clone()))),
        "webdav" => {
            let storage = WebDavStorage::new(
                &cfg.webdav_url,
                Some(&cfg.webdav_username),
                Some(&cfg.webdav_password),
                Some(&cfg.webdav_prefix),
            )
            .map_err(|e| format!("media backend `webdav` is misconfigured: {e}"))?;
            Ok(Arc::new(storage))
        }
        "s3" => {
            let opts = cfg
                .s3
                .clone()
                .ok_or_else(|| "media backend `s3` has no credentials".to_string())?;
            let storage = S3Storage::from_options(opts)
                .map_err(|e| format!("media backend `s3` is misconfigured: {e}"))?;
            Ok(Arc::new(storage))
        }
        other => Err(format!(
            "unknown media backend `{other}`; use `local`, `webdav` or `s3`"
        )),
    }
}

/// 读取后台配置（控制库优先、env 兜底）构造当前媒体后端。
pub async fn from_state(
    state: &crate::state::AppState,
) -> Result<Arc<dyn MediaStorage>, String> {
    let cfg = crate::control::settings::load_media(state)
        .await
        .map_err(|err| format!("media settings unavailable: {}", err.message))?;
    from_values(&cfg)
}

/// 按 `CHIRON_MEDIA_BACKEND` 选择后端：local（默认）/ webdav；S3 预留但明确不可用。
pub fn from_config(cfg: &Config) -> Result<Arc<dyn MediaStorage>, String> {
    match cfg.media_backend.trim().to_ascii_lowercase().as_str() {
        "" | "local" => Ok(Arc::new(LocalStorage::new(cfg.media_root_path()))),
        "webdav" => {
            let storage = WebDavStorage::from_env()
                .map_err(|e| format!("media backend `webdav` is misconfigured: {e}"))?;
            Ok(Arc::new(storage))
        }
        "s3" => {
            let storage = S3Storage::from_env()
                .map_err(|e| format!("media backend `s3` is misconfigured: {e}"))?;
            Ok(Arc::new(storage))
        }
        other => Err(format!(
            "unknown media backend `{other}`; use `local`, `webdav` or `s3`"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_roundtrip_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let storage = LocalStorage::new(dir.path().to_path_buf());
        storage.put("t/u/id/blob", b"hello").await.unwrap();
        assert_eq!(storage.get("t/u/id/blob").await.unwrap(), b"hello");
        storage.delete("t/u/id/blob").await.unwrap();
        assert!(storage.get("t/u/id/blob").await.is_err());
    }

    #[test]
    fn rejects_traversal_keys() {
        let storage = LocalStorage::new(PathBuf::from("/tmp/media"));
        assert!(storage.path("../etc/passwd").is_err());
        assert!(storage.path("a//b").is_err());
        assert!(storage.path("a/b").is_ok());
    }

    fn dav() -> WebDavStorage {
        WebDavStorage::new("https://dav.example.com/dav", None, None, None).unwrap()
    }

    #[test]
    fn webdav_rejects_bad_keys() {
        let storage = dav();
        for bad in [
            "",
            "   ",
            "/leadingslash",
            "a//b",
            "./a",
            "a/./b",
            "a/../b",
            "..",
            "a/b?x=1",
            "a/b#frag",
            "a/b\\c",
            "a/b\nc",
            "a/b\u{7f}",
            " padded",
        ] {
            assert!(storage.target(bad).is_err(), "key `{bad}` must be rejected");
        }
        assert!(storage.target("1/2b6f3e9c/UUID/blob").is_ok());
    }

    #[test]
    fn webdav_builds_encoded_object_url() {
        let storage = dav();
        let (url, key) = storage.target("t/u/id/blob").unwrap();
        assert_eq!(key, "t/u/id/blob");
        assert_eq!(url.as_str(), "https://dav.example.com/dav/t/u/id/blob");

        // 非保留字符原样，其余按字节编码。
        let (url, _) = storage.target("团队/a b+c/d").unwrap();
        assert_eq!(
            url.as_str(),
            "https://dav.example.com/dav/%E5%9B%A2%E9%98%9F/a%20b%2Bc/d"
        );

        // base 末尾斜杠被吃掉，prefix 段也编码。
        let with_prefix =
            WebDavStorage::new("https://dav.example.com/dav/", None, None, Some("/team media/"))
                .unwrap();
        let (url, _) = with_prefix.target("t/u/id/blob").unwrap();
        assert_eq!(
            url.as_str(),
            "https://dav.example.com/dav/team%20media/t/u/id/blob"
        );

        let bare_host = WebDavStorage::new("https://dav.example.com", None, None, Some(""))
            .unwrap();
        let (url, _) = bare_host.target("k").unwrap();
        assert_eq!(url.as_str(), "https://dav.example.com/k");
    }

    #[test]
    fn webdav_rejects_bad_base_and_prefix() {
        for bad in ["", "   ", "ftp://dav.example.com/dav", "not a url"] {
            assert!(WebDavStorage::new(bad, None, None, None).is_err(), "base `{bad}`");
        }
        assert!(
            WebDavStorage::new("https://user:pw@dav.example.com/dav", None, None, None).is_err()
        );
        assert!(WebDavStorage::new(
            "https://dav.example.com/dav?token=x",
            None,
            None,
            None
        )
        .is_err());
        assert!(WebDavStorage::new(
            "https://dav.example.com/dav",
            None,
            None,
            Some("../escape")
        )
        .is_err());
        assert!(WebDavStorage::new(
            "https://dav.example.com/dav",
            None,
            None,
            Some("a//b")
        )
        .is_err());
    }

    #[test]
    fn webdav_auth_requires_username() {
        let err = WebDavStorage::new("https://dav.example.com/dav", None, Some("pw"), None)
            .unwrap_err();
        assert!(err.contains("USERNAME"), "unexpected error: {err}");
        // 只有用户名（空密码）是合法的 Basic 凭据。
        assert!(
            WebDavStorage::new("https://dav.example.com/dav", Some("user"), None, None).is_ok()
        );
    }

    fn cfg_with_backend(backend: &str) -> Config {
        let mut cfg = Config::for_test("storage-test");
        cfg.media_backend = backend.to_string();
        cfg
    }

    #[test]
    fn backend_selection_errors() {
        assert!(from_config(&cfg_with_backend("local")).is_ok());
        assert!(from_config(&cfg_with_backend("")).is_ok());
        // s3 现在是真实后端：环境变量齐全时构造成功，缺失时明确报配置错误。
        match from_config(&cfg_with_backend("s3")) {
            Ok(_) => {}
            Err(err) => assert!(
                err.contains("media backend `s3` is misconfigured"),
                "unexpected error: {err}"
            ),
        }
        match from_config(&cfg_with_backend("ftp")) {
            Err(err) => assert!(err.contains("unknown media backend"), "unexpected: {err}"),
            Ok(_) => panic!("unknown backend must be rejected"),
        }
    }

    // ---- S3 / SigV4 -------------------------------------------------------

    /// AWS 文档示例密钥（公开值，非真实凭证）。
    const DOC_SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
    const DOC_ACCESS: &str = "AKIAIOSFODNN7EXAMPLE";

    fn s3_path_style() -> S3Storage {
        S3Storage::new(S3Options {
            bucket: "my-bucket".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some("http://localhost:9000".to_string()),
            access_key: DOC_ACCESS.to_string(),
            secret_key: DOC_SECRET.to_string(),
            session_token: None,
            // 自定义端点默认 path-style（None 即走默认）。
            path_style: None,
            prefix: Some("media".to_string()),
        })
        .unwrap()
    }

    /// 已知向量（AWS 文档 Signing Key 示例，service=iam）。
    #[test]
    fn s3_signing_key_matches_aws_doc_vector() {
        let key = signing_key(DOC_SECRET, "20120215", "us-east-1", "iam");
        assert_eq!(
            hex_lower(&key),
            "f4780e2d9f65fa895f9c67b32ce1baf0b0d8a43505a000a1a9e090d414db404d"
        );
    }

    /// 固定向量：PUT 一个 body，Authorization 与独立实现（Python hmac/sha256）对齐。
    #[test]
    fn s3_authorization_matches_fixed_vector() {
        let storage = s3_path_style();
        let path = storage.object_path("t/u/id/blob");
        assert_eq!(path, "/my-bucket/media/t/u/id/blob");
        let payload_hash = sha256_hex(b"hello");
        assert_eq!(
            payload_hash,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        let auth = storage.authorization("PUT", &path, &payload_hash, "20130524T000000Z");
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;x-amz-content-sha256;x-amz-date, \
             Signature=ee11bd8561c1ebc4c4651075b1cf184c93ce2f616400d7eb5f7eaff29064b3ba"
        );
    }

    #[test]
    fn s3_empty_payload_hash_and_canonical_request_shaping() {
        assert_eq!(sha256_hex(b""), S3_EMPTY_PAYLOAD_SHA256);
        let headers = format!(
            "host:localhost:9000\nx-amz-content-sha256:{S3_EMPTY_PAYLOAD_SHA256}\nx-amz-date:20130524T000000Z\n"
        );
        let signed = "host;x-amz-content-sha256;x-amz-date";
        let creq = canonical_request(
            "GET",
            "/my-bucket/media/t/u/id/blob",
            "",
            &headers,
            signed,
            S3_EMPTY_PAYLOAD_SHA256,
        );
        // 第三行是空 query，头块后必须再多一个空行（SigV4 canonical request 规范）。
        assert_eq!(
            creq,
            format!(
                "GET\n/my-bucket/media/t/u/id/blob\n\n{headers}\n{signed}\n{S3_EMPTY_PAYLOAD_SHA256}"
            )
        );
        assert!(creq.contains("\n\nhost:localhost:9000\n"));
    }

    #[test]
    fn s3_session_token_is_signed() {
        let mut opts = S3Options::new("my-bucket", DOC_ACCESS, DOC_SECRET);
        opts.endpoint = Some("http://localhost:9000".to_string());
        opts.session_token = Some("SESSION-VALUE-123".to_string());
        let storage = S3Storage::new(opts).unwrap();
        let auth = storage.authorization(
            "GET",
            "/my-bucket/k",
            S3_EMPTY_PAYLOAD_SHA256,
            "20130524T000000Z",
        );
        assert!(
            auth.contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date;x-amz-security-token"),
            "unexpected authorization: {auth}"
        );
        // 密钥与 token 不得出现在 Authorization 里。
        assert!(!auth.contains(DOC_SECRET));
        assert!(!auth.contains("SESSION-VALUE-123"));
        // Debug 也不能泄漏密钥 / token。
        let debug = format!("{storage:?}");
        assert!(!debug.contains(DOC_SECRET), "debug leaked secret: {debug}");
        assert!(
            !debug.contains("SESSION-VALUE-123"),
            "debug leaked token: {debug}"
        );
    }

    #[test]
    fn s3_path_style_vs_virtual_host() {
        // path-style：bucket 进路径。
        let path_style = s3_path_style();
        assert!(path_style.path_style);
        assert_eq!(path_style.host, "localhost:9000");
        let (url, uri) = path_style.target("t/u/id/blob").unwrap();
        assert_eq!(uri, "/my-bucket/media/t/u/id/blob");
        assert_eq!(url.as_str(), "http://localhost:9000/my-bucket/media/t/u/id/blob");

        // virtual-hosted：bucket 进 Host，路径不带 bucket。
        let virtual_host = S3Storage::new(S3Options {
            bucket: "my-bucket".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some("https://s3.us-east-1.amazonaws.com".to_string()),
            access_key: DOC_ACCESS.to_string(),
            secret_key: DOC_SECRET.to_string(),
            session_token: None,
            path_style: Some(false),
            prefix: Some("/media/".to_string()),
        })
        .unwrap();
        assert_eq!(virtual_host.host, "my-bucket.s3.us-east-1.amazonaws.com");
        let (url, uri) = virtual_host.target("t/u/id/blob").unwrap();
        assert_eq!(uri, "/media/t/u/id/blob");
        assert_eq!(
            url.as_str(),
            "https://my-bucket.s3.us-east-1.amazonaws.com/media/t/u/id/blob"
        );

        // 未配 endpoint：默认 AWS 端点 + virtual-hosted style。
        let defaulted = S3Storage::new(S3Options::new("my-bucket", DOC_ACCESS, DOC_SECRET)).unwrap();
        assert!(!defaulted.path_style);
        assert_eq!(defaulted.host, "my-bucket.s3.us-east-1.amazonaws.com");
        assert_eq!(defaulted.scheme, "https");

        // 显式覆盖：自定义端点也可以要求 virtual-hosted。
        let mut opts = S3Options::new("my-bucket", DOC_ACCESS, DOC_SECRET);
        opts.endpoint = Some("http://localhost:9000".to_string());
        opts.path_style = Some(false);
        let forced = S3Storage::new(opts).unwrap();
        assert_eq!(forced.host, "my-bucket.localhost:9000");
        let (_, uri) = forced.target("k").unwrap();
        assert_eq!(uri, "/k");
    }

    #[test]
    fn s3_encodes_key_segments_and_prefix() {
        let storage = S3Storage::new(S3Options {
            bucket: "my-bucket".to_string(),
            region: "us-east-1".to_string(),
            endpoint: Some("http://localhost:9000".to_string()),
            access_key: DOC_ACCESS.to_string(),
            secret_key: DOC_SECRET.to_string(),
            session_token: None,
            path_style: Some(true),
            prefix: Some("team media".to_string()),
        })
        .unwrap();
        // 前缀与 key 都按 RFC 3986 编码，空格是 %20，非保留字符原样。
        let (url, uri) = storage.target("团队/a b+c/d").unwrap();
        assert_eq!(uri, "/my-bucket/team%20media/%E5%9B%A2%E9%98%9F/a%20b%2Bc/d");
        assert_eq!(
            url.as_str(),
            "http://localhost:9000/my-bucket/team%20media/%E5%9B%A2%E9%98%9F/a%20b%2Bc/d"
        );
    }

    #[test]
    fn s3_rejects_bad_keys() {
        let storage = s3_path_style();
        for bad in [
            "",
            "   ",
            "/leadingslash",
            "a//b",
            "./a",
            "a/./b",
            "a/../b",
            "..",
            "a/b?x=1",
            "a/b#frag",
            "a/b\\c",
            "a/b\nc",
            " padded",
        ] {
            assert!(
                storage.target(bad).is_err(),
                "key `{bad}` must be rejected"
            );
        }
        assert!(storage.target("1/2b6f3e9c/UUID/blob").is_ok());
    }

    #[test]
    fn s3_rejects_bad_configuration() {
        // 空 bucket / 空凭证 / 空 region 一律 fail closed。
        assert!(S3Storage::new(S3Options::new("", DOC_ACCESS, DOC_SECRET)).is_err());
        assert!(S3Storage::new(S3Options::new("b", "", DOC_SECRET)).is_err());
        assert!(S3Storage::new(S3Options::new("b", DOC_ACCESS, "")).is_err());
        let mut bad_region = S3Options::new("b", DOC_ACCESS, DOC_SECRET);
        bad_region.region = "  ".to_string();
        assert!(S3Storage::new(bad_region).is_err());

        // 端点只允许 http/https，且不得内嵌凭据 / query / fragment。
        for bad in ["ftp://minio.local", "minio.local:9000", "not a url"] {
            let mut opts = S3Options::new("b", DOC_ACCESS, DOC_SECRET);
            opts.endpoint = Some(bad.to_string());
            assert!(S3Storage::new(opts).is_err(), "endpoint `{bad}`");
        }
        let mut creds = S3Options::new("b", DOC_ACCESS, DOC_SECRET);
        creds.endpoint = Some("http://user:pw@minio.local:9000".to_string());
        assert!(S3Storage::new(creds).is_err());

        // 前缀与 bucket 也做校验。
        let mut bad_prefix = S3Options::new("b", DOC_ACCESS, DOC_SECRET);
        bad_prefix.prefix = Some("../escape".to_string());
        assert!(S3Storage::new(bad_prefix).is_err());
        let mut bad_bucket = S3Options::new("b", DOC_ACCESS, DOC_SECRET);
        bad_bucket.bucket = "a/b".to_string();
        assert!(S3Storage::new(bad_bucket).is_err());
    }

    #[test]
    fn s3_error_body_redacts_credentials() {
        let storage = s3_path_style();
        let body = format!(
            "<Error><Code>InvalidAccessKeyId</Code><Message>key {} / secret {} does not exist</Message></Error>",
            DOC_ACCESS, DOC_SECRET
        );
        let sanitized = storage.sanitize_error(&body);
        assert!(!sanitized.contains(DOC_ACCESS), "access key leaked: {sanitized}");
        assert!(!sanitized.contains(DOC_SECRET), "secret leaked: {sanitized}");
        assert!(sanitized.contains("<redacted>"));
        // 控制字符被压平，长响应被截断。
        let noisy = "a\nb".repeat(600);
        let cleaned = sanitize_body(&noisy);
        assert!(!cleaned.contains('\n'));
        assert!(cleaned.chars().count() <= 403);
    }

    #[test]
    fn s3_region_selects_default_endpoint() {
        let mut opts = S3Options::new("my-bucket", DOC_ACCESS, DOC_SECRET);
        opts.region = "ap-southeast-2".to_string();
        let storage = S3Storage::new(opts).unwrap();
        assert_eq!(
            storage.host,
            "my-bucket.s3.ap-southeast-2.amazonaws.com"
        );
        assert!(storage.target("k").unwrap().0.as_str().starts_with(
            "https://my-bucket.s3.ap-southeast-2.amazonaws.com/"
        ));
    }
}
