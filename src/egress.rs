//! S7：租户级 egress 正向代理。
//!
//! 沙箱通过 `HTTP(S)_PROXY` 指向本代理；代理从 `Proxy-Authorization: Basic <tenant>:<token>`
//! 识别租户，按套餐 `egress_allowlist` 放行，并在连接前解析 DNS、拒绝私网/回环/链路本地/
//! 云 metadata 地址（防 DNS rebinding 与内网穿透）。
//!
//! 支持 `CONNECT`（HTTPS 透传）与普通 HTTP absolute-form 请求；TLS 内容不解密，
//! 因此凭证哨兵不在本期（provider 密钥本来就只在服务端使用）。

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::STANDARD, Engine};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{lookup_host, TcpListener, TcpStream},
    sync::Semaphore,
};
use url::Url;
use uuid::Uuid;

use crate::{control::ControlState, error::AppError};

const MAX_HEADER_BYTES: usize = 32 * 1024;
const POLICY_TTL: Duration = Duration::from_secs(60);
const MAX_EGRESS_CONNS: usize = 256;
const EGRESS_TUNNEL_SECS: u64 = 600;

/// 租户代理令牌：HMAC-SHA256(SECRETS_KEY, tenant_uuid)，十六进制。
pub fn proxy_token(secrets_key: &str, tenant: Uuid) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secrets_key.as_bytes())
        .expect("hmac accepts any key length");
    mac.update(tenant.to_string().as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 允许列表：精确 host 或 `*.suffix` 通配（不含裸 IP）。
pub fn host_allowed(allow: &[String], host: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || host.parse::<IpAddr>().is_ok() {
        return false;
    }
    allow.iter().any(|rule| {
        let rule = rule.trim().to_ascii_lowercase();
        if let Some(suffix) = rule.strip_prefix("*.") {
            host.ends_with(&format!(".{suffix}"))
        } else {
            !rule.is_empty() && host == rule
        }
    })
}

pub fn forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_documentation()
                // 0.0.0.0/8
                || octets[0] == 0
                // 100.64.0.0/10 CGNAT
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                // 192.0.0.0/24 IETF protocol assignments
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
                // 192.88.99.0/24 6to4 relay
                || (octets[0] == 192 && octets[1] == 88 && octets[2] == 99)
                // 198.18.0.0/15 benchmarking
                || (octets[0] == 198 && (18..=19).contains(&octets[1]))
                // 240.0.0.0/4 reserved
                || octets[0] >= 240
                // 云 metadata
                || octets == [169, 254, 169, 254]
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return forbidden_ip(IpAddr::V4(v4));
            }
            let head = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (head[0] == 0x2001 && head[1] == 0x0db8) // documentation
                || (head[0] & 0xfe00) == 0xfc00 // fc00::/7 unique local
                || (head[0] & 0xffc0) == 0xfe80 // fe80::/10 link local
                || (head[0] == 0x2001 && head[1] == 0) // Teredo embeds IPv4
                || head[0] == 0x2002 // 6to4 embeds IPv4
                || (head[0] == 0x0064 && head[1] == 0xff9b) // NAT64
        }
    }
}

async fn connect_checked(host: &str, port: u16) -> Result<TcpStream, String> {
    if host.parse::<IpAddr>().is_ok() {
        return Err("IP literals are not allowed".into());
    }
    let addrs: Vec<SocketAddr> = tokio::time::timeout(
        Duration::from_secs(5),
        lookup_host((host, port)),
    )
    .await
    .map_err(|_| "dns: timeout".to_string())?
    .map_err(|e| format!("dns: {e}"))?
    .collect();
    if addrs.is_empty() {
        return Err("dns: no addresses".into());
    }
    for addr in &addrs {
        if forbidden_ip(addr.ip()) {
            return Err(format!("resolved to forbidden address {}", addr.ip()));
        }
    }
    // IPv4 优先，逐地址短超时：双栈环境下 IPv6 黑洞不会拖死整个连接。
    let mut ordered = addrs;
    ordered.sort_by_key(|addr| addr.is_ipv6());
    let mut last = String::from("no address");
    for addr in ordered {
        match tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(e)) => last = format!("{addr}: {e}"),
            Err(_) => last = format!("{addr}: connect timeout"),
        }
    }
    Err(format!("upstream connect failed ({last})"))
}

#[derive(Clone)]
struct Policy {
    mode: String,
    allow: Vec<String>,
}

pub struct EgressProxy {
    control: Arc<ControlState>,
    secrets_key: String,
    allow_plaintext: bool,
    counters: Arc<crate::admin::runtime::RuntimeCounters>,
    cache: Mutex<HashMap<Uuid, (Instant, Arc<Policy>)>>,
}

impl EgressProxy {
    pub fn new(
        control: Arc<ControlState>,
        secrets_key: String,
        allow_plaintext: bool,
        counters: Arc<crate::admin::runtime::RuntimeCounters>,
    ) -> Self {
        Self {
            control,
            secrets_key,
            allow_plaintext,
            counters,
            cache: Mutex::new(HashMap::new()),
        }
    }

    async fn policy(&self, tenant: Uuid) -> Result<Arc<Policy>, AppError> {
        if let Ok(cache) = self.cache.lock() {
            if let Some((at, policy)) = cache.get(&tenant) {
                if at.elapsed() < POLICY_TTL {
                    return Ok(policy.clone());
                }
            }
        }
        let row: Option<(String, Vec<String>)> = sqlx::query_as(
            "SELECT p.egress_mode, p.egress_allowlist
               FROM tenants t JOIN plans p ON p.id = t.plan_id
              WHERE t.id = $1 AND t.status = 'active'",
        )
        .bind(tenant)
        .fetch_optional(&self.control.db)
        .await?;
        let (mode, allow) = row.ok_or_else(|| AppError::forbidden("unknown tenant"))?;
        let policy = Arc::new(Policy { mode, allow });
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(tenant, (Instant::now(), policy.clone()));
        }
        Ok(policy)
    }

    async fn authorize(&self, header: Option<&str>) -> Result<Uuid, String> {
        let raw = header.ok_or("missing proxy authorization")?;
        let encoded = raw
            .strip_prefix("Basic ")
            .or_else(|| raw.strip_prefix("basic "))
            .ok_or("unsupported proxy auth")?;
        let decoded = STANDARD
            .decode(encoded.trim())
            .map_err(|_| "invalid proxy auth encoding")?;
        let text = String::from_utf8(decoded).map_err(|_| "invalid proxy auth")?;
        let (user, pass) = text.split_once(':').ok_or("invalid proxy auth")?;
        let tenant = Uuid::parse_str(user.trim()).map_err(|_| "invalid tenant in proxy auth")?;
        if proxy_token(&self.secrets_key, tenant) != pass.trim() {
            return Err("invalid proxy token".into());
        }
        let policy = self
            .policy(tenant)
            .await
            .map_err(|_| "tenant policy unavailable".to_string())?;
        if policy.mode != "proxy" {
            return Err("tenant egress mode does not allow proxy".into());
        }
        Ok(tenant)
    }

    async fn check_host(&self, tenant: Uuid, host: &str) -> Result<(), String> {
        let policy = self
            .policy(tenant)
            .await
            .map_err(|_| "tenant policy unavailable".to_string())?;
        if !host_allowed(&policy.allow, host) {
            return Err(format!("host {host} is not in the egress allowlist"));
        }
        Ok(())
    }
}

/// 启动代理监听（bind 由 `CHIRON_EGRESS_PROXY_BIND` 提供）。
pub async fn spawn(state: Arc<crate::state::AppState>, bind: String) -> std::io::Result<()> {
    let control = state
        .control
        .clone()
        .ok_or_else(|| std::io::Error::other("egress proxy requires control plane"))?;
    let proxy = Arc::new(EgressProxy::new(
        control,
        state.cfg.secrets_key.clone(),
        state.cfg.egress_allow_plaintext,
        state.runtime.clone(),
    ));
    let listener = TcpListener::bind(&bind).await?;
    let permits = Arc::new(Semaphore::new(MAX_EGRESS_CONNS));
    tracing::info!(bind = %bind, "egress proxy listening");
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    let Ok(permit) = permits.clone().try_acquire_owned() else {
                        tracing::warn!(%peer, "egress proxy connection cap reached");
                        drop(stream);
                        continue;
                    };
                    let proxy = proxy.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        if let Err(err) = handle(stream, proxy).await {
                            tracing::debug!(%peer, error = %err, "egress proxy connection closed");
                        }
                    });
                }
                Err(e) => tracing::warn!(error = %e, "egress proxy accept failed"),
            }
        }
    });
    Ok(())
}

async fn handle(mut stream: TcpStream, proxy: Arc<EgressProxy>) -> Result<(), String> {
    let head = read_head(&mut stream).await?;
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.split("\r\n");
    let request_line = lines.next().ok_or("empty request")?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().ok_or("bad request line")?;
    let target = parts.next().ok_or("bad request line")?;

    let mut headers: Vec<String> = lines
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    let auth = headers
        .iter()
        .find(|h| h.to_ascii_lowercase().starts_with("proxy-authorization:"))
        .map(|h| {
            h.split_once(':')
                .map(|(_, v)| v.trim().to_string())
                .unwrap_or_default()
        });

    let tenant = match proxy.authorize(auth.as_deref()).await {
        Ok(t) => t,
        Err(reason) => return deny(&mut stream, 407, &reason).await,
    };

    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = split_host_port(target, 443)?;
        if port != 443 {
            return deny(&mut stream, 403, "CONNECT is limited to port 443").await;
        }
        if let Err(reason) = proxy.check_host(tenant, &host).await {
            proxy.counters.egress_denied();
            return deny(&mut stream, 403, &reason).await;
        }
        let mut upstream = match connect_checked(&host, port).await {
            Ok(s) => s,
            Err(reason) => {
                proxy.counters.egress_denied();
                return deny(&mut stream, 502, &reason).await;
            }
        };
        stream
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .map_err(|e| e.to_string())?;
        proxy.counters.egress_allowed();
        let _ = tokio::time::timeout(
            Duration::from_secs(EGRESS_TUNNEL_SECS),
            tokio::io::copy_bidirectional(&mut stream, &mut upstream),
        )
        .await;
        return Ok(());
    }

    // 普通 HTTP：absolute-form → origin-form
    if !proxy.allow_plaintext {
        proxy.counters.egress_denied();
        return deny(
            &mut stream,
            403,
            "plaintext HTTP egress disabled; use HTTPS (CONNECT)",
        )
        .await;
    }
    let url =
        Url::parse(target).map_err(|_| "proxy requires absolute-form HTTP URL".to_string())?;
    if url.scheme() != "http" {
        return deny(&mut stream, 400, "only http and CONNECT are supported").await;
    }
    let host = url.host_str().ok_or("missing host")?.to_string();
    let port = url.port_or_known_default().unwrap_or(80);
    if port != 80 {
        return deny(&mut stream, 403, "plain HTTP is limited to port 80").await;
    }
    if let Err(reason) = proxy.check_host(tenant, &host).await {
        proxy.counters.egress_denied();
        return deny(&mut stream, 403, &reason).await;
    }
    headers.retain(|h| !h.to_ascii_lowercase().starts_with("proxy-authorization:"));
    if headers_carry_credentials(&headers) {
        proxy.counters.egress_denied();
        return deny(
            &mut stream,
            403,
            "refusing credentials over plaintext HTTP; use HTTPS",
        )
        .await;
    }
    let mut path = url.path().to_string();
    if let Some(query) = url.query() {
        path.push('?');
        path.push_str(query);
    }
    let mut out = format!("{method} {path} HTTP/1.1\r\n");
    for header in &headers {
        out.push_str(header);
        out.push_str("\r\n");
    }
    out.push_str("\r\n");
    let mut upstream = match connect_checked(&host, port).await {
        Ok(stream) => stream,
        Err(reason) => {
            proxy.counters.egress_denied();
            return deny(&mut stream, 502, &format!("upstream connect: {reason}")).await;
        }
    };
    upstream
        .write_all(out.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    proxy.counters.egress_allowed();
    let _ = tokio::time::timeout(
        Duration::from_secs(EGRESS_TUNNEL_SECS),
        tokio::io::copy_bidirectional(&mut stream, &mut upstream),
    )
    .await;
    Ok(())
}

async fn read_head(stream: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .map_err(|_| "header read timeout".to_string())?
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("connection closed".into());
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            return Ok(buf);
        }
        if buf.len() > MAX_HEADER_BYTES {
            return Err("headers too large".into());
        }
    }
}

async fn deny(stream: &mut TcpStream, status: u16, reason: &str) -> Result<(), String> {
    let body = format!("egress denied: {reason}\n");
    let reason_phrase = match status {
        407 => "Proxy Authentication Required",
        502 => "Bad Gateway",
        400 => "Bad Request",
        _ => "Forbidden",
    };
    let resp = format!(
        "HTTP/1.1 {status} {reason_phrase}\r\nProxy-Authenticate: Basic realm=\"chiron\"\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    Ok(())
}

fn headers_carry_credentials(headers: &[String]) -> bool {
    headers.iter().any(|header| {
        let name = header
            .split_once(':')
            .map(|(name, _)| name.trim().to_ascii_lowercase())
            .unwrap_or_default();
        matches!(
            name.as_str(),
            "authorization" | "cookie" | "x-api-key" | "api-key" | "x-auth-token"
        )
    })
}

fn split_host_port(target: &str, default_port: u16) -> Result<(String, u16), String> {    if let Some((host, port)) = target.rsplit_once(':') {
        let port: u16 = port
            .parse()
            .map_err(|_| "invalid CONNECT port".to_string())?;
        Ok((host.trim_matches(['[', ']']).to_string(), port))
    } else {
        Ok((target.to_string(), default_port))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plaintext_credential_headers_are_detected() {
        assert!(headers_carry_credentials(&[
            "Authorization: Bearer x".into()
        ]));
        assert!(headers_carry_credentials(&["cookie: a=b".into()]));
        assert!(headers_carry_credentials(&["X-Api-Key: k".into()]));
        assert!(!headers_carry_credentials(&[
            "Accept: */*".into(),
            "User-Agent: pip".into()
        ]));
    }

    #[test]
    fn wildcard_and_exact_allowlist() {
        let allow = vec!["github.com".into(), "*.githubusercontent.com".into()];
        assert!(host_allowed(&allow, "GitHub.com."));
        assert!(host_allowed(&allow, "raw.githubusercontent.com"));
        assert!(!host_allowed(&allow, "githubusercontent.com"));
        assert!(!host_allowed(&allow, "evil.com"));
        assert!(!host_allowed(&allow, "127.0.0.1"));
    }

    #[test]
    fn forbidden_ranges_cover_private_and_metadata() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "::1",
            "fc00::1",
            "fe80::1",
        ] {
            assert!(
                forbidden_ip(ip.parse().unwrap()),
                "expected forbidden: {ip}"
            );
        }
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(!forbidden_ip(ip.parse().unwrap()), "expected public: {ip}");
        }
    }

    #[test]
    fn token_is_keyed_and_stable() {
        let t = Uuid::nil();
        assert_eq!(proxy_token("k1", t), proxy_token("k1", t));
        assert_ne!(proxy_token("k1", t), proxy_token("k2", t));
        assert_eq!(proxy_token("k1", t).len(), 64);
    }
}
